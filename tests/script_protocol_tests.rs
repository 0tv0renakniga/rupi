use std::path::PathBuf;

use local_agent_harness::script_runner::{
    ActionId, MAX_ERROR_MESSAGE_BYTES, MAX_REQUEST_BYTES, MAX_STDERR_BYTES, ProtocolErrorCode,
    ScriptOutcome, ScriptRunner,
};
use serde_json::{Map, Value, json};
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
            std::env::temp_dir().join(format!("rupi-script-protocol-{label}-{}", Uuid::new_v4()));
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

    fn write_script(&self, contents: &str) {
        std::fs::write(self.scripts.join("read_file.py"), contents).expect("write mock script");
    }

    async fn run(&self, contents: &str) -> ScriptOutcome {
        self.run_with_params(contents, Map::new()).await
    }

    async fn run_with_params(&self, contents: &str, params: Map<String, Value>) -> ScriptOutcome {
        self.write_script(contents);
        self.runner()
            .execute_with_params(ActionId::ReadFile, params)
            .await
            .expect("execute mock script")
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

    panic!("python3 is required for script-protocol tests");
}

fn error_code(outcome: &ScriptOutcome) -> &str {
    outcome.error().expect("action error").code()
}

#[tokio::test]
async fn executes_one_versioned_request_and_accepts_one_response() {
    let layout = TempLayout::new("success");
    let mut params = Map::new();
    params.insert("path".to_owned(), json!("src/lib.rs"));
    let outcome = layout
        .run_with_params(
            r#"
import json
import sys

request = json.load(sys.stdin)
assert request["protocol_version"] == 1
assert request["action"] == "read_file"
assert request["project_root"]
assert request["params"]["path"] == "src/lib.rs"

json.dump(
    {
        "protocol_version": 1,
        "ok": True,
        "data": {"action": request["action"], "path": request["params"]["path"]},
    },
    sys.stdout,
)
"#,
            params,
        )
        .await;

    assert!(outcome.is_ok(), "unexpected error: {:?}", outcome.error());
    assert_eq!(outcome.action(), ActionId::ReadFile);
    assert_eq!(outcome.protocol_version(), 1);
    assert_eq!(outcome.exit_code(), Some(0));
    assert_eq!(outcome.data().expect("data")["action"], "read_file");
    assert_eq!(outcome.data().expect("data")["path"], "src/lib.rs");
}

#[tokio::test]
async fn returns_structured_helper_errors() {
    let layout = TempLayout::new("helper-error");
    let outcome = layout
        .run(
            r#"
import json
import sys

json.load(sys.stdin)
json.dump(
    {
        "protocol_version": 1,
        "ok": False,
        "error": {
            "code": "PATH_OUTSIDE_PROJECT_ROOT",
            "message": "The requested path is not permitted.",
        },
    },
    sys.stdout,
)
"#,
        )
        .await;

    assert!(!outcome.is_ok());
    assert_eq!(outcome.exit_code(), Some(0));
    assert_eq!(error_code(&outcome), "PATH_OUTSIDE_PROJECT_ROOT");
    assert_eq!(
        outcome.error().expect("error").message(),
        "The requested path is not permitted."
    );
}

#[tokio::test]
async fn rejects_invalid_json_responses() {
    let layout = TempLayout::new("invalid-json");
    let outcome = layout
        .run(
            r#"
import sys

sys.stdin.read()
sys.stdout.write("{not valid json")
"#,
        )
        .await;

    assert_eq!(
        error_code(&outcome),
        ProtocolErrorCode::InvalidProtocolJson.as_str()
    );
}

#[tokio::test]
async fn rejects_multiple_json_responses() {
    let layout = TempLayout::new("multiple");
    let outcome = layout
        .run(
            r#"
import json
import sys

sys.stdin.read()
response = {"protocol_version": 1, "ok": True, "data": {"first": True}}
json.dump(response, sys.stdout)
json.dump(response, sys.stdout)
"#,
        )
        .await;

    assert_eq!(
        error_code(&outcome),
        ProtocolErrorCode::TrailingProtocolData.as_str()
    );
}

#[tokio::test]
async fn rejects_responses_missing_required_fields() {
    let layout = TempLayout::new("missing-fields");
    let outcome = layout
        .run(
            r#"
import json
import sys

sys.stdin.read()
json.dump({"protocol_version": 1, "ok": True}, sys.stdout)
"#,
        )
        .await;

    assert_eq!(
        error_code(&outcome),
        ProtocolErrorCode::InvalidProtocolResponse.as_str()
    );
}

