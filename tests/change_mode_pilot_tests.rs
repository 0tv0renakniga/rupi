use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use local_agent_harness::approval::{
    ApprovalDecision, ApprovalFuture, ApprovalGate, ApprovalPolicy, ApprovalRequest,
};
use local_agent_harness::audit::{AuditKind, AuditLog, AuditRecord};
use local_agent_harness::error::{AgentError, Result};
use local_agent_harness::events::AgentEvent;
use local_agent_harness::jev::{Route, Verdict};
use local_agent_harness::llm::{
    AssistantTurn, ChatRequest, FnCall, LlmBackend, LlmStreamEvent, ToolCall,
};
use local_agent_harness::orchestrator::{
    EventSink, JevAdvisor, LocalWikiStore, Orchestrator, OrchestratorLimits, TaskRequest,
    ToolExecutor,
};
use local_agent_harness::policy::Policy;
use local_agent_harness::script_runner::ScriptRunner;
use local_agent_harness::script_tools::ScriptToolExecutor;
use local_agent_harness::script_verification::ScriptVerificationEngine;
use local_agent_harness::state_machine::{LoopState, TaskMode};
use local_agent_harness::wiki::compile::SessionTrace;
use sha2::{Digest, Sha256};
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const OBJECTIVE: &str = "Set the pilot fixture value to 42";
const TARGET: &str = "src/value.txt";
const INITIAL_VALUE: &str = "value=0\n";
const FAILING_VALUE: &str = "value=wrong\n";
const FINAL_VALUE: &str = "value=42\n";

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!("rupi-pilot-{label}-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&path).expect("create temp directory");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

struct RecordingApprovals {
    requests: Mutex<Vec<ApprovalRequest>>,
    decision: ApprovalDecision,
}

impl RecordingApprovals {
    fn new(decision: ApprovalDecision) -> Self {
        Self {
            requests: Mutex::new(Vec::new()),
            decision,
        }
    }

    fn requests(&self) -> Vec<ApprovalRequest> {
        self.requests.lock().expect("requests lock").clone()
    }
}

impl ApprovalGate for RecordingApprovals {
    fn request(&self, request: &ApprovalRequest) -> ApprovalFuture {
        self.requests
            .lock()
            .expect("requests lock")
            .push(request.clone());
        let decision = self.decision;
        Box::pin(async move { decision })
    }
}

struct FakeLlm {
    turns: Mutex<VecDeque<AssistantTurn>>,
    calls: AtomicUsize,
}

impl FakeLlm {
    fn new(turns: Vec<AssistantTurn>) -> Self {
        Self {
            turns: Mutex::new(turns.into()),
            calls: AtomicUsize::new(0),
        }
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl LlmBackend for FakeLlm {
    async fn stream_chat(
        &self,
        _request: ChatRequest,
        _event_tx: Option<&UnboundedSender<LlmStreamEvent>>,
    ) -> Result<AssistantTurn> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.turns
            .lock()
            .expect("turns lock")
            .pop_front()
            .ok_or_else(|| AgentError::Llm("script exhausted".into()))
    }

    async fn complete_text(&self, _system: &str, _user: &str) -> Result<String> {
        Ok("unused".into())
    }
}

struct FakeJev {
    choice: Route,
}

#[async_trait]
impl JevAdvisor for FakeJev {
    async fn choice(&self, _prompt: &str, _options: &[&str], _minimum_confidence: f32) -> Route {
        self.choice
    }

    async fn noul(&self, _claim: &str, _evidence: &str, _threshold: f32) -> Verdict {
        Verdict::Pass(0.99)
    }

    async fn score(&self, _query: &str, _docs: &[String]) -> Option<Vec<f32>> {
        None
    }
}

#[derive(Default)]
struct CollectEvents {
    events: Mutex<Vec<AgentEvent>>,
}

impl CollectEvents {
    fn events(&self) -> Vec<AgentEvent> {
        self.events.lock().expect("events lock").clone()
    }
}

impl EventSink for CollectEvents {
    fn emit(&self, event: AgentEvent) {
        self.events.lock().expect("events lock").push(event);
    }
}

struct PilotHarness {
    _base: TempDir,
    project: PathBuf,
    audit_path: PathBuf,
    llm: Arc<FakeLlm>,
    approvals: Arc<RecordingApprovals>,
    events: Arc<CollectEvents>,
    orchestrator: Arc<Orchestrator>,
}

impl PilotHarness {
    fn new(
        label: &str,
        turns: Vec<AssistantTurn>,
        decision: ApprovalDecision,
        jev_choice: Route,
    ) -> Self {
        let base = TempDir::new(label);
        let project = base.path().join("project");
        create_pilot_repository(&project);

        let runner = ScriptRunner::new(&project, scripts_dir(), python_interpreter())
            .expect("construct script runner");
        let policy = Policy::new(&project).expect("policy");
        let audit_path = base.path().join("audit/audit.jsonl");
        let audit = Arc::new(AuditLog::new(&audit_path));
        audit.prepare().expect("prepare audit log");

        let llm = Arc::new(FakeLlm::new(turns));
        let approvals = Arc::new(RecordingApprovals::new(decision));
        let events = Arc::new(CollectEvents::default());
        let tools: Arc<dyn ToolExecutor> = Arc::new(
            ScriptToolExecutor::with_audit(runner.clone(), policy, Some(audit.clone()))
                .expect("script executor"),
        );
        let verifier = Arc::new(ScriptVerificationEngine::new(runner));

        let orchestrator = Arc::new(
            Orchestrator::new(
                llm.clone(),
                Arc::new(FakeJev { choice: jev_choice }),
                tools,
                verifier,
                Arc::new(LocalWikiStore::default()),
            )
            .with_limits(OrchestratorLimits::default())
            .with_approval_policy(ApprovalPolicy {
                timeout: Duration::from_secs(120),
                ..ApprovalPolicy::default()
            })
            .with_approval_gate(approvals.clone())
            .with_audit(audit)
            .with_event_sink(events.clone()),
        );

        Self {
            _base: base,
            project,
            audit_path,
            llm,
            approvals,
            events,
            orchestrator,
        }
    }

    fn root(&self) -> &Path {
        &self.project
    }

    fn target(&self) -> PathBuf {
        self.root().join(TARGET)
    }

    async fn run(&self) -> local_agent_harness::orchestrator::OrchestratorOutcome {
        self.orchestrator
            .clone()
            .run(
                TaskRequest::new(OBJECTIVE, self.root()).with_mode(TaskMode::Change),
                CancellationToken::new(),
            )
            .await
            .expect("run pilot task")
    }

    fn audit_records(&self) -> Vec<AuditRecord> {
        std::fs::read_to_string(&self.audit_path)
            .expect("read audit log")
            .lines()
            .map(|line| serde_json::from_str(line).expect("parse audit record"))
            .collect()
    }

    fn write_audit_records(&self) -> Vec<AuditRecord> {
        self.audit_records()
            .into_iter()
            .filter(|record| {
                record.kind == AuditKind::ScriptFinished
                    && record.metadata["action"].as_str() == Some("write_file")
            })
            .collect()
    }
}

fn create_pilot_repository(project: &Path) {
    std::fs::create_dir_all(project.join("src")).expect("create project");
    std::fs::create_dir_all(project.join(".venv/bin")).expect("create venv");
    std::fs::write(project.join(".gitignore"), ".ai-wiki/\n").expect("write gitignore");
    std::fs::write(
        project.join("pyproject.toml"),
        "[project]\nname = \"pilot-fixture\"\nversion = \"0.1.0\"\n",
    )
    .expect("write pyproject");
    std::fs::write(project.join(TARGET), INITIAL_VALUE).expect("write initial value");

    let verifier = format!(
        "#!{}\nfrom pathlib import Path\nimport sys\n\n\
         root = Path(__file__).resolve().parents[2]\n\
         actual = (root / \"src\" / \"value.txt\").read_text(encoding=\"utf-8\")\n\
         if actual == \"value=42\\n\":\n\
         \x20   print(\"pilot verification passed\")\n\
         \x20   raise SystemExit(0)\n\
         print(f\"expected value=42, found {{actual!r}}\", file=sys.stderr)\n\
         raise SystemExit(1)\n",
        python_interpreter().display()
    );
    let verifier_path = project.join(".venv/bin/python");
    std::fs::write(&verifier_path, verifier).expect("write verifier");
    make_executable(&verifier_path);

    git(project, &["init", "-q"]);
    git(project, &["add", "."]);
    git(
        project,
        &[
            "-c",
            "user.name=Rupi Pilot",
            "-c",
            "user.email=pilot@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-qm",
            "initial fixture",
        ],
    );
}

fn make_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = std::fs::metadata(path)
        .expect("verifier metadata")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions).expect("set verifier executable");
}

