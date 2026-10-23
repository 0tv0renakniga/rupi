use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use local_agent_harness::agent::verify::{VerificationCommand, VerifyReport, VerifyStep};
use local_agent_harness::approval::{
    ApprovalDecision, ApprovalFuture, ApprovalGate, ApprovalPolicy, ApprovalRequest,
    DenyApprovalGate,
};
use local_agent_harness::audit::{AuditKind, AuditRecord, AuditSink};
use local_agent_harness::error::{AgentError, Result};
use local_agent_harness::events::AgentEvent;
use local_agent_harness::jev::{Route, Verdict};
use local_agent_harness::llm::{
    AssistantTurn, ChatRequest, FnCall, LlmBackend, LlmStreamEvent, ToolCall,
};
use local_agent_harness::mcp::{AgentTool, ToolOutcome};
use local_agent_harness::orchestrator::{
    ChannelEventSink, Clock, EventSink, JevAdvisor, Orchestrator, OrchestratorLimits,
    OrchestratorOutcome, TaskContext, TaskRequest, ToolExecutor, VerificationEngine, WikiStore,
};
use local_agent_harness::state_machine::{LoopState, TaskBudget};
use local_agent_harness::wiki::compile::{CompileReport, SessionTrace};
use local_agent_harness::wiki::{WikiContext, WikiStatus};
use tokio::sync::mpsc;
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const OBJECTIVE: &str = "Fix the fixture safely";

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!("rupi-orch-{label}-{}", Uuid::new_v4()));
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

struct AllowApprovals;

