//! Repeatable production-safe demonstration tasks.
//!
//! Run through `demo/run.sh`; the default test suite ignores this module because
//! it starts real Bubblewrap-contained MCP processes and nested Cargo
//! verification. The model is scripted so the demonstration is deterministic.

use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use local_agent_harness::approval::{
    ApprovalAction, ApprovalDecision, ApprovalFuture, ApprovalGate, ApprovalPolicy, ApprovalRequest,
};
use local_agent_harness::audit::{AuditLog, MAX_AUDIT_RECORD_BYTES};
use local_agent_harness::config::{
    McpProfile, McpRuntimeConfig, McpSandboxConfig, McpServerConfig, McpServerKind,
};
use local_agent_harness::error::{AgentError, Result};
use local_agent_harness::jev::{Route, Verdict};
use local_agent_harness::llm::{
    AssistantTurn, ChatRequest, FnCall, LlmBackend, LlmStreamEvent, ToolCall,
};
use local_agent_harness::mcp::McpRuntime;
use local_agent_harness::mcp::client::DEFAULT_MCP_PROTOCOL_VERSION;
use local_agent_harness::mcp::process::DEFAULT_STDERR_CAPACITY_BYTES;
use local_agent_harness::orchestrator::{
    JevAdvisor, LocalVerificationEngine, LocalWikiStore, Orchestrator, OrchestratorLimits,
    TaskRequest,
};
use local_agent_harness::policy::Policy;
use local_agent_harness::state_machine::{LoopState, TaskBudget};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

const MOCK_SERVER: &str = env!("CARGO_BIN_EXE_mock_mcp_server");
const TOOL_SECRET: &str = "MCP_E2E_SECRET";
const BASELINE_LIB: &str = r#"pub fn value() -> u8 {
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn value_is_one() {
        assert_eq!(value(), 1);
    }
}
"#;
const ADD_DOUBLE_LIB: &str = r#"pub fn value() -> u8 {
    1
}

pub fn double(value: u8) -> u8 {
    value * 2
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn value_is_one() {
        assert_eq!(value(), 1);
    }

    #[test]
    fn double_doubles() {
        assert_eq!(double(21), 42);
    }
}
"#;
const FAILING_LIB: &str = r#"pub fn value() -> u8 {
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn value_is_two() {
        assert_eq!(value(), 2);
    }
}
"#;
const BROKEN_ATTEMPT_LIB: &str = r#"pub fn value() -> u8 {
    3
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn value_is_two() {
        assert_eq!(value(), 2);
    }
}
"#;
const FIXED_LIB: &str = r#"pub fn value() -> u8 {
    2
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn value_is_two() {
        assert_eq!(value(), 2);
    }
}
"#;

struct TaskSpec {
    name: &'static str,
    objective: &'static str,
    profile: McpProfile,
    initial_lib: &'static str,
    turns: Vec<AssistantTurn>,
    max_verify_retries: u8,
}