fn git(project: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(project)
        .args(args)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_stdout(project: &Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(project)
        .args(args)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn turn(text: impl Into<String>) -> AssistantTurn {
    AssistantTurn {
        text: text.into(),
        ..AssistantTurn::default()
    }
}

fn tool_turn(name: &str, arguments: impl Into<String>) -> AssistantTurn {
    AssistantTurn {
        text: "working".into(),
        calls: vec![ToolCall {
            id: "call_1".into(),
            kind: "function".into(),
            function: FnCall {
                name: name.into(),
                arguments: arguments.into(),
            },
        }],
        ..AssistantTurn::default()
    }
}

fn read_turn(path: &str) -> AssistantTurn {
    tool_turn(
        "read_file",
        format!(
            r#"{{"path":{}}}"#,
            serde_json::to_string(path).expect("path JSON")
        ),
    )
}

fn write_turn(path: &str, content: &str, expected_sha256: &str) -> AssistantTurn {
    tool_turn(
        "write_file",
        format!(
            r#"{{"path":{},"content":{},"expected_sha256":{}}}"#,
            serde_json::to_string(path).expect("path JSON"),
            serde_json::to_string(content).expect("content JSON"),
            serde_json::to_string(expected_sha256).expect("digest JSON")
        ),
    )
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn pilot_turns() -> Vec<AssistantTurn> {
    vec![
        turn(
            "1. Inspect src/value.txt\n2. Set the value to 42\n3. Run verification and review the diff",
        ),
        read_turn(TARGET),
        write_turn(TARGET, FAILING_VALUE, &sha256_hex(INITIAL_VALUE.as_bytes())),
        turn("DONE"),
        write_turn(TARGET, FINAL_VALUE, &sha256_hex(FAILING_VALUE.as_bytes())),
        tool_turn("git_diff", "{}"),
        turn("DONE"),
    ]
}

fn denial_turns() -> Vec<AssistantTurn> {
    vec![
        turn("1. Replace the fixture value with 42"),
        write_turn(TARGET, FINAL_VALUE, &sha256_hex(INITIAL_VALUE.as_bytes())),
        turn("The change was not approved, so I need direction before continuing."),
    ]
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
    panic!("python3 is required for the Change-mode pilot test");
}

fn scripts_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts")
}

fn session_trace(project: &Path) -> SessionTrace {
    let record = project.join(".ai-wiki/sessions/session-log.jsonl");
    let raw = std::fs::read_to_string(record).expect("read session record");
    let line = raw.lines().last().expect("session record line");
    serde_json::from_str(line).expect("parse session trace")
}

#[tokio::test]
async fn change_mode_pilot_recovers_from_failure_and_persists_evidence() {
    let harness = PilotHarness::new(
        "recovery",
        pilot_turns(),
        ApprovalDecision::Approved,
        Route::Pick(0),
    );

    let outcome = harness.run().await;

    assert_eq!(outcome.state(), LoopState::Completed);
    assert!(outcome.verified());
    assert!(outcome.context.mutated);
    assert!(!outcome.summary.trim().is_empty());
    assert_eq!(
        std::fs::read_to_string(harness.target()).unwrap(),
        FINAL_VALUE
    );
    assert_eq!(
        git_stdout(harness.root(), &["status", "--porcelain"]),
        " M src/value.txt\n"
    );
    let diff = git_stdout(harness.root(), &["diff", "--", TARGET]);
    assert!(diff.contains("-value=0"));
    assert!(diff.contains("+value=42"));

    let requests = harness.approvals.requests();
    assert_eq!(requests.len(), 2);
    for request in &requests {
        assert_eq!(request.target.as_deref(), Some(TARGET));
        assert!(!request.summary().contains("wrong"));
        assert!(!request.summary().contains("42"));
    }

    let records = harness.audit_records();
    assert_eq!(harness.write_audit_records().len(), 2);
    assert_eq!(
        records
            .iter()
            .filter(|record| record.kind == AuditKind::ApprovalRequested)
            .count(),
        2
    );
    assert_eq!(
        records
            .iter()
            .filter(|record| record.kind == AuditKind::ApprovalResolved
                && record.metadata["approved"] == true)
            .count(),
        2
    );

    let verification = records
        .iter()
        .filter(|record| record.kind == AuditKind::VerificationFinished)
        .collect::<Vec<_>>();
    assert_eq!(verification.len(), 2);
    assert_eq!(verification[0].metadata["passed"], false);
    assert!(
        verification[0].metadata["tail"]
            .as_str()
            .is_some_and(|tail| tail.contains("expected value=42"))
    );
    assert_eq!(verification[1].metadata["passed"], true);
    assert!(
        records
            .iter()
            .any(|record| record.kind == AuditKind::TaskFinished
                && record.metadata["state"] == "Completed"
                && record.metadata["verified"] == true)
    );
    assert!(!serde_json::to_string(&records).unwrap().contains("mcp"));
    assert_eq!(harness.llm.calls(), 7);

    assert!(
        records
            .iter()
            .any(|record| record.kind == AuditKind::ToolFinished
                && record.metadata["tool"] == "git_diff")
    );

    let events = harness.events.events();
    assert!(
        events
            .iter()
            .any(|event| matches!(event, AgentEvent::WikiLoaded { .. }))
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, AgentEvent::PlanProduced { steps: 3, .. }))
    );
    assert!(events.iter().any(|event| matches!(
        event,
        AgentEvent::VerificationFinished { passed: false, .. }
    )));
    assert!(
        events
            .iter()
            .any(|event| matches!(event, AgentEvent::VerificationFinished { passed: true, .. }))
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, AgentEvent::WikiCompiled { .. }))
    );
    assert!(
        events
            .iter()
            .any(|event| matches!(event, AgentEvent::Completed { .. }))
    );

    let state_path = harness.root().join(".ai-wiki/state-of-the-world.md");
    let state = std::fs::read_to_string(state_path).expect("read compiled Wiki");
    assert!(state.contains("## How to verify"));
    assert!(state.contains("- PASS `.venv/bin/python -m pytest -q`"));
    assert!(state.contains(TARGET));

    let trace = session_trace(harness.root());
    assert!(trace.changed_files.contains(TARGET));
    assert!(
        trace
            .git_state
            .iter()
            .any(|observation| observation.contains(TARGET))
    );
    assert_eq!(trace.verification.len(), 1);
    assert_eq!(trace.verification[0].exit_code, Some(0));
}

