use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crate::context::{GlobalContext, ProjectContext};
use crate::events::{AgentEvent, ChatTurnTelemetry, EventLevel};
use crate::session::{SessionMessage, SessionRole};
use crate::state_machine::{LoopState, TaskMode};
use crate::wiki::redact::redact;

/// Maximum number of transcript entries retained by the UI.
pub const MAX_TRANSCRIPT_ENTRIES: usize = 500;
/// Maximum aggregate bytes retained by the transcript.
pub const MAX_TRANSCRIPT_BYTES: usize = 64 * 1024;
/// Maximum bytes for one transcript entry or detail summary.
pub const MAX_DISPLAY_TEXT_BYTES: usize = 2 * 1024;
/// Maximum bytes accepted for the task-entry buffer.
pub const MAX_TASK_INPUT_BYTES: usize = 8 * 1024;
/// Grace period before a second Ctrl-C may exit a cancelling task.
pub const CANCEL_GRACE: Duration = Duration::from_secs(2);

pub(crate) fn context_summary(
    utilization_percent: u32,
    effective_context_cap_tokens: u32,
) -> String {
    format!(
        "{}% / {}",
        utilization_percent.min(100),
        format_context_cap(effective_context_cap_tokens)
    )
}

pub(crate) fn format_context_cap(tokens: u32) -> String {
    if tokens >= 1_000 {
        format!("{}k", (u64::from(tokens) + 500) / 1_000)
    } else {
        tokens.to_string()
    }
}

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

/// Main workspace tabs in the static shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MainTab {
    #[default]
    Chat,
    Context,
    Diff,
    Output,
    Session,
}

impl MainTab {
    pub const ALL: [Self; 5] = [
        Self::Chat,
        Self::Context,
        Self::Diff,
        Self::Output,
        Self::Session,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Chat => "Chat",
            Self::Context => "Context",
            Self::Diff => "Diff",
            Self::Output => "Output",
            Self::Session => "Session",
        }
    }

    pub fn index(self) -> usize {
        match self {
            Self::Chat => 0,
            Self::Context => 1,
            Self::Diff => 2,
            Self::Output => 3,
            Self::Session => 4,
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::Chat => Self::Context,
            Self::Context => Self::Diff,
            Self::Diff => Self::Output,
            Self::Output => Self::Session,
            Self::Session => Self::Chat,
        }
    }

    pub fn previous(self) -> Self {
        match self {
            Self::Chat => Self::Session,
            Self::Context => Self::Chat,
            Self::Diff => Self::Context,
            Self::Output => Self::Diff,
            Self::Session => Self::Output,
        }
    }
}

/// Keyboard focus for the static shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FocusPane {
    Tabs,
    Content,
    Explorer,
    #[default]
    Prompt,
}

impl FocusPane {
    pub fn label(self) -> &'static str {
        match self {
            Self::Tabs => "tabs",
            Self::Content => "content",
            Self::Explorer => "explorer",
            Self::Prompt => "prompt",
        }
    }

    pub fn next(self) -> Self {
        match self {
            Self::Prompt => Self::Tabs,
            Self::Tabs => Self::Content,
            Self::Content => Self::Explorer,
            Self::Explorer => Self::Prompt,
        }
    }

    pub fn previous(self) -> Self {
        match self {
            Self::Prompt => Self::Explorer,
            Self::Tabs => Self::Prompt,
            Self::Content => Self::Tabs,
            Self::Explorer => Self::Content,
        }
    }
}

/// Typed presentation-only commands used by the shell and tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiCommand {
    FocusNext,
    FocusPrevious,
    NextTab,
    PreviousTab,
    SelectTab(MainTab),
}

/// Static, I/O-free content used by the initial shell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellFixtures {
    pub chat: Vec<String>,
    pub context: Vec<String>,
    pub diff: Vec<String>,
    pub output: Vec<String>,
    pub session: Vec<String>,
    pub explorer: Vec<String>,
}

impl ShellFixtures {
    pub fn lines_for(&self, tab: MainTab) -> &[String] {
        match tab {
            MainTab::Chat => &self.chat,
            MainTab::Context => &self.context,
            MainTab::Diff => &self.diff,
            MainTab::Output => &self.output,
            MainTab::Session => &self.session,
        }
    }
}

