use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use local_agent_harness::agent::verify::{VerificationCommand, VerifyReport, VerifyStep};
use local_agent_harness::approval::{
    ApprovalDecision, ApprovalFuture, ApprovalGate, ApprovalPolicy, ApprovalRequest,
    ApprovalResolveError, InteractiveApprovalGate,
};
use local_agent_harness::audit::{AuditKind, AuditRecord, AuditSink};
use local_agent_harness::error::{AgentError, Result};
use local_agent_harness::events::AgentEvent;
use local_agent_harness::jev::{Route, Verdict};
use local_agent_harness::llm::{
    AssistantTurn, ChatRequest, FnCall, LlmBackend, LlmStreamEvent, ToolCall,
};
use local_agent_harness::orchestrator::{
    EventSink, JevAdvisor, Orchestrator, OrchestratorLimits, ToolExecutor, VerificationEngine,
    WikiStore,
};
use local_agent_harness::policy::Policy;
use local_agent_harness::script_runner::ScriptRunner;
use local_agent_harness::script_tools::ScriptToolExecutor;
use local_agent_harness::state_machine::{LoopState, TaskBudget, TaskMode};
use local_agent_harness::wiki::compile::{CompileReport, SessionTrace};
use local_agent_harness::wiki::{WikiContext, WikiStatus};
use sha2::{Digest, Sha256};
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const OBJECTIVE: &str = "Create the requested fixture file";

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!("rupi-write-{label}-{}", Uuid::new_v4()));
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

struct MutateOnApproval {
    path: Mutex<Option<PathBuf>>,
    replacement: String,
}

impl MutateOnApproval {
    fn new(replacement: &str) -> Self {
        Self {
            path: Mutex::new(None),
            replacement: replacement.into(),
        }
    }

    fn arm(&self, path: PathBuf) {
        *self.path.lock().expect("path lock") = Some(path);
    }
}

