use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use local_agent_harness::agent::verify::{VerificationCommand, VerifyReport};
use local_agent_harness::approval::{
    ApprovalDecision, ApprovalFuture, ApprovalGate, ApprovalRequest,
};
use local_agent_harness::audit::{AuditKind, AuditRecord, AuditSink};
use local_agent_harness::error::{AgentError, Result};
use local_agent_harness::events::AgentEvent;
use local_agent_harness::jev::{Route, Verdict};
use local_agent_harness::llm::{
    AssistantTurn, ChatRequest, FnCall, LlmBackend, LlmStreamEvent, ToolCall,
};
use local_agent_harness::orchestrator::{
    EventSink, JevAdvisor, LocalWikiStore, Orchestrator, OrchestratorLimits, TaskRequest,
    VerificationEngine,
};
use local_agent_harness::policy::Policy;
use local_agent_harness::script_runner::ScriptRunner;
use local_agent_harness::script_tools::ScriptToolExecutor;
use local_agent_harness::state_machine::{LoopState, TaskBudget, TaskMode};
use local_agent_harness::wiki::plan::{PLAN_REQUIRED_HEADINGS, plan_path};
use local_agent_harness::wiki::state_path;
use serde_json::Value;
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const OBJECTIVE: &str = "Plan a fixture change without modifying the repository";
const SECRET: &str = "PLAN_TOOL_SECRET";

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!("rupi-plan-{label}-{}", Uuid::new_v4()));
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

#[derive(Default)]
struct CollectAudit {
    records: Mutex<Vec<AuditRecord>>,
}

impl CollectAudit {
    fn records(&self) -> Vec<AuditRecord> {
        self.records.lock().expect("audit lock").clone()
    }
}

#[async_trait]
impl AuditSink for CollectAudit {
    async fn record(&self, record: &AuditRecord) -> Result<()> {
        self.records
            .lock()
            .expect("audit lock")
            .push(record.clone());
        Ok(())
    }
}

struct RecordingApprovals {
    requests: Mutex<Vec<ApprovalRequest>>,
}

impl RecordingApprovals {
    fn new() -> Self {
        Self {
            requests: Mutex::new(Vec::new()),
        }
    }

    fn requests(&self) -> Vec<ApprovalRequest> {
        self.requests.lock().expect("approvals lock").clone()
    }
}

impl ApprovalGate for RecordingApprovals {
    fn request(&self, request: &ApprovalRequest) -> ApprovalFuture {
        self.requests
            .lock()
            .expect("approvals lock")
            .push(request.clone());
        Box::pin(async { ApprovalDecision::Approved })
    }
}

struct FakeLlm {
    turns: Mutex<VecDeque<AssistantTurn>>,
    requests: Mutex<Vec<ChatRequest>>,
    calls: AtomicUsize,
}

impl FakeLlm {
    fn new(turns: Vec<AssistantTurn>) -> Self {
        Self {
            turns: Mutex::new(turns.into()),
            requests: Mutex::new(Vec::new()),
            calls: AtomicUsize::new(0),
        }
    }