impl Default for ShellFixtures {
    fn default() -> Self {
        Self {
            chat: vec![
                "Chat transcript (fixture)".into(),
                "Operator: inspect the request pipeline".into(),
                "Assistant: fixture response; live streaming is not connected".into(),
                "Status: idle".into(),
            ],
            context: vec![
                "Global context (fixture)".into(),
                "AGENTS.md: not loaded".into(),
                "Project context: not loaded".into(),
                "Token estimate: 0 / 12,288".into(),
            ],
            diff: vec![
                "Session write log (fixture)".into(),
                "No session changes yet.".into(),
                "Git diff: not loaded".into(),
            ],
            output: vec![
                "Diagnostics (fixture)".into(),
                "No tool output yet.".into(),
                "Verifier output: not loaded".into(),
            ],
            session: vec![
                "Session metadata (fixture)".into(),
                "Persistence: enabled (append-only JSONL)".into(),
                "New session: /new".into(),
                "Resume: /resume <session-id>".into(),
            ],
            explorer: vec![
                "Read-only placeholder".into(),
                "Project tree".into(),
                "  src/".into(),
                "  tasks/".into(),
                "Permitted paths".into(),
                "  src/**".into(),
                "Changed paths".into(),
                "  (none)".into(),
                "Wiki artifacts".into(),
                "  not loaded".into(),
            ],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranscriptKind {
    Task,
    Operator,
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
    EmptyResponse,
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
            Self::Operator => "YOU",
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
            Self::EmptyResponse => "EMPTY",
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

/// Named, deterministic transcript scenarios used by renderer fixtures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranscriptFixtureCase {
    NormalText,
    MultilineMarkdown,
    CodeBlock,
    Unicode,
    BlankLines,
    LongUnbrokenLine,
    StreamingChunks,
    ErrorStatus,
    EmptyResponse,
    TruncationIndicator,
}

impl TranscriptFixtureCase {
    pub const ALL: [Self; 10] = [
        Self::NormalText,
        Self::MultilineMarkdown,
        Self::CodeBlock,
        Self::Unicode,
        Self::BlankLines,
        Self::LongUnbrokenLine,
        Self::StreamingChunks,
        Self::ErrorStatus,
        Self::EmptyResponse,
        Self::TruncationIndicator,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::NormalText => "normal text",
            Self::MultilineMarkdown => "multiline markdown",
            Self::CodeBlock => "code block",
            Self::Unicode => "unicode",
            Self::BlankLines => "blank lines",
            Self::LongUnbrokenLine => "long unbroken line",
            Self::StreamingChunks => "streaming chunks",
            Self::ErrorStatus => "error/status message",
            Self::EmptyResponse => "empty response",
            Self::TruncationIndicator => "truncation indicator",
        }
    }

    pub fn fixture(self) -> TranscriptFixture {
        match self {
            Self::NormalText => TranscriptFixture::new([
                (TranscriptKind::Info, "status: idle"),
                (TranscriptKind::Assistant, "Normal assistant text."),
            ]),
            Self::MultilineMarkdown => TranscriptFixture::new([(
                TranscriptKind::Assistant,
                "## Summary\n\n- first item\n- second item\n\n**Done.**",
            )]),
            Self::CodeBlock => TranscriptFixture::new([(
                TranscriptKind::Assistant,
                "Patch:\n\n```rust\nfn main() {\n    println!(\"hello\");\n}\n```",
            )]),
            Self::Unicode => TranscriptFixture::new([(
                TranscriptKind::Assistant,
                "Unicode: café, naïve, 東京, résumé, 🚀",
            )]),
            Self::BlankLines => {
                TranscriptFixture::new([(TranscriptKind::Assistant, "Before.\n\nAfter.")])
            }
            Self::LongUnbrokenLine => TranscriptFixture::new(vec![
                (
                    TranscriptKind::Assistant,
                    format!("start-{}-end", "x".repeat(400)),
                ),
                (TranscriptKind::Info, "after long line".to_owned()),
            ]),
            Self::StreamingChunks => {
                TranscriptFixture::new([(TranscriptKind::Assistant, "Hello world\nsecond line")])
                    .with_stream_state(StreamState::Streaming)
            }
            Self::ErrorStatus => TranscriptFixture::new([
                (TranscriptKind::Info, "status: connected"),
                (
                    TranscriptKind::Error,
                    "endpoint unavailable: connection refused",
                ),
            ]),
            Self::EmptyResponse => TranscriptFixture::new([(
                TranscriptKind::EmptyResponse,
                "visible output = 0; no assistant message was stored",
            )]),
            Self::TruncationIndicator => TranscriptFixture::new([(
                TranscriptKind::Assistant,
                "visible text before truncation",
            )])
            .with_truncation(true, true),
        }
    }
}

/// Deterministic transcript state used by fixture-driven renderer tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptFixture {
    pub entries: Vec<TranscriptEntry>,
    pub transcript_truncated: bool,
    pub stream_truncated: bool,
    pub stream_state: StreamState,
}

impl TranscriptFixture {
    pub fn new<I, S>(entries: I) -> Self
    where
        I: IntoIterator<Item = (TranscriptKind, S)>,
        S: Into<String>,
    {
        let entries = entries
            .into_iter()
            .enumerate()
            .map(|(index, (kind, text))| TranscriptEntry {
                sequence: index as u64 + 1,
                timestamp: format!("12:00:{:02}", index % 60),
                kind,
                text: bounded_multiline_display_text(&text.into(), MAX_DISPLAY_TEXT_BYTES),
            })
            .collect();
        Self {
            entries,
            transcript_truncated: false,
            stream_truncated: false,
            stream_state: StreamState::Idle,
        }
    }

