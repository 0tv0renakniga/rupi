use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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
use local_agent_harness::wiki::evidence::evidence_path;
use local_agent_harness::wiki::state_path;
use serde_json::Value;
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const OBJECTIVE: &str = "Investigate the fixture without changing it";
const SECRET: &str = "INVESTIGATE_TOOL_SECRET";

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(label: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("rupi-investigate-{label}-{}", Uuid::new_v4()));
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

struct UnusedVerifier;

#[async_trait]
impl VerificationEngine for UnusedVerifier {
    async fn detect(&self, _root: &Path) -> Result<Vec<VerificationCommand>> {
        Ok(Vec::new())
    }

    async fn run(
        &self,
        _root: &Path,
        _commands: &[VerificationCommand],
        _output_cap: usize,
    ) -> Result<VerifyReport> {
        Err(AgentError::Blocked(
            "verification must not run in Investigate mode".into(),
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
    panic!("python3 is required for Investigate tests");
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
    fn new(turns: Vec<AssistantTurn>) -> Self {
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
            Arc::new(UnusedVerifier),
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
            max_tool_output_bytes: 512,
            ..TaskBudget::default()
        }
    }

    async fn run(&self, task_id: Uuid) -> local_agent_harness::orchestrator::OrchestratorOutcome {
        self.orchestrator
            .run(
                TaskRequest::new(OBJECTIVE, self.root())
                    .with_task_id(task_id)
                    .with_mode(TaskMode::Investigate)
                    .with_budget(Self::budget()),
                CancellationToken::new(),
            )
            .await
            .expect("run task")
    }
}

fn findings() -> AssistantTurn {
    turn(
        "CONFIRMED FACTS:\n- The fixture contains a tracked file.\n\
         HYPOTHESES:\n- The fixture is used to exercise investigation.\n\
         MISSING EVIDENCE:\n- No tests were inspected.\n\
         RECOMMENDED NEXT ACTION:\n- Review the brief and decide whether a Change task is needed.",
    )
}

#[tokio::test]
async fn investigate_inspects_source_and_git_then_compiles_a_brief() {
    let harness = Harness::new(vec![
        tool_turn(vec![
            ("read_file", r#"{"path":"src/lib.rs"}"#.into()),
            ("git_status", "{}".into()),
            ("git_diff", r#"{"staged":false}"#.into()),
        ]),
        findings(),
    ]);
    harness.write(
        "src/lib.rs",
        format!("pub fn value() -> u8 {{ 1 }}\n// API_KEY={SECRET}\n"),
    );
    harness.write("tracked.txt", "initial\n");
    harness.git(&["init", "--quiet"]);
    harness.git(&["add", "tracked.txt"]);
    harness.git(&["commit", "--quiet", "-m", "initial"]);
    harness.write("tracked.txt", "changed\n");
    let task_id = Uuid::new_v4();

    let outcome = harness.run(task_id).await;

    assert_eq!(outcome.state(), LoopState::Completed);
    assert!(!outcome.context.mutated);

    let requests = harness.llm.requests();
    let tool_request = requests
        .iter()
        .find(|request| !tool_names(request).is_empty())
        .expect("investigation request with tools");
    assert_eq!(
        tool_names(tool_request),
        ["read_file", "git_status", "git_diff"]
    );

    let brief_path = evidence_path(&harness.root(), task_id);
    let content = std::fs::read_to_string(&brief_path).expect("evidence brief exists");
    assert!(content.contains(&task_id.to_string()));
    assert!(content.contains("## Confirmed facts"));
    assert!(content.contains("## Hypotheses"));
    assert!(content.contains("## Missing evidence and uncertainty"));
    assert!(content.contains("## Inspected files"));
    assert!(content.contains("`src/lib.rs`"));
    assert!(content.contains("## Git state"));
    assert!(content.contains("git_status clean=false"));
    assert!(content.contains("## Recommended next action"));
    assert!(content.contains("Review the brief"));
    assert!(!content.contains(SECRET));
    assert!(content.len() <= 32 * 1024);

    assert_eq!(
        std::fs::read_to_string(harness.root().join("tracked.txt")).expect("read tracked"),
        "changed\n"
    );
    assert!(!harness.root().join("new.txt").exists());
    assert!(harness.approvals.requests().is_empty());
    assert!(!state_path(&harness.root()).exists());
    assert!(harness.audit.records().iter().any(|record| {
        record.kind == AuditKind::EvidenceCompiled
            && record.metadata["fallback"].as_bool() == Some(false)
    }));
    assert!(
        harness
            .events
            .events()
            .iter()
            .any(|event| matches!(event, AgentEvent::EvidenceCompiled { .. }))
    );
    assert!(tool_message_text(&requests).contains("git_diff staged=false"));
}

#[tokio::test]
async fn compiler_fallback_completes_when_model_findings_are_unavailable() {
    let harness = Harness::new(vec![tool_turn(vec![(
        "read_file",
        r#"{"path":"src/lib.rs"}"#.into(),
    )])]);
    harness.write("src/lib.rs", "pub fn value() -> u8 { 1 }\n");
    let task_id = Uuid::new_v4();

    let outcome = harness.run(task_id).await;

    assert_eq!(outcome.state(), LoopState::Completed);
    assert_eq!(harness.llm.calls(), 2);
    let content = std::fs::read_to_string(evidence_path(&harness.root(), task_id))
        .expect("fallback evidence brief exists");
    assert!(content.contains("`src/lib.rs`"));
    assert!(content.contains("Operator should review this evidence brief"));
    assert!(content.contains("## Hypotheses"));
    assert!(harness.audit.records().iter().any(|record| {
        record.kind == AuditKind::EvidenceCompiled
            && record.metadata["fallback"].as_bool() == Some(true)
    }));
}

#[tokio::test]
async fn investigate_rejects_mutation_before_approval_or_dispatch() {
    let harness = Harness::new(vec![
        tool_turn(vec![
            ("read_file", r#"{"path":"src/lib.rs"}"#.into()),
            (
                "write_file",
                r#"{"path":"new.txt","content":"mutated"}"#.into(),
            ),
        ]),
        findings(),
    ]);
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
}

#[tokio::test]
async fn cancelled_investigation_does_not_compile_a_brief_or_complete() {
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
    let policy = Policy::new(&project).expect("policy");
    let llm = Arc::new(FakeLlm::new(vec![tool_turn(vec![(
        "read_file",
        r#"{"path":"file.txt"}"#.into(),
    )])]));
    let audit = Arc::new(CollectAudit::default());
    let tools = Arc::new(
        ScriptToolExecutor::with_audit(runner, policy, Some(audit.clone()))
            .expect("script executor"),
    );
    let orchestrator = Arc::new(
        Orchestrator::new(
            llm.clone(),
            Arc::new(FakeJev),
            tools,
            Arc::new(UnusedVerifier),
            Arc::new(LocalWikiStore::default()),
        )
        .with_audit(audit.clone()),
    );
    let task_id = Uuid::new_v4();
    let cancel = CancellationToken::new();
    let task_cancel = cancel.clone();
    let root = project.clone();
    let started = Instant::now();
    let task = tokio::spawn(async move {
        orchestrator
            .run(
                TaskRequest::new(OBJECTIVE, &root)
                    .with_task_id(task_id)
                    .with_mode(TaskMode::Investigate)
                    .with_budget(Harness::budget()),
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
    assert!(!evidence_path(&project, task_id).exists());
    assert_eq!(
        std::fs::read_to_string(project.join("file.txt")).expect("read fixture"),
        "unchanged"
    );
    assert!(llm.calls() == 1);
}
