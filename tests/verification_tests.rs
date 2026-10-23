use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use local_agent_harness::agent::verify::{
    CommandOutput, LocalCommandExecutor, VerificationCommand, VerificationExecutor, VerifyReport,
    VerifyStep, detect, run, run_with_executor,
};
use local_agent_harness::error::AgentError;
use uuid::Uuid;

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!("rupi-verify-{label}-{}", Uuid::new_v4()));
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

fn command(root: &Path, program: &str, args: &[&str], timeout: Duration) -> VerificationCommand {
    VerificationCommand {
        program: program.into(),
        args: args.iter().map(|arg| (*arg).to_owned()).collect(),
        cwd: root.to_path_buf(),
        timeout,
    }
}

fn output(
    exit_code: i32,
    stdout: impl Into<String>,
    stderr: impl Into<String>,
    timed_out: bool,
    millis: u64,
) -> CommandOutput {
    CommandOutput {
        exit_code,
        timed_out,
        duration: Duration::from_millis(millis),
        stdout: stdout.into(),
        stderr: stderr.into(),
    }
}

struct FakeExecutor {
    outputs: Mutex<VecDeque<CommandOutput>>,
    seen: Mutex<Vec<VerificationCommand>>,
}

impl FakeExecutor {
    fn new(outputs: Vec<CommandOutput>) -> Self {
        Self {
            outputs: Mutex::new(outputs.into()),
            seen: Mutex::new(Vec::new()),
        }
    }

    fn seen(&self) -> Vec<VerificationCommand> {
        self.seen.lock().expect("seen lock").clone()
    }
}

#[async_trait::async_trait]
impl VerificationExecutor for FakeExecutor {
    async fn execute(
        &self,
        command: &VerificationCommand,
        _output_cap: usize,
    ) -> local_agent_harness::error::Result<CommandOutput> {
        self.seen.lock().expect("seen lock").push(command.clone());
        Ok(self
            .outputs
            .lock()
            .expect("outputs lock")
            .pop_front()
            .unwrap_or_else(|| output(0, "", "", false, 0)))
    }
}

#[test]
fn detects_rust_commands_without_shell_strings() {
    let root = TempDir::new("rust");
    root.write("Cargo.toml", "[package]\nname = \"fixture\"\n");

    let commands = detect(root.path()).expect("detect rust project");
    assert_eq!(commands.len(), 3);
    assert_eq!(commands[0].program, "cargo");
    assert_eq!(commands[0].args, ["fmt", "--check"]);
    assert_eq!(commands[1].args, ["check", "--all-targets"]);
    assert_eq!(commands[2].args, ["test"]);
    for command in &commands {
        assert_eq!(
            command.cwd,
            root.path().canonicalize().expect("canonical root")
        );
        assert!(command.timeout > Duration::ZERO);
    }
}

#[test]
fn detects_node_scripts_and_rejects_malformed_manifests() {
    let root = TempDir::new("node");
    root.write(
        "package.json",
        r#"{"scripts":{"test":"node test.js","build":"node build.js","typecheck":"tsc --noEmit"}}"#,
    );

    let commands = detect(root.path()).expect("detect node project");
    assert_eq!(commands.len(), 3);
    assert_eq!(commands[0].program, "npm");
    assert_eq!(commands[0].args, ["run", "test"]);
    assert_eq!(commands[1].args, ["run", "build"]);
    assert_eq!(commands[2].args, ["run", "typecheck"]);

    root.write("package.json", "{not json");
    assert!(matches!(
        detect(root.path()),
        Err(AgentError::Blocked(message)) if message.contains("package.json")
    ));
}

#[test]
fn detects_python_tests_and_prefers_a_project_local_venv() {
    let root = TempDir::new("python");
    root.write("pyproject.toml", "[project]\nname = \"fixture\"\n");

    let commands = detect(root.path()).expect("detect python project");
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0].program, "python3");
    assert_eq!(commands[0].args, ["-m", "pytest", "-q"]);

    root.write(".venv/bin/python", "");
    let commands = detect(root.path()).expect("detect python project with venv");
    assert_eq!(commands[0].program, ".venv/bin/python");
}