#[tokio::test]
#[ignore = "run with demo/run.sh"]
async fn production_safe_demo_tasks() {
    let Some(bwrap) = require_bwrap() else {
        return;
    };
    let run_dir = evidence_run_dir();
    std::fs::create_dir_all(&run_dir).expect("create evidence run directory");
    let mock_sha256 = sha256_file(Path::new(MOCK_SERVER));

    let specs = vec![
        TaskSpec {
            name: "read-only-inspection",
            objective: "Inspect src/lib.rs and report whether `value` is covered by a test. Do not modify files.",
            profile: McpProfile::ReadOnly,
            initial_lib: BASELINE_LIB,
            turns: vec![
                plan_turn("1. Read src/lib.rs\n2. Report the existing test coverage"),
                tool_turn("read_file", json!({"path": "src/lib.rs"}).to_string()),
                turn("DONE: `value` is covered by `value_is_one`."),
            ],
            max_verify_retries: 1,
        },
        TaskSpec {
            name: "add-function-and-test",
            objective: "Add a `double` function with a test while preserving the existing behavior.",
            profile: McpProfile::WorkspaceMutation,
            initial_lib: BASELINE_LIB,
            turns: vec![
                plan_turn("1. Add double\n2. Add a test\n3. Run verification"),
                tool_turn(
                    "write_file",
                    json!({"path": "src/lib.rs", "content": ADD_DOUBLE_LIB}).to_string(),
                ),
                turn("DONE"),
            ],
            max_verify_retries: 1,
        },
        TaskSpec {
            name: "verification-recovery",
            objective: "Fix the failing `value_is_two` test.",
            profile: McpProfile::WorkspaceMutation,
            initial_lib: FAILING_LIB,
            turns: vec![
                plan_turn("1. Inspect the failure\n2. Fix value\n3. Re-run verification"),
                tool_turn(
                    "write_file",
                    json!({"path": "src/lib.rs", "content": BROKEN_ATTEMPT_LIB}).to_string(),
                ),
                turn("DONE"),
                tool_turn(
                    "write_file",
                    json!({"path": "src/lib.rs", "content": FIXED_LIB}).to_string(),
                ),
                turn("DONE"),
            ],
            max_verify_retries: 2,
        },
    ];

    let mut summaries = Vec::new();
    for spec in specs {
        let summary = run_demo_task(&bwrap, &run_dir, spec).await;
        summaries.push(summary);
    }

    let run_summary = json!({
        "bwrap": bwrap.display().to_string(),
        "bwrap_version": command_stdout(&bwrap, &["--version"]),
        "mock_mcp_sha256": mock_sha256,
        "tasks": summaries,
    });
    let summary_path = run_dir.join("summary.json");
    std::fs::write(
        &summary_path,
        serde_json::to_vec_pretty(&run_summary).expect("serialize demo summary"),
    )
    .expect("write demo summary");
    println!("demo evidence: {}", summary_path.display());
}