impl ApprovalGate for AllowApprovals {
    fn request(&self, _request: &ApprovalRequest) -> ApprovalFuture {
        Box::pin(async { ApprovalDecision::Approved })
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

struct PendingApprovals;

impl ApprovalGate for PendingApprovals {
    fn request(&self, _request: &ApprovalRequest) -> ApprovalFuture {
        Box::pin(std::future::pending())
    }
}

struct CancelOnApproval {
    cancel: CancellationToken,
}

impl ApprovalGate for CancelOnApproval {
    fn request(&self, _request: &ApprovalRequest) -> ApprovalFuture {
        self.cancel.cancel();
        Box::pin(std::future::pending())
    }
}

struct FakeLlm {
    turns: Mutex<VecDeque<AssistantTurn>>,
    streams: Mutex<VecDeque<Vec<LlmStreamEvent>>>,
    requests: Mutex<Vec<ChatRequest>>,
    calls: AtomicUsize,
}

impl FakeLlm {
    fn new(turns: Vec<AssistantTurn>) -> Self {
        Self {
            turns: Mutex::new(turns.into()),
            streams: Mutex::new(VecDeque::new()),
            requests: Mutex::new(Vec::new()),
            calls: AtomicUsize::new(0),
        }
    }

    fn with_streams(mut self, streams: Vec<Vec<LlmStreamEvent>>) -> Self {
        self.streams = Mutex::new(streams.into());
        self
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
        event_tx: Option<&UnboundedSender<LlmStreamEvent>>,
    ) -> Result<AssistantTurn> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.requests.lock().expect("requests lock").push(request);
        if let Some(event_tx) = event_tx
            && let Some(events) = self.streams.lock().expect("streams lock").pop_front()
        {
            for event in events {
                let _ = event_tx.send(event);
            }
        }
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
    verdict: Verdict,
    scores: Option<Vec<f32>>,
    choice_calls: AtomicUsize,
    noul_calls: AtomicUsize,
}

impl FakeJev {
    fn pass() -> Self {
        Self {
            choice: Route::Pick(0),
            verdict: Verdict::Pass(0.99),
            scores: None,
            choice_calls: AtomicUsize::new(0),
            noul_calls: AtomicUsize::new(0),
        }
    }

    fn escalate() -> Self {
        Self {
            choice: Route::Escalate,
            verdict: Verdict::Uncertain,
            scores: None,
            choice_calls: AtomicUsize::new(0),
            noul_calls: AtomicUsize::new(0),
        }
    }

    fn noul_calls(&self) -> usize {
        self.noul_calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl JevAdvisor for FakeJev {
    async fn choice(&self, _prompt: &str, _options: &[&str], _minimum_confidence: f32) -> Route {
        self.choice_calls.fetch_add(1, Ordering::SeqCst);
        self.choice
    }

    async fn noul(&self, _claim: &str, _evidence: &str, _threshold: f32) -> Verdict {
        self.noul_calls.fetch_add(1, Ordering::SeqCst);
        self.verdict
    }

    async fn score(&self, _query: &str, _docs: &[String]) -> Option<Vec<f32>> {
        self.scores.clone()
    }
}

enum ToolReply {
    Success(String),
    PolicyDenied(String),
}

struct FakeTools {
    replies: Mutex<VecDeque<ToolReply>>,
    seen: Mutex<Vec<AgentTool>>,
    authorize_deny: Option<String>,
    capability_deny: Option<String>,
}

impl FakeTools {
    fn new(replies: Vec<ToolReply>) -> Self {
        Self {
            replies: Mutex::new(replies.into()),
            seen: Mutex::new(Vec::new()),
            authorize_deny: None,
            capability_deny: None,
        }
    }

    fn with_authorize_denial(mut self, reason: impl Into<String>) -> Self {
        self.authorize_deny = Some(reason.into());
        self
    }

    fn with_capability_denial(mut self, reason: impl Into<String>) -> Self {
        self.capability_deny = Some(reason.into());
        self
    }

    fn seen(&self) -> Vec<AgentTool> {
        self.seen.lock().expect("seen lock").clone()
    }
}

#[async_trait]
impl ToolExecutor for FakeTools {
    fn authorize(&self, _tool: &AgentTool) -> std::result::Result<(), String> {
        match &self.authorize_deny {
            Some(reason) => Err(reason.clone()),
            None => Ok(()),
        }
    }

    fn check_capability(&self, _tool: &AgentTool) -> std::result::Result<(), String> {
        match &self.capability_deny {
            Some(reason) => Err(reason.clone()),
            None => Ok(()),
        }
    }

    async fn execute(&self, tool_call_id: String, tool: &AgentTool) -> ToolOutcome {
        self.seen.lock().expect("seen lock").push(tool.clone());
        match self
            .replies
            .lock()
            .expect("replies lock")
            .pop_front()
            .unwrap_or_else(|| ToolReply::Success("ok".into()))
        {
            ToolReply::Success(text) => ToolOutcome::success(tool_call_id, text),
            ToolReply::PolicyDenied(text) => ToolOutcome::policy_denied(tool_call_id, text),
        }
    }
}

struct FakeVerifier {
    commands: Vec<VerificationCommand>,
    reports: Mutex<VecDeque<VerifyReport>>,
    detect_error: Option<String>,
    run_error: Option<String>,
    detect_calls: AtomicUsize,
    run_calls: AtomicUsize,
}

impl FakeVerifier {
    fn new(commands: Vec<VerificationCommand>, reports: Vec<VerifyReport>) -> Self {
        Self {
            commands,
            reports: Mutex::new(reports.into()),
            detect_error: None,
            run_error: None,
            detect_calls: AtomicUsize::new(0),
            run_calls: AtomicUsize::new(0),
        }
    }

    fn detect_calls(&self) -> usize {
        self.detect_calls.load(Ordering::SeqCst)
    }

    fn run_calls(&self) -> usize {
        self.run_calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl VerificationEngine for FakeVerifier {
    async fn detect(&self, _root: &Path) -> Result<Vec<VerificationCommand>> {
        self.detect_calls.fetch_add(1, Ordering::SeqCst);
        if let Some(error) = &self.detect_error {
            return Err(AgentError::Blocked(error.clone()));
        }
        Ok(self.commands.clone())
    }

    async fn run(
        &self,
        _root: &Path,
        _commands: &[VerificationCommand],
        _output_cap: usize,
    ) -> Result<VerifyReport> {
        self.run_calls.fetch_add(1, Ordering::SeqCst);
        if let Some(error) = &self.run_error {
            return Err(AgentError::Blocked(error.clone()));
        }
        Ok(self
            .reports
            .lock()
            .expect("reports lock")
            .pop_front()
            .unwrap_or_else(|| fail_report("verification did not pass")))
    }
}

struct FakeWiki {
    wiki: WikiContext,
    compile_error: Option<String>,
    compile_calls: AtomicUsize,
}

impl FakeWiki {
    fn ok(root: &Path) -> Self {
        Self {
            wiki: WikiContext::empty(root),
            compile_error: None,
            compile_calls: AtomicUsize::new(0),
        }
    }

    fn failing(root: &Path) -> Self {
        Self {
            wiki: WikiContext::empty(root),
            compile_error: Some("compile failed".into()),
            compile_calls: AtomicUsize::new(0),
        }
    }

    fn compile_calls(&self) -> usize {
        self.compile_calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl WikiStore for FakeWiki {
    async fn load(&self, _root: &Path) -> Result<WikiContext> {
        Ok(self.wiki.clone())
    }

    async fn compile(
        &self,
        root: &Path,
        _prior: &WikiContext,
        _trace: &SessionTrace,
        _today: &str,
    ) -> Result<CompileReport> {
        self.compile_calls.fetch_add(1, Ordering::SeqCst);
        if let Some(error) = &self.compile_error {
            return Err(AgentError::Wiki(error.clone()));
        }
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
    fail: bool,
}

impl CollectAudit {
    fn failing() -> Self {
        Self {
            records: Mutex::new(Vec::new()),
            fail: true,
        }
    }

    fn records(&self) -> Vec<AuditRecord> {
        self.records.lock().expect("audit lock").clone()
    }
}

#[async_trait]
impl AuditSink for CollectAudit {
    async fn record(&self, record: &AuditRecord) -> Result<()> {
        if self.fail {
            return Err(AgentError::Io(std::io::Error::other("audit failed")));
        }
        self.records
            .lock()
            .expect("audit lock")
            .push(record.clone());
        Ok(())
    }
}

struct Fixture {
    root: TempDir,
    llm: Arc<FakeLlm>,
    jev: Arc<FakeJev>,
    tools: Arc<FakeTools>,
    verifier: Arc<FakeVerifier>,
    wiki: Arc<FakeWiki>,
    events: Arc<CollectEvents>,
    audit: Arc<CollectAudit>,
    orchestrator: Orchestrator,
}

impl Fixture {
    fn new(
        script: Vec<AssistantTurn>,
        jev: FakeJev,
        tools: FakeTools,
        verifier: FakeVerifier,
        wiki: FakeWiki,
        limits: OrchestratorLimits,
    ) -> Self {
        Self::with_audit(
            script,
            jev,
            tools,
            verifier,
            wiki,
            limits,
            CollectAudit::default(),
        )
    }

    fn with_audit(
        script: Vec<AssistantTurn>,
        jev: FakeJev,
        tools: FakeTools,
        verifier: FakeVerifier,
        wiki: FakeWiki,
        limits: OrchestratorLimits,
        audit: CollectAudit,
    ) -> Self {
        Self::build(
            script,
            Vec::new(),
            jev,
            tools,
            verifier,
            wiki,
            limits,
            audit,
            ApprovalPolicy::default(),
            Arc::new(AllowApprovals),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn build(
        script: Vec<AssistantTurn>,
        streams: Vec<Vec<LlmStreamEvent>>,
        jev: FakeJev,
        tools: FakeTools,
        verifier: FakeVerifier,
        wiki: FakeWiki,
        limits: OrchestratorLimits,
        audit: CollectAudit,
        policy: ApprovalPolicy,
        gate: Arc<dyn ApprovalGate>,
    ) -> Self {
        let root = TempDir::new("case");
        let llm = Arc::new(FakeLlm::new(script).with_streams(streams));
        let jev = Arc::new(jev);
        let tools = Arc::new(tools);
        let verifier = Arc::new(verifier);
        let wiki = Arc::new(wiki);
        let events = Arc::new(CollectEvents::default());
        let audit = Arc::new(audit);
        let orchestrator = Orchestrator::new(
            llm.clone(),
            jev.clone(),
            tools.clone(),
            verifier.clone(),
            wiki.clone(),
        )
        .with_audit(audit.clone())
        .with_clock(Arc::new(FakeClock::default()))
        .with_event_sink(events.clone())
        .with_approval_policy(policy)
        .with_approval_gate(gate)
        .with_limits(limits);
        Self {
            root,
            llm,
            jev,
            tools,
            verifier,
            wiki,
            events,
            audit,
            orchestrator,
        }
    }

    async fn run(&self, budget: TaskBudget) -> Result<OrchestratorOutcome> {
        self.run_with_objective(OBJECTIVE, budget, CancellationToken::new())
            .await
    }

    async fn run_with_objective(
        &self,
        objective: &str,
        budget: TaskBudget,
        cancel: CancellationToken,
    ) -> Result<OrchestratorOutcome> {
        self.orchestrator
            .run(
                TaskRequest::new(objective, self.root.path()).with_budget(budget),
                cancel,
            )
            .await
    }
}

fn turn(text: impl Into<String>) -> AssistantTurn {
    AssistantTurn {
        text: text.into(),
        ..AssistantTurn::default()
    }
}

fn turn_with_reasoning(text: impl Into<String>, reasoning: impl Into<String>) -> AssistantTurn {
    AssistantTurn {
        text: text.into(),
        reasoning_content: Some(reasoning.into()),
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

fn verification_command(root: &Path) -> VerificationCommand {
    VerificationCommand {
        program: "fake".into(),
        args: vec!["check".into()],
        cwd: root.to_path_buf(),
        timeout: Duration::from_secs(5),
    }
}

fn pass_report() -> VerifyReport {
    VerifyReport {
        passed: true,
        steps: vec![VerifyStep {
            command: "fake check".into(),
            exit_code: 0,
            duration: Duration::from_millis(1),
            timed_out: false,
        }],
        tail: String::new(),
    }
}

fn fail_report(tail: impl Into<String>) -> VerifyReport {
    VerifyReport {
        passed: false,
        steps: vec![VerifyStep {
            command: "fake check".into(),
            exit_code: 1,
            duration: Duration::from_millis(1),
            timed_out: false,
        }],
        tail: tail.into(),
    }
}

fn conversation_text(ctx: &TaskContext) -> String {
    ctx.conversation
        .iter()
        .filter_map(|message| message.content.as_deref())
        .collect::<Vec<_>>()
        .join("\n")
}

fn has_event(events: &[AgentEvent], predicate: impl Fn(&AgentEvent) -> bool) -> bool {
    events.iter().any(predicate)
}

fn standard_fixture(script: Vec<AssistantTurn>, tools: FakeTools) -> Fixture {
    let base = std::env::temp_dir();
    let verifier = FakeVerifier::new(vec![verification_command(&base)], vec![pass_report()]);
    let wiki = FakeWiki::ok(&base);
    Fixture::new(
        script,
        FakeJev::pass(),
        tools,
        verifier,
        wiki,
        OrchestratorLimits::default(),
    )
}

#[tokio::test]
async fn happy_path_reaches_completed_only_through_verification_and_review() {
    let fx = standard_fixture(
        vec![
            turn("1. Inspect\n2. Edit"),
            tool_turn("write_file", r#"{"path":"src/lib.rs","content":"ok"}"#),
            turn("DONE"),
        ],
        FakeTools::new(vec![ToolReply::Success("wrote file".into())]),
    );

    let outcome = fx.run(TaskBudget::default()).await.expect("run");

    assert_eq!(outcome.state(), LoopState::Completed);
    assert!(outcome.verified());
    assert!(outcome.context.mutated);
    assert_eq!(fx.tools.seen().len(), 1);
    assert_eq!(fx.verifier.detect_calls(), 1);
    let events = fx.events.events();
    assert!(has_event(&events, |event| matches!(
        event,
        AgentEvent::VerificationFinished { passed: true, .. }
    )));
    assert!(has_event(&events, |event| matches!(
        event,
        AgentEvent::PhaseChanged {
            from: LoopState::Act,
            to: LoopState::Verify
        }
    )));
    assert!(has_event(&events, |event| matches!(
        event,
        AgentEvent::PhaseChanged {
            from: LoopState::Compile,
            to: LoopState::Completed
        }
    )));
}

#[tokio::test]
async fn completion_claim_without_mutation_still_runs_harness_verification() {
    let fx = standard_fixture(
        vec![turn("1. Check"), turn("DONE")],
        FakeTools::new(Vec::new()),
    );

    let outcome = fx.run(TaskBudget::default()).await.expect("run");

    assert_eq!(outcome.state(), LoopState::Completed);
    assert!(outcome.verified());
    assert!(!outcome.context.mutated);
    assert_eq!(fx.verifier.detect_calls(), 1);
    assert_eq!(fx.verifier.run_calls(), 1);
    assert!(has_event(&fx.events.events(), |event| matches!(
        event,
        AgentEvent::PhaseChanged {
            from: LoopState::Verify,
            to: LoopState::Review
        }
    )));
}

#[tokio::test]
async fn a_successful_mutation_forces_verify_before_review() {
    let placeholder = TempDir::new("mutation");
    let fx = Fixture::new(
        vec![
            turn("1. Edit"),
            tool_turn("write_file", r#"{"path":"src/lib.rs","content":"ok"}"#),
            turn("I am still working"),
            turn("DONE"),
        ],
        FakeJev::pass(),
        FakeTools::new(vec![ToolReply::Success("wrote file".into())]),
        FakeVerifier::new(
            vec![verification_command(placeholder.path())],
            vec![fail_report("first failure"), pass_report()],
        ),
        FakeWiki::ok(placeholder.path()),
        OrchestratorLimits::default(),
    );

    let outcome = fx.run(TaskBudget::default()).await.expect("run");

    assert_eq!(outcome.state(), LoopState::Completed);
    assert_eq!(fx.verifier.run_calls(), 2);
    let events = fx.events.events();
    assert!(has_event(&events, |event| matches!(
        event,
        AgentEvent::PhaseChanged {
            from: LoopState::Act,
            to: LoopState::Verify
        }
    )));
    assert!(has_event(&events, |event| matches!(
        event,
        AgentEvent::PhaseChanged {
            from: LoopState::Verify,
            to: LoopState::Act
        }
    )));
}

#[tokio::test]
async fn verification_failure_returns_bounded_redacted_feedback_to_act() {
    let placeholder = TempDir::new("feedback");
    let fx = Fixture::new(
        vec![
            turn("1. Edit"),
            tool_turn("write_file", r#"{"path":"src/lib.rs","content":"ok"}"#),
            turn("DONE"),
            turn("DONE"),
        ],
        FakeJev::pass(),
        FakeTools::new(vec![ToolReply::Success("wrote file".into())]),
        FakeVerifier::new(
            vec![verification_command(placeholder.path())],
            vec![
                fail_report("failed secret=supersecret token=abc123"),
                pass_report(),
            ],
        ),
        FakeWiki::ok(placeholder.path()),
        OrchestratorLimits::default(),
    );

    let outcome = fx.run(TaskBudget::default()).await.expect("run");

    assert_eq!(outcome.state(), LoopState::Completed);
    let requests = fx.llm.requests();
    let feedback = requests[3]
        .messages
        .iter()
        .filter_map(|message| message.content.as_deref())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(feedback.contains("VERIFICATION FAILED"));
    assert!(feedback.contains("[REDACTED]"));
    assert!(!feedback.contains("supersecret"));
    assert!(!feedback.contains("abc123"));
    let stored_tail = &outcome
        .context
        .verification_report
        .as_ref()
        .expect("verification report")
        .tail;
    assert!(!stored_tail.contains("supersecret"));
}

#[tokio::test]
async fn verification_retry_exhaustion_blocks_instead_of_completing() {
    let placeholder = TempDir::new("retry");
    let budget = TaskBudget {
        max_verify_retries: 1,
        ..TaskBudget::default()
    };
    let fx = Fixture::new(
        vec![
            turn("1. Edit"),
            tool_turn("write_file", r#"{"path":"src/lib.rs","content":"ok"}"#),
            turn("DONE"),
            turn("DONE"),
        ],
        FakeJev::pass(),
        FakeTools::new(vec![ToolReply::Success("wrote file".into())]),
        FakeVerifier::new(
            vec![verification_command(placeholder.path())],
            vec![fail_report("still failing"), fail_report("still failing")],
        ),
        FakeWiki::ok(placeholder.path()),
        OrchestratorLimits::default(),
    );

    let outcome = fx.run(budget).await.expect("run");

    assert_eq!(outcome.state(), LoopState::Blocked);
    assert!(!outcome.verified());
    assert_eq!(fx.verifier.run_calls(), 2);
    assert_eq!(fx.wiki.compile_calls(), 0);
}

#[tokio::test]
async fn missing_verifier_blocks_even_after_a_completion_claim() {
    let placeholder = TempDir::new("no-verifier");
    let fx = Fixture::new(
        vec![turn("1. Check"), turn("DONE")],
        FakeJev::pass(),
        FakeTools::new(Vec::new()),
        FakeVerifier::new(Vec::new(), Vec::new()),
        FakeWiki::ok(placeholder.path()),
        OrchestratorLimits::default(),
    );

    let outcome = fx.run(TaskBudget::default()).await.expect("run");

    assert_eq!(outcome.state(), LoopState::Blocked);
    assert!(!outcome.verified());
    assert_eq!(fx.verifier.detect_calls(), 1);
    assert_eq!(fx.verifier.run_calls(), 0);
    assert_eq!(fx.wiki.compile_calls(), 0);
}

#[tokio::test]
async fn jev_pass_cannot_override_failed_verification() {
    let placeholder = TempDir::new("jev-override");
    let budget = TaskBudget {
        max_verify_retries: 0,
        ..TaskBudget::default()
    };
    let fx = Fixture::new(
        vec![turn("1. Check"), turn("DONE")],
        FakeJev::pass(),
        FakeTools::new(Vec::new()),
        FakeVerifier::new(
            vec![verification_command(placeholder.path())],
            vec![fail_report("verifier failed")],
        ),
        FakeWiki::ok(placeholder.path()),
        OrchestratorLimits::default(),
    );

    let outcome = fx.run(budget).await.expect("run");

    assert_eq!(outcome.state(), LoopState::Blocked);
    assert_eq!(fx.jev.noul_calls(), 0);
    assert_eq!(fx.wiki.compile_calls(), 0);
}

#[tokio::test]
async fn jev_unavailable_uses_deterministic_fallbacks() {
    let placeholder = TempDir::new("jev-unavailable");
    let fx = Fixture::new(
        vec![turn("1. Check"), turn("DONE"), turn("DONE")],
        FakeJev::escalate(),
        FakeTools::new(Vec::new()),
        FakeVerifier::new(
            vec![verification_command(placeholder.path())],
            vec![pass_report()],
        ),
        FakeWiki::ok(placeholder.path()),
        OrchestratorLimits::default(),
    );

    let outcome = fx.run(TaskBudget::default()).await.expect("run");

    assert_eq!(outcome.state(), LoopState::Completed);
    assert_eq!(fx.jev.noul_calls(), 1);
    assert_eq!(fx.llm.calls(), 3);
}

#[tokio::test]
async fn malformed_tool_arguments_teach_without_calling_the_dispatcher() {
    let placeholder = TempDir::new("malformed");
    let fx = Fixture::new(
        vec![
            turn("1. Inspect"),
            tool_turn("read_file", "{"),
            turn("DONE"),
        ],
        FakeJev::pass(),
        FakeTools::new(Vec::new()),
        FakeVerifier::new(
            vec![verification_command(placeholder.path())],
            vec![pass_report()],
        ),
        FakeWiki::ok(placeholder.path()),
        OrchestratorLimits::default(),
    );

    let outcome = fx.run(TaskBudget::default()).await.expect("run");

    assert_eq!(outcome.state(), LoopState::Completed);
    assert!(fx.tools.seen().is_empty());
    assert_eq!(outcome.context.usage.tool_calls, 1);
    assert!(conversation_text(&outcome.context).contains("TOOL_INPUT_ERROR"));
}

#[tokio::test]
async fn policy_denial_is_fed_back_and_recorded_without_crashing() {
    let placeholder = TempDir::new("policy");
    let fx = Fixture::new(
        vec![
            turn("1. Edit"),
            tool_turn("write_file", r#"{"path":"src/lib.rs","content":"ok"}"#),
            turn("DONE"),
        ],
        FakeJev::pass(),
        FakeTools::new(vec![ToolReply::PolicyDenied(
            "policy denied: writes are disabled".into(),
        )]),
        FakeVerifier::new(
            vec![verification_command(placeholder.path())],
            vec![pass_report()],
        ),
        FakeWiki::ok(placeholder.path()),
        OrchestratorLimits::default(),
    );

    let outcome = fx.run(TaskBudget::default()).await.expect("run");

    assert_eq!(outcome.state(), LoopState::Completed);
    assert!(!outcome.context.mutated);
    assert!(!outcome.context.session_trace.policy_denials.is_empty());
    assert!(conversation_text(&outcome.context).contains("policy denied"));
    assert!(
        fx.audit
            .records()
            .iter()
            .any(|record| record.kind == AuditKind::PolicyDenied)
    );
}

#[tokio::test]
async fn tool_call_budget_exhaustion_blocks_the_task() {
    let placeholder = TempDir::new("tool-budget");
    let budget = TaskBudget {
        max_tool_calls: 1,
        ..TaskBudget::default()
    };
    let fx = Fixture::new(
        vec![
            turn("1. Inspect"),
            tool_turn("read_file", r#"{"path":"src/lib.rs"}"#),
            tool_turn("read_file", r#"{"path":"src/lib.rs"}"#),
        ],
        FakeJev::pass(),
        FakeTools::new(vec![ToolReply::Success("file".into())]),
        FakeVerifier::new(
            vec![verification_command(placeholder.path())],
            vec![pass_report()],
        ),
        FakeWiki::ok(placeholder.path()),
        OrchestratorLimits::default(),
    );

    let outcome = fx.run(budget).await.expect("run");

    assert_eq!(outcome.state(), LoopState::Blocked);
    assert_eq!(outcome.context.usage.tool_calls, 1);
    assert_eq!(fx.tools.seen().len(), 1);
}

#[tokio::test]
async fn cancellation_prevents_model_calls_and_completion() {
    let placeholder = TempDir::new("cancel");
    let fx = Fixture::new(
        vec![turn("1. Check"), turn("DONE")],
        FakeJev::pass(),
        FakeTools::new(Vec::new()),
        FakeVerifier::new(
            vec![verification_command(placeholder.path())],
            vec![pass_report()],
        ),
        FakeWiki::ok(placeholder.path()),
        OrchestratorLimits::default(),
    );
    let cancel = CancellationToken::new();
    cancel.cancel();

    let outcome = fx
        .run_with_objective(OBJECTIVE, TaskBudget::default(), cancel)
        .await
        .expect("run");

    assert_eq!(outcome.state(), LoopState::Cancelled);
    assert_eq!(fx.llm.calls(), 0);
    assert_eq!(fx.verifier.detect_calls(), 0);
    assert!(has_event(&fx.events.events(), |event| matches!(
        event,
        AgentEvent::Cancelled { .. }
    )));
}

#[tokio::test]
async fn wiki_compilation_failure_prevents_completed() {
    let placeholder = TempDir::new("wiki-failure");
    let fx = Fixture::new(
        vec![turn("1. Check"), turn("DONE")],
        FakeJev::pass(),
        FakeTools::new(Vec::new()),
        FakeVerifier::new(
            vec![verification_command(placeholder.path())],
            vec![pass_report()],
        ),
        FakeWiki::failing(placeholder.path()),
        OrchestratorLimits::default(),
    );

    let outcome = fx.run(TaskBudget::default()).await.expect("run");

    assert_eq!(outcome.state(), LoopState::Failed);
    assert_eq!(fx.wiki.compile_calls(), 1);
    let events = fx.events.events();
    assert!(!has_event(&events, |event| matches!(
        event,
        AgentEvent::Completed { .. }
    )));
    assert!(has_event(&events, |event| matches!(
        event,
        AgentEvent::Failed { .. }
    )));
}

#[tokio::test]
async fn context_compaction_keeps_objective_policy_and_verification_evidence() {
    let placeholder = TempDir::new("compaction");
    let limits = OrchestratorLimits {
        max_recent_messages: 4,
        ..OrchestratorLimits::default()
    };
    let fx = Fixture::new(
        vec![
            turn("1. Edit"),
            tool_turn("write_file", r#"{"path":"src/lib.rs","content":"ok"}"#),
            turn("DONE"),
            tool_turn("read_file", r#"{"path":"src/lib.rs"}"#),
            tool_turn("read_file", r#"{"path":"Cargo.toml"}"#),
            turn("DONE"),
        ],
        FakeJev::pass(),
        FakeTools::new(vec![
            ToolReply::Success("wrote file".into()),
            ToolReply::Success("file".into()),
            ToolReply::Success("manifest".into()),
        ]),
        FakeVerifier::new(
            vec![verification_command(placeholder.path())],
            vec![fail_report("first failure"), pass_report()],
        ),
        FakeWiki::ok(placeholder.path()),
        limits,
    );

    let outcome = fx.run(TaskBudget::default()).await.expect("run");

    assert_eq!(outcome.state(), LoopState::Completed);
    assert!(outcome.context.context_compactions >= 1);
    assert!(has_event(&fx.events.events(), |event| matches!(
        event,
        AgentEvent::ContextCompacted { .. }
    )));
    let text = conversation_text(&outcome.context);
    assert!(text.contains("TASK OBJECTIVE:"));
    assert!(text.contains(OBJECTIVE));
    assert!(text.contains("the Rust harness owns policy"));
    assert!(text.contains("EVIDENCE SUMMARY (untrusted):"));
    assert!(text.contains("Verification:"));
    assert!(text.contains("FAIL"));
}

#[tokio::test]
async fn reasoning_content_is_never_persisted() {
    let placeholder = TempDir::new("reasoning");
    let fx = Fixture::new(
        vec![
            turn_with_reasoning("1. Inspect", "HIDDEN_REASONING_PLAN"),
            turn_with_reasoning("DONE", "HIDDEN_REASONING_ACT"),
        ],
        FakeJev::pass(),
        FakeTools::new(Vec::new()),
        FakeVerifier::new(
            vec![verification_command(placeholder.path())],
            vec![pass_report()],
        ),
        FakeWiki::ok(placeholder.path()),
        OrchestratorLimits::default(),
    );

    let outcome = fx.run(TaskBudget::default()).await.expect("run");

    assert_eq!(outcome.state(), LoopState::Completed);
    let conversation = serde_json::to_string(&outcome.context.conversation).expect("conversation");
    let trace = serde_json::to_string(&outcome.context.session_trace).expect("trace");
    let audit = serde_json::to_string(&fx.audit.records()).expect("audit");
    let events = format!("{:?}", fx.events.events());
    for surface in [conversation, trace, audit, events] {
        assert!(!surface.contains("HIDDEN_REASONING"));
    }
}

#[tokio::test]
async fn audit_and_event_payloads_are_bounded_and_redacted() {
    let placeholder = TempDir::new("redaction");
    let fx = Fixture::new(
        vec![turn("1. Use token=PLAN_SECRET"), turn("DONE")],
        FakeJev::pass(),
        FakeTools::new(Vec::new()),
        FakeVerifier::new(
            vec![verification_command(placeholder.path())],
            vec![pass_report()],
        ),
        FakeWiki::ok(placeholder.path()),
        OrchestratorLimits::default(),
    );

    let outcome = fx
        .run_with_objective(
            "Fix token=OBJECTIVE_SECRET",
            TaskBudget::default(),
            CancellationToken::new(),
        )
        .await
        .expect("run");

    assert_eq!(outcome.state(), LoopState::Completed);
    let audit = serde_json::to_string(&fx.audit.records()).expect("audit");
    let events = format!("{:?}", fx.events.events());
    assert!(!audit.contains("OBJECTIVE_SECRET"));
    assert!(!audit.contains("PLAN_SECRET"));
    assert!(!events.contains("OBJECTIVE_SECRET"));
    assert!(!events.contains("PLAN_SECRET"));
    assert!(
        fx.audit
            .records()
            .iter()
            .all(|record| record.message.len() <= 512)
    );
}

#[tokio::test]
async fn audit_failures_surface_as_errors_instead_of_false_completion() {
    let placeholder = TempDir::new("audit-failure");
    let fx = Fixture::with_audit(
        vec![turn("1. Check"), turn("DONE")],
        FakeJev::pass(),
        FakeTools::new(Vec::new()),
        FakeVerifier::new(
            vec![verification_command(placeholder.path())],
            vec![pass_report()],
        ),
        FakeWiki::ok(placeholder.path()),
        OrchestratorLimits::default(),
        CollectAudit::failing(),
    );

    let error = fx
        .run(TaskBudget::default())
        .await
        .expect_err("audit failure must surface");

    assert!(matches!(error, AgentError::Io(_)));
    assert_eq!(fx.llm.calls(), 0);
}

#[tokio::test]
async fn streamed_visible_text_is_forwarded_without_tool_arguments() {
    let placeholder = TempDir::new("stream");
    let streams = vec![
        vec![
            LlmStreamEvent::TextDelta("Visible token=STREAM_SECRET ".into()),
            LlmStreamEvent::ToolCallDelta {
                index: 0,
                id: Some("raw-id".into()),
                name: Some("write_file".into()),
                arguments: Some(r#"{"content":"RAW_TOOL_ARGUMENT_SECRET"}"#.into()),
            },
            LlmStreamEvent::TextDelta("plan".into()),
            LlmStreamEvent::Finished {
                finish_reason: Some("stop".into()),
            },
        ],
        Vec::new(),
        Vec::new(),
    ];
    let fx = Fixture::build(
        vec![turn("1. Inspect\n2. Edit"), turn("DONE")],
        streams,
        FakeJev::pass(),
        FakeTools::new(Vec::new()),
        FakeVerifier::new(
            vec![verification_command(placeholder.path())],
            vec![pass_report()],
        ),
        FakeWiki::ok(placeholder.path()),
        OrchestratorLimits::default(),
        CollectAudit::default(),
        ApprovalPolicy::default(),
        Arc::new(AllowApprovals),
    );

    let outcome = fx.run(TaskBudget::default()).await.expect("run");
    assert_eq!(outcome.state(), LoopState::Completed);

    let deltas = fx
        .events
        .events()
        .into_iter()
        .filter_map(|event| match event {
            AgentEvent::StreamDelta { text } => Some(text),
            _ => None,
        })
        .collect::<String>();
    assert_eq!(deltas, "Visible token=[REDACTED]plan");
    let rendered = format!("{:?}", fx.events.events());
    assert!(!rendered.contains("STREAM_SECRET"));
    assert!(!rendered.contains("RAW_TOOL_ARGUMENT_SECRET"));
    assert!(conversation_text(&outcome.context).contains("1. Inspect"));
}

#[tokio::test]
async fn policy_denial_never_reaches_the_approval_gate() {
    let placeholder = TempDir::new("policy-before-approval");
    let approvals = Arc::new(RecordingApprovals::new(ApprovalDecision::Approved));
    let fx = Fixture::build(
        vec![
            turn("1. Inspect"),
            tool_turn("write_file", r#"{"path":".env","content":"SECRET"}"#),
            turn("DONE"),
        ],
        Vec::new(),
        FakeJev::pass(),
        FakeTools::new(Vec::new()).with_authorize_denial("sensitive path is denied: .env"),
        FakeVerifier::new(
            vec![verification_command(placeholder.path())],
            vec![pass_report()],
        ),
        FakeWiki::ok(placeholder.path()),
        OrchestratorLimits::default(),
        CollectAudit::default(),
        ApprovalPolicy::default(),
        approvals.clone(),
    );

    let outcome = fx.run(TaskBudget::default()).await.expect("run");

    assert!(approvals.requests().is_empty());
    assert!(fx.tools.seen().is_empty());
    assert_eq!(outcome.context.session_trace.policy_denials.len(), 1);
    assert!(!has_event(&fx.events.events(), |event| matches!(
        event,
        AgentEvent::ApprovalRequired { .. }
    )));
}

#[tokio::test]
async fn capability_failure_never_reaches_the_approval_gate() {
    let placeholder = TempDir::new("capability-before-approval");
    let approvals = Arc::new(RecordingApprovals::new(ApprovalDecision::Approved));
    let fx = Fixture::build(
        vec![
            turn("1. Inspect"),
            tool_turn("write_file", r#"{"path":"src/lib.rs","content":"x"}"#),
            turn("DONE"),
        ],
        Vec::new(),
        FakeJev::pass(),
        FakeTools::new(Vec::new()).with_capability_denial(
            "capability unavailable: `write_file` is not enabled by the configured MCP profile",
        ),
        FakeVerifier::new(
            vec![verification_command(placeholder.path())],
            vec![pass_report()],
        ),
        FakeWiki::ok(placeholder.path()),
        OrchestratorLimits::default(),
        CollectAudit::default(),
        ApprovalPolicy::default(),
        approvals.clone(),
    );

    let outcome = fx.run(TaskBudget::default()).await.expect("run");

    assert!(approvals.requests().is_empty());
    assert!(fx.tools.seen().is_empty());
    assert!(!has_event(&fx.events.events(), |event| matches!(
        event,
        AgentEvent::ApprovalRequired { .. }
    )));
    assert!(
        conversation_text(&outcome.context)
            .contains("capability unavailable: `write_file` is not enabled")
    );
}

#[tokio::test]
async fn approval_denial_prevents_execution_and_returns_model_feedback() {
    let placeholder = TempDir::new("approval-denied");
    let approvals = Arc::new(RecordingApprovals::new(ApprovalDecision::Denied));
    let fx = Fixture::build(
        vec![
            turn("1. Inspect"),
            tool_turn(
                "write_file",
                r#"{"path":"src/lib.rs","content":"APPROVAL_RAW_CONTENT_SECRET"}"#,
            ),
            turn("DONE"),
        ],
        Vec::new(),
        FakeJev::pass(),
        FakeTools::new(vec![ToolReply::Success("should not run".into())]),
        FakeVerifier::new(
            vec![verification_command(placeholder.path())],
            vec![pass_report()],
        ),
        FakeWiki::ok(placeholder.path()),
        OrchestratorLimits::default(),
        CollectAudit::default(),
        ApprovalPolicy::default(),
        approvals.clone(),
    );

    let outcome = fx.run(TaskBudget::default()).await.expect("run");

    assert!(fx.tools.seen().is_empty());
    let feedback = conversation_text(&outcome.context);
    assert!(feedback.contains("APPROVAL_DENIED"));
    assert!(!feedback.contains("APPROVAL_RAW_CONTENT_SECRET"));
    let requests = approvals.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].target.as_deref(), Some("src/lib.rs"));
    assert!(
        !requests[0]
            .summary()
            .contains("APPROVAL_RAW_CONTENT_SECRET")
    );
    assert!(has_event(&fx.events.events(), |event| matches!(
        event,
        AgentEvent::ApprovalResolved {
            approved: false,
            ..
        }
    )));
}

#[tokio::test]
async fn approved_mutation_runs_only_through_the_gate() {
    let placeholder = TempDir::new("approval-approved");
    let approvals = Arc::new(RecordingApprovals::new(ApprovalDecision::Approved));
    let fx = Fixture::build(
        vec![
            turn("1. Inspect"),
            tool_turn("write_file", r#"{"path":"src/lib.rs","content":"ok"}"#),
            turn("DONE"),
        ],
        Vec::new(),
        FakeJev::pass(),
        FakeTools::new(vec![ToolReply::Success("wrote file".into())]),
        FakeVerifier::new(
            vec![verification_command(placeholder.path())],
            vec![pass_report()],
        ),
        FakeWiki::ok(placeholder.path()),
        OrchestratorLimits::default(),
        CollectAudit::default(),
        ApprovalPolicy::default(),
        approvals.clone(),
    );

    let outcome = fx.run(TaskBudget::default()).await.expect("run");

    assert_eq!(outcome.state(), LoopState::Completed);
    assert_eq!(fx.tools.seen().len(), 1);
    assert_eq!(approvals.requests().len(), 1);
    let events = fx.events.events();
    assert!(
        events
            .iter()
            .any(|event| matches!(event, AgentEvent::ApprovalResolved { approved: true, .. }))
            && events
                .iter()
                .any(|event| matches!(event, AgentEvent::ToolFinished { .. }))
    );
}

#[tokio::test]
async fn approval_timeout_is_bounded_and_never_executes() {
    let placeholder = TempDir::new("approval-timeout");
    let policy = ApprovalPolicy {
        timeout: Duration::from_millis(5),
        ..ApprovalPolicy::default()
    };
    let fx = Fixture::build(
        vec![
            turn("1. Inspect"),
            tool_turn("write_file", r#"{"path":"src/lib.rs","content":"ok"}"#),
            turn("DONE"),
        ],
        Vec::new(),
        FakeJev::pass(),
        FakeTools::new(vec![ToolReply::Success("should not run".into())]),
        FakeVerifier::new(
            vec![verification_command(placeholder.path())],
            vec![pass_report()],
        ),
        FakeWiki::ok(placeholder.path()),
        OrchestratorLimits::default(),
        CollectAudit::default(),
        policy,
        Arc::new(PendingApprovals),
    );

    let outcome = tokio::time::timeout(Duration::from_secs(1), fx.run(TaskBudget::default()))
        .await
        .expect("approval timeout must not hang")
        .expect("run");

    assert!(fx.tools.seen().is_empty());
    assert!(conversation_text(&outcome.context).contains("APPROVAL_EXPIRED"));
    assert!(has_event(&fx.events.events(), |event| matches!(
        event,
        AgentEvent::Notice {
            level: local_agent_harness::events::EventLevel::Warning,
            message,
        } if message.contains("approval expired")
    )));
    assert!(has_event(&fx.events.events(), |event| matches!(
        event,
        AgentEvent::ApprovalResolved {
            approved: false,
            ..
        }
    )));
}

#[tokio::test]
async fn missing_interactive_gate_fails_closed() {
    let placeholder = TempDir::new("approval-no-gate");
    let fx = Fixture::build(
        vec![
            turn("1. Inspect"),
            tool_turn("write_file", r#"{"path":"src/lib.rs","content":"ok"}"#),
            turn("DONE"),
        ],
        Vec::new(),
        FakeJev::pass(),
        FakeTools::new(vec![ToolReply::Success("should not run".into())]),
        FakeVerifier::new(
            vec![verification_command(placeholder.path())],
            vec![pass_report()],
        ),
        FakeWiki::ok(placeholder.path()),
        OrchestratorLimits::default(),
        CollectAudit::default(),
        ApprovalPolicy::default(),
        Arc::new(DenyApprovalGate),
    );

    let outcome = fx.run(TaskBudget::default()).await.expect("run");

    assert!(fx.tools.seen().is_empty());
    let feedback = conversation_text(&outcome.context);
    assert!(feedback.contains("APPROVAL_DENIED"));
    assert!(feedback.contains("no interactive approval gate"));
}

#[tokio::test]
async fn cancellation_while_waiting_for_approval_never_executes() {
    let placeholder = TempDir::new("approval-cancel");
    let cancel = CancellationToken::new();
    let fx = Fixture::build(
        vec![
            turn("1. Inspect"),
            tool_turn("write_file", r#"{"path":"src/lib.rs","content":"ok"}"#),
            turn("DONE"),
        ],
        Vec::new(),
        FakeJev::pass(),
        FakeTools::new(vec![ToolReply::Success("should not run".into())]),
        FakeVerifier::new(
            vec![verification_command(placeholder.path())],
            vec![pass_report()],
        ),
        FakeWiki::ok(placeholder.path()),
        OrchestratorLimits::default(),
        CollectAudit::default(),
        ApprovalPolicy::default(),
        Arc::new(CancelOnApproval {
            cancel: cancel.clone(),
        }),
    );

    let outcome = fx
        .run_with_objective(OBJECTIVE, TaskBudget::default(), cancel)
        .await
        .expect("run");

    assert_eq!(outcome.state(), LoopState::Cancelled);
    assert!(fx.tools.seen().is_empty());
    assert!(has_event(&fx.events.events(), |event| matches!(
        event,
        AgentEvent::Cancelled { .. }
    )));
}

#[tokio::test]
async fn closed_event_observer_does_not_fail_the_task() {
    let root = TempDir::new("closed-events");
    let (event_tx, event_rx) = mpsc::channel(1);
    drop(event_rx);
    let llm = Arc::new(
        FakeLlm::new(vec![turn("1. Inspect"), turn("DONE")]).with_streams(vec![vec![
            LlmStreamEvent::TextDelta("observer-free stream".into()),
        ]]),
    );
    let verifier = FakeVerifier::new(vec![verification_command(root.path())], vec![pass_report()]);
    let orchestrator = Orchestrator::new(
        llm,
        Arc::new(FakeJev::pass()),
        Arc::new(FakeTools::new(Vec::new())),
        Arc::new(verifier),
        Arc::new(FakeWiki::ok(root.path())),
    )
    .with_event_sink(Arc::new(ChannelEventSink::new(event_tx)))
    .with_approval_policy(ApprovalPolicy::default())
    .with_approval_gate(Arc::new(AllowApprovals))
    .with_limits(OrchestratorLimits::default());

    let outcome = orchestrator
        .run(
            TaskRequest::new(OBJECTIVE, root.path()).with_budget(TaskBudget::default()),
            CancellationToken::new(),
        )
        .await
        .expect("closed observer must not fail the task");

    assert_eq!(outcome.state(), LoopState::Completed);
}
