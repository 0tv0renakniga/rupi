use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use local_agent_harness::agent::verify::{VerificationCommand, VerifyReport};
use local_agent_harness::error::AgentError;
use local_agent_harness::orchestrator::VerificationEngine;
use local_agent_harness::script_runner::ScriptRunner;
use local_agent_harness::script_verification::ScriptVerificationEngine;
use uuid::Uuid;

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "rupi-script-verification-{label}-{}",
            Uuid::new_v4()
        ));
        std::fs::create_dir_all(&path).expect("create temp directory");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn write(&self, relative: &str, contents: &str) {
        let path = self.path.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create fixture parent");
        }
        std::fs::write(path, contents).expect("write fixture");
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn python_interpreter() -> PathBuf {
    for candidate in [
        "/usr/bin/python3",
        "/usr/local/bin/python3",
        "/bin/python3",
        "/usr/bin/python",
    ] {
        let path = PathBuf::from(candidate);
        if path.is_file() {
            return path;
        }
    }

    if let Some(paths) = std::env::var_os("PATH") {
        for directory in std::env::split_paths(&paths) {
            for name in ["python3", "python3.exe", "python", "python.exe"] {
                let candidate = directory.join(name);
                if candidate.is_file() {
                    return candidate;
                }
            }
        }
    }

    panic!("python3 is required for script verification tests");
}

fn scripts_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts")
}

fn engine(root: &Path) -> ScriptVerificationEngine {
    ScriptVerificationEngine::new(
        ScriptRunner::new(root, scripts_dir(), python_interpreter()).expect("script runner"),
    )
}

fn fixture(root: &Path) -> PathBuf {
    let path = root.join("fixture.py");
    std::fs::write(
        &path,
        r#"
import json
import os
import sys
import time

mode = sys.argv[1]
if mode == "argv":
    print(json.dumps(sys.argv[2:]))
elif mode == "exit":
    print("fixture stdout")
    print("fixture stderr", file=sys.stderr)
    raise SystemExit(int(sys.argv[2]))
elif mode == "sleep":
    time.sleep(5)
elif mode == "output":
    sys.stdout.write("O" * 5000)
    sys.stderr.write("E" * 5000)
    raise SystemExit(3)
elif mode == "pid":
    print(os.getpid())
else:
    raise SystemExit(9)
"#,
    )
    .expect("write fixture");
    path
}

fn command(root: &Path, fixture: &Path, mode: &str, timeout: Duration) -> VerificationCommand {
    VerificationCommand {
        program: python_interpreter().to_string_lossy().into_owned(),
        args: vec![fixture.to_string_lossy().into_owned(), mode.to_owned()],
        cwd: root.to_path_buf(),
        timeout,
    }
}

#[tokio::test]
async fn detects_the_same_profiles_without_model_control() {
    let root = TempDir::new("detect");
    root.write("Cargo.toml", "[package]\nname = \"fixture\"\n");
    root.write(
        "package.json",
        r#"{"scripts":{"test":"node test.js","build":"node build.js","typecheck":"tsc --noEmit"}}"#,
    );
    root.write("pyproject.toml", "[project]\nname = \"fixture\"\n");

    let commands = engine(root.path())
        .detect(root.path())
        .await
        .expect("detect profiles");

    assert_eq!(commands.len(), 7);
    assert_eq!(commands[0].program, "cargo");
    assert_eq!(commands[0].args, ["fmt", "--check"]);
    assert_eq!(commands[1].program, "cargo");
    assert_eq!(commands[1].args, ["check", "--all-targets"]);
    assert_eq!(commands[2].program, "cargo");
    assert_eq!(commands[2].args, ["test"]);
    assert_eq!(commands[3].program, "npm");
    assert_eq!(commands[3].args, ["run", "test"]);
    assert_eq!(commands[4].program, "npm");
    assert_eq!(commands[4].args, ["run", "build"]);
    assert_eq!(commands[5].program, "npm");
    assert_eq!(commands[5].args, ["run", "typecheck"]);
    assert_eq!(commands[6].program, "python3");
    assert_eq!(commands[6].args, ["-m", "pytest", "-q"]);
    for command in &commands {
        assert_eq!(
            command.cwd,
            root.path().canonicalize().expect("canonical root")
        );
    }
}

#[tokio::test]
async fn runs_argv_through_the_fixed_helper_and_preserves_success() {
    let root = TempDir::new("success");
    let fixture = fixture(root.path());
    let args = ["two words", "$HOME", "semi;colon", "quote'and\"double", "*"];
    let mut run = command(root.path(), &fixture, "argv", Duration::from_secs(5));
    run.args.extend(args.iter().map(|arg| (*arg).to_owned()));

    let report = engine(root.path())
        .run(root.path(), &[run], 4096)
        .await
        .expect("run verifier");
    assert!(report.passed, "{report:?}");
    assert!(report.verified(), "{report:?}");
    assert_eq!(report.steps.len(), 1);
    assert_eq!(report.steps[0].exit_code, 0);
    assert!(!report.steps[0].timed_out);
    assert!(report.tail.is_empty(), "{}", report.tail);
}