async fn run_demo_task(bwrap: &Path, run_dir: &Path, spec: TaskSpec) -> Value {
    let task_dir = run_dir.join(spec.name);
    let repo = task_dir.join("repo");
    create_fixture(&repo, spec.initial_lib);
    let evidence_dir = task_dir.join("evidence");
    std::fs::create_dir_all(&evidence_dir).expect("create task evidence directory");

    let audit_path = evidence_dir.join("audit.jsonl");
    let audit = Arc::new(AuditLog::new(audit_path.clone()));
    audit.prepare().expect("prepare demo audit");

    let config = demo_mcp_config(&repo, spec.profile, bwrap);
    let runtime = McpRuntime::start(
        &config,
        &repo,
        Policy::default_for_root(repo.clone()),
        8_000,
    )
    .await
    .expect("start Bubblewrap-contained MCP runtime");
    assert!(
        runtime
            .warnings()
            .iter()
            .any(|warning| warning.contains("bubblewrap network namespaces")),
        "demo must run under Bubblewrap: {:?}",
        runtime.warnings()
    );

    let approvals = Arc::new(DemoApprovalGate::new(
        spec.profile == McpProfile::WorkspaceMutation,
    ));
    let orchestrator = Orchestrator::new(
        Arc::new(ScriptedLlm::new(spec.turns.clone())),
        Arc::new(PassingJev),
        runtime.dispatcher(),
        Arc::new(LocalVerificationEngine),
        Arc::new(LocalWikiStore::default()),
    )
    .with_limits(OrchestratorLimits::default())
    .with_approval_policy(ApprovalPolicy::default())
    .with_approval_gate(approvals.clone())
    .with_audit(audit.clone())
    .with_mcp_provenance(runtime.provenance().to_vec());

    let budget = TaskBudget {
        max_act_turns: 12,
        max_tool_calls: 16,
        max_verify_retries: spec.max_verify_retries,
        max_system2_calls: 20,
        max_wall_time: Duration::from_secs(180),
        max_tool_output_bytes: 4_000,
    };
    let outcome = orchestrator
        .run(
            TaskRequest::new(spec.objective, &repo).with_budget(budget),
            CancellationToken::new(),
        )
        .await
        .expect("run demo task");
    assert_eq!(outcome.state(), LoopState::Completed, "{}", outcome.summary);
    assert!(outcome.verified(), "{}", outcome.summary);

    drop(orchestrator);
    runtime
        .shutdown(Duration::from_secs(2))
        .await
        .expect("shutdown demo runtime");

    let records = read_audit(&audit_path);
    let audit_text = std::fs::read_to_string(&audit_path).expect("read demo audit");
    assert!(
        !audit_text.contains(TOOL_SECRET),
        "secret leaked into audit"
    );
    assert!(!audit_text.contains("pub fn"), "source leaked into audit");
    assert_task_evidence(&records, &spec, &audit_text);

    let final_lib = std::fs::read_to_string(repo.join("src/lib.rs")).expect("read final lib");
    let wiki = local_agent_harness::wiki::load(&repo)
        .await
        .expect("load demo wiki");
    let verification_failures = records
        .iter()
        .filter(|record| {
            record["kind"] == "verification_finished" && record["metadata"]["passed"] == false
        })
        .count();
    let summary = json!({
        "task": spec.name,
        "profile": spec.profile.as_str(),
        "state": format!("{:?}", outcome.state()),
        "verified": outcome.verified(),
        "mutated": outcome.context.mutated,
        "verification_retries": outcome.context.usage.verify_retries,
        "verification_failures": verification_failures,
        "approvals": approvals.seen(),
        "audit_records": records.len(),
        "audit_bytes": audit_text.len(),
        "max_audit_record_bytes": max_record_bytes(&audit_text),
        "audit_path": audit_path.display().to_string(),
        "final_lib_sha256": sha256_bytes(final_lib.as_bytes()),
        "wiki_status": wiki.frontmatter.status.as_str(),
        "wiki_sessions": wiki.frontmatter.session_count,
    });
    std::fs::write(
        evidence_dir.join("summary.json"),
        serde_json::to_vec_pretty(&summary).expect("serialize task summary"),
    )
    .expect("write task summary");
    let _ = std::fs::remove_dir_all(&repo);
    summary
}

fn assert_task_evidence(records: &[Value], spec: &TaskSpec, audit_text: &str) {
    assert!(
        records
            .iter()
            .any(|record| record["kind"] == "task_started"),
        "missing task_started for {}",
        spec.name
    );
    assert!(
        records
            .iter()
            .any(|record| record["kind"] == "task_finished"),
        "missing task_finished for {}",
        spec.name
    );
    let started = records
        .iter()
        .find(|record| record["kind"] == "task_started")
        .expect("task_started");
    let servers = started["metadata"]["mcp_servers"]
        .as_array()
        .expect("mcp_servers metadata");
    assert!(!servers.is_empty(), "missing MCP provenance");
    for server in servers {
        assert!(server["executable"].is_string());
        assert!(server["sha256"].is_string());
        assert_eq!(server["version"], "0.0.0");
        assert!(
            audit_text.contains(server["executable"].as_str().unwrap()),
            "provenance path missing from audit"
        );
        assert!(
            audit_text.contains(server["sha256"].as_str().unwrap()),
            "provenance digest missing from audit"
        );
    }

    match spec.name {
        "read-only-inspection" => {
            assert!(
                records
                    .iter()
                    .any(|record| record["kind"] == "tool_finished"),
                "read-only task did not record its read"
            );
        }
        "add-function-and-test" => {
            assert!(
                records
                    .iter()
                    .any(|record| record["kind"] == "approval_resolved"
                        && record["metadata"]["approved"] == true),
                "mutation approval was not audited"
            );
        }
        "verification-recovery" => {
            let failures = records
                .iter()
                .filter(|record| {
                    record["kind"] == "verification_finished"
                        && record["metadata"]["passed"] == false
                })
                .count();
            let passes = records
                .iter()
                .filter(|record| {
                    record["kind"] == "verification_finished"
                        && record["metadata"]["passed"] == true
                })
                .count();
            assert!(failures >= 1, "failed verification was not audited");
            assert!(passes >= 1, "recovered verification was not audited");
        }
        other => panic!("unexpected demo task {other}"),
    }
}