#[tokio::test]
async fn rejects_unsupported_protocol_versions() {
    let layout = TempLayout::new("version");
    let outcome = layout
        .run(
            r#"
import json
import sys

sys.stdin.read()
json.dump({"protocol_version": 2, "ok": True, "data": {}}, sys.stdout)
"#,
        )
        .await;

    assert_eq!(
        error_code(&outcome),
        ProtocolErrorCode::ProtocolVersionMismatch.as_str()
    );
}

#[tokio::test]
async fn reports_nonzero_exit_without_parsing_stdout() {
    let layout = TempLayout::new("nonzero");
    let outcome = layout
        .run(
            r#"
import json
import sys

json.load(sys.stdin)
json.dump({"protocol_version": 1, "ok": True, "data": {}}, sys.stdout)
sys.exit(7)
"#,
        )
        .await;

    assert_eq!(
        error_code(&outcome),
        ProtocolErrorCode::ScriptExitNonzero.as_str()
    );
    assert_eq!(outcome.exit_code(), Some(7));
}

#[tokio::test]
async fn bounds_stdout_and_rejects_oversized_responses() {
    let layout = TempLayout::new("stdout-cap");
    let outcome = layout
        .run(
            r#"
import sys

sys.stdin.read()
sys.stdout.write("x" * (1024 * 1024 + 1))
"#,
        )
        .await;

    assert_eq!(
        error_code(&outcome),
        ProtocolErrorCode::ResponseTooLarge.as_str()
    );
    assert_eq!(outcome.exit_code(), Some(0));
}

#[tokio::test]
async fn bounds_stderr_and_redacts_diagnostics() {
    let layout = TempLayout::new("stderr-cap");
    let outcome = layout
        .run(
            r#"
import json
import sys

sys.stdin.read()
sys.stderr.write("authorization: Bearer SUPER_SECRET\n")
sys.stderr.write("y" * (16 * 1024 * 2))
json.dump({"protocol_version": 1, "ok": True, "data": {}}, sys.stdout)
"#,
        )
        .await;

    assert!(outcome.is_ok());
    let diagnostic = outcome.diagnostic().expect("bounded diagnostic");
    assert!(diagnostic.len() < MAX_STDERR_BYTES);
    assert!(diagnostic.contains("[stderr truncated]"));
    assert!(!diagnostic.contains("SUPER_SECRET"));
}

#[tokio::test]
async fn rejects_oversized_requests_before_launching_the_script() {
    let layout = TempLayout::new("request-cap");
    let mut params = Map::new();
    params.insert("blob".to_owned(), json!("x".repeat(MAX_REQUEST_BYTES)));

    let outcome = layout
        .runner()
        .execute_with_params(ActionId::ReadFile, params)
        .await
        .expect("protocol failure outcome");

    assert_eq!(
        error_code(&outcome),
        ProtocolErrorCode::RequestTooLarge.as_str()
    );
    assert_eq!(outcome.exit_code(), None);
    assert!(
        !layout.scripts.join("read_file.py").exists(),
        "oversized requests must not launch a missing helper script"
    );
}

#[tokio::test]
async fn helper_error_messages_are_bounded_and_redacted() {
    let layout = TempLayout::new("error-bounds");
    let outcome = layout
        .run(
            r#"
import json
import sys

sys.stdin.read()
json.dump(
    {
        "protocol_version": 1,
        "ok": False,
        "error": {
            "code": "HELPER_FAILED",
            "message": "authorization: Bearer SUPER_SECRET\n" + ("z" * (1024 * 2)),
        },
    },
    sys.stdout,
)
"#,
        )
        .await;

    let message = outcome.error().expect("error").message();
    assert!(message.len() <= MAX_ERROR_MESSAGE_BYTES);
    assert!(message.contains("truncated"));
    assert!(!message.contains("SUPER_SECRET"));
}

#[tokio::test]
async fn empty_stdout_is_a_missing_protocol_response() {
    let layout = TempLayout::new("empty");
    let outcome = layout
        .run(
            r#"
import sys

sys.stdin.read()
"#,
        )
        .await;

    assert_eq!(
        error_code(&outcome),
        ProtocolErrorCode::MissingProtocolResponse.as_str()
    );
}