#[tokio::test]
async fn failing_exit_status_becomes_bounded_blocking_evidence() {
    let root = TempDir::new("failure");
    let fixture = fixture(root.path());
    let mut run = command(root.path(), &fixture, "exit", Duration::from_secs(5));
    run.args.push("7".into());

    let report = engine(root.path())
        .run(root.path(), &[run], 4096)
        .await
        .expect("run verifier");
    assert!(!report.passed);
    assert!(!report.verified());
    assert_eq!(report.steps.len(), 1);
    assert_eq!(report.steps[0].exit_code, 7);
    assert!(report.tail.contains("fixture stdout"), "{}", report.tail);
    assert!(report.tail.contains("fixture stderr"), "{}", report.tail);
}

#[tokio::test]
async fn timeout_is_reported_and_keeps_the_tail_bounded() {
    let root = TempDir::new("timeout");
    let fixture = fixture(root.path());
    let run = command(root.path(), &fixture, "sleep", Duration::from_millis(75));
    let started = Instant::now();

    let report = engine(root.path())
        .run(root.path(), &[run], 256)
        .await
        .expect("run verifier");
    let elapsed = started.elapsed();

    assert!(!report.passed);
    assert_eq!(report.steps.len(), 1);
    assert!(report.steps[0].timed_out);
    assert_eq!(report.steps[0].exit_code, -2);
    assert!(
        report.tail.contains("verification timed out"),
        "{}",
        report.tail
    );
    assert!(report.tail.len() <= 512, "{}", report.tail.len());
    assert!(elapsed < Duration::from_secs(3), "{elapsed:?}");
}

#[tokio::test]
async fn output_is_bounded_before_it_reaches_verification_evidence() {
    let root = TempDir::new("output");
    let fixture = fixture(root.path());
    let run = command(root.path(), &fixture, "output", Duration::from_secs(5));

    let report = engine(root.path())
        .run(root.path(), &[run], 100)
        .await
        .expect("run verifier");
    assert!(!report.passed);
    assert!(report.tail.contains("bytes omitted"), "{}", report.tail);
    assert!(report.tail.len() <= 512, "{}", report.tail);
}

#[tokio::test]
async fn missing_dependency_is_a_structured_blocker_not_a_pass() {
    let root = TempDir::new("missing-program");
    let run = VerificationCommand {
        program: "definitely-not-a-real-verifier-program".into(),
        args: Vec::new(),
        cwd: root.path().to_path_buf(),
        timeout: Duration::from_secs(2),
    };

    let report = engine(root.path())
        .run(root.path(), &[run], 4096)
        .await
        .expect("run verifier");
    assert!(!report.passed);
    assert!(!report.verified());
    assert_eq!(report.steps.len(), 1);
    assert_eq!(report.steps[0].exit_code, -1);
    assert!(
        report.tail.contains("VERIFIER_START_FAILED"),
        "{}",
        report.tail
    );
}

#[tokio::test]
async fn empty_or_unsupported_projects_block_instead_of_passing() {
    let root = TempDir::new("empty");
    let report = engine(root.path())
        .run(root.path(), &[], 4096)
        .await
        .expect("run verifier");
    assert_unsupported(&report);
    assert!(report.tail.contains("no verification commands"));

    let commands = engine(root.path())
        .detect(root.path())
        .await
        .expect("detect empty project");
    assert!(commands.is_empty());
}

#[tokio::test]
async fn cwd_and_root_must_stay_inside_the_fixed_runtime_root() {
    let root = TempDir::new("root");
    let elsewhere = TempDir::new("other-root");
    let fixture = fixture(root.path());
    let mut run = command(root.path(), &fixture, "exit", Duration::from_secs(2));
    run.args.push("0".into());
    run.cwd = elsewhere.path().to_path_buf();

    let error = engine(root.path())
        .run(root.path(), &[run], 4096)
        .await
        .expect_err("outside cwd must be rejected");
    assert!(
        matches!(error, AgentError::Blocked(ref message) if message.contains("escapes")),
        "{error}"
    );

    let error = engine(root.path())
        .detect(elsewhere.path())
        .await
        .expect_err("mismatched root must be rejected");
    assert!(
        matches!(error, AgentError::Blocked(ref message) if message.contains("does not match")),
        "{error}"
    );
}

fn assert_unsupported(report: &VerifyReport) {
    assert!(!report.passed);
    assert!(!report.verified());
    assert!(report.steps.is_empty());
}