#[test]
fn multiple_ecosystems_and_missing_manifests_are_handled_deterministically() {
    let empty = TempDir::new("empty");
    assert!(
        detect(empty.path())
            .expect("detect empty project")
            .is_empty()
    );

    let root = TempDir::new("multi");
    root.write("Cargo.toml", "[package]\nname = \"fixture\"\n");
    root.write("package.json", r#"{"scripts":{"test":"node test.js"}}"#);
    root.write("requirements.txt", "pytest\n");

    let commands = detect(root.path()).expect("detect multiple ecosystems");
    assert_eq!(commands.len(), 5);
    assert_eq!(commands[0].program, "cargo");
    assert_eq!(commands[3].program, "npm");
    assert_eq!(commands[4].program, "python3");
}

#[test]
fn rejects_oversized_manifests() {
    let root = TempDir::new("oversized");
    let oversized = vec![b' '; 1024 * 1024 + 1];
    std::fs::write(root.path().join("package.json"), oversized).expect("write oversized manifest");

    assert!(matches!(
        detect(root.path()),
        Err(AgentError::Blocked(message)) if message.contains("size limit")
    ));
}

#[test]
fn no_commands_is_a_blocked_report_not_a_pass() {
    let root = TempDir::new("no-verifier");
    let report = tokio::runtime::Runtime::new()
        .expect("runtime")
        .block_on(run(root.path(), &[], 128))
        .expect("empty verification report");

    assert!(!report.passed);
    assert!(report.steps.is_empty());
    assert!(report.tail.contains("no verification"));
    assert!(!report.verified());
}

#[tokio::test]
async fn runner_records_exact_exit_status_and_stops_after_failure() {
    let root = TempDir::new("failure");
    let commands = [
        command(root.path(), "fake", &["first"], Duration::from_secs(5)),
        command(root.path(), "fake", &["second"], Duration::from_secs(5)),
    ];
    let executor = FakeExecutor::new(vec![
        output(7, "HEAD-", "-TAIL", false, 12),
        output(0, "should not run", "", false, 1),
    ]);

    let report = run_with_executor(root.path(), &commands, 64, &executor)
        .await
        .expect("run fake verifier");

    assert!(!report.passed);
    assert!(!report.verified());
    assert_eq!(report.steps.len(), 1);
    assert_eq!(report.steps[0].exit_code, 7);
    assert_eq!(report.steps[0].duration, Duration::from_millis(12));
    assert!(!report.steps[0].timed_out);
    assert!(report.steps[0].command.contains("fake first"));
    assert!(report.tail.contains("HEAD-"));
    assert!(report.tail.contains("-TAIL"));
    assert_eq!(executor.seen().len(), 1);
}

#[tokio::test]
async fn runner_bounds_head_and_tail_output_and_passes_only_on_success() {
    let root = TempDir::new("bounded");
    let long_stdout = format!("HEAD{}TAIL", "x".repeat(10_000));
    let commands = [
        command(root.path(), "fake", &["one"], Duration::from_secs(5)),
        command(root.path(), "fake", &["two"], Duration::from_secs(5)),
    ];
    let executor = FakeExecutor::new(vec![
        output(0, "", "", false, 1),
        output(0, &long_stdout, "", false, 2),
    ]);

    let report = run_with_executor(root.path(), &commands, 64, &executor)
        .await
        .expect("run fake verifier");
    assert!(report.passed);
    assert!(report.verified());
    assert_eq!(report.steps.len(), 2);
    assert!(report.tail.is_empty());

    let failing = FakeExecutor::new(vec![output(1, &long_stdout, "", false, 2)]);
    let report = run_with_executor(root.path(), &commands[..1], 64, &failing)
        .await
        .expect("run failing fake verifier");
    assert!(!report.passed);
    assert!(report.tail.contains("HEAD"));
    assert!(report.tail.contains("TAIL"));
    assert!(report.tail.contains("bytes omitted"));
    assert!(report.tail.len() < long_stdout.len());
}

#[tokio::test]
async fn local_executor_records_real_status_and_bounded_output() {
    let root = TempDir::new("local-output");
    let long_stdout = format!("HEAD{}TAIL", "y".repeat(10_000));
    let printf_command = command(
        root.path(),
        "printf",
        &["%s", &long_stdout],
        Duration::from_secs(5),
    );

    let output = LocalCommandExecutor
        .execute(&printf_command, 64)
        .await
        .expect("run printf");
    assert_eq!(output.exit_code, 0);
    assert!(!output.timed_out);
    assert!(output.duration > Duration::ZERO);
    assert!(output.stdout.contains("HEAD"));
    assert!(output.stdout.contains("TAIL"));
    assert!(output.stdout.contains("bytes omitted"));
    assert!(output.stdout.len() < long_stdout.len());

    let report = run(
        root.path(),
        &[command(root.path(), "false", &[], Duration::from_secs(5))],
        128,
    )
    .await
    .expect("run false");
    assert!(!report.passed);
    assert_eq!(report.steps.len(), 1);
    assert_eq!(report.steps[0].exit_code, 1);
    assert!(!report.steps[0].timed_out);
}

#[tokio::test]
async fn runner_records_timeout_and_kills_the_process() {
    let root = TempDir::new("timeout");
    let commands = [command(
        root.path(),
        "sleep",
        &["2"],
        Duration::from_millis(50),
    )];

    let report = run(root.path(), &commands, 128)
        .await
        .expect("run timed-out command");
    assert!(!report.passed);
    assert_eq!(report.steps.len(), 1);
    assert!(report.steps[0].timed_out);
    assert_eq!(report.steps[0].exit_code, -2);
    assert!(report.steps[0].duration >= Duration::from_millis(40));
    assert!(report.tail.contains("timed out"));
}

#[tokio::test]
async fn runner_rejects_shell_programs_zero_timeouts_and_cwd_escape() {
    let root = TempDir::new("reject");
    let outside = TempDir::new("outside");

    for program in ["sh", "bash", "/bin/sh", "sudo", "env"] {
        let commands = [command(
            root.path(),
            program,
            &["-c", "true"],
            Duration::from_secs(1),
        )];
        assert!(
            matches!(
                run(root.path(), &commands, 64).await,
                Err(AgentError::Blocked(_))
            ),
            "{program} should be rejected"
        );
    }

    let commands = [command(root.path(), "true", &[], Duration::from_secs(0))];
    assert!(matches!(
        run(root.path(), &commands, 64).await,
        Err(AgentError::Blocked(_))
    ));

    let commands = [VerificationCommand {
        program: "true".into(),
        args: Vec::new(),
        cwd: outside.path().to_path_buf(),
        timeout: Duration::from_secs(1),
    }];
    assert!(matches!(
        run(root.path(), &commands, 64).await,
        Err(AgentError::Blocked(_))
    ));
}

#[tokio::test]
async fn argv_execution_does_not_interpret_shell_metacharacters() {
    let root = TempDir::new("no-shell");
    let commands = [command(
        root.path(),
        "printf",
        &["%s", "$(touch pwned); `touch pwned`"],
        Duration::from_secs(5),
    )];

    let report = run(root.path(), &commands, 128)
        .await
        .expect("run printf without a shell");
    assert!(report.passed);
    assert!(!root.path().join("pwned").exists());
}

#[test]
fn reports_cannot_pass_without_real_successful_steps() {
    let empty = VerifyReport::from_steps(Vec::new(), "MODEL CLAIMED DONE");
    assert!(!empty.passed);
    assert!(!empty.verified());

    let failed = VerifyReport::from_steps(
        vec![VerifyStep {
            command: "cargo test".into(),
            exit_code: 1,
            duration: Duration::from_millis(1),
            timed_out: false,
        }],
        "passed",
    );
    assert!(!failed.passed);

    let timed_out = VerifyReport::from_steps(
        vec![VerifyStep {
            command: "cargo test".into(),
            exit_code: 0,
            duration: Duration::from_millis(1),
            timed_out: true,
        }],
        "passed",
    );
    assert!(!timed_out.passed);

    let passed = VerifyReport::from_steps(
        vec![VerifyStep {
            command: "cargo test".into(),
            exit_code: 0,
            duration: Duration::from_millis(1),
            timed_out: false,
        }],
        "MODEL CLAIMED FAILURE",
    );
    assert!(passed.passed);
    assert!(passed.verified());
}
