use std::path::{Path, PathBuf};

use local_agent_harness::error::AgentError;
use local_agent_harness::script_runner::{ActionId, ScriptRunner};
use serde_json::Value;
use uuid::Uuid;

const RECEIPT_SCRIPT: &str = r#"
import json
import os
import sys

receipt = {
    "argv": sys.argv,
    "cwd": os.getcwd(),
    "executable": sys.executable,
}

try:
    with open("/proc/self/cmdline", "rb") as handle:
        receipt["cmdline"] = [
            part.decode("utf-8", "replace")
            for part in handle.read().split(b"\0")
            if part
        ]
except OSError:
    pass

with open("script-receipt.json", "w", encoding="utf-8") as handle:
    json.dump(receipt, handle)
"#;

const EXPECTED_MAPPINGS: [(ActionId, &str); 5] = [
    (ActionId::ReadFile, "read_file.py"),
    (ActionId::GitStatus, "git_status.py"),
    (ActionId::GitDiff, "git_diff.py"),
    (ActionId::WriteFile, "write_file.py"),
    (ActionId::RunVerifier, "run_verifier.py"),
];

struct TempLayout {
    base: PathBuf,
    project: PathBuf,
    scripts: PathBuf,
    python: PathBuf,
}

impl TempLayout {
    fn new(label: &str) -> Self {
        let base =
            std::env::temp_dir().join(format!("rupi-script-runner-{label}-{}", Uuid::new_v4()));
        let project = base.join("project");
        let scripts = base.join("scripts");
        std::fs::create_dir_all(&project).expect("create project fixture");
        std::fs::create_dir_all(&scripts).expect("create scripts fixture");

        Self {
            base,
            project,
            scripts,
            python: python_interpreter(),
        }
    }

    fn runner(&self) -> ScriptRunner {
        ScriptRunner::new(&self.project, &self.scripts, &self.python).expect("construct runner")
    }

    fn canonical_project(&self) -> PathBuf {
        self.project.canonicalize().expect("canonical project")
    }

    fn canonical_scripts(&self) -> PathBuf {
        self.scripts.canonicalize().expect("canonical scripts")
    }

    fn write_script(&self, name: &str, contents: &str) -> PathBuf {
        let path = self.scripts.join(name);
        std::fs::write(&path, contents).expect("write mock script");
        path.canonicalize().expect("canonical mock script")
    }

    fn write_all_scripts(&self) {
        for (_, name) in EXPECTED_MAPPINGS {
            self.write_script(name, RECEIPT_SCRIPT);
        }
    }

    fn receipt(&self) -> Value {
        let path = self.project.join("script-receipt.json");
        let contents = std::fs::read_to_string(&path).expect("read mock receipt");
        serde_json::from_str(&contents).expect("parse mock receipt")
    }
}

impl Drop for TempLayout {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
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
            return path.canonicalize().expect("canonical python");
        }
    }

    if let Some(paths) = std::env::var_os("PATH") {
        for directory in std::env::split_paths(&paths) {
            for name in ["python3", "python3.exe", "python", "python.exe"] {
                let candidate = directory.join(name);
                if candidate.is_file() {
                    return candidate.canonicalize().expect("canonical python");
                }
            }
        }
    }

    panic!("python3 is required for script-runner tests");
}

#[test]
fn known_actions_resolve_only_to_fixed_canonical_mappings() {
    let layout = TempLayout::new("mappings");
    layout.write_all_scripts();
    let runner = layout.runner();
    let project_root = layout.canonical_project();
    let script_root = layout.canonical_scripts();
    let python = layout.python.canonicalize().expect("canonical python");

    for (action, file_name) in EXPECTED_MAPPINGS {
        let invocation = runner.invocation(action).expect("resolve known action");
        assert_eq!(invocation.action(), action);
        assert_eq!(invocation.interpreter(), python);
        assert_eq!(invocation.script(), script_root.join(file_name));
        assert_eq!(invocation.working_directory(), project_root);
        assert_eq!(
            invocation.action().script_file_name(),
            file_name,
            "action {action} must retain its fixed script name"
        );
    }
}

#[test]
fn unknown_actions_and_model_controlled_identifiers_are_rejected() {
    for raw in [
        "",
        "ReadFile",
        "read_file.py",
        "scripts/read_file.py",
        "/bin/sh",
        "../read_file",
        "./read_file",
        "run_command",
        "run_shell",
        "apply_patch",
        "read_file\0",
        "read_file ",
    ] {
        assert!(
            ActionId::from_identifier(raw).is_err(),
            "`{raw}` must be rejected"
        );
        assert!(
            serde_json::from_value::<ActionId>(Value::String(raw.to_owned())).is_err(),
            "`{raw}` must not deserialize as an action"
        );
    }
}

#[test]
fn model_controlled_repository_content_cannot_select_the_script() {
    let layout = TempLayout::new("decoy");
    layout.write_all_scripts();
    std::fs::write(
        layout.project.join("read_file.py"),
        "raise SystemExit('model-controlled decoy executed')",
    )
    .expect("write decoy script");

    let runner = layout.runner();
    let invocation = runner
        .invocation(ActionId::ReadFile)
        .expect("resolve action");
    assert_eq!(
        invocation.script(),
        layout.canonical_scripts().join("read_file.py")
    );
    assert!(
        !invocation.script().starts_with(layout.canonical_project()),
        "the project root must not be able to supply helper scripts"
    );
}

