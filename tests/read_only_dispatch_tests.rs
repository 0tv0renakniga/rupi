use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use local_agent_harness::agent::verify::{VerificationCommand, VerifyReport, VerifyStep};
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
    Clock, EventSink, JevAdvisor, Orchestrator, OrchestratorLimits, TaskRequest, ToolCatalog,
    VerificationEngine,
};
use local_agent_harness::policy::Policy;
use local_agent_harness::script_runner::ScriptRunner;
use local_agent_harness::script_tools::ScriptToolExecutor;
use local_agent_harness::state_machine::{LoopState, TaskBudget};
use local_agent_harness::wiki::compile::{CompileReport, SessionTrace};
use local_agent_harness::wiki::{WikiContext, WikiStatus};
use serde_json::Value;
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const OBJECTIVE: &str = "Inspect the fixture without changing it";
const MAX_TOOL_OUTPUT: usize = 512;

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!("rupi-readonly-{label}-{}", Uuid::new_v4()));
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

struct FakeVerifier;

#[async_trait]
impl VerificationEngine for FakeVerifier {
    async fn detect(&self, root: &Path) -> Result<Vec<VerificationCommand>> {
        Ok(vec![VerificationCommand {
            program: "true".into(),
            args: Vec::new(),
            cwd: root.to_path_buf(),
            timeout: Duration::from_secs(5),
        }])
    }

    async fn run(
        &self,
        _root: &Path,
        _commands: &[VerificationCommand],
        _output_cap: usize,
    ) -> Result<VerifyReport> {
        Ok(VerifyReport {
            passed: true,
            steps: vec![VerifyStep {
                command: "true".into(),
                exit_code: 0,
                duration: Duration::from_millis(1),
                timed_out: false,
            }],
            tail: String::new(),
        })
    }
}

struct FakeWiki;

#[async_trait]
impl local_agent_harness::orchestrator::WikiStore for FakeWiki {
    async fn load(&self, root: &Path) -> Result<WikiContext> {
        Ok(WikiContext::empty(root))
    }

    async fn compile(
        &self,
        root: &Path,
        _prior: &WikiContext,
        _trace: &SessionTrace,
        _today: &str,
    ) -> Result<CompileReport> {
        Ok(CompileReport {
            state_path: root.join(".ai-wiki/state-of-the-world.md"),
            log_path: root.join(".ai-wiki/log.md"),
            session_record_path: root.join(".ai-wiki/sessions/session-log.jsonl"),
            status: WikiStatus::Green,
            session_count: 1,
        })
    }
}

struct FakeClock {
    now: DateTime<Utc>,
}

impl Default for FakeClock {
    fn default() -> Self {
        Self { now: Utc::now() }
    }
}

impl Clock for FakeClock {
    fn now_utc(&self) -> DateTime<Utc> {
        self.now
    }

    fn monotonic_now(&self) -> Duration {
        Duration::ZERO
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
    panic!("python3 is required for read-only dispatch tests");
}

fn scripts_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts")
}

struct Harness {
    project: TempDir,
    llm: Arc<FakeLlm>,
    events: Arc<CollectEvents>,
    audit: Arc<CollectAudit>,
    approvals: Arc<RecordingApprovals>,
    orchestrator: Orchestrator,
}

impl Harness {
    fn new(turns: Vec<AssistantTurn>) -> Self {
        let base = TempDir::new("project");
        let project = base.path().join("project");
        std::fs::create_dir_all(&project).expect("create project");
        let runner = ScriptRunner::new(&project, scripts_dir(), python_interpreter())
            .expect("construct script runner");
        Self::with_runner(base, runner, turns)
    }

    fn with_runner(base: TempDir, runner: ScriptRunner, turns: Vec<AssistantTurn>) -> Self {
        let project = base.path().join("project");
        std::fs::create_dir_all(&project).expect("create project");
        let llm = Arc::new(FakeLlm::new(turns));
        let events = Arc::new(CollectEvents::default());
        let audit = Arc::new(CollectAudit::default());
        let approvals = Arc::new(RecordingApprovals::new());
        let policy = Policy::new(&project).expect("policy");
        let tools = Arc::new(
            ScriptToolExecutor::with_audit(runner, policy, Some(audit.clone()))
                .expect("script executor"),
        );
        let orchestrator = Orchestrator::new(
            llm.clone(),
            Arc::new(FakeJev),
            tools,
            Arc::new(FakeVerifier),
            Arc::new(FakeWiki),
        )
        .with_tool_catalog(ToolCatalog::ReadOnly)
        .with_limits(OrchestratorLimits::default())
        .with_approval_gate(approvals.clone())
        .with_audit(audit.clone())
        .with_clock(Arc::new(FakeClock::default()))
        .with_event_sink(events.clone());
        Self {
            project: base,
            llm,
            events,
            audit,
            approvals,
            orchestrator,
        }
    }