impl ApprovalGate for MutateOnApproval {
    fn request(&self, _request: &ApprovalRequest) -> ApprovalFuture {
        if let Some(path) = self.path.lock().expect("path lock").take() {
            std::fs::write(path, &self.replacement).expect("mutate target during approval");
        }
        Box::pin(async { ApprovalDecision::Approved })
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

struct FakeVerifier {
    commands: Vec<VerificationCommand>,
    reports: Mutex<VecDeque<VerifyReport>>,
    run_calls: AtomicUsize,
}

impl FakeVerifier {
    fn new(commands: Vec<VerificationCommand>, reports: Vec<VerifyReport>) -> Self {
        Self {
            commands,
            reports: Mutex::new(reports.into()),
            run_calls: AtomicUsize::new(0),
        }
    }

    fn run_calls(&self) -> usize {
        self.run_calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl VerificationEngine for FakeVerifier {
    async fn detect(&self, _root: &Path) -> Result<Vec<VerificationCommand>> {
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

struct FakeWiki;

#[async_trait]
impl WikiStore for FakeWiki {
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

struct Harness {
    _base: TempDir,
    project: PathBuf,
    llm: Arc<FakeLlm>,
    audit: Arc<CollectAudit>,
    events: Arc<CollectEvents>,
    verifier: Arc<FakeVerifier>,
    orchestrator: Arc<Orchestrator>,
}

struct HarnessOptions {
    approval_timeout: Duration,
    verification_passes: bool,
}

impl Default for HarnessOptions {
    fn default() -> Self {
        Self {
            approval_timeout: Duration::from_secs(120),
            verification_passes: true,
        }
    }
}

impl Harness {
    fn new(
        turns: Vec<AssistantTurn>,
        approvals: Arc<dyn ApprovalGate>,
        options: HarnessOptions,
    ) -> Self {
        let base = TempDir::new("case");
        let project = base.path().join("project");
        std::fs::create_dir_all(&project).expect("create project");

        let runner = ScriptRunner::new(&project, scripts_dir(), python_interpreter())
            .expect("construct script runner");
        let policy = Policy::new(&project).expect("policy");
        let llm = Arc::new(FakeLlm::new(turns));
        let events = Arc::new(CollectEvents::default());
        let audit = Arc::new(CollectAudit::default());
        let approvals_for_orchestrator = approvals.clone();
        let verifier = Arc::new(FakeVerifier::new(
            vec![verification_command(&project)],
            vec![if options.verification_passes {
                pass_report()
            } else {
                fail_report("fixture verification failed")
            }],
        ));

        let tools: Arc<dyn ToolExecutor> = Arc::new(
            ScriptToolExecutor::with_audit(runner, policy, Some(audit.clone()))
                .expect("script executor"),
        );
        let orchestrator = Arc::new(
            Orchestrator::new(
                llm.clone(),
                Arc::new(FakeJev),
                tools,
                verifier.clone(),
                Arc::new(FakeWiki),
            )
            .with_limits(OrchestratorLimits::default())
            .with_approval_policy(ApprovalPolicy {
                timeout: options.approval_timeout,
                ..ApprovalPolicy::default()
            })
            .with_approval_gate(approvals_for_orchestrator)
            .with_audit(audit.clone())
            .with_event_sink(events.clone()),
        );

        Self {
            _base: base,
            project,
            llm,
            audit,
            events,
            verifier,
            orchestrator,
        }
    }

    fn root(&self) -> &Path {
        &self.project
    }

    fn write(&self, relative: &str, content: &str) {
        let path = self.root().join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create parent");
        }
        std::fs::write(path, content).expect("write fixture");
    }

    async fn run(
        &self,
        budget: TaskBudget,
        cancel: CancellationToken,
    ) -> local_agent_harness::orchestrator::OrchestratorOutcome {
        self.orchestrator
            .clone()
            .run(
                local_agent_harness::orchestrator::TaskRequest::new(OBJECTIVE, self.root())
                    .with_mode(TaskMode::Change)
                    .with_budget(budget),
                cancel,
            )
            .await
            .expect("run task")
    }

    fn write_audit_records(&self) -> Vec<AuditRecord> {
        self.audit
            .records()
            .into_iter()
            .filter(|record| {
                record.kind == AuditKind::ScriptFinished
                    && record.metadata["action"].as_str() == Some("write_file")
            })
            .collect()
    }

    fn temporary_files(&self) -> Vec<PathBuf> {
        let mut files = Vec::new();
        collect_temporary_files(self.root(), &mut files);
        files
    }
}

fn collect_temporary_files(directory: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_temporary_files(&path, files);
        } else if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with(".tmp"))
        {
            files.push(path);
        }
    }
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

fn write_turn(path: &str, content: &str) -> AssistantTurn {
    tool_turn(
        "write_file",
        format!(
            r#"{{"path":{},"content":{}}}"#,
            serde_json::to_string(path).expect("path JSON"),
            serde_json::to_string(content).expect("content JSON")
        ),
    )
}

fn write_turn_with_digest(path: &str, content: &str, expected_sha256: &str) -> AssistantTurn {
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

fn task_script(path: &str, content: &str) -> Vec<AssistantTurn> {
    vec![
        turn("1. Create the requested file"),
        write_turn(path, content),
        turn("DONE"),
    ]
}

fn task_script_with_digest(path: &str, content: &str, expected_sha256: &str) -> Vec<AssistantTurn> {
    vec![
        turn("1. Update the requested file"),
        write_turn_with_digest(path, content, expected_sha256),
        turn("DONE"),
    ]
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
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
    panic!("python3 is required for write-file tests");
}

fn scripts_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts")
}

#[tokio::test]
async fn approved_nested_creation_is_audited_and_verified() {
    let approvals = Arc::new(RecordingApprovals::new(ApprovalDecision::Approved));
    let harness = Harness::new(
        task_script("src/generated/new.txt", "hello\nsnowman: \u{2603}\n"),
        approvals.clone(),
        HarnessOptions::default(),
    );

    let outcome = harness
        .run(TaskBudget::default(), CancellationToken::new())
        .await;

    assert_eq!(outcome.state(), LoopState::Completed);
    let target = harness.root().join("src/generated/new.txt");
    assert_eq!(
        std::fs::read_to_string(&target).expect("created file"),
        "hello\nsnowman: \u{2603}\n"
    );
    assert!(harness.temporary_files().is_empty());

    let requests = approvals.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].target.as_deref(), Some("src/generated/new.txt"));
    assert!(!requests[0].summary().contains("hello"));

    let records = harness.write_audit_records();
    assert_eq!(records.len(), 1);
    let metadata = &records[0].metadata;
    assert_eq!(metadata["path"], "src/generated/new.txt");
    assert_eq!(metadata["created"], true);
    assert_eq!(
        metadata["bytes_written"],
        "hello\nsnowman: \u{2603}\n".len()
    );
    assert!(metadata["sha256"].as_str().is_some_and(|hash| {
        hash.len() == 64
            && hash
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    }));
    assert!(!records[0].metadata.to_string().contains("snowman"));
    assert!(!records[0].message.contains("snowman"));

    assert_eq!(harness.verifier.run_calls(), 1);
    assert!(outcome.context.mutated);
    assert!(outcome.verified());
    assert!(
        harness
            .events
            .events()
            .iter()
            .any(|event| matches!(event, AgentEvent::VerificationFinished { passed: true, .. }))
    );
}

#[tokio::test]
async fn stale_approval_id_cannot_authorize_the_write() {
    let gate = Arc::new(InteractiveApprovalGate::new());
    let harness = Harness::new(
        task_script(
            "stale.txt",
            "stale
",
        ),
        gate.clone(),
        HarnessOptions::default(),
    );

    let orchestrator = harness.orchestrator.clone();
    let root = harness.root().to_path_buf();
    let task = tokio::spawn(async move {
        orchestrator
            .run(
                local_agent_harness::orchestrator::TaskRequest::new(OBJECTIVE, root)
                    .with_mode(TaskMode::Change)
                    .with_budget(TaskBudget::default()),
                CancellationToken::new(),
            )
            .await
    });

    let approval_id = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(approval_id) =
                harness
                    .events
                    .events()
                    .iter()
                    .find_map(|event| match event {
                        AgentEvent::ApprovalRequired { request } => {
                            Some(request.approval_id.clone())
                        }
                        _ => None,
                    })
            {
                return approval_id;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("approval request");

    assert_eq!(
        gate.resolve("approval-stale", true),
        Err(ApprovalResolveError::Mismatched)
    );
    assert!(!harness.root().join("stale.txt").exists());

    gate.resolve(&approval_id, true)
        .expect("resolve matching approval");
    let outcome = task.await.expect("join task").expect("run result");

    assert_eq!(outcome.state(), LoopState::Completed);
    assert_eq!(
        std::fs::read_to_string(harness.root().join("stale.txt")).expect("stale file"),
        "stale
"
    );
}

#[tokio::test]
async fn denied_approval_creates_no_file_or_dispatch() {
    let approvals = Arc::new(RecordingApprovals::new(ApprovalDecision::Denied));
    let harness = Harness::new(
        task_script("new.txt", "denied\n"),
        approvals.clone(),
        HarnessOptions::default(),
    );

    let outcome = harness
        .run(TaskBudget::default(), CancellationToken::new())
        .await;

    assert!(!harness.root().join("new.txt").exists());
    assert!(harness.temporary_files().is_empty());
    assert!(harness.write_audit_records().is_empty());
    assert_eq!(approvals.requests().len(), 1);
    assert!(!outcome.context.conversation.iter().any(|message| {
        message
            .content
            .as_deref()
            .is_some_and(|content| content.contains("write_file path=new.txt"))
    }));
}

#[tokio::test]
async fn expired_approval_creates_no_file_or_dispatch() {
    let harness = Harness::new(
        task_script("expired.txt", "expired\n"),
        Arc::new(PendingApprovals),
        HarnessOptions {
            approval_timeout: Duration::from_millis(25),
            ..HarnessOptions::default()
        },
    );

    let outcome = harness
        .run(TaskBudget::default(), CancellationToken::new())
        .await;

    assert!(!harness.root().join("expired.txt").exists());
    assert!(harness.write_audit_records().is_empty());
    assert!(harness.events.events().iter().any(|event| matches!(
        event,
        AgentEvent::Notice { message, .. } if message.contains("approval expired")
    )));
    assert!(!outcome.context.conversation.iter().any(|message| {
        message
            .content
            .as_deref()
            .is_some_and(|content| content.contains("write_file path=expired.txt"))
    }));
}

#[tokio::test]
async fn cancellation_while_waiting_for_approval_creates_no_file() {
    let cancel = CancellationToken::new();
    let harness = Harness::new(
        task_script("cancelled.txt", "cancelled\n"),
        Arc::new(CancelOnApproval {
            cancel: cancel.clone(),
        }),
        HarnessOptions::default(),
    );

    let outcome = harness.run(TaskBudget::default(), cancel).await;

    assert_eq!(outcome.state(), LoopState::Cancelled);
    assert!(!harness.root().join("cancelled.txt").exists());
    assert!(harness.write_audit_records().is_empty());
}

#[tokio::test]
async fn cancellation_before_dispatch_creates_no_file() {
    let cancel = CancellationToken::new();
    cancel.cancel();
    let harness = Harness::new(
        task_script("pre-cancelled.txt", "cancelled\n"),
        Arc::new(AllowApprovals),
        HarnessOptions::default(),
    );

    let outcome = harness.run(TaskBudget::default(), cancel).await;

    assert_eq!(outcome.state(), LoopState::Cancelled);
    assert!(!harness.root().join("pre-cancelled.txt").exists());
    assert_eq!(harness.llm.calls(), 0);
    assert!(harness.write_audit_records().is_empty());
}

#[tokio::test]
async fn approved_overwrite_with_matching_digest_is_audited() {
    let old_content = "original\n";
    let new_content = "replacement\n";
    let old_digest = sha256_hex(old_content.as_bytes());
    let new_digest = sha256_hex(new_content.as_bytes());
    let approvals = Arc::new(RecordingApprovals::new(ApprovalDecision::Approved));
    let harness = Harness::new(
        task_script_with_digest("existing.txt", new_content, &old_digest),
        approvals.clone(),
        HarnessOptions::default(),
    );
    harness.write("existing.txt", old_content);

    let outcome = harness
        .run(TaskBudget::default(), CancellationToken::new())
        .await;

    assert_eq!(outcome.state(), LoopState::Completed);
    assert_eq!(
        std::fs::read_to_string(harness.root().join("existing.txt")).expect("existing file"),
        new_content
    );
    assert!(harness.temporary_files().is_empty());
    assert_eq!(approvals.requests().len(), 1);
    assert_eq!(
        approvals.requests()[0].target.as_deref(),
        Some("existing.txt")
    );

    let records = harness.write_audit_records();
    assert_eq!(records.len(), 1);
    let metadata = &records[0].metadata;
    assert_eq!(metadata["path"], "existing.txt");
    assert_eq!(metadata["created"], false);
    assert_eq!(metadata["bytes_written"], new_content.len());
    assert_eq!(metadata["sha256"], new_digest);
    assert_eq!(metadata["previous_sha256"], old_digest);
    assert!(!metadata.to_string().contains("replacement"));
    assert_eq!(harness.verifier.run_calls(), 1);
}

#[tokio::test]
async fn overwrite_without_expected_digest_is_rejected() {
    let harness = Harness::new(
        task_script("existing.txt", "replacement\n"),
        Arc::new(AllowApprovals),
        HarnessOptions::default(),
    );
    harness.write("existing.txt", "original\n");

    let outcome = harness
        .run(TaskBudget::default(), CancellationToken::new())
        .await;

    assert_eq!(
        std::fs::read_to_string(harness.root().join("existing.txt")).expect("existing file"),
        "original\n"
    );
    assert!(harness.temporary_files().is_empty());
    assert!(
        harness
            .write_audit_records()
            .iter()
            .all(|record| record.metadata["ok"] == false)
    );
    let feedback = outcome
        .context
        .conversation
        .iter()
        .filter_map(|message| message.content.as_deref())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(feedback.contains("WRITE_HASH_REQUIRED"), "{feedback}");
}

#[tokio::test]
async fn overwrite_digest_mismatch_preserves_target() {
    let harness = Harness::new(
        task_script_with_digest(
            "existing.txt",
            "replacement\n",
            &sha256_hex(b"different content\n"),
        ),
        Arc::new(AllowApprovals),
        HarnessOptions::default(),
    );
    harness.write("existing.txt", "original\n");

    let outcome = harness
        .run(TaskBudget::default(), CancellationToken::new())
        .await;

    assert_eq!(
        std::fs::read_to_string(harness.root().join("existing.txt")).expect("existing file"),
        "original\n"
    );
    assert!(harness.temporary_files().is_empty());
    let feedback = outcome
        .context
        .conversation
        .iter()
        .filter_map(|message| message.content.as_deref())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(feedback.contains("WRITE_HASH_MISMATCH"), "{feedback}");
}

#[tokio::test]
async fn target_changed_during_approval_is_rejected() {
    let old_digest = sha256_hex(b"original\n");
    let gate = Arc::new(MutateOnApproval::new("changed elsewhere\n"));
    let harness = Harness::new(
        task_script_with_digest("existing.txt", "replacement\n", &old_digest),
        gate.clone(),
        HarnessOptions::default(),
    );
    harness.write("existing.txt", "original\n");
    gate.arm(harness.root().join("existing.txt"));

    let outcome = harness
        .run(TaskBudget::default(), CancellationToken::new())
        .await;

    assert_eq!(
        std::fs::read_to_string(harness.root().join("existing.txt")).expect("existing file"),
        "changed elsewhere\n"
    );
    assert!(harness.temporary_files().is_empty());
    let feedback = outcome
        .context
        .conversation
        .iter()
        .filter_map(|message| message.content.as_deref())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(feedback.contains("WRITE_HASH_MISMATCH"), "{feedback}");
}

#[tokio::test]
async fn successful_mutation_cannot_complete_without_verification() {
    let harness = Harness::new(
        task_script("requires-verification.txt", "content\n"),
        Arc::new(AllowApprovals),
        HarnessOptions {
            verification_passes: false,
            ..HarnessOptions::default()
        },
    );
    let budget = TaskBudget {
        max_verify_retries: 0,
        ..TaskBudget::default()
    };

    let outcome = harness.run(budget, CancellationToken::new()).await;

    assert_eq!(outcome.state(), LoopState::Blocked);
    assert!(harness.root().join("requires-verification.txt").is_file());
    assert_eq!(harness.verifier.run_calls(), 1);
    assert!(!outcome.verified());
    assert!(outcome.context.mutated);
}

#[tokio::test]
async fn tool_results_contain_write_metadata_but_not_content() {
    let harness = Harness::new(
        task_script("metadata.txt", "MODEL_WRITE_CONTENT_SECRET\n"),
        Arc::new(AllowApprovals),
        HarnessOptions::default(),
    );

    let outcome = harness
        .run(TaskBudget::default(), CancellationToken::new())
        .await;

    assert_eq!(outcome.state(), LoopState::Completed);
    let feedback = outcome
        .context
        .conversation
        .iter()
        .filter_map(|message| message.content.as_deref())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        feedback.contains("write_file path=metadata.txt"),
        "{feedback}"
    );
    assert!(feedback.contains("sha256="), "{feedback}");
    assert!(!feedback.contains("MODEL_WRITE_CONTENT_SECRET"));
    assert!(!harness.llm.requests().iter().any(|request| {
        request.messages.iter().any(|message| {
            message.role == "tool"
                && message
                    .content
                    .as_deref()
                    .is_some_and(|content| content.contains("MODEL_WRITE_CONTENT_SECRET"))
        })
    }));
}