    pub fn case(case: TranscriptFixtureCase) -> Self {
        case.fixture()
    }

    pub fn with_truncation(mut self, transcript: bool, stream: bool) -> Self {
        self.transcript_truncated = transcript;
        self.stream_truncated = stream;
        self
    }

    pub fn with_stream_state(mut self, stream_state: StreamState) -> Self {
        self.stream_state = stream_state;
        self
    }
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
    pub chat_active: bool,
    pub input_mode: InputMode,
    pub selected_mode: TaskMode,
    pub change_available: bool,
    pub task_input: String,
    pub transcript: VecDeque<TranscriptEntry>,
    pub transcript_bytes: usize,
    pub transcript_truncated: bool,
    pub selected_detail: DetailPane,
    pub active_tab: MainTab,
    pub focus: FocusPane,
    pub explorer_selected: usize,
    pub content_scroll: usize,
    pub fixtures: ShellFixtures,
    pub global_context: GlobalContext,
    pub project_context: ProjectContext,
    pub project: String,
    pub context_summary: String,
    pub last_chat_telemetry: Option<ChatTurnTelemetry>,
    pub session: String,
    pub tool_indicator: String,
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
    chat_visible_received: bool,
    next_sequence: u64,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            phase: LoopState::Boot,
            task_status: TaskStatus::Idle,
            chat_active: false,
            input_mode: InputMode::TaskEntry,
            selected_mode: TaskMode::default(),
            change_available: true,
            task_input: String::new(),
            transcript: VecDeque::new(),
            transcript_bytes: 0,
            transcript_truncated: false,
            selected_detail: DetailPane::Transcript,
            active_tab: MainTab::Chat,
            focus: FocusPane::Prompt,
            explorer_selected: 0,
            content_scroll: 0,
            fixtures: ShellFixtures::default(),
            global_context: GlobalContext::empty(""),
            project_context: ProjectContext::empty(""),
            project: "workspace".into(),
            context_summary: "0% / 16k".into(),
            last_chat_telemetry: None,
            session: "preview".into(),
            tool_indicator: "read-only".into(),
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
            chat_visible_received: false,
            next_sequence: 0,
        }
    }
}