    fn root(&self) -> PathBuf {
        self.project.path().join("project")
    }

    fn write(&self, relative: &str, content: impl AsRef<[u8]>) {
        let path = self.root().join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create parent");
        }
        std::fs::write(path, content).expect("write fixture file");
    }

    fn git(&self, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(self.root())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_AUTHOR_NAME", "Rupi Test")
            .env("GIT_AUTHOR_EMAIL", "rupi-test@example.invalid")
            .env("GIT_COMMITTER_NAME", "Rupi Test")
            .env("GIT_COMMITTER_EMAIL", "rupi-test@example.invalid")
            .status()
            .expect("run git");
        assert!(status.success(), "git command failed: {args:?}");
    }

    fn budget() -> TaskBudget {
        TaskBudget {
            max_tool_output_bytes: MAX_TOOL_OUTPUT,
            ..TaskBudget::default()
        }
    }

    async fn run(&self) -> local_agent_harness::orchestrator::OrchestratorOutcome {
        self.orchestrator
            .run(
                TaskRequest::new(OBJECTIVE, self.root()).with_budget(Self::budget()),
                CancellationToken::new(),
            )
            .await
            .expect("run task")
    }
}

#[tokio::test]
async fn read_only_catalog_dispatches_scripts_and_bounds_redacts_model_context() {
    let secret = "super-secret-value-123";
    let huge = format!("API_KEY={secret}\n{}", "x".repeat(8_000));
    let harness = Harness::new(vec![
        turn("1. Inspect the fixture"),
        tool_turn(vec![
            ("read_file", r#"{"path":"src/huge.txt"}"#.into()),
            ("git_status", "{}".into()),
            ("git_diff", r#"{"staged":false}"#.into()),
        ]),
        turn("DONE"),
    ]);
    harness.write("src/huge.txt", &huge);
    harness.write("tracked.txt", "initial\n");
    harness.git(&["init", "--quiet"]);
    harness.git(&["add", "tracked.txt"]);
    harness.git(&["commit", "--quiet", "-m", "initial"]);
    harness.write("tracked.txt", format!("changed\nAPI_KEY={secret}\n"));

    let outcome = harness.run().await;

    assert_eq!(outcome.state(), LoopState::Completed);
    let requests = harness.llm.requests();
    let act_request = requests
        .iter()
        .find(|request| !tool_names(request).is_empty())
        .expect("act request with tools");
    assert_eq!(
        tool_names(act_request),
        ["read_file", "git_status", "git_diff"]
    );
    for name in ["write_file", "apply_patch", "run_command", "run_verifier"] {
        assert!(!tool_names(act_request).iter().any(|tool| tool == name));
    }

    let tool_text = tool_message_text(&requests);
    assert!(tool_text.contains("git_status clean=false"));
    assert!(tool_text.contains("git_diff staged=false"));
    assert!(tool_text.contains("changed"));
    assert!(tool_text.contains("...[truncated]..."));
    assert!(!tool_text.contains(secret));
    assert!(tool_text.contains("[REDACTED]"));

    let tool_messages = requests
        .iter()
        .flat_map(|request| &request.messages)
        .filter(|message| message.role == "tool")
        .collect::<Vec<_>>();
    assert_eq!(tool_messages.len(), 3);
    for message in tool_messages {
        let bytes = message.content.as_deref().unwrap_or_default().len();
        assert!(bytes <= MAX_TOOL_OUTPUT, "tool message was {bytes} bytes");
    }

    let records = harness.audit.records();
    for action in ["read_file", "git_status", "git_diff"] {
        assert!(records.iter().any(|record| {
            record.kind == AuditKind::ScriptFinished
                && record.metadata["action"].as_str() == Some(action)
        }));
    }
    assert!(harness.events.events().iter().any(|event| matches!(
        event,
        AgentEvent::ToolFinished {
            is_error: false,
            ..
        }
    )));
    assert!(harness.approvals.requests().is_empty());
}

#[tokio::test]
async fn malformed_action_returns_teaching_error_without_script_dispatch() {
    let harness = Harness::new(vec![
        turn("1. Inspect the fixture"),
        tool_turn(vec![("read_file", "{}".into())]),
        turn("DONE"),
    ]);

    let outcome = harness.run().await;

    assert_eq!(outcome.state(), LoopState::Completed);
    let text = tool_message_text(&harness.llm.requests());
    assert!(text.contains("TOOL_INPUT_ERROR"));
    assert!(text.contains("read_file requires string field `path`"));
    assert!(text.contains("Correct the request"));
    assert!(
        !harness
            .audit
            .records()
            .iter()
            .any(|record| record.kind == AuditKind::ScriptFinished)
    );
    assert!(harness.approvals.requests().is_empty());
}

#[tokio::test]
async fn read_only_catalog_rejects_mutation_before_approval_or_dispatch() {
    let harness = Harness::new(vec![
        turn("1. Inspect the fixture"),
        tool_turn(vec![
            (
                "write_file",
                r#"{"path":"new.txt","content":"mutated"}"#.into(),
            ),
            (
                "run_command",
                r#"{"program":"git","args":["status"]}"#.into(),
            ),
        ]),
        turn("DONE"),
    ]);

    let outcome = harness.run().await;

    assert_eq!(outcome.state(), LoopState::Completed);
    let text = tool_message_text(&harness.llm.requests());
    assert!(text.contains("not available in the active tool catalog"));
    assert!(!harness.root().join("new.txt").exists());
    assert!(harness.approvals.requests().is_empty());
    assert!(
        !harness
            .events
            .events()
            .iter()
            .any(|event| matches!(event, AgentEvent::ApprovalRequired { .. }))
    );
    assert!(
        !harness
            .audit
            .records()
            .iter()
            .any(|record| record.kind == AuditKind::ScriptFinished)
    );
}

#[tokio::test]
async fn policy_denial_happens_before_approval_and_script_dispatch() {
    let base = TempDir::new("escape");
    let project = base.path().join("project");
    std::fs::create_dir_all(&project).expect("create project");
    std::fs::write(base.path().join("outside.txt"), "outside-secret").expect("outside");
    let runner = ScriptRunner::new(&project, scripts_dir(), python_interpreter())
        .expect("construct script runner");
    let harness = Harness::with_runner(
        base,
        runner,
        vec![
            turn("1. Inspect the fixture"),
            tool_turn(vec![("read_file", r#"{"path":"../outside.txt"}"#.into())]),
            turn("DONE"),
        ],
    );

    let outcome = harness.run().await;

    assert_eq!(outcome.state(), LoopState::Completed);
    let text = tool_message_text(&harness.llm.requests());
    assert!(text.contains("policy denied action"), "{text}");
    assert!(text.contains("outside"));
    assert!(!text.contains("outside-secret"));
    assert!(harness.approvals.requests().is_empty());
    assert!(
        !harness
            .audit
            .records()
            .iter()
            .any(|record| record.kind == AuditKind::ScriptFinished)
    );
}

#[tokio::test]
async fn cancellation_terminates_an_in_flight_read_only_script() {
    let base = TempDir::new("cancel");
    let project = base.path().join("project");
    let scripts = base.path().join("scripts");
    std::fs::create_dir_all(&project).expect("create project");
    std::fs::create_dir_all(&scripts).expect("create scripts");
    std::fs::write(project.join("file.txt"), "unchanged").expect("fixture file");
    std::fs::write(
        scripts.join("read_file.py"),
        "import json, sys, time\ntime.sleep(30)\njson.dump({'protocol_version':1,'ok':True,'data':{'path':'file.txt','content':'late','line_start':1,'line_end':1,'next_line':None,'truncated':False,'partial_line':False}}, sys.stdout)\n",
    )
    .expect("slow script");
    let runner = ScriptRunner::new(&project, &scripts, python_interpreter())
        .expect("construct script runner");
    let harness = Harness::with_runner(
        base,
        runner,
        vec![
            turn("1. Inspect the fixture"),
            tool_turn(vec![("read_file", r#"{"path":"file.txt"}"#.into())]),
        ],
    );
    let root = harness.root();
    let root_check = root.clone();
    let orchestrator = Arc::new(harness.orchestrator);
    let cancel = CancellationToken::new();
    let task_cancel = cancel.clone();
    let started = Instant::now();
    let task = tokio::spawn(async move {
        orchestrator
            .run(
                TaskRequest::new(OBJECTIVE, root).with_budget(Harness::budget()),
                task_cancel,
            )
            .await
    });

    tokio::time::sleep(Duration::from_millis(250)).await;
    cancel.cancel();
    let outcome = tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .expect("cancellation completed promptly")
        .expect("task join")
        .expect("run task");

    assert_eq!(outcome.state(), LoopState::Cancelled);
    assert!(started.elapsed() < Duration::from_secs(3));
    assert_eq!(
        std::fs::read_to_string(root_check.join("file.txt")).expect("read fixture"),
        "unchanged"
    );
    assert_eq!(harness.llm.calls(), 2);
}
