use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crate::events::{AgentEvent, EventLevel};
use crate::state_machine::{LoopState, TaskMode};
use crate::wiki::redact::redact;

/// Maximum number of transcript entries retained by the UI.
pub const MAX_TRANSCRIPT_ENTRIES: usize = 500;
/// Maximum aggregate bytes retained by the transcript.
pub const MAX_TRANSCRIPT_BYTES: usize = 64 * 1024;
/// Maximum bytes for one transcript line or detail summary.
pub const MAX_DISPLAY_TEXT_BYTES: usize = 2 * 1024;
/// Maximum bytes accepted for the task-entry buffer.
pub const MAX_TASK_INPUT_BYTES: usize = 8 * 1024;
/// Grace period before a second Ctrl-C may exit a cancelling task.
pub const CANCEL_GRACE: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InputMode {
    #[default]
    TaskEntry,
    Running,
    Approval,
    Blocked,
    Completed,
    Cancelled,
    Failed,
}

impl InputMode {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Blocked | Self::Completed | Self::Cancelled | Self::Failed
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TaskStatus {
    #[default]
    Idle,
    Running,
    Cancelling,
    Blocked,
    Completed,
    Cancelled,
    Failed,
}

impl TaskStatus {
    pub fn is_active(self) -> bool {
        matches!(self, Self::Running | Self::Cancelling)
    }

    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Blocked | Self::Completed | Self::Cancelled | Self::Failed
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StreamState {
    #[default]
    Idle,
    Streaming,
    Truncated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DetailPane {
    #[default]
    Transcript,
    Detail,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranscriptKind {
    Task,
    Info,
    Assistant,
    Phase,
    Plan,
    Tool,
    ToolError,
    PolicyDenied,
    VerificationPassed,
    VerificationFailed,
    Review,
    Context,
    Wiki,
    Evidence,
    Warning,
    Error,
    Approval,
    Completed,
    Blocked,
    Cancelled,
    Failed,
}

impl TranscriptKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Task => "TASK",
            Self::Info => "INFO",
            Self::Assistant => "ASSIST",
            Self::Phase => "PHASE",
            Self::Plan => "PLAN",
            Self::Tool => "TOOL",
            Self::ToolError => "TOOL!",
            Self::PolicyDenied => "POLICY",
            Self::VerificationPassed => "VERIFY",
            Self::VerificationFailed => "VERIFY!",
            Self::Review => "REVIEW",
            Self::Context => "CONTEXT",
            Self::Wiki => "WIKI",
            Self::Evidence => "EVIDENCE",
            Self::Warning => "WARN",
            Self::Error => "ERROR",
            Self::Approval => "APPROVAL",
            Self::Completed => "DONE",
            Self::Blocked => "BLOCKED",
            Self::Cancelled => "CANCELLED",
            Self::Failed => "FAILED",
        }
    }

