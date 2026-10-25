use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use local_agent_harness::audit::{AuditKind, AuditLog, AuditRecord, MAX_AUDIT_RECORD_BYTES};
use local_agent_harness::script_runner::{
    ActionId, DEFAULT_SCRIPT_TIMEOUT, MAX_SCRIPT_TIMEOUT, ProtocolErrorCode, SCRIPT_ENV_PATH,
    ScriptOutcome, ScriptRunner,
};
use serde_json::{Map, Value};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

struct TempLayout {
    base: PathBuf,
    project: PathBuf,
    scripts: PathBuf,
    python: PathBuf,
}

impl TempLayout {
    fn new(label: &str) -> Self {
        let base =
            std::env::temp_dir().join(format!("rupi-script-runtime-{label}-{}", Uuid::new_v4()));
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

    fn runner_with_timeout(&self, timeout: Duration) -> ScriptRunner {
        ScriptRunner::new_with_timeout(&self.project, &self.scripts, &self.python, timeout)
            .expect("construct runner with timeout")
    }

    fn write_script(&self, contents: &str) -> PathBuf {
        let path = self.scripts.join("read_file.py");
        std::fs::write(&path, contents).expect("write mock script");
        path.canonicalize().expect("canonical mock script")
    }

    async fn run(&self, contents: &str) -> ScriptOutcome {
        self.write_script(contents);
        self.runner()
            .execute(ActionId::ReadFile)
            .await
            .expect("execute mock script")
    }

    fn canonical_script(&self) -> PathBuf {
        self.scripts
            .join("read_file.py")
            .canonicalize()
            .expect("canonical script")
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

    panic!("python3 is required for script-runtime tests");
}

fn error_code(outcome: &ScriptOutcome) -> &str {
    outcome.error().expect("action error").code()
}

async fn wait_for_pid(path: &Path) -> u32 {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(text) = tokio::fs::read_to_string(path).await
                && let Ok(pid) = text.trim().parse()
            {
                return pid;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("mock script did not write its PID")
}

#[cfg(target_os = "linux")]
fn assert_process_gone(pid: u32) {
    let path = PathBuf::from(format!("/proc/{pid}"));
    for _ in 0..200 {
        if !path.exists() {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("child process {pid} was not reaped");
}

#[tokio::test]
async fn uses_a_minimal_environment_and_cleans_temporary_home() {
    let layout = TempLayout::new("environment");
    let outcome = layout
        .run(
            r#"
import json
import os
import sys

sys.stdin.read()
home = os.environ["HOME"]
with open("script-home.txt", "w", encoding="utf-8") as handle:
    handle.write(home)
json.dump(
    {
        "protocol_version": 1,
        "ok": True,
        "data": {"env": dict(os.environ), "home_is_dir": os.path.isdir(home)},
    },
    sys.stdout,
)
"#,
        )
        .await;

    assert!(outcome.is_ok(), "unexpected error: {:?}", outcome.error());
    let data = outcome.data().expect("data");
    let environment = data["env"].as_object().expect("environment object");
    let mut keys = environment.keys().map(String::as_str).collect::<Vec<_>>();
    keys.sort_unstable();

    #[cfg(unix)]
    assert_eq!(keys, ["HOME", "LANG", "LC_ALL", "PATH", "TMPDIR"]);
    #[cfg(windows)]
    assert_eq!(keys, ["HOME", "PATH", "TEMP", "TMP", "USERPROFILE"]);

    assert_eq!(environment["PATH"], SCRIPT_ENV_PATH);
    assert_eq!(data["home_is_dir"], true);
    for key in &keys {
        let upper = key.to_ascii_uppercase();
        for forbidden in [
            "PROXY",
            "AWS_",
            "AZURE_",
            "GOOGLE_",
            "OPENAI",
            "ANTHROPIC",
            "CREDENTIAL",
            "SECRET",
            "TOKEN",
        ] {
            assert!(
                !upper.contains(forbidden),
                "ambient credential/proxy variable leaked: {key}"
            );
        }
    }

    let home = std::fs::read_to_string(layout.project.join("script-home.txt"))
        .expect("read child HOME receipt");
    let home = PathBuf::from(home);
    assert!(home.is_absolute());
    assert!(!home.starts_with(layout.project.canonicalize().expect("project root")));
    assert!(!home.exists(), "temporary script HOME was not cleaned up");
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn timeout_terminates_reaps_and_audits_the_child_as_failed() {
    let layout = TempLayout::new("timeout");
    layout.write_script(
        r#"
import os
import sys
import time

with open("pid.txt", "w", encoding="utf-8") as handle:
    handle.write(str(os.getpid()))
sys.stdin.read()
time.sleep(10)
"#,
    );
    let audit_path = layout.base.join("audit.jsonl");
    let audit = AuditLog::new(&audit_path);
    audit.prepare().expect("prepare audit");
    let started = Instant::now();

    let outcome = layout
        .runner_with_timeout(Duration::from_secs(1))
        .execute_for_task(
            Uuid::new_v4(),
            ActionId::ReadFile,
            Map::new(),
            &CancellationToken::new(),
            Some(&audit),
        )
        .await
        .expect("timeout outcome");

    assert_eq!(
        error_code(&outcome),
        ProtocolErrorCode::ScriptTimeout.as_str()
    );
    assert!(!outcome.is_ok());
    assert!(started.elapsed() < Duration::from_secs(5));
    let pid = tokio::fs::read_to_string(layout.project.join("pid.txt"))
        .await
        .expect("read child PID")
        .trim()
        .parse()
        .expect("parse child PID");
    assert_process_gone(pid);

    let record = read_single_audit_record(&audit_path);
    assert_eq!(record.kind, AuditKind::ScriptFinished);
    assert_eq!(record.metadata["result"], "SCRIPT_TIMEOUT");
    assert_eq!(record.metadata["action"], "read_file");
    assert_eq!(
        record.metadata["script"],
        layout.canonical_script().to_string_lossy().as_ref()
    );
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn cancellation_terminates_reaps_and_cannot_report_success() {
    let layout = TempLayout::new("cancellation");
    layout.write_script(
        r#"
import os
import sys
import time

with open("pid.txt", "w", encoding="utf-8") as handle:
    handle.write(str(os.getpid()))
sys.stdin.read()
time.sleep(10)
"#,
    );

    let runner = layout.runner_with_timeout(Duration::from_secs(30));
    let cancel = CancellationToken::new();
    let task_cancel = cancel.clone();
    let task = tokio::spawn(async move {
        runner
            .execute_with_cancel(ActionId::ReadFile, Map::new(), &task_cancel)
            .await
    });

    let pid = wait_for_pid(&layout.project.join("pid.txt")).await;
    cancel.cancel();
    let outcome = task
        .await
        .expect("join cancellable execution")
        .expect("cancellation outcome");

    assert_eq!(
        error_code(&outcome),
        ProtocolErrorCode::ScriptCancelled.as_str()
    );
    assert!(!outcome.is_ok());
    assert_process_gone(pid);
}

#[tokio::test]
async fn writes_bounded_action_provenance_without_secret_values() {
    let layout = TempLayout::new("audit");
    layout.write_script(
        r#"
import json
import sys

sys.stdin.read()
sys.stderr.write("authorization: Bearer SUPER_SECRET\n")
json.dump(
    {
        "protocol_version": 1,
        "ok": True,
        "data": {"hidden": "SUPER_SECRET"},
    },
    sys.stdout,
)
"#,
    );
    let task_id = Uuid::new_v4();
    let audit_path = layout.base.join("audit.jsonl");
    let audit = AuditLog::new(&audit_path);
    audit.prepare().expect("prepare audit");

    let outcome = layout
        .runner()
        .execute_for_task(
            task_id,
            ActionId::ReadFile,
            Map::new(),
            &CancellationToken::new(),
            Some(&audit),
        )
        .await
        .expect("audited execution");

    assert!(outcome.is_ok());
    assert!(outcome.duration() > Duration::ZERO);
    assert_eq!(outcome.script(), Some(layout.canonical_script().as_path()));
    assert!(!format!("{outcome:?}").contains("SUPER_SECRET"));

    let bytes = std::fs::read(&audit_path).expect("read audit");
    let line = bytes
        .strip_suffix(b"\n")
        .expect("newline-terminated record");
    assert!(line.len() < MAX_AUDIT_RECORD_BYTES);
    assert!(!String::from_utf8_lossy(line).contains("SUPER_SECRET"));

    let record: AuditRecord = serde_json::from_slice(line).expect("parse audit record");
    assert_eq!(record.task_id, task_id);
    assert_eq!(record.kind, AuditKind::ScriptFinished);
    assert_eq!(record.message, "script action read_file OK");
    assert_eq!(record.metadata["action"], "read_file");
    assert_eq!(record.metadata["script_file"], "read_file.py");
    assert_eq!(record.metadata["ok"], true);
    assert_eq!(record.metadata["result"], "OK");
    assert_eq!(record.metadata["exit_code"], 0);
    assert_eq!(
        record.metadata["script"],
        layout.canonical_script().to_string_lossy().as_ref()
    );
    assert!(record.metadata.get("duration_ms").is_some());
    assert!(record.metadata.get("params").is_none());
    assert!(record.metadata.get("data").is_none());
    assert!(record.metadata.get("diagnostic").is_none());
}

fn read_single_audit_record(path: &Path) -> AuditRecord {
    let bytes = std::fs::read(path).expect("read audit");
    let mut lines = bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty());
    let line = lines.next().expect("one audit record");
    assert!(lines.next().is_none(), "expected exactly one audit record");
    serde_json::from_slice(line).expect("parse audit record")
}

#[test]
fn timeout_configuration_is_bounded_and_validated() {
    let layout = TempLayout::new("timeout-config");
    let zero = ScriptRunner::new_with_timeout(
        &layout.project,
        &layout.scripts,
        &layout.python,
        Duration::ZERO,
    )
    .expect_err("zero timeout must be rejected");
    assert!(zero.to_string().contains("timeout"), "{zero}");

    let too_long = ScriptRunner::new_with_timeout(
        &layout.project,
        &layout.scripts,
        &layout.python,
        MAX_SCRIPT_TIMEOUT + Duration::from_secs(1),
    )
    .expect_err("oversized timeout must be rejected");
    assert!(too_long.to_string().contains("timeout"), "{too_long}");

    let runner = ScriptRunner::new(&layout.project, &layout.scripts, &layout.python)
        .expect("default runner");
    assert_eq!(runner.timeout(), DEFAULT_SCRIPT_TIMEOUT);
    let runner = runner
        .with_timeout(Duration::from_secs(5))
        .expect("valid timeout");
    assert_eq!(runner.timeout(), Duration::from_secs(5));
}

#[test]
fn audit_metadata_is_bounded_for_unlaunched_protocol_failures() {
    let layout = TempLayout::new("audit-metadata");
    let mut params = Map::new();
    params.insert("blob".to_owned(), Value::String("x".repeat(64 * 1024)));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");
    let outcome = runtime
        .block_on(
            layout
                .runner()
                .execute_with_params(ActionId::ReadFile, params),
        )
        .expect("request-too-large outcome");
    assert_eq!(
        error_code(&outcome),
        ProtocolErrorCode::RequestTooLarge.as_str()
    );
    let metadata = outcome.audit_metadata();
    assert_eq!(metadata["action"], "read_file");
    assert_eq!(metadata["script"], "read_file.py");
    assert_eq!(metadata["result"], "REQUEST_TOO_LARGE");
    let record = outcome.audit_record(Uuid::new_v4());
    assert!(serde_json::to_vec(&record).expect("record JSON").len() < MAX_AUDIT_RECORD_BYTES);
}