impl AppState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn active_tab_lines(&self) -> &[String] {
        self.fixtures.lines_for(self.active_tab)
    }

    /// Install the startup snapshot of global trusted context.
    ///
    /// This is not reachable from model output, tool results, or repository
    /// content. Context-tab provenance is refreshed with explicit trust labels.
    pub fn set_global_context(&mut self, global_context: GlobalContext) {
        self.global_context = global_context;
        self.refresh_context_fixture();
    }

    /// Install the startup snapshot of repository-owned untrusted context.
    ///
    /// Repository content cannot modify this snapshot after startup, and the
    /// Context tab labels every source as `[untrusted]`.
    pub fn set_project_context(&mut self, project_context: ProjectContext) {
        self.project_context = project_context;
        self.refresh_context_fixture();
    }

    fn refresh_context_fixture(&mut self) {
        let mut lines = self.global_context.display_lines();
        lines.push(String::new());
        lines.extend(self.project_context.display_lines());
        self.fixtures.context = lines;
    }

    /// Replace presentation-only transcript state with a deterministic fixture.
    pub fn load_transcript_fixture(&mut self, fixture: &TranscriptFixture) {
        self.transcript.clear();
        self.transcript_bytes = 0;
        self.next_sequence = 0;
        for entry in &fixture.entries {
            self.transcript_bytes = self.transcript_bytes.saturating_add(entry.text.len());
            self.next_sequence = self.next_sequence.max(entry.sequence);
            self.transcript.push_back(TranscriptEntry {
                sequence: entry.sequence,
                timestamp: entry.timestamp.clone(),
                kind: entry.kind,
                text: entry.text.clone(),
            });
        }
        self.transcript_truncated = false;
        self.enforce_transcript_bounds();
        self.transcript_truncated |= fixture.transcript_truncated;
        self.stream_truncated = fixture.stream_truncated;
        self.stream_state = fixture.stream_state;
        self.scroll_offset = 0;
        self.content_scroll = 0;
        self.active_tab = MainTab::Chat;
    }

    pub fn select_tab(&mut self, tab: MainTab) {
        if self.active_tab != tab {
            self.active_tab = tab;
            self.content_scroll = 0;
        }
    }

    pub fn next_tab(&mut self) {
        self.select_tab(self.active_tab.next());
    }

    pub fn previous_tab(&mut self) {
        self.select_tab(self.active_tab.previous());
    }

    pub fn cycle_focus(&mut self) {
        self.focus = self.focus.next();
    }

    pub fn cycle_focus_previous(&mut self) {
        self.focus = self.focus.previous();
    }

    pub fn move_explorer_selection(&mut self, delta: isize) {
        let max = self.fixtures.explorer.len().saturating_sub(1);
        self.explorer_selected = if delta.is_negative() {
            self.explorer_selected.saturating_sub(delta.unsigned_abs())
        } else {
            self.explorer_selected
                .saturating_add(delta as usize)
                .min(max)
        };
    }

    pub fn scroll_content(&mut self, delta: isize) {
        if self.active_tab == MainTab::Chat {
            let max = self.transcript.len().max(self.active_tab_lines().len());
            self.scroll_offset = adjusted_scroll(self.scroll_offset, delta, max);
        } else {
            let max = self.active_tab_lines().len();
            self.content_scroll = adjusted_scroll(self.content_scroll, delta, max);
        }
    }

    /// Apply a typed presentation command. No command performs I/O.
    pub fn apply_ui_command(&mut self, command: UiCommand) {
        match command {
            UiCommand::FocusNext => self.cycle_focus(),
            UiCommand::FocusPrevious => self.cycle_focus_previous(),
            UiCommand::NextTab => {
                self.focus = FocusPane::Tabs;
                self.next_tab();
            }
            UiCommand::PreviousTab => {
                self.focus = FocusPane::Tabs;
                self.previous_tab();
            }
            UiCommand::SelectTab(tab) => {
                self.focus = FocusPane::Tabs;
                self.select_tab(tab);
            }
        }
    }

    /// Request cancellation of the active task or Chat turn. Returns true when
    /// this is the first request for the current run.
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
            AgentEvent::SessionLoaded {
                session_id,
                messages,
            } => self.restore_session(session_id, messages),
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
            AgentEvent::ChatTurnStarted { prompt } => {
                let prompt = bounded_multiline_display_text(&prompt, MAX_DISPLAY_TEXT_BYTES);
                self.chat_active = true;
                self.task_status = TaskStatus::Running;
                self.input_mode = InputMode::Running;
                self.phase = LoopState::Boot;
                self.task_started_at = Some(Instant::now());
                self.elapsed = Duration::ZERO;
                self.approval = None;
                self.verification = None;
                self.stream_state = StreamState::Idle;
                self.stream_truncated = false;
                self.chat_visible_received = false;
                self.last_chat_telemetry = None;
                self.cancel_requested_at = None;
                self.cancel_grace_elapsed = false;
                self.completion_summary = None;
                self.last_error = None;
                self.show_help = false;
                self.scroll_offset = 0;
                self.detail = "Waiting for the local model...".into();
                self.push_entry(TranscriptKind::Operator, prompt);
            }
            AgentEvent::ChatTelemetry { telemetry } => {
                self.apply_chat_telemetry(telemetry);
            }
            AgentEvent::ChatTurnFinished {
                finish_reason,
                empty_visible_response,
            } => {
                let finish_reason = bounded_display_text(&finish_reason, 128);
                let empty = empty_visible_response || !self.chat_visible_received;
                if empty {
                    self.remove_empty_chat_assistant();
                    self.detail = "Chat completed without a visible response.".into();
                    self.push_entry(
                        TranscriptKind::EmptyResponse,
                        "visible output = 0; no assistant message was stored",
                    );
                } else {
                    self.detail = format!("Chat response complete (finish: {finish_reason}).");
                }
                self.finish_chat_run();
            }
            AgentEvent::ChatTurnCancelled { reason } => {
                let reason = bounded_display_text(&reason, MAX_DISPLAY_TEXT_BYTES);
                self.detail = format!("Chat turn cancelled: {reason}");
                self.push_entry(
                    TranscriptKind::Cancelled,
                    format!("chat turn cancelled: {reason}"),
                );
                self.finish_chat_run();
            }
            AgentEvent::ChatTurnFailed { message } => {
                let message = bounded_display_text(&message, MAX_DISPLAY_TEXT_BYTES);
                self.last_error = Some(message.clone());
                self.detail = format!("Chat turn failed: {message}");
                self.push_entry(
                    TranscriptKind::Error,
                    format!("chat turn failed: {message}"),
                );
                self.finish_chat_run();
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
                self.focus = FocusPane::Prompt;
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
            AgentEvent::PlanCompiled { path } => {
                let path = bounded_display_text(&path, MAX_DISPLAY_TEXT_BYTES);
                self.detail = format!("Plan artifact compiled: {path}");
                self.push_entry(
                    TranscriptKind::Plan,
                    format!("plan artifact compiled: {path}"),
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

    fn restore_session(&mut self, session_id: String, messages: Vec<SessionMessage>) {
        self.session = bounded_display_text(&session_id, 64);
        self.transcript.clear();
        self.transcript_bytes = 0;
        self.transcript_truncated = false;
        self.next_sequence = 0;
        self.task_input.clear();
        self.chat_active = false;
        self.chat_visible_received = false;
        self.task_status = TaskStatus::Idle;
        self.input_mode = InputMode::TaskEntry;
        self.phase = LoopState::Boot;
        self.approval = None;
        self.verification = None;
        self.stream_state = StreamState::Idle;
        self.stream_truncated = false;
        self.scroll_offset = 0;
        self.content_scroll = 0;
        self.task_started_at = None;
        self.cancel_requested_at = None;
        self.cancel_grace_elapsed = false;
        self.elapsed = Duration::ZERO;
        self.completion_summary = None;
        self.last_error = None;
        self.last_chat_telemetry = None;
        self.detail = format!(
            "Session {} restored ({} visible message(s)).",
            self.session,
            messages.len()
        );

        for message in messages {
            let kind = match message.role {
                SessionRole::User => TranscriptKind::Operator,
                SessionRole::Assistant => TranscriptKind::Assistant,
            };
            let text = bounded_multiline_display_text(&message.text, MAX_DISPLAY_TEXT_BYTES);
            self.push_entry_raw(kind, text);
        }
    }

    fn apply_chat_telemetry(&mut self, mut telemetry: ChatTurnTelemetry) {
        telemetry.finish_reason = telemetry
            .finish_reason
            .map(|reason| bounded_display_text(&reason, 128));
        telemetry.transport_error_category = telemetry
            .transport_error_category
            .map(|category| bounded_display_text(&category, 32));
        self.context_summary = context_summary(
            telemetry.context_utilization_percent,
            telemetry.effective_context_cap_tokens,
        );
        self.last_chat_telemetry = Some(telemetry);
    }

    fn reset_for_task(&mut self) {
        self.chat_active = false;
        self.chat_visible_received = false;
        self.last_chat_telemetry = None;
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
        self.chat_active = false;
        self.chat_visible_received = false;
        let summary = bounded_display_text(summary, MAX_DISPLAY_TEXT_BYTES);
        self.enter_terminal(status, phase, mode);
        self.completion_summary = Some(summary.clone());
        self.detail = summary.clone();
        self.push_entry(kind, &summary);
    }

    fn finish_chat_run(&mut self) {
        self.chat_active = false;
        self.chat_visible_received = false;
        self.task_status = TaskStatus::Idle;
        self.input_mode = InputMode::TaskEntry;
        self.phase = LoopState::Boot;
        self.approval = None;
        self.verification = None;
        self.stream_state = StreamState::Idle;
        self.cancel_requested_at = None;
        self.cancel_grace_elapsed = false;
        self.freeze_elapsed();
    }

    fn remove_empty_chat_assistant(&mut self) {
        while self.transcript.back().is_some_and(|entry| {
            entry.kind == TranscriptKind::Assistant && entry.text.trim().is_empty()
        }) {
            if let Some(removed) = self.transcript.pop_back() {
                self.transcript_bytes = self.transcript_bytes.saturating_sub(removed.text.len());
            }
        }
    }

    fn freeze_elapsed(&mut self) {
        if let Some(started) = self.task_started_at.take() {
            self.elapsed = started.elapsed();
        }
    }

    fn push_stream_delta(&mut self, text: &str) {
        let text = bounded_stream_text(text, MAX_DISPLAY_TEXT_BYTES);
        if text.is_empty() {
            return;
        }
        let starts_entry = self
            .transcript
            .back()
            .is_none_or(|entry| entry.kind != TranscriptKind::Assistant);
        if starts_entry && text.trim().is_empty() {
            return;
        }
        if self.stream_state != StreamState::Truncated {
            self.stream_state = StreamState::Streaming;
        }
        if self.chat_active && text.chars().any(|ch| !ch.is_whitespace()) {
            self.chat_visible_received = true;
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

        self.push_entry_raw(TranscriptKind::Assistant, text);
    }

    pub(crate) fn push_entry(&mut self, kind: TranscriptKind, text: impl AsRef<str>) {
        let text = bounded_multiline_display_text(text.as_ref(), MAX_DISPLAY_TEXT_BYTES);
        self.push_entry_raw(kind, text);
    }

    fn push_entry_raw(&mut self, kind: TranscriptKind, text: String) {
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

fn adjusted_scroll(current: usize, delta: isize, max: usize) -> usize {
    if delta.is_negative() {
        current.saturating_sub(delta.unsigned_abs())
    } else {
        current.saturating_add(delta as usize).min(max)
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

/// Redact and normalize a display string while preserving line breaks.
pub fn bounded_multiline_display_text(input: &str, cap: usize) -> String {
    let redacted = redact(input).unwrap_or_else(|_| "[redaction unavailable]".to_owned());
    let normalized = redacted.replace("\r\n", "\n").replace('\r', "\n");
    truncate_utf8(normalized.trim_end(), cap, "\n...[truncated]")
}

fn bounded_stream_text(input: &str, cap: usize) -> String {
    let redacted = redact(input).unwrap_or_else(|_| "[redaction unavailable]".to_owned());
    let normalized = redacted.replace("\r\n", "\n").replace('\r', "\n");
    truncate_utf8(&normalized, cap, "\n...[truncated]")
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