    pub fn is_error(self) -> bool {
        matches!(
            self,
            Self::ToolError
                | Self::PolicyDenied
                | Self::VerificationFailed
                | Self::Error
                | Self::Failed
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptEntry {
    pub sequence: u64,
    pub timestamp: String,
    pub kind: TranscriptKind,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalPrompt {
    pub approval_id: String,
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerificationView {
    pub passed: bool,
    pub summary: String,
}

#[derive(Debug, Clone)]
pub struct AppState {
    pub phase: LoopState,
    pub task_status: TaskStatus,
    pub input_mode: InputMode,
    pub selected_mode: TaskMode,
    pub change_available: bool,
    pub task_input: String,
    pub transcript: VecDeque<TranscriptEntry>,
    pub transcript_bytes: usize,
    pub transcript_truncated: bool,
    pub selected_detail: DetailPane,
    pub detail: String,
    pub approval: Option<ApprovalPrompt>,
    pub verification: Option<VerificationView>,
    pub stream_state: StreamState,
    pub stream_truncated: bool,
    pub scroll_offset: usize,
    pub should_quit: bool,
    pub show_help: bool,
    pub worker_closed: bool,
    pub cancel_grace_elapsed: bool,
    pub elapsed: Duration,
    pub spinner_tick: usize,
    pub budget_summary: String,
    pub endpoint_mode: String,
    pub model: String,
    pub tool_count: u32,
    pub completion_summary: Option<String>,
    pub last_error: Option<String>,
    task_started_at: Option<Instant>,
    cancel_requested_at: Option<Instant>,
    next_sequence: u64,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            phase: LoopState::Boot,
            task_status: TaskStatus::Idle,
            input_mode: InputMode::TaskEntry,
            selected_mode: TaskMode::default(),
            change_available: true,
            task_input: String::new(),
            transcript: VecDeque::new(),
            transcript_bytes: 0,
            transcript_truncated: false,
            selected_detail: DetailPane::Transcript,
            detail: String::new(),
            approval: None,
            verification: None,
            stream_state: StreamState::Idle,
            stream_truncated: false,
            scroll_offset: 0,
            should_quit: false,
            show_help: false,
            worker_closed: false,
            cancel_grace_elapsed: false,
            elapsed: Duration::ZERO,
            spinner_tick: 0,
            budget_summary: String::new(),
            endpoint_mode: String::new(),
            model: String::new(),
            tool_count: 0,
            completion_summary: None,
            last_error: None,
            task_started_at: None,
            cancel_requested_at: None,
            next_sequence: 0,
        }
    }
}

impl AppState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Request cancellation of the active task. Returns true when this is the
    /// first request for the current task.
    pub fn request_cancel(&mut self) -> bool {
        if !self.task_status.is_active() || self.task_status == TaskStatus::Cancelling {
            return false;
        }
        self.task_status = TaskStatus::Cancelling;
        self.cancel_requested_at = Some(Instant::now());
        self.cancel_grace_elapsed = false;
        self.push_entry(
            TranscriptKind::Warning,
            "cancellation requested; waiting for the worker to stop",
        );
        true
    }

    /// Advance spinner/elapsed state. This is presentation-only.
    pub fn tick(&mut self) {
        self.spinner_tick = self.spinner_tick.wrapping_add(1);
        if let Some(started) = self.task_started_at
            && self.task_status.is_active()
        {
            self.elapsed = started.elapsed();
        }
        if self.task_status == TaskStatus::Cancelling
            && !self.cancel_grace_elapsed
            && self
                .cancel_requested_at
                .is_some_and(|requested| requested.elapsed() >= CANCEL_GRACE)
        {
            self.cancel_grace_elapsed = true;
            self.push_entry(
                TranscriptKind::Warning,
                "cancellation is still pending; press Ctrl-C again to exit",
            );
        }
    }

    /// Append a bounded notice entry without changing task status.
    pub fn push_notice(&mut self, kind: TranscriptKind, text: &str) {
        self.push_entry(kind, text);
    }

    /// Mark the worker channel as closed. Terminal summaries are preserved;
    /// an unexpectedly closed active task becomes a visible failure.
    pub fn mark_worker_closed(&mut self) {
        if self.worker_closed {
            return;
        }
        self.worker_closed = true;
        if self.task_status.is_terminal() {
            if self.completion_summary.is_none() {
                self.finish_task(
                    TaskStatus::Failed,
                    InputMode::Failed,
                    LoopState::Failed,
                    "worker stopped before a final summary was available",
                    TranscriptKind::Error,
                );
            } else {
                self.push_entry(TranscriptKind::Info, "worker stopped");
            }
        } else {
            self.apply(AgentEvent::WorkerError {
                message: "worker event channel closed unexpectedly".into(),
            });
        }
    }

    /// Apply one bounded orchestration/UI event to the presentation state.
    ///
    /// This method performs no I/O and never changes task authorization. It
    /// redacts and bounds every display string defensively.
    pub fn apply(&mut self, event: AgentEvent) {
        if !matches!(
            &event,
            AgentEvent::StreamDelta { .. } | AgentEvent::StreamTruncated { .. }
        ) && self.stream_state == StreamState::Streaming
        {
            self.stream_state = StreamState::Idle;
        }

        match event {
            AgentEvent::TaskStarted { summary } => {
                self.reset_for_task();
                self.task_status = TaskStatus::Running;
                self.input_mode = InputMode::Running;
                self.phase = LoopState::Boot;
                self.task_started_at = Some(Instant::now());
                self.elapsed = Duration::ZERO;
                self.detail = "Waiting for the orchestrator...".into();
                self.push_entry(TranscriptKind::Task, &summary);
            }
            AgentEvent::StreamDelta { text } => self.push_stream_delta(&text),
            AgentEvent::StreamTruncated { dropped } => {
                self.stream_truncated = true;
                self.stream_state = StreamState::Truncated;
                if !self.transcript.iter().any(|entry| {
                    entry.kind == TranscriptKind::Warning
                        && entry.text.contains("stream output truncated")
                }) {
                    self.push_entry(
                        TranscriptKind::Warning,
                        format!(
                            "stream output truncated after {dropped} dropped delta(s); display is incomplete"
                        ),
                    );
                }
            }
            AgentEvent::ApprovalRequired { request } => {
                let summary = bounded_display_text(&request.summary(), MAX_DISPLAY_TEXT_BYTES);
                let approval_id = bounded_display_text(&request.approval_id, 128);
                self.approval = Some(ApprovalPrompt {
                    approval_id,
                    summary: summary.clone(),
                });
                self.input_mode = InputMode::Approval;
                self.detail = format!("Approval required: {summary}");
                self.push_entry(
                    TranscriptKind::Approval,
                    format!("approval required: {summary}"),
                );
            }
            AgentEvent::ApprovalResolved {
                approval_id,
                approved,
            } => {
                let matches_pending = self
                    .approval
                    .as_ref()
                    .is_some_and(|approval| approval.approval_id == approval_id);
                if !matches_pending {
                    return;
                }
                self.approval = None;
                if self.task_status.is_active() {
                    self.input_mode = InputMode::Running;
                }
                self.detail = if approved {
                    "Approval granted; worker resumed.".into()
                } else {
                    "Approval denied; worker notified.".into()
                };
                self.push_entry(
                    TranscriptKind::Approval,
                    if approved {
                        "approval granted"
                    } else {
                        "approval denied"
                    },
                );
            }
            AgentEvent::Notice { level, message } => {
                let kind = match level {
                    EventLevel::Info => TranscriptKind::Info,
                    EventLevel::Warning => TranscriptKind::Warning,
                    EventLevel::Error => TranscriptKind::Error,
                };
                let message = bounded_display_text(&message, MAX_DISPLAY_TEXT_BYTES);
                if level == EventLevel::Error {
                    self.last_error = Some(message.clone());
                }
                self.detail = message.clone();
                self.push_entry(kind, &message);
            }
            AgentEvent::PolicyDenied { summary } => {
                let summary = bounded_display_text(&summary, MAX_DISPLAY_TEXT_BYTES);
                self.detail = format!("Policy denial: {summary}");
                self.push_entry(
                    TranscriptKind::PolicyDenied,
                    format!("policy denied: {summary}"),
                );
            }
            AgentEvent::WorkerError { message } => {
                let message = bounded_display_text(&message, MAX_DISPLAY_TEXT_BYTES);
                self.worker_closed = true;
                self.last_error = Some(message.clone());
                if self.task_status.is_terminal() && self.completion_summary.is_some() {
                    self.push_entry(
                        TranscriptKind::Warning,
                        format!("worker error after terminal state: {message}"),
                    );
                } else {
                    self.finish_task(
                        TaskStatus::Failed,
                        InputMode::Failed,
                        LoopState::Failed,
                        &message,
                        TranscriptKind::Error,
                    );
                }
            }
            AgentEvent::PhaseChanged { from, to } => {
                self.phase = to;
                let status = status_for_phase(to);
                if to.is_terminal() {
                    self.enter_terminal(status, to, mode_for_status(status));
                } else if to == LoopState::Boot {
                    self.task_status = TaskStatus::Idle;
                    self.input_mode = InputMode::TaskEntry;
                } else {
                    self.task_status = TaskStatus::Running;
                    self.input_mode = InputMode::Running;
                }
                self.detail = format!("Phase: {from:?} -> {to:?}");
                self.push_entry(TranscriptKind::Phase, format!("{from:?} -> {to:?}"));
            }
            AgentEvent::WikiLoaded { summary } => {
                let summary = bounded_display_text(&summary, MAX_DISPLAY_TEXT_BYTES);
                self.detail = format!("Wiki loaded: {summary}");
                self.push_entry(TranscriptKind::Wiki, format!("wiki loaded: {summary}"));
            }
            AgentEvent::PlanProduced { summary, steps } => {
                let summary = bounded_display_text(&summary, MAX_DISPLAY_TEXT_BYTES);
                self.detail = format!("Plan ({steps} step(s)): {summary}");
                self.push_entry(
                    TranscriptKind::Plan,
                    format!("plan ({steps} step(s)): {summary}"),
                );
            }
            AgentEvent::ToolRequested { tool_call_id, kind } => {
                self.tool_count = self.tool_count.saturating_add(1);
                let kind = bounded_display_text(&kind, 128);
                let tool_call_id = bounded_display_text(&tool_call_id, 128);
                self.detail = format!("Tool requested: {kind} ({tool_call_id})");
                self.push_entry(
                    TranscriptKind::Tool,
                    format!("requested {kind} ({tool_call_id})"),
                );
            }
            AgentEvent::ToolFinished {
                tool_call_id,
                is_error,
                summary,
            } => {
                let summary = bounded_display_text(&summary, MAX_DISPLAY_TEXT_BYTES);
                let tool_call_id = bounded_display_text(&tool_call_id, 128);
                self.detail = format!("Tool {tool_call_id}: {summary}");
                self.push_entry(
                    if is_error {
                        TranscriptKind::ToolError
                    } else {
                        TranscriptKind::Tool
                    },
                    format!("{tool_call_id}: {summary}"),
                );
            }
            AgentEvent::VerificationStarted => {
                self.verification = None;
                self.detail = "Verification started.".into();
                self.push_entry(TranscriptKind::Info, "verification started");
            }
            AgentEvent::VerificationFinished { passed, summary } => {
                let summary = bounded_display_text(&summary, MAX_DISPLAY_TEXT_BYTES);
                self.verification = Some(VerificationView {
                    passed,
                    summary: summary.clone(),
                });
                self.detail = format!(
                    "Verification {}: {summary}",
                    if passed { "passed" } else { "failed" }
                );
                self.push_entry(
                    if passed {
                        TranscriptKind::VerificationPassed
                    } else {
                        TranscriptKind::VerificationFailed
                    },
                    format!(
                        "verification {}: {summary}",
                        if passed { "passed" } else { "failed" }
                    ),
                );
            }
            AgentEvent::ReviewVerdict { verdict } => {
                let verdict = bounded_display_text(&verdict, MAX_DISPLAY_TEXT_BYTES);
                self.detail = format!("Review: {verdict}");
                self.push_entry(TranscriptKind::Review, format!("review: {verdict}"));
            }
            AgentEvent::ContextCompacted { removed_messages } => {
                self.detail = format!("Context compacted: removed {removed_messages} message(s).");
                self.push_entry(
                    TranscriptKind::Context,
                    format!("context compacted; removed {removed_messages} message(s)"),
                );
            }
            AgentEvent::WikiCompiled { path } => {
                let path = bounded_display_text(&path, MAX_DISPLAY_TEXT_BYTES);
                self.detail = format!("Wiki compiled: {path}");
                self.push_entry(TranscriptKind::Wiki, format!("wiki compiled: {path}"));
            }
            AgentEvent::EvidenceCompiled { path } => {
                let path = bounded_display_text(&path, MAX_DISPLAY_TEXT_BYTES);
                self.detail = format!("Evidence brief compiled: {path}");
                self.push_entry(
                    TranscriptKind::Evidence,
                    format!("evidence brief compiled: {path}"),
                );
            }
            AgentEvent::Blocked { reason } => {
                self.finish_task(
                    TaskStatus::Blocked,
                    InputMode::Blocked,
                    LoopState::Blocked,
                    &reason,
                    TranscriptKind::Blocked,
                );
            }
            AgentEvent::Completed { summary } => {
                self.finish_task(
                    TaskStatus::Completed,
                    InputMode::Completed,
                    LoopState::Completed,
                    &summary,
                    TranscriptKind::Completed,
                );
            }
            AgentEvent::Cancelled { reason } => {
                self.finish_task(
                    TaskStatus::Cancelled,
                    InputMode::Cancelled,
                    LoopState::Cancelled,
                    &reason,
                    TranscriptKind::Cancelled,
                );
            }
            AgentEvent::Failed { reason } => {
                self.finish_task(
                    TaskStatus::Failed,
                    InputMode::Failed,
                    LoopState::Failed,
                    &reason,
                    TranscriptKind::Failed,
                );
            }
        }
    }

    fn reset_for_task(&mut self) {
        self.transcript.clear();
        self.transcript_bytes = 0;
        self.transcript_truncated = false;
        self.next_sequence = 0;
        self.task_input.clear();
        self.approval = None;
        self.verification = None;
        self.stream_state = StreamState::Idle;
        self.stream_truncated = false;
        self.scroll_offset = 0;
        self.show_help = false;
        self.worker_closed = false;
        self.cancel_grace_elapsed = false;
        self.cancel_requested_at = None;
        self.completion_summary = None;
        self.last_error = None;
        self.tool_count = 0;
    }

    fn enter_terminal(&mut self, status: TaskStatus, phase: LoopState, mode: InputMode) {
        self.task_status = status;
        self.phase = phase;
        self.input_mode = mode;
        self.approval = None;
        self.freeze_elapsed();
    }

    fn finish_task(
        &mut self,
        status: TaskStatus,
        mode: InputMode,
        phase: LoopState,
        summary: &str,
        kind: TranscriptKind,
    ) {
        let summary = bounded_display_text(summary, MAX_DISPLAY_TEXT_BYTES);
        self.enter_terminal(status, phase, mode);
        self.completion_summary = Some(summary.clone());
        self.detail = summary.clone();
        self.push_entry(kind, &summary);
    }

    fn freeze_elapsed(&mut self) {
        if let Some(started) = self.task_started_at.take() {
            self.elapsed = started.elapsed();
        }
    }

    fn push_stream_delta(&mut self, text: &str) {
        let text = bounded_display_text(text, MAX_DISPLAY_TEXT_BYTES);
        if text.is_empty() {
            return;
        }
        if self.stream_state != StreamState::Truncated {
            self.stream_state = StreamState::Streaming;
        }

        if let Some(last) = self.transcript.back_mut()
            && last.kind == TranscriptKind::Assistant
            && last.text.len().saturating_add(text.len()) <= MAX_DISPLAY_TEXT_BYTES
        {
            last.text.push_str(&text);
            self.transcript_bytes = self.transcript_bytes.saturating_add(text.len());
            self.enforce_transcript_bounds();
            return;
        }

        self.push_entry(TranscriptKind::Assistant, &text);
    }

    pub(crate) fn push_entry(&mut self, kind: TranscriptKind, text: impl AsRef<str>) {
        let text = bounded_display_text(text.as_ref(), MAX_DISPLAY_TEXT_BYTES);
        if text.is_empty() {
            return;
        }
        self.next_sequence = self.next_sequence.saturating_add(1);
        self.transcript_bytes = self.transcript_bytes.saturating_add(text.len());
        self.transcript.push_back(TranscriptEntry {
            sequence: self.next_sequence,
            timestamp: chrono::Local::now().format("%H:%M:%S").to_string(),
            kind,
            text,
        });
        self.enforce_transcript_bounds();
    }

    fn enforce_transcript_bounds(&mut self) {
        while self.transcript.len() > MAX_TRANSCRIPT_ENTRIES
            || self.transcript_bytes > MAX_TRANSCRIPT_BYTES
        {
            let Some(removed) = self.transcript.pop_front() else {
                break;
            };
            self.transcript_bytes = self.transcript_bytes.saturating_sub(removed.text.len());
            self.transcript_truncated = true;
        }
    }
}

fn status_for_phase(phase: LoopState) -> TaskStatus {
    match phase {
        LoopState::Boot => TaskStatus::Idle,
        LoopState::Completed => TaskStatus::Completed,
        LoopState::Blocked => TaskStatus::Blocked,
        LoopState::Cancelled => TaskStatus::Cancelled,
        LoopState::Failed => TaskStatus::Failed,
        _ => TaskStatus::Running,
    }
}

fn mode_for_status(status: TaskStatus) -> InputMode {
    match status {
        TaskStatus::Idle => InputMode::TaskEntry,
        TaskStatus::Running | TaskStatus::Cancelling => InputMode::Running,
        TaskStatus::Blocked => InputMode::Blocked,
        TaskStatus::Completed => InputMode::Completed,
        TaskStatus::Cancelled => InputMode::Cancelled,
        TaskStatus::Failed => InputMode::Failed,
    }
}

/// Redact and flatten a display string, then UTF-8-safely bound it.
pub fn bounded_display_text(input: &str, cap: usize) -> String {
    let redacted = redact(input).unwrap_or_else(|_| "[redaction unavailable]".to_owned());
    let single_line = redacted.replace("\r\n", " ").replace(['\r', '\n'], " ");
    truncate_utf8(single_line.trim(), cap, "...[truncated]")
}

fn truncate_utf8(text: &str, cap: usize, marker: &str) -> String {
    if text.len() <= cap {
        return text.to_owned();
    }
    let mut end = cap.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{marker}", &text[..end])
}