#[tokio::test]
async fn known_action_starts_the_mapped_script_in_the_project_root() {
    let layout = TempLayout::new("launch");
    let expected_script = layout.write_script("read_file.py", RECEIPT_SCRIPT);
    let runner = layout.runner();

    let mut process = runner
        .launch(ActionId::ReadFile)
        .await
        .expect("launch known action");
    let status = process.wait().await.expect("wait for mock script");
    assert!(status.success(), "mock script failed: {status}");

    let receipt = layout.receipt();
    let argv = receipt["argv"].as_array().expect("argv array");
    assert_eq!(argv.len(), 1, "script receives only its own path in argv");
    assert_eq!(
        argv[0].as_str().expect("argv[0]"),
        expected_script.to_string_lossy()
    );
    assert_eq!(
        PathBuf::from(receipt["cwd"].as_str().expect("cwd")),
        layout.canonical_project()
    );
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn child_is_launched_directly_without_a_shell() {
    let layout = TempLayout::new("no-shell");
    let expected_script = layout.write_script("read_file.py", RECEIPT_SCRIPT);
    let runner = layout.runner();

    let mut process = runner
        .launch(ActionId::ReadFile)
        .await
        .expect("launch known action");
    assert!(process.wait().await.expect("wait").success());

    let receipt = layout.receipt();
    let cmdline = receipt["cmdline"]
        .as_array()
        .expect("/proc/self/cmdline must be captured on Linux")
        .iter()
        .map(|part| part.as_str().expect("cmdline string"))
        .collect::<Vec<_>>();

    assert!(cmdline.len() >= 2);
    assert_eq!(
        PathBuf::from(cmdline[0]),
        layout.python.canonicalize().expect("canonical python")
    );
    assert_eq!(PathBuf::from(cmdline[1]), expected_script);
    assert!(
        cmdline.iter().all(|argument| *argument != "-c"),
        "no inline-code flag may be used: {cmdline:?}"
    );

    let program_name = Path::new(cmdline[0])
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    for shell in ["sh", "bash", "zsh", "dash", "ksh", "fish"] {
        assert!(
            program_name != shell,
            "helper script was launched through shell `{shell}`"
        );
    }
}

#[cfg(unix)]
#[test]
fn shell_interpreters_are_rejected() {
    let layout = TempLayout::new("shell-interpreter");
    let shell = PathBuf::from("/bin/sh");
    if !shell.exists() {
        return;
    }

    let error = ScriptRunner::new(&layout.project, &layout.scripts, &shell)
        .expect_err("shell interpreter must be rejected");
    assert!(
        matches!(error, AgentError::Script(ref message) if message.contains("shell interpreters")),
        "unexpected error: {error}"
    );
}

#[test]
fn relative_interpreter_paths_do_not_use_path_lookup() {
    let layout = TempLayout::new("relative-interpreter");
    let error = ScriptRunner::new(&layout.project, &layout.scripts, "python3")
        .expect_err("relative interpreter must be rejected");
    assert!(
        matches!(error, AgentError::Script(ref message) if message.contains("absolute")),
        "unexpected error: {error}"
    );
}

#[tokio::test]
async fn missing_scripts_fail_safely() {
    let layout = TempLayout::new("missing");
    let runner = layout.runner();
    assert!(runner.invocation(ActionId::ReadFile).is_err());

    let error = runner
        .launch(ActionId::ReadFile)
        .await
        .expect_err("missing script must not launch");
    assert!(
        matches!(error, AgentError::Script(_)),
        "unexpected error: {error}"
    );
}

#[cfg(unix)]
#[test]
fn scripts_escaping_the_script_root_are_rejected() {
    use std::os::unix::fs::symlink;

    let layout = TempLayout::new("script-symlink");
    let outside = layout.base.join("outside.py");
    std::fs::write(&outside, RECEIPT_SCRIPT).expect("write outside script");
    symlink(&outside, layout.scripts.join("read_file.py")).expect("create escaping symlink");

    let runner = layout.runner();
    let error = runner
        .invocation(ActionId::ReadFile)
        .expect_err("escaping script must be rejected");
    assert!(
        matches!(error, AgentError::Script(ref message) if message.contains("outside")),
        "unexpected error: {error}"
    );
}

#[test]
fn harness_scripts_and_interpreter_must_live_outside_the_project_root() {
    let layout = TempLayout::new("inside-project");
    let inside_scripts = layout.project.join("scripts");
    std::fs::create_dir_all(&inside_scripts).expect("create inside scripts");

    let error = ScriptRunner::new(&layout.project, &inside_scripts, &layout.python)
        .expect_err("project-owned script root must be rejected");
    assert!(
        matches!(error, AgentError::Script(ref message) if message.contains("outside")),
        "unexpected error: {error}"
    );
}
