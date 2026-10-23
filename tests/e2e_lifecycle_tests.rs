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
};
use local_agent_harness::audit::{AuditKind, AuditLog, AuditRecord, AuditSink};
use local_agent_harness::config::{
    McpProfile, McpRuntimeConfig, McpSandboxConfig, McpServerConfig,
};
use local_agent_harness::error::{AgentError, Result};
use local_agent_harness::events::AgentEvent;
use local_agent_harness::jev::{Route, Verdict};
use local_agent_harness::llm::{
    AssistantTurn, ChatRequest, FnCall, LlmBackend, LlmStreamEvent, ToolCall,
};
use local_agent_harness::mcp::client::DEFAULT_MCP_PROTOCOL_VERSION;
use local_agent_harness::mcp::process::DEFAULT_STDERR_CAPACITY_BYTES;
use local_agent_harness::mcp::{AgentTool, McpRuntime, ToolOutcome};
use local_agent_harness::orchestrator::{
    Clock, EventSink, JevAdvisor, LocalWikiStore, Orchestrator, OrchestratorLimits,
    OrchestratorOutcome, TaskContext, TaskRequest, ToolExecutor, VerificationEngine, WikiStore,
};
use local_agent_harness::policy::Policy;
use local_agent_harness::state_machine::{LoopState, TaskBudget};
use local_agent_harness::wiki::compile::{CompileReport, SessionTrace};
use local_agent_harness::wiki::{
    REQUIRED_HEADINGS, WikiContext, WikiLimits, WikiStatus, state_path,
};
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const OBJECTIVE: &str = "Fix the fixture safely";
const HIDDEN_REASONING: &str = "HIDDEN_E2E_REASONING";
const TOOL_SECRET: &str = "E2E_TOOL_SECRET";
const DENIED_SECRET: &str = "E2E_DENIED_SECRET";
const FAIL_SECRET: &str = "E2E_FAIL_SECRET";

struct TempRepo {
    path: PathBuf,
}