#[tokio::test]
async fn change_mode_pilot_approval_denial_leaves_repository_untouched() {
    let harness = PilotHarness::new(
        "denial",
        denial_turns(),
        ApprovalDecision::Denied,
        Route::Pick(1),
    );

    let outcome = harness.run().await;

    assert_eq!(outcome.state(), LoopState::Blocked);
    assert!(!outcome.context.mutated);
    assert!(!outcome.verified());
    assert_eq!(
        std::fs::read_to_string(harness.target()).unwrap(),
        INITIAL_VALUE
    );
    assert_eq!(git_stdout(harness.root(), &["status", "--porcelain"]), "");

    let requests = harness.approvals.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].target.as_deref(), Some(TARGET));
    assert!(harness.write_audit_records().is_empty());

    let records = harness.audit_records();
    assert!(
        records
            .iter()
            .any(|record| record.kind == AuditKind::ApprovalResolved
                && record.metadata["approved"] == false)
    );
    assert!(
        !records
            .iter()
            .any(|record| record.kind == AuditKind::VerificationStarted)
    );
    assert!(!serde_json::to_string(&records).unwrap().contains("mcp"));
    assert!(
        records
            .iter()
            .any(|record| record.kind == AuditKind::TaskFinished
                && record.metadata["state"] == "Blocked")
    );
    assert!(
        harness
            .events
            .events()
            .iter()
            .any(|event| matches!(event, AgentEvent::Blocked { .. }))
    );
    assert!(!harness.root().join(".ai-wiki").exists());
}