fn read_audit(path: &Path) -> Vec<Value> {
    let text = std::fs::read_to_string(path).expect("read audit");
    assert!(text.len() <= 256 * 1024, "demo audit grew beyond its bound");
    let mut records = Vec::new();
    for line in text.lines() {
        assert!(
            line.len() < MAX_AUDIT_RECORD_BYTES,
            "audit record exceeded bound: {}",
            line.len() + 1
        );
        records.push(serde_json::from_str(line).expect("parse audit line"));
    }
    assert!(!records.is_empty(), "audit was empty");
    records
}

fn max_record_bytes(text: &str) -> usize {
    text.lines().map(|line| line.len() + 1).max().unwrap_or(0)
}

fn demo_mcp_config(root: &Path, profile: McpProfile, bwrap: &Path) -> McpRuntimeConfig {
    let digest = sha256_file(Path::new(MOCK_SERVER));
    let mut allowlist = BTreeMap::new();
    allowlist.insert(McpServerKind::Filesystem, digest.clone());
    allowlist.insert(McpServerKind::Git, digest.clone());
    if profile == McpProfile::WorkspaceMutation {
        allowlist.insert(McpServerKind::Shell, digest);
    }

    let filesystem_tools = if profile == McpProfile::WorkspaceMutation {
        "read_file,write_file,apply_patch"
    } else {
        "read_file"
    };
    McpRuntimeConfig {
        profile,
        filesystem: Some(demo_server(
            root,
            filesystem_tools,
            &["--real-filesystem"],
            bwrap,
        )),
        git: Some(demo_server(root, "status,diff", &[], bwrap)),
        shell: (profile == McpProfile::WorkspaceMutation)
            .then(|| demo_server(root, "run", &[], bwrap)),
        path: None,
        allow_unsandboxed: false,
        digest_allowlist: Some(allowlist),
    }
}

fn demo_server(root: &Path, tools: &str, extra: &[&str], bwrap: &Path) -> McpServerConfig {
    let mut args = vec!["--tools".to_owned(), tools.to_owned()];
    args.extend(extra.iter().map(|arg| (*arg).to_owned()));
    McpServerConfig {
        enabled: true,
        executable: PathBuf::from(MOCK_SERVER),
        args,
        working_directory: root.to_path_buf(),
        protocol_version: DEFAULT_MCP_PROTOCOL_VERSION.into(),
        startup_timeout: Duration::from_secs(10),
        request_timeout: Duration::from_secs(10),
        stderr_capacity_bytes: DEFAULT_STDERR_CAPACITY_BYTES,
        sandbox: McpSandboxConfig::Bubblewrap {
            executable: bwrap.to_path_buf(),
        },
    }
}

fn create_fixture(root: &Path, lib: &str) {
    std::fs::create_dir_all(root.join("src")).expect("create fixture source");
    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"rupi-demo-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\n",
    )
    .expect("write fixture manifest");
    std::fs::write(root.join("src/lib.rs"), lib).expect("write fixture library");
}

fn require_bwrap() -> Option<PathBuf> {
    let found = ["/usr/bin/bwrap", "/bin/bwrap"]
        .into_iter()
        .map(PathBuf::from)
        .find(|path| path.is_file());
    if found.is_none() {
        if std::env::var_os("HARNESS_REQUIRE_BWRAP").is_some() {
            panic!("HARNESS_REQUIRE_BWRAP is set but bubblewrap is not installed");
        }
        eprintln!(
            "SKIPPED: production-safe demo not executed because bwrap is unavailable; set HARNESS_REQUIRE_BWRAP=1 to fail instead"
        );
    }
    found
}