impl TempRepo {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!("rupi-e2e-{label}-{}", Uuid::new_v4()));
        std::fs::create_dir_all(path.join("src")).expect("create temp repository");
        std::fs::write(path.join("src/lib.rs"), "pub fn value() -> u8 { 1 }\n")
            .expect("seed fixture");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempRepo {
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

#[derive(Default)]
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

struct RepoExecutor {
    root: PathBuf,
    policy: Policy,
    cancel_on_write: Option<CancellationToken>,
    seen: Mutex<Vec<AgentTool>>,
    process_calls: AtomicUsize,
}

impl RepoExecutor {
    fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
            policy: Policy::default_for_root(root),
            cancel_on_write: None,
            seen: Mutex::new(Vec::new()),
            process_calls: AtomicUsize::new(0),
        }
    }

    fn with_cancel(root: &Path, cancel: CancellationToken) -> Self {
        Self {
            cancel_on_write: Some(cancel),
            ..Self::new(root)
        }
    }

    fn seen(&self) -> Vec<AgentTool> {
        self.seen.lock().expect("seen lock").clone()
    }

    fn process_calls(&self) -> usize {
        self.process_calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl ToolExecutor for RepoExecutor {
    fn authorize(&self, tool: &AgentTool) -> std::result::Result<(), String> {
        self.policy
            .authorize(tool)
            .map_err(|error| error.to_string())
    }

    fn check_capability(&self, _tool: &AgentTool) -> std::result::Result<(), String> {
        Ok(())
    }

    async fn execute(&self, tool_call_id: String, tool: &AgentTool) -> ToolOutcome {
        self.seen.lock().expect("seen lock").push(tool.clone());

        if let Err(violation) = self.policy.authorize(tool) {
            return ToolOutcome::policy_denied(
                tool_call_id,
                format!("policy denied action: {violation}"),
            );
        }

        match tool {
            AgentTool::WriteFile { path, content, .. } => {
                let relative = self
                    .policy
                    .authorize_path(path)
                    .expect("path was already authorized");
                let destination = self.root.join(relative);
                if let Some(parent) = destination.parent() {
                    std::fs::create_dir_all(parent).expect("create destination parent");
                }
                std::fs::write(&destination, content).expect("write fixture file");

                if let Some(cancel) = &self.cancel_on_write {
                    cancel.cancel();
                }

                ToolOutcome::success(
                    tool_call_id,
                    format!("wrote {path} token={TOOL_SECRET} {}", "x".repeat(8_000)),
                )
            }
            AgentTool::RunCommand { .. } => {
                self.process_calls.fetch_add(1, Ordering::SeqCst);
                ToolOutcome::capability_unavailable(
                    tool_call_id,
                    "E2E executor does not spawn processes",
                )
            }
            _ => ToolOutcome::success(tool_call_id, "ok"),
        }
    }
}

struct FakeVerifier {
    commands: Vec<VerificationCommand>,
    reports: Mutex<VecDeque<VerifyReport>>,
    detect_calls: AtomicUsize,
    run_calls: AtomicUsize,
}

impl FakeVerifier {
    fn new(commands: Vec<VerificationCommand>, reports: Vec<VerifyReport>) -> Self {
        Self {
            commands,
            reports: Mutex::new(reports.into()),
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
        Ok(self.commands.clone())
    }

    async fn run(
        &self,
        _root: &Path,
        _commands: &[VerificationCommand],
        _output_cap: usize,
    ) -> Result<VerifyReport> {
        self.run_calls.fetch_add(1, Ordering::SeqCst);
        Ok(self
            .reports
            .lock()
            .expect("reports lock")
            .pop_front()
            .unwrap_or_else(|| fail_report("verification did not pass")))
    }
}

struct FailingWiki {
    compile_calls: AtomicUsize,
}

impl FailingWiki {
    fn new() -> Self {
        Self {
            compile_calls: AtomicUsize::new(0),
        }
    }

    fn compile_calls(&self) -> usize {
        self.compile_calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl WikiStore for FailingWiki {
    async fn load(&self, root: &Path) -> Result<WikiContext> {
        Ok(WikiContext::empty(root))
    }

    async fn compile(
        &self,
        _root: &Path,
        _prior: &WikiContext,
        _trace: &SessionTrace,
        _today: &str,
    ) -> Result<CompileReport> {
        self.compile_calls.fetch_add(1, Ordering::SeqCst);
        Err(AgentError::Wiki("deterministic compile failure".into()))
    }
}

struct FakeClock {
    now: DateTime<Utc>,
}

impl Default for FakeClock {
    fn default() -> Self {
        Self {
            now: DateTime::parse_from_rfc3339("2026-01-02T03:04:05Z")
                .expect("fixed timestamp")
                .with_timezone(&Utc),
        }
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

struct E2E {
    root: PathBuf,
    llm: Arc<FakeLlm>,
    tools: Arc<RepoExecutor>,
    verifier: Arc<FakeVerifier>,
    events: Arc<CollectEvents>,
    audit: Arc<CollectAudit>,
    orchestrator: Orchestrator,
}

impl E2E {
    fn new(
        script: Vec<AssistantTurn>,
        root: &Path,
        tools: RepoExecutor,
        verifier: FakeVerifier,
        wiki: Arc<dyn WikiStore>,
    ) -> Self {
        let llm = Arc::new(FakeLlm::new(script));
        let tools = Arc::new(tools);
        let verifier = Arc::new(verifier);
        let events = Arc::new(CollectEvents::default());
        let audit = Arc::new(CollectAudit::default());
        let orchestrator = Orchestrator::new(
            llm.clone(),
            Arc::new(PassingJev),
            tools.clone(),
            verifier.clone(),
            wiki,
        )
        .with_audit(audit.clone())
        .with_clock(Arc::new(FakeClock::default()))
        .with_event_sink(events.clone())
        .with_approval_policy(ApprovalPolicy::default())
        .with_approval_gate(Arc::new(AllowApprovals))
        .with_limits(OrchestratorLimits::default());

        Self {
            root: root.to_path_buf(),
            llm,
            tools,
            verifier,
            events,
            audit,
            orchestrator,
        }
    }

    async fn run(
        &self,
        budget: TaskBudget,
        cancel: CancellationToken,
    ) -> Result<OrchestratorOutcome> {
        self.orchestrator
            .run(
                TaskRequest::new(OBJECTIVE, &self.root).with_budget(budget),
                cancel,
            )
            .await
    }

    fn events(&self) -> Vec<AgentEvent> {
        self.events.events()
    }

    fn audit(&self) -> Vec<AuditRecord> {
        self.audit.records()
    }
}

fn turn(text: impl Into<String>) -> AssistantTurn {
    AssistantTurn {
        text: text.into(),
        ..AssistantTurn::default()
    }
}

fn plan_turn() -> AssistantTurn {
    AssistantTurn {
        text: "1. Inspect\n2. Update".into(),
        reasoning_content: Some(HIDDEN_REASONING.into()),
        ..AssistantTurn::default()
    }
}

fn tool_call(id: &str, name: &str, arguments: impl Into<String>) -> ToolCall {
    ToolCall {
        id: id.into(),
        kind: "function".into(),
        function: FnCall {
            name: name.into(),
            arguments: arguments.into(),
        },
    }
}

fn tool_turn(name: &str, arguments: impl Into<String>) -> AssistantTurn {
    multi_tool_turn(vec![tool_call("call_1", name, arguments)])
}

fn multi_tool_turn(calls: Vec<ToolCall>) -> AssistantTurn {
    AssistantTurn {
        text: "working".into(),
        calls,
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

async fn assert_wiki_green(root: &Path) {
    let wiki = local_agent_harness::wiki::load(root)
        .await
        .expect("load compiled wiki");
    assert_eq!(wiki.frontmatter.status, WikiStatus::Green);
    assert_eq!(wiki.frontmatter.session_count, 1);

    let state = tokio::fs::read_to_string(state_path(root))
        .await
        .expect("read wiki state");
    for heading in REQUIRED_HEADINGS {
        assert!(
            state.contains(&format!("## {heading}")),
            "missing required heading: {heading}"
        );
    }
    assert!(state.len() <= WikiLimits::default().max_state_bytes);
}

#[tokio::test]
async fn verified_mutation_reaches_completed_with_real_wiki_output() {
    let repo = TempRepo::new("happy");
    let tools = RepoExecutor::new(repo.path());
    let verifier = FakeVerifier::new(vec![verification_command(repo.path())], vec![pass_report()]);
    let fx = E2E::new(
        vec![
            plan_turn(),
            tool_turn(
                "write_file",
                r#"{"path":"src/lib.rs","content":"pub fn value() -> u8 { 42 }"}"#,
            ),
            turn("DONE"),
        ],
        repo.path(),
        tools,
        verifier,
        Arc::new(LocalWikiStore::default()),
    );

    let outcome = fx
        .run(TaskBudget::default(), CancellationToken::new())
        .await
        .expect("run");

    assert_eq!(outcome.state(), LoopState::Completed);
    assert!(outcome.verified());
    assert!(outcome.context.mutated);
    assert_eq!(fx.llm.calls(), 3);
    assert_eq!(fx.tools.seen().len(), 1);
    assert_eq!(fx.tools.process_calls(), 0);
    assert_eq!(fx.verifier.detect_calls(), 1);
    assert_eq!(fx.verifier.run_calls(), 1);

    let source = std::fs::read_to_string(repo.path().join("src/lib.rs")).expect("read source");
    assert!(source.contains("42"));

    assert_wiki_green(repo.path()).await;

    let state = tokio::fs::read_to_string(state_path(repo.path()))
        .await
        .expect("read wiki state");
    let session_record =
        tokio::fs::read_to_string(local_agent_harness::wiki::session_record_path(repo.path()))
            .await
            .expect("read session record");
    let trace = serde_json::to_string(&outcome.context.session_trace).expect("trace");
    let audit = serde_json::to_string(&fx.audit()).expect("audit");
    let events = format!("{:?}", fx.events());

    assert!(state.contains("[REDACTED]"));
    for surface in [&state, &session_record, &trace, &audit, &events] {
        assert!(!surface.contains(TOOL_SECRET));
        assert!(!surface.contains(HIDDEN_REASONING));
        assert!(!surface.contains(&"x".repeat(1_000)));
        assert!(!surface.contains("You are the System 2 reasoning component"));
        assert!(!surface.contains("2. Update"));
    }
    assert!(
        session_record
            .lines()
            .all(|line| line.len() <= WikiLimits::default().max_state_bytes)
    );
    assert!(has_event(&fx.events(), |event| matches!(
        event,
        AgentEvent::WikiCompiled { .. }
    )));
}

#[tokio::test]
async fn failed_verification_retries_within_budget_then_blocks_without_wiki() {
    let repo = TempRepo::new("verify-failure");
    let tools = RepoExecutor::new(repo.path());
    let verifier = FakeVerifier::new(
        vec![verification_command(repo.path())],
        vec![
            fail_report(format!("still failing token={FAIL_SECRET}")),
            fail_report(format!("still failing token={FAIL_SECRET}")),
        ],
    );
    let fx = E2E::new(
        vec![
            plan_turn(),
            tool_turn(
                "write_file",
                r#"{"path":"src/lib.rs","content":"pub fn value() -> u8 { 2 }"}"#,
            ),
            turn("DONE"),
            turn("DONE"),
        ],
        repo.path(),
        tools,
        verifier,
        Arc::new(LocalWikiStore::default()),
    );
    let budget = TaskBudget {
        max_verify_retries: 1,
        ..TaskBudget::default()
    };

    let outcome = fx.run(budget, CancellationToken::new()).await.expect("run");

    assert_eq!(outcome.state(), LoopState::Blocked);
    assert!(!outcome.verified());
    assert_eq!(fx.verifier.run_calls(), 2);
    assert!(!state_path(repo.path()).exists());
    assert!(!has_event(&fx.events(), |event| matches!(
        event,
        AgentEvent::Completed { .. }
    )));
    assert!(has_event(&fx.events(), |event| matches!(
        event,
        AgentEvent::VerificationFinished { passed: false, .. }
    )));
    assert!(conversation_text(&outcome.context).contains("[REDACTED]"));
    assert!(!conversation_text(&outcome.context).contains(FAIL_SECRET));
    assert!(
        !serde_json::to_string(&fx.audit())
            .expect("audit")
            .contains(FAIL_SECRET)
    );
}

#[tokio::test]
async fn no_mutation_completion_still_requires_harness_verification() {
    let repo = TempRepo::new("no-mutation");
    let tools = RepoExecutor::new(repo.path());
    let verifier = FakeVerifier::new(vec![verification_command(repo.path())], vec![pass_report()]);
    let fx = E2E::new(
        vec![plan_turn(), turn("DONE")],
        repo.path(),
        tools,
        verifier,
        Arc::new(LocalWikiStore::default()),
    );

    let outcome = fx
        .run(TaskBudget::default(), CancellationToken::new())
        .await
        .expect("run");

    assert_eq!(outcome.state(), LoopState::Completed);
    assert!(outcome.verified());
    assert!(!outcome.context.mutated);
    assert_eq!(fx.tools.seen().len(), 0);
    assert_eq!(fx.tools.process_calls(), 0);
    assert_eq!(fx.verifier.detect_calls(), 1);
    assert_eq!(fx.verifier.run_calls(), 1);
    assert_wiki_green(repo.path()).await;
}

#[tokio::test]
async fn no_mutation_claim_without_a_verifier_blocks() {
    let repo = TempRepo::new("no-verifier");
    let tools = RepoExecutor::new(repo.path());
    let verifier = FakeVerifier::new(Vec::new(), Vec::new());
    let fx = E2E::new(
        vec![plan_turn(), turn("DONE")],
        repo.path(),
        tools,
        verifier,
        Arc::new(LocalWikiStore::default()),
    );

    let outcome = fx
        .run(TaskBudget::default(), CancellationToken::new())
        .await
        .expect("run");

    assert_eq!(outcome.state(), LoopState::Blocked);
    assert!(!outcome.verified());
    assert_eq!(fx.verifier.detect_calls(), 1);
    assert_eq!(fx.verifier.run_calls(), 0);
    assert!(!state_path(repo.path()).exists());
    assert!(
        outcome
            .summary
            .contains("no applicable verification commands")
    );
}

#[tokio::test]
async fn policy_denial_has_no_effect_and_a_later_success_claim_cannot_complete() {
    let repo = TempRepo::new("policy");
    let tools = RepoExecutor::new(repo.path());
    let verifier = FakeVerifier::new(
        vec![verification_command(repo.path())],
        vec![fail_report("verification failed")],
    );
    let fx = E2E::new(
        vec![
            plan_turn(),
            multi_tool_turn(vec![
                tool_call(
                    "call_1",
                    "write_file",
                    format!(r#"{{"path":".env","content":"token={DENIED_SECRET}"}}"#),
                ),
                tool_call(
                    "call_2",
                    "run_command",
                    r#"{"program":"curl","args":["https://example.invalid"],"timeout_secs":5}"#,
                ),
            ]),
            turn("DONE"),
        ],
        repo.path(),
        tools,
        verifier,
        Arc::new(LocalWikiStore::default()),
    );
    let budget = TaskBudget {
        max_verify_retries: 0,
        ..TaskBudget::default()
    };

    let outcome = fx.run(budget, CancellationToken::new()).await.expect("run");

    assert_eq!(outcome.state(), LoopState::Blocked);
    assert!(!outcome.context.mutated);
    assert_eq!(fx.tools.seen().len(), 0);
    assert_eq!(fx.tools.process_calls(), 0);
    assert!(!repo.path().join(".env").exists());
    assert_eq!(outcome.context.session_trace.policy_denials.len(), 2);

    let feedback = conversation_text(&outcome.context);
    assert!(feedback.contains("policy denied action"));
    assert!(feedback.contains("sensitive path is denied"));
    assert!(feedback.contains("program is not allowlisted"));
    assert!(has_event(&fx.events(), |event| matches!(
        event,
        AgentEvent::PolicyDenied { .. }
    )));
    assert!(!has_event(&fx.events(), |event| matches!(
        event,
        AgentEvent::Completed { .. }
    )));
    assert!(!state_path(repo.path()).exists());
    assert!(
        !serde_json::to_string(&fx.audit())
            .expect("audit")
            .contains(DENIED_SECRET)
    );
    assert!(!format!("{:?}", fx.events()).contains(DENIED_SECRET));
}

#[tokio::test]
async fn cancellation_after_a_mutation_never_completes_or_compiles_wiki() {
    let repo = TempRepo::new("cancel");
    let cancel = CancellationToken::new();
    let tools = RepoExecutor::with_cancel(repo.path(), cancel.clone());
    let verifier = FakeVerifier::new(vec![verification_command(repo.path())], vec![pass_report()]);
    let fx = E2E::new(
        vec![
            plan_turn(),
            tool_turn(
                "write_file",
                r#"{"path":"src/lib.rs","content":"pub fn value() -> u8 { 3 }"}"#,
            ),
            turn("DONE"),
        ],
        repo.path(),
        tools,
        verifier,
        Arc::new(LocalWikiStore::default()),
    );

    let outcome = fx.run(TaskBudget::default(), cancel).await.expect("run");

    assert_eq!(outcome.state(), LoopState::Cancelled);
    assert!(outcome.context.mutated);
    assert_eq!(fx.tools.seen().len(), 1);
    assert_eq!(fx.verifier.detect_calls(), 0);
    assert!(!state_path(repo.path()).exists());
    assert!(has_event(&fx.events(), |event| matches!(
        event,
        AgentEvent::Cancelled { .. }
    )));
    assert!(!has_event(&fx.events(), |event| matches!(
        event,
        AgentEvent::Completed { .. }
    )));
    let finished = fx
        .audit()
        .into_iter()
        .find(|record| record.kind == AuditKind::TaskFinished)
        .expect("terminal audit record");
    assert_eq!(finished.metadata["state"].as_str(), Some("Cancelled"));
}

#[tokio::test]
async fn wiki_compile_failure_after_verified_work_prevents_completion() {
    let repo = TempRepo::new("compile-failure");
    let tools = RepoExecutor::new(repo.path());
    let verifier = FakeVerifier::new(vec![verification_command(repo.path())], vec![pass_report()]);
    let wiki = Arc::new(FailingWiki::new());
    let fx = E2E::new(
        vec![
            plan_turn(),
            tool_turn(
                "write_file",
                r#"{"path":"src/lib.rs","content":"pub fn value() -> u8 { 4 }"}"#,
            ),
            turn("DONE"),
        ],
        repo.path(),
        tools,
        verifier,
        wiki.clone(),
    );

    let outcome = fx
        .run(TaskBudget::default(), CancellationToken::new())
        .await
        .expect("run");

    assert_eq!(outcome.state(), LoopState::Failed);
    assert!(outcome.verified());
    assert_eq!(wiki.compile_calls(), 1);
    assert!(!state_path(repo.path()).exists());
    assert!(!has_event(&fx.events(), |event| matches!(
        event,
        AgentEvent::Completed { .. }
    )));
    assert!(has_event(&fx.events(), |event| matches!(
        event,
        AgentEvent::Failed { .. }
    )));
    assert!(
        outcome
            .context
            .session_trace
            .blockers
            .iter()
            .any(|blocker| blocker.contains("Wiki compilation failed"))
    );
}

const MCP_SERVER: &str = env!("CARGO_BIN_EXE_mock_mcp_server");
const MCP_TOOL_SECRET: &str = "MCP_E2E_SECRET";

struct ContentVerifier {
    relative_path: String,
    expected: String,
}

#[async_trait]
impl VerificationEngine for ContentVerifier {
    async fn detect(&self, root: &Path) -> Result<Vec<VerificationCommand>> {
        Ok(vec![VerificationCommand {
            program: "content-check".into(),
            args: vec![self.relative_path.clone()],
            cwd: root.to_path_buf(),
            timeout: Duration::from_secs(5),
        }])
    }

    async fn run(
        &self,
        root: &Path,
        _commands: &[VerificationCommand],
        _output_cap: usize,
    ) -> Result<VerifyReport> {
        let content = std::fs::read_to_string(root.join(&self.relative_path))?;
        let passed = content.contains(&self.expected);
        Ok(VerifyReport {
            passed,
            steps: vec![VerifyStep {
                command: format!("content-check {}", self.relative_path),
                exit_code: i32::from(!passed),
                duration: Duration::from_millis(1),
                timed_out: false,
            }],
            tail: if passed {
                String::new()
            } else {
                "expected content missing".into()
            },
        })
    }
}

fn contained_server(root: &Path, tools: &str, extra: &[&str], bwrap: &Path) -> McpServerConfig {
    let mut args = vec!["--tools".to_owned(), tools.to_owned()];
    args.extend(extra.iter().map(|arg| (*arg).to_owned()));
    McpServerConfig {
        enabled: true,
        executable: PathBuf::from(MCP_SERVER),
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

#[tokio::test]
async fn contained_real_mcp_mutation_is_verified_audited_and_wiki_compiled() {
    let bwrap = ["/usr/bin/bwrap", "/bin/bwrap"]
        .into_iter()
        .map(PathBuf::from)
        .find(|path| path.is_file());
    let Some(bwrap) = bwrap else {
        eprintln!("skipping real-MCP boundary E2E: bwrap not installed");
        return;
    };

    let repo = TempRepo::new("real-mcp-boundary");
    let config = McpRuntimeConfig {
        profile: McpProfile::WorkspaceMutation,
        filesystem: Some(contained_server(
            repo.path(),
            "read_file,write_file,apply_patch",
            &["--real-filesystem"],
            &bwrap,
        )),
        git: Some(contained_server(repo.path(), "status,diff", &[], &bwrap)),
        shell: Some(contained_server(repo.path(), "run", &[], &bwrap)),
        path: None,
        allow_unsandboxed: false,
    };
    let runtime = McpRuntime::start(
        &config,
        repo.path(),
        Policy::default_for_root(repo.path()),
        8_000,
    )
    .await
    .expect("start contained MCP runtime");
    assert!(
        runtime
            .warnings()
            .iter()
            .any(|warning| warning.contains("bubblewrap network namespaces"))
    );

    let llm = Arc::new(FakeLlm::new(vec![
        plan_turn(),
        tool_turn(
            "write_file",
            r#"{"path":"src/lib.rs","content":"pub fn value() -> u8 { 42 }\n"}"#,
        ),
        turn("DONE"),
    ]));
    let events = Arc::new(CollectEvents::default());
    let audit = Arc::new(AuditLog::new(repo.path().join("audit.jsonl")));
    let tools: Arc<dyn ToolExecutor> = runtime.dispatcher();
    let orchestrator = Orchestrator::new(
        llm.clone(),
        Arc::new(PassingJev),
        tools,
        Arc::new(ContentVerifier {
            relative_path: "src/lib.rs".into(),
            expected: "42".into(),
        }),
        Arc::new(LocalWikiStore::default()),
    )
    .with_audit(audit.clone())
    .with_clock(Arc::new(FakeClock::default()))
    .with_event_sink(events.clone())
    .with_approval_policy(ApprovalPolicy::default())
    .with_approval_gate(Arc::new(AllowApprovals));

    let outcome = orchestrator
        .run(
            TaskRequest::new(OBJECTIVE, repo.path()).with_budget(TaskBudget::default()),
            CancellationToken::new(),
        )
        .await
        .expect("run real-MCP task");

    assert_eq!(outcome.state(), LoopState::Completed);
    assert!(outcome.verified());
    assert!(outcome.context.mutated);
    assert!(
        std::fs::read_to_string(repo.path().join("src/lib.rs"))
            .expect("read mutated file")
            .contains("42")
    );
    assert_wiki_green(repo.path()).await;

    let audit_text = std::fs::read_to_string(repo.path().join("audit.jsonl")).expect("audit log");
    assert!(audit_text.contains("task_started"));
    assert!(audit_text.contains("tool_finished"));
    assert!(audit_text.contains("task_finished"));
    for surface in [
        audit_text,
        format!("{:?}", events.events()),
        serde_json::to_string(&outcome.context.session_trace).expect("trace"),
    ] {
        assert!(!surface.contains(MCP_TOOL_SECRET));
        assert!(!surface.contains(HIDDEN_REASONING));
    }

    drop(orchestrator);
    runtime
        .shutdown(Duration::from_secs(2))
        .await
        .expect("shutdown runtime");
}
