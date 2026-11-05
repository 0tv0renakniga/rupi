use crate::approval::ApprovalRequest;
use crate::session::SessionMessage;
use crate::state_machine::{LoopState, TaskMode};

/// Severity for user-facing notices. Display code must still bound and redact
/// the message before rendering it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChatTurnTelemetry {
    pub estimated_prompt_tokens: u32,
    pub reserved_output_tokens: u32,
    pub effective_context_cap_tokens: u32,
    pub safety_margin_tokens: u32,
    pub context_utilization_percent: u32,
    pub time_to_first_token_ms: Option<u64>,
    pub visible_output_chars: usize,
    pub visible_output_tokens: u32,
    pub finish_reason: Option<String>,
    pub empty_visible_response: bool,
    pub transport_error_category: Option<String>,
    pub admission_rejected: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventLevel {
    Info,
    Warning,
    Error,
}

/// Headless orchestration events. Payloads are bounded and redacted by the
/// orchestrator; they never contain prompts, reasoning content, or raw bodies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentEvent {
    /// The worker accepted a task and is about to start the orchestrator.
    TaskStarted {
        summary: String,
    },
    /// A reusable Chat turn was accepted by the worker.
    ChatTurnStarted {
        prompt: String,
    },
    /// The worker opened a session and restored its visible Chat messages.
    SessionLoaded {
        session_id: String,
        messages: Vec<SessionMessage>,
    },
    /// A Chat turn completed without entering a terminal task state.
    ChatTurnFinished {
        finish_reason: String,
        empty_visible_response: bool,
    },
    /// A Chat turn was cancelled by the operator.
    ChatTurnCancelled {
        reason: String,
    },
    /// A Chat turn failed before producing a complete response.
    ChatTurnFailed {
        message: String,
    },
    /// Bounded per-turn context and response telemetry. This payload never
    /// contains hidden reasoning, credentials, or unbounded model text.
    ChatTelemetry {
        telemetry: ChatTurnTelemetry,
    },
    /// Best-effort assistant text delta. The TUI may coalesce or drop these
    /// under backpressure, but must surface truncation when it does.
    StreamDelta {
        text: String,
    },
    /// Emitted by the TUI event sink when stream deltas were coalesced/dropped.
    StreamTruncated {
        dropped: usize,
    },
    /// A worker-side approval gate is waiting for an explicit decision.
    ///
    /// The request contains only bounded, redacted action metadata. It never
    /// contains file contents, patches, environment values, or raw tool JSON.
    ApprovalRequired {
        request: ApprovalRequest,
    },
    /// The worker resolved an approval gate. The ID binds the decision to the
    /// exact displayed action so stale decisions cannot clear a newer prompt.
    ApprovalResolved {
        approval_id: String,
        approved: bool,
    },
    /// A bounded, non-fatal status message from the worker or UI bridge.
    Notice {
        level: EventLevel,
        message: String,
    },
    /// A policy denial distinct from an ordinary tool failure.
    PolicyDenied {
        summary: String,
    },
    /// The worker task failed, panicked, or lost its event channel.
    WorkerError {
        message: String,
    },
    PhaseChanged {
        from: LoopState,
        to: LoopState,
    },
    WikiLoaded {
        summary: String,
    },
    PlanProduced {
        summary: String,
        steps: usize,
    },
    ToolRequested {
        tool_call_id: String,
        kind: String,
    },
    ToolFinished {
        tool_call_id: String,
        is_error: bool,
        summary: String,
    },
    VerificationStarted,
    VerificationFinished {
        passed: bool,
        summary: String,
    },
    ReviewVerdict {
        verdict: String,
    },
    ContextCompacted {
        removed_messages: usize,
    },
    WikiCompiled {
        path: String,
    },
    EvidenceCompiled {
        path: String,
    },
    PlanCompiled {
        path: String,
    },
    Blocked {
        reason: String,
    },
    Completed {
        summary: String,
    },
    Cancelled {
        reason: String,
    },
    Failed {
        reason: String,
    },
}

/// Worker-to-UI events. Display never performs I/O or network access.
#[derive(Debug, Clone)]
pub enum UiEvent {
    Token(String),
    PhaseChanged(LoopState),
    ToolStarted {
        id: String,
        label: String,
    },
    ToolFinished {
        id: String,
        ok: bool,
        preview: String,
    },
    NeedApproval {
        label: String,
    },
    VerificationStarted {
        command: String,
    },
    VerificationFinished {
        passed: bool,
        summary: String,
    },
    WikiCompiled {
        path: String,
    },
    Notice(String),
    Error(String),
    Finished(Result<(), String>),
}

/// UI-to-worker commands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Submit { objective: String, mode: TaskMode },
    Chat { prompt: String },
    NewSession,
    ResumeSession { session_id: Option<String> },
    ResolveApproval { approval_id: String, approved: bool },
    Cancel,
    Quit,
}