    fn requests(&self) -> Vec<ChatRequest> {
        self.requests.lock().expect("requests lock").clone()
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl LlmBackend for FakeLlm {
    async fn stream_chat(
        &self,
        request: ChatRequest,
        _event_tx: Option<&UnboundedSender<LlmStreamEvent>>,
    ) -> Result<AssistantTurn> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.requests.lock().expect("requests lock").push(request);
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

struct FakeJev;

#[async_trait]
impl JevAdvisor for FakeJev {
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

struct ProfileVerifier {
    commands: Vec<VerificationCommand>,
    detects: AtomicUsize,
    runs: AtomicUsize,
}

impl ProfileVerifier {
    fn new(commands: Vec<VerificationCommand>) -> Self {
        Self {
            commands,
            detects: AtomicUsize::new(0),
            runs: AtomicUsize::new(0),
        }
    }

    fn detects(&self) -> usize {
        self.detects.load(Ordering::SeqCst)
    }

    fn runs(&self) -> usize {
        self.runs.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl VerificationEngine for ProfileVerifier {
    async fn detect(&self, _root: &Path) -> Result<Vec<VerificationCommand>> {
        self.detects.fetch_add(1, Ordering::SeqCst);
        Ok(self.commands.clone())
    }

    async fn run(
        &self,
        _root: &Path,
        _commands: &[VerificationCommand],
        _output_cap: usize,
    ) -> Result<VerifyReport> {
        self.runs.fetch_add(1, Ordering::SeqCst);
        Err(AgentError::Blocked(
            "verification must not run in Plan mode".into(),
        ))
    }
}

fn turn(text: impl Into<String>) -> AssistantTurn {
    AssistantTurn {
        text: text.into(),
        ..AssistantTurn::default()
    }
}

fn tool_turn(calls: Vec<(&str, String)>) -> AssistantTurn {
    AssistantTurn {
        text: "inspecting".into(),
        calls: calls
            .into_iter()
            .enumerate()
            .map(|(index, (name, arguments))| ToolCall {
                id: format!("call_{index}"),
                kind: "function".into(),
                function: FnCall {
                    name: name.into(),
                    arguments,
                },
            })
            .collect(),
        ..AssistantTurn::default()
    }
}

fn plan_turn() -> AssistantTurn {
    let mut text = String::new();
    for heading in PLAN_REQUIRED_HEADINGS {
        text.push_str(&format!("## {heading}\n"));
        match heading {
            "Goal" => text.push_str("- Add the requested fixture behavior.\n\n"),
            "Repository evidence used" => {
                text.push_str("- The existing fixture source was inspected.\n\n")
            }
            "Likely files" => text.push_str("- `src/lib.rs`\n\n"),
            "Ordered steps" => {
                text.push_str("1. Add a focused test.\n2. Implement the smallest change.\n\n")
            }
            "Assumptions" => text.push_str("- The fixture is representative.\n\n"),
            "Risks" => text.push_str("- Verification may reveal a wider regression.\n\n"),
            "Expected verifier profile" => {
                text.push_str("- The harness should select the repository profile.\n\n")
            }
            "Blockers and missing evidence" => text.push_str("- No blockers identified.\n\n"),
            "Confirmed facts and model inferences" => text.push_str(
                "Confirmed: the fixture source exists.\nInference: the change is localized.\n\n",
            ),
            _ => text.push_str("- fixture\n\n"),
        }
    }
    turn(text)
}

fn tool_message_text(requests: &[ChatRequest]) -> String {
    requests
        .iter()
        .flat_map(|request| &request.messages)
        .filter(|message| message.role == "tool")
        .filter_map(|message| message.content.as_deref())
        .collect::<Vec<_>>()
        .join("\n")
}

fn tool_names(request: &ChatRequest) -> Vec<String> {
    request
        .tools
        .as_array()
        .expect("tools array")
        .iter()
        .filter_map(|tool| {
            tool.get("function")
                .and_then(|function| function.get("name"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .collect()
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
    panic!("python3 is required for Plan tests");
}

fn scripts_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts")
}

struct Harness {
    base: TempDir,
    llm: Arc<FakeLlm>,
    events: Arc<CollectEvents>,
    audit: Arc<CollectAudit>,
    approvals: Arc<RecordingApprovals>,
    orchestrator: Orchestrator,
}

impl Harness {
    fn new(turns: Vec<AssistantTurn>, verifier: Arc<ProfileVerifier>) -> Self {
        let base = TempDir::new("project");
        let project = base.path().join("project");
        std::fs::create_dir_all(&project).expect("create project");
        let runner = ScriptRunner::new(&project, scripts_dir(), python_interpreter())
            .expect("construct script runner");
        let policy = Policy::new(&project).expect("policy");
        let llm = Arc::new(FakeLlm::new(turns));
        let events = Arc::new(CollectEvents::default());
        let audit = Arc::new(CollectAudit::default());
        let approvals = Arc::new(RecordingApprovals::new());
        let tools = Arc::new(
            ScriptToolExecutor::with_audit(runner, policy, Some(audit.clone()))
                .expect("script executor"),
        );
        let orchestrator = Orchestrator::new(
            llm.clone(),
            Arc::new(FakeJev),
            tools,
            verifier.clone(),
            Arc::new(LocalWikiStore::default()),
        )
        .with_limits(OrchestratorLimits::default())
        .with_approval_gate(approvals.clone())
        .with_audit(audit.clone())
        .with_event_sink(events.clone());
        Self {
            base,
            llm,
            events,
            audit,
            approvals,
            orchestrator,
        }
    }

    fn root(&self) -> PathBuf {
        self.base.path().join("project")
    }

    fn write(&self, relative: &str, content: impl AsRef<[u8]>) {
        let path = self.root().join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create parent");
        }
        std::fs::write(path, content).expect("write fixture file");
    }

    fn budget() -> TaskBudget {
        TaskBudget {
            max_tool_output_bytes: 512,
            ..TaskBudget::default()
        }
    }

    async fn run(&self, task_id: Uuid) -> local_agent_harness::orchestrator::OrchestratorOutcome {
        self.orchestrator
            .run(
                TaskRequest::new(OBJECTIVE, self.root())
                    .with_task_id(task_id)
                    .with_mode(TaskMode::Plan)
                    .with_budget(Self::budget()),
                CancellationToken::new(),
            )
            .await
            .expect("run plan task")
    }
}

fn command(root: &Path) -> VerificationCommand {
    VerificationCommand {
        program: "cargo".into(),
        args: vec!["test".into()],
        cwd: root.to_path_buf(),
        timeout: Duration::from_secs(60),
    }
}

fn assert_required_headings(content: &str) {
    for heading in PLAN_REQUIRED_HEADINGS {
        assert!(
            content.contains(&format!("## {heading}")),
            "missing heading `{heading}` in:\n{content}"
        );
    }
}

#[tokio::test]
async fn plan_inspects_with_read_only_tools_and_compiles_a_durable_artifact() {
    let verifier = Arc::new(ProfileVerifier::new(vec![command(Path::new("."))]));
    let harness = Harness::new(
        vec![
            tool_turn(vec![(
                "read_file",
                r#"{"path":"src/lib.rs","line_start":1,"line_end":20}"#.into(),
            )]),
            plan_turn(),
        ],
        verifier.clone(),
    );
    harness.write(
        "src/lib.rs",
        format!("pub fn value() -> u8 {{ 1 }}\n// API_KEY={SECRET}\n"),
    );
    let task_id = Uuid::new_v4();

    let outcome = harness.run(task_id).await;

    assert_eq!(outcome.state(), LoopState::Completed);
    assert!(!outcome.context.mutated);
    assert!(!outcome.context.verified);
    assert_eq!(verifier.detects(), 1);
    assert_eq!(verifier.runs(), 0);
    assert_eq!(harness.llm.calls(), 2);

    let requests = harness.llm.requests();
    let tool_request = requests
        .iter()
        .find(|request| !tool_names(request).is_empty())
        .expect("planning request with tools");
    assert_eq!(
        tool_names(tool_request),
        ["read_file", "git_status", "git_diff"]
    );

    let path = plan_path(&harness.root(), task_id);
    let content = std::fs::read_to_string(&path).expect("plan artifact exists");
    assert_required_headings(&content);
    assert!(content.contains(&task_id.to_string()));
    assert!(content.contains("cargo test"));
    assert!(content.contains("src/lib.rs"));
    assert!(content.contains("Harness-confirmed facts"));
    assert!(!content.contains(SECRET));
    assert!(content.len() <= 32 * 1024);
    assert!(!state_path(&harness.root()).exists());

    assert!(harness.audit.records().iter().any(|record| {
        record.kind == AuditKind::PlanCompiled
            && record.metadata["fallback"].as_bool() == Some(false)
            && record.metadata["verifier_blocker"].is_null()
    }));
    assert!(
        harness
            .events
            .events()
            .iter()
            .any(|event| matches!(event, AgentEvent::PlanCompiled { .. }))
    );
}

#[tokio::test]
async fn plan_rejects_mutation_before_approval_or_dispatch() {
    let verifier = Arc::new(ProfileVerifier::new(vec![command(Path::new("."))]));
    let harness = Harness::new(
        vec![
            tool_turn(vec![
                ("read_file", r#"{"path":"src/lib.rs"}"#.into()),
                (
                    "write_file",
                    r#"{"path":"new.txt","content":"mutated"}"#.into(),
                ),
            ]),
            plan_turn(),
        ],
        verifier,
    );
    harness.write("src/lib.rs", "pub fn value() -> u8 { 1 }\n");
    let task_id = Uuid::new_v4();

    let outcome = harness.run(task_id).await;

    assert_eq!(outcome.state(), LoopState::Completed);
    assert!(!harness.root().join("new.txt").exists());
    assert!(harness.approvals.requests().is_empty());
    let text = tool_message_text(&harness.llm.requests());
    assert!(
        text.contains("not available in the active tool catalog"),
        "{text}"
    );
    assert!(!harness.audit.records().iter().any(|record| {
        record.kind == AuditKind::ScriptFinished
            && record.metadata["action"].as_str() == Some("write_file")
    }));
    assert!(plan_path(&harness.root(), task_id).is_file());
}

#[tokio::test]
async fn invalid_plan_uses_a_deterministic_fallback_artifact() {
    let verifier = Arc::new(ProfileVerifier::new(vec![command(Path::new("."))]));
    let harness = Harness::new(
        vec![
            tool_turn(vec![("read_file", r#"{"path":"src/lib.rs"}"#.into())]),
            turn(format!("API_KEY={SECRET}\nthis is not a structured plan")),
        ],
        verifier,
    );
    harness.write("src/lib.rs", "pub fn value() -> u8 { 1 }\n");
    let task_id = Uuid::new_v4();

    let outcome = harness.run(task_id).await;

    assert_eq!(outcome.state(), LoopState::Completed);
    let content = std::fs::read_to_string(plan_path(&harness.root(), task_id))
        .expect("fallback plan artifact");
    assert_required_headings(&content);
    assert!(content.contains("deterministic compiler fallback"));
    assert!(content.contains("missing or has empty required section(s)"));
    assert!(!content.contains(SECRET));
    assert!(harness.audit.records().iter().any(|record| {
        record.kind == AuditKind::PlanCompiled
            && record.metadata["fallback"].as_bool() == Some(true)
    }));
}

#[tokio::test]
async fn unsupported_verifier_is_a_recorded_blocker() {
    let verifier = Arc::new(ProfileVerifier::new(Vec::new()));
    let harness = Harness::new(
        vec![
            tool_turn(vec![("read_file", r#"{"path":"src/lib.rs"}"#.into())]),
            plan_turn(),
        ],
        verifier.clone(),
    );
    harness.write("src/lib.rs", "pub fn value() -> u8 { 1 }\n");
    let task_id = Uuid::new_v4();

    let outcome = harness.run(task_id).await;

    assert_eq!(outcome.state(), LoopState::Blocked);
    assert_eq!(verifier.detects(), 1);
    assert_eq!(verifier.runs(), 0);
    assert!(outcome.summary.contains("verification is unsupported"));

    let content = std::fs::read_to_string(plan_path(&harness.root(), task_id))
        .expect("blocked plan artifact exists");
    assert_required_headings(&content);
    assert!(content.contains("No supported verifier profile is available"));
    assert!(content.contains("will not invent verification commands"));
    assert!(content.contains("Verifier blocker"));
    assert!(!content.contains("cargo test"));

    assert!(harness.audit.records().iter().any(|record| {
        record.kind == AuditKind::PlanCompiled
            && record.metadata["verifier_blocker"].as_str()
                == Some(
                    "no supported verifier profile was detected; the harness will not invent verification commands",
                )
    }));
}