fn evidence_run_dir() -> PathBuf {
    let base = std::env::var_os("HARNESS_DEMO_EVIDENCE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("target")
                .join("demo-evidence")
        });
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0);
    base.join(format!("run-{timestamp}-{}", std::process::id()))
}

fn command_stdout(program: &Path, args: &[&str]) -> String {
    std::process::Command::new(program)
        .args(args)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .unwrap_or_else(|| "unavailable".into())
}

fn sha256_file(path: &Path) -> String {
    sha256_bytes(&std::fs::read(path).expect("read file for digest"))
}

fn sha256_bytes(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

struct ScriptedLlm {
    turns: Mutex<VecDeque<AssistantTurn>>,
    calls: AtomicUsize,
}

impl ScriptedLlm {
    fn new(turns: Vec<AssistantTurn>) -> Self {
        Self {
            turns: Mutex::new(turns.into()),
            calls: AtomicUsize::new(0),
        }
    }
}

#[async_trait]
impl LlmBackend for ScriptedLlm {
    async fn stream_chat(
        &self,
        _request: ChatRequest,
        _event_tx: Option<&UnboundedSender<LlmStreamEvent>>,
    ) -> Result<AssistantTurn> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.turns
            .lock()
            .expect("script lock")
            .pop_front()
            .ok_or_else(|| AgentError::Llm("demo script exhausted".into()))
    }

    async fn complete_text(&self, _system: &str, _user: &str) -> Result<String> {
        Ok("unused".into())
    }
}

struct PassingJev;

#[async_trait]
impl JevAdvisor for PassingJev {
    async fn choice(&self, _prompt: &str, _options: &[&str], _minimum_confidence: f32) -> Route {
        Route::Pick(0)
    }

    async fn noul(&self, _claim: &str, _evidence: &str, _threshold: f32) -> Verdict {
        Verdict::Pass(0.99)
    }

    async fn score(&self, _query: &str, _docs: &[String]) -> Option<Vec<f32>> {
        None
    }
}

struct DemoApprovalGate {
    approvals_enabled: bool,
    seen: Mutex<Vec<String>>,
}

impl DemoApprovalGate {
    fn new(approvals_enabled: bool) -> Self {
        Self {
            approvals_enabled,
            seen: Mutex::new(Vec::new()),
        }
    }

    fn seen(&self) -> Vec<String> {
        self.seen.lock().expect("approval lock").clone()
    }
}

impl ApprovalGate for DemoApprovalGate {
    fn request(&self, request: &ApprovalRequest) -> ApprovalFuture {
        self.seen
            .lock()
            .expect("approval lock")
            .push(request.summary());
        let approve = self.approvals_enabled
            && matches!(
                request.action,
                ApprovalAction::WriteFile | ApprovalAction::ApplyPatch
            );
        Box::pin(async move {
            if approve {
                ApprovalDecision::Approved
            } else {
                ApprovalDecision::Denied
            }
        })
    }
}

fn plan_turn(text: &str) -> AssistantTurn {
    AssistantTurn {
        text: text.into(),
        ..AssistantTurn::default()
    }
}

fn turn(text: &str) -> AssistantTurn {
    AssistantTurn {
        text: text.into(),
        ..AssistantTurn::default()
    }
}

fn tool_turn(name: &str, arguments: String) -> AssistantTurn {
    AssistantTurn {
        text: "working".into(),
        calls: vec![ToolCall {
            id: format!("demo-call-{}", uuid::Uuid::new_v4()),
            kind: "function".into(),
            function: FnCall {
                name: name.into(),
                arguments,
            },
        }],
        ..AssistantTurn::default()
    }
}
