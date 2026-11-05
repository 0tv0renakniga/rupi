use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TrySendError;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::approval::{ApprovalResolveError, InteractiveApprovalGate};
use crate::events::{AgentEvent, ChatTurnTelemetry, Command, EventLevel};
use crate::llm::{
    ChatMessage, ChatRequest, ContextPlan, VisibleChatBackend, VisibleChatEvent,
    estimate_text_tokens,
};
use crate::orchestrator::{EventSink, Orchestrator, TaskRequest};
use crate::session::{SessionMessage, SessionRole, SessionStore};
use crate::state_machine::{TaskBudget, TaskMode};

/// Bounded command queue. The UI uses `try_send` so a stalled worker can never
/// block rendering or terminal restoration indefinitely.
pub const COMMAND_CHANNEL_CAPACITY: usize = 16;
/// Bounded event queue. Critical transitions are retried by a dedicated task;
/// best-effort stream deltas are dropped and explicitly marked truncated.
pub const EVENT_CHANNEL_CAPACITY: usize = 256;
const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);
const CHAT_SYSTEM_PROMPT: &str = "You are the local coding assistant in a terminal harness. \
Reply with concise visible text only. Do not reveal hidden reasoning. Do not claim to run \
tools or modify files in this text-only chat.";
const MAX_CHAT_HISTORY_MESSAGES: usize = 120;
const MAX_CHAT_HISTORY_BYTES: usize = 256 * 1024;

/// Owns the worker task and the two bounded channel endpoints used by the TUI.
pub struct Worker {
    pub commands: mpsc::Sender<Command>,
    pub events: mpsc::Receiver<AgentEvent>,
    pub cancel: CancellationToken,
    join: JoinHandle<()>,
}

impl Worker {
    /// Cancel the active task and wait briefly for the worker to stop.
    pub async fn shutdown(self) {
        let Worker {
            commands,
            events,
            cancel,
            join,
        } = self;
        // Dropping the receiver first guarantees that a worker blocked on a
        // full event queue observes the close instead of waiting out the grace
        // period.
        drop(events);
        cancel.cancel();
        let _ = commands.try_send(Command::Quit);
        drop(commands);

        let mut join = join;
        tokio::select! {
            _ = &mut join => {}
            _ = tokio::time::sleep(SHUTDOWN_GRACE) => {
                join.abort();
                let _ = join.await;
            }
        }
    }
}

/// Start the single-task worker. The orchestrator owns policy, verification,
/// and terminal state; this bridge only carries bounded commands/events.
pub fn spawn(
    orchestrator: Orchestrator,
    repo_root: PathBuf,
    budget: TaskBudget,
    approvals: InteractiveApprovalGate,
    change_available: bool,
) -> Worker {
    spawn_with_chat(
        orchestrator,
        repo_root,
        budget,
        approvals,
        change_available,
        None,
    )
}

/// Start the worker with an optional visible Chat backend. Task execution and
/// reusable Chat turns share one command/event queue and remain mutually
/// exclusive.
pub fn spawn_with_chat(
    orchestrator: Orchestrator,
    repo_root: PathBuf,
    budget: TaskBudget,
    approvals: InteractiveApprovalGate,
    change_available: bool,
    chat: Option<Arc<dyn VisibleChatBackend>>,
) -> Worker {
    spawn_with_chat_and_session(
        orchestrator,
        repo_root,
        budget,
        approvals,
        change_available,
        chat,
        None,
    )
}

/// Start the worker with an optional visible Chat backend and session log.
///
/// The caller normally opens the session before terminal setup so recovery
/// failures remain printable on a normal terminal. Session commands such as
/// `/new` and `/resume` replace the active log at runtime.
pub fn spawn_with_chat_and_session(
    orchestrator: Orchestrator,
    repo_root: PathBuf,
    budget: TaskBudget,
    approvals: InteractiveApprovalGate,
    change_available: bool,
    chat: Option<Arc<dyn VisibleChatBackend>>,
    session: Option<SessionStore>,
) -> Worker {
    let (commands, command_rx) = mpsc::channel(COMMAND_CHANNEL_CAPACITY);
    let (event_tx, events) = mpsc::channel(EVENT_CHANNEL_CAPACITY);
    let cancel = CancellationToken::new();
    let sink = TuiEventSink::new(event_tx);

    let orchestrator = Arc::new(orchestrator.with_event_sink(Arc::new(sink.clone())));
    let worker_cancel = cancel.clone();
    let settings = WorkerSettings {
        repo_root,
        budget,
        change_available,
        chat,
        session,
    };
    let join = tokio::spawn(async move {
        run_worker(
            orchestrator,
            settings,
            sink,
            approvals,
            command_rx,
            worker_cancel,
        )
        .await;
    });

    Worker {
        commands,
        events,
        cancel,
        join,
    }
}

struct WorkerSettings {
    repo_root: PathBuf,
    budget: TaskBudget,
    change_available: bool,
    chat: Option<Arc<dyn VisibleChatBackend>>,
    session: Option<SessionStore>,
}

async fn run_worker(
    orchestrator: Arc<Orchestrator>,
    settings: WorkerSettings,
    sink: TuiEventSink,
    approvals: InteractiveApprovalGate,
    mut commands: mpsc::Receiver<Command>,
    cancel: CancellationToken,
) {
    let WorkerSettings {
        repo_root,
        budget,
        change_available,
        chat,
        mut session,
    } = settings;
    let mut chat_history = match session.as_ref() {
        Some(store) => chat_history_from_session(store.messages()),
        None => vec![ChatMessage::system(CHAT_SYSTEM_PROMPT)],
    };

    loop {
        let command = match commands.recv().await {
            Some(command) => command,
            None => {
                cancel.cancel();
                break;
            }
        };

        match command {
            Command::Submit { objective, mode } => {
                if cancel.is_cancelled() {
                    break;
                }
                if mode == TaskMode::Change && !change_available {
                    let _ = sink
                        .send_critical(AgentEvent::Failed {
                            reason: "Change mode is gated until the reviewed verifier script is installed"
                                .into(),
                        })
                        .await;
                    continue;
                }

                let task_cancel = cancel.child_token();
                if !sink
                    .send_critical(AgentEvent::TaskStarted {
                        summary: objective.clone(),
                    })
                    .await
                {
                    cancel.cancel();
                    break;
                }

                let request = TaskRequest::new(objective, repo_root.clone())
                    .with_mode(mode)
                    .with_budget(budget.clone());
                let run = orchestrator.run(request, task_cancel.clone());
                tokio::pin!(run);

                let outcome = loop {
                    tokio::select! {
                        biased;
                        command = commands.recv() => match command {
                            Some(Command::Cancel) => task_cancel.cancel(),
                            Some(Command::Quit) => {
                                task_cancel.cancel();
                                cancel.cancel();
                                return;
                            }
                            Some(Command::ResolveApproval {
                                approval_id,
                                approved,
                            }) => {
                                notify_approval_resolution(
                                    &sink,
                                    approvals.resolve(&approval_id, approved),
                                );
                            }
                            Some(Command::Submit { .. }) => {
                                sink.notice(
                                    EventLevel::Warning,
                                    "a task is already running; wait for it or cancel it",
                                );
                            }
                            Some(Command::Chat { .. }) => {
                                sink.notice(
                                    EventLevel::Warning,
                                    "a task is already running; finish or cancel it before chatting",
                                );
                            }
                            Some(Command::NewSession)
                            | Some(Command::ResumeSession { .. }) => {
                                sink.notice(
                                    EventLevel::Warning,
                                    "a task is already running; finish or cancel it before changing sessions",
                                );
                            }
                            None => {
                                task_cancel.cancel();
                                cancel.cancel();
                                return;
                            }
                        },
                        result = &mut run => break result,
                    }
                };

                if let Err(error) = outcome {
                    let _ = sink
                        .send_critical(AgentEvent::WorkerError {
                            message: error.to_string(),
                        })
                        .await;
                }

                if sink.is_closed() || cancel.is_cancelled() {
                    cancel.cancel();
                    break;
                }
            }
            Command::Chat { prompt } => {
                if cancel.is_cancelled() {
                    break;
                }
                let Some(backend) = chat.as_ref().cloned() else {
                    let _ = sink
                        .send_critical(AgentEvent::ChatTurnFailed {
                            message: "chat backend is not configured".into(),
                        })
                        .await;
                    continue;
                };
                if !run_chat_turn_persisted(
                    backend,
                    &sink,
                    &mut chat_history,
                    session.as_mut(),
                    prompt,
                    &mut commands,
                    &cancel,
                )
                .await
                {
                    break;
                }
            }
            Command::NewSession => {
                let Some(current) = session.as_ref() else {
                    sink.notice(EventLevel::Warning, "session persistence is not configured");
                    continue;
                };
                let config = current.config().clone();
                match SessionStore::create_new(&config) {
                    Ok(mut new_session) => {
                        if let Err(error) = new_session.append_command("new") {
                            sink.notice(
                                EventLevel::Error,
                                format!("session command write failed: {error}"),
                            );
                        }
                        chat_history = chat_history_from_session(new_session.messages());
                        let session_id = new_session.id().to_owned();
                        let messages = new_session.messages().to_vec();
                        session = Some(new_session);
                        if !sink
                            .send_critical(AgentEvent::SessionLoaded {
                                session_id,
                                messages,
                            })
                            .await
                        {
                            cancel.cancel();
                            break;
                        }
                    }
                    Err(error) => sink.notice(
                        EventLevel::Error,
                        format!("could not create a new session: {error}"),
                    ),
                }
            }
            Command::ResumeSession { session_id } => {
                let Some(current) = session.as_ref() else {
                    sink.notice(EventLevel::Warning, "session persistence is not configured");
                    continue;
                };
                let config = current.config().clone();
                let loaded = match session_id.as_deref() {
                    Some(id) => SessionStore::resume(&config, id).map(Some),
                    None => SessionStore::open_latest(&config),
                };
                match loaded {
                    Ok(Some(mut restored)) => {
                        if let Err(error) = restored.append_command("resume") {
                            sink.notice(
                                EventLevel::Error,
                                format!("session command write failed: {error}"),
                            );
                        }
                        chat_history = chat_history_from_session(restored.messages());
                        let session_id = restored.id().to_owned();
                        let messages = restored.messages().to_vec();
                        session = Some(restored);
                        if !sink
                            .send_critical(AgentEvent::SessionLoaded {
                                session_id,
                                messages,
                            })
                            .await
                        {
                            cancel.cancel();
                            break;
                        }
                    }
                    Ok(None) => sink.notice(EventLevel::Warning, "no saved session to resume"),
                    Err(error) => sink.notice(
                        EventLevel::Error,
                        format!("could not resume session: {error}"),
                    ),
                }
            }
            Command::Cancel => {
                sink.notice(EventLevel::Info, "no active task or chat turn to cancel");
            }
            Command::Quit => {
                cancel.cancel();
                break;
            }
            Command::ResolveApproval {
                approval_id,
                approved,
            } => {
                notify_approval_resolution(&sink, approvals.resolve(&approval_id, approved));
            }
        }
    }
}

#[cfg(test)]
async fn run_chat_turn(
    backend: Arc<dyn VisibleChatBackend>,
    sink: &TuiEventSink,
    history: &mut Vec<ChatMessage>,
    prompt: String,
    commands: &mut mpsc::Receiver<Command>,
    cancel: &CancellationToken,
) -> bool {
    run_chat_turn_persisted(backend, sink, history, None, prompt, commands, cancel).await
}

async fn run_chat_turn_persisted(
    backend: Arc<dyn VisibleChatBackend>,
    sink: &TuiEventSink,
    history: &mut Vec<ChatMessage>,
    mut session: Option<&mut SessionStore>,
    prompt: String,
    commands: &mut mpsc::Receiver<Command>,
    cancel: &CancellationToken,
) -> bool {
    if !sink
        .send_critical(AgentEvent::ChatTurnStarted {
            prompt: prompt.clone(),
        })
        .await
    {
        cancel.cancel();
        return false;
    }

    if let Some(store) = session.as_mut()
        && let Err(error) = store.append_user_message(&prompt)
    {
        sink.notice(EventLevel::Error, format!("session write failed: {error}"));
    }

    let mut messages = history.clone();
    messages.push(ChatMessage::user(prompt.clone()));
    let request = ChatRequest::new(messages);
    let context_plan = match backend.context_admission() {
        Some(admission) => match admission.admit(&request.messages, &request.tools) {
            Ok(plan) => Some(plan),
            Err(rejection) => {
                let telemetry = build_chat_telemetry(
                    Some(rejection.plan),
                    None,
                    "",
                    Some("admission_rejected".into()),
                    true,
                    None,
                    true,
                );
                persist_session_telemetry(&mut session, sink, &telemetry);
                let _ = sink
                    .send_critical(AgentEvent::ChatTelemetry { telemetry })
                    .await;
                let _ = sink
                    .send_critical(AgentEvent::ChatTurnFailed {
                        message: rejection.message(),
                    })
                    .await;
                if sink.is_closed() || cancel.is_cancelled() {
                    cancel.cancel();
                    return false;
                }
                return true;
            }
        },
        None => None,
    };
    let started = Instant::now();
    let mut first_token_at = None;
    let (event_tx, mut event_rx) = mpsc::unbounded_channel();
    let call = backend.stream_visible_chat(request, Some(&event_tx));
    tokio::pin!(call);

    let mut assistant = String::new();
    let mut failure_sent = false;
    let mut events_open = true;

    let result = loop {
        tokio::select! {
            biased;
            command = commands.recv() => match command {
                Some(Command::Cancel) => {
                    let telemetry = build_chat_telemetry(
                        context_plan,
                        ttft_ms(started, first_token_at),
                        &assistant,
                        Some("cancelled".into()),
                        false,
                        None,
                        false,
                    );
                    persist_session_telemetry(&mut session, sink, &telemetry);
                    let _ = sink
                        .send_critical(AgentEvent::ChatTelemetry { telemetry })
                        .await;
                    let _ = sink
                        .send_critical(AgentEvent::ChatTurnCancelled {
                            reason: "operator cancelled the active turn".into(),
                        })
                        .await;
                    return true;
                }
                Some(Command::Quit) => {
                    cancel.cancel();
                    return false;
                }
                Some(Command::Submit { .. }) | Some(Command::Chat { .. }) => {
                    sink.notice(
                        EventLevel::Warning,
                        "a chat turn is already running; cancel it before starting another run",
                    );
                }
                Some(Command::ResolveApproval { .. }) => {
                    sink.notice(EventLevel::Warning, "no approval gate is active");
                }
                Some(Command::NewSession) | Some(Command::ResumeSession { .. }) => {
                    sink.notice(
                        EventLevel::Warning,
                        "a chat turn is already running; cancel it before changing sessions",
                    );
                }
                None => {
                    cancel.cancel();
                    return false;
                }
            },
            event = event_rx.recv(), if events_open => match event {
                Some(event) => {
                    handle_visible_event(
                        sink,
                        event,
                        &mut assistant,
                        &mut failure_sent,
                        &mut first_token_at,
                    )
                    .await;
                }
                None => events_open = false,
            },
            result = &mut call => {
                while let Ok(event) = event_rx.try_recv() {
                    handle_visible_event(
                        sink,
                        event,
                        &mut assistant,
                        &mut failure_sent,
                        &mut first_token_at,
                    )
                    .await;
                }
                break result;
            }
        }
    };

    match result {
        Ok(outcome) => {
            let completed = !outcome.empty_visible_response && !assistant.trim().is_empty();
            if completed {
                append_chat_history(history, prompt, assistant.clone());
                if let Some(store) = session.as_mut()
                    && let Err(error) = store.append_assistant_message(&assistant)
                {
                    sink.notice(EventLevel::Error, format!("session write failed: {error}"));
                }
            }
            let telemetry = build_chat_telemetry(
                context_plan,
                ttft_ms(started, first_token_at),
                &assistant,
                Some(outcome.finish_reason.clone()),
                outcome.empty_visible_response,
                None,
                false,
            );
            persist_session_telemetry(&mut session, sink, &telemetry);
            let _ = sink
                .send_critical(AgentEvent::ChatTelemetry { telemetry })
                .await;
            let _ = sink
                .send_critical(AgentEvent::ChatTurnFinished {
                    finish_reason: outcome.finish_reason,
                    empty_visible_response: outcome.empty_visible_response,
                })
                .await;
        }
        Err(error) => {
            let telemetry = build_chat_telemetry(
                context_plan,
                ttft_ms(started, first_token_at),
                &assistant,
                None,
                false,
                transport_error_category(&error),
                false,
            );
            persist_session_telemetry(&mut session, sink, &telemetry);
            let _ = sink
                .send_critical(AgentEvent::ChatTelemetry { telemetry })
                .await;
            if !failure_sent {
                let _ = sink
                    .send_critical(AgentEvent::ChatTurnFailed {
                        message: error.to_string(),
                    })
                    .await;
            }
        }
    }

    if sink.is_closed() || cancel.is_cancelled() {
        cancel.cancel();
        return false;
    }
    true
}

fn ttft_ms(started: Instant, first_token_at: Option<Instant>) -> Option<u64> {
    first_token_at.map(|first| {
        u64::try_from(first.saturating_duration_since(started).as_millis())
            .unwrap_or(u64::MAX)
            .min(600_000)
    })
}

fn build_chat_telemetry(
    plan: Option<ContextPlan>,
    time_to_first_token_ms: Option<u64>,
    assistant: &str,
    finish_reason: Option<String>,
    empty_visible_response: bool,
    transport_error_category: Option<String>,
    admission_rejected: bool,
) -> ChatTurnTelemetry {
    let (
        estimated_prompt_tokens,
        reserved_output_tokens,
        effective_context_cap_tokens,
        safety_margin_tokens,
        context_utilization_percent,
    ) = plan.map_or((0, 0, 0, 0, 0), |plan| {
        (
            plan.estimated_prompt_tokens,
            plan.reserved_output_tokens,
            plan.effective_context_cap_tokens,
            plan.safety_margin_tokens,
            plan.utilization_percent(),
        )
    });

    ChatTurnTelemetry {
        estimated_prompt_tokens,
        reserved_output_tokens,
        effective_context_cap_tokens,
        safety_margin_tokens,
        context_utilization_percent,
        time_to_first_token_ms,
        visible_output_chars: assistant.chars().count(),
        visible_output_tokens: estimate_text_tokens(assistant),
        finish_reason,
        empty_visible_response,
        transport_error_category,
        admission_rejected,
    }
}

fn transport_error_category(error: &crate::error::AgentError) -> Option<String> {
    let crate::error::AgentError::Llm(message) = error else {
        return Some("other".into());
    };
    let message = message.to_ascii_lowercase();
    let category = if message.contains("timed out") {
        "timeout"
    } else if message.contains("could not connect") {
        "connect"
    } else if message.contains("http status") {
        "http_status"
    } else if message.contains("stream")
        || message.contains("sse")
        || message.contains("before [done]")
    {
        "stream"
    } else {
        "other"
    };
    Some(category.into())
}

async fn handle_visible_event(
    sink: &TuiEventSink,
    event: VisibleChatEvent,
    assistant: &mut String,
    failure_sent: &mut bool,
    first_token_at: &mut Option<Instant>,
) {
    match event {
        VisibleChatEvent::TextDelta(text) => {
            if text.is_empty() {
                return;
            }
            if first_token_at.is_none() {
                *first_token_at = Some(Instant::now());
            }
            assistant.push_str(&text);
            sink.emit(AgentEvent::StreamDelta { text });
        }
        VisibleChatEvent::Warning(message) => {
            if message != "empty visible response" {
                sink.notice(EventLevel::Warning, message);
            }
        }
        VisibleChatEvent::Error(message) => {
            if !*failure_sent {
                *failure_sent = true;
                let _ = sink
                    .send_critical(AgentEvent::ChatTurnFailed { message })
                    .await;
            }
        }
        VisibleChatEvent::Finish { .. } => {}
    }
}

fn persist_session_telemetry(
    session: &mut Option<&mut SessionStore>,
    sink: &TuiEventSink,
    telemetry: &ChatTurnTelemetry,
) {
    if let Some(store) = session.as_mut()
        && let Err(error) = store.append_telemetry(telemetry)
    {
        sink.notice(
            EventLevel::Error,
            format!("session telemetry write failed: {error}"),
        );
    }
}

fn chat_history_from_session(messages: &[SessionMessage]) -> Vec<ChatMessage> {
    let mut history = vec![ChatMessage::system(CHAT_SYSTEM_PROMPT)];
    for message in messages {
        history.push(match message.role {
            SessionRole::User => ChatMessage::user(message.text.clone()),
            SessionRole::Assistant => ChatMessage::assistant(message.text.clone()),
        });
    }
    trim_chat_history(&mut history);
    history
}

fn append_chat_history(history: &mut Vec<ChatMessage>, prompt: String, assistant: String) {
    history.push(ChatMessage::user(prompt));
    history.push(ChatMessage::assistant(assistant));
    trim_chat_history(history);
}

fn trim_chat_history(history: &mut Vec<ChatMessage>) {
    while history.len() > 1 && chat_history_bytes(history) > MAX_CHAT_HISTORY_BYTES {
        remove_oldest_chat_pair(history);
    }
    while history.len() > MAX_CHAT_HISTORY_MESSAGES {
        remove_oldest_chat_pair(history);
    }
}

fn remove_oldest_chat_pair(history: &mut Vec<ChatMessage>) {
    if history.len() <= 1 {
        return;
    }
    history.remove(1);
    if history.len() > 1 {
        history.remove(1);
    }
}

fn chat_history_bytes(history: &[ChatMessage]) -> usize {
    history
        .iter()
        .map(|message| message.content.as_deref().map_or(0, str::len))
        .sum()
}

fn notify_approval_resolution(
    sink: &TuiEventSink,
    result: std::result::Result<(), ApprovalResolveError>,
) {
    match result {
        Ok(()) => {}
        Err(ApprovalResolveError::NoPending) => {
            sink.notice(EventLevel::Warning, "no approval gate is active")
        }
        Err(ApprovalResolveError::Mismatched) => sink.notice(
            EventLevel::Warning,
            "approval decision did not match the pending action",
        ),
    }
}

#[derive(Clone)]
struct TuiEventSink {
    tx: mpsc::Sender<AgentEvent>,
    stream_truncated: Arc<AtomicBool>,
}

impl TuiEventSink {
    fn new(tx: mpsc::Sender<AgentEvent>) -> Self {
        Self {
            tx,
            stream_truncated: Arc::new(AtomicBool::new(false)),
        }
    }

    async fn send_critical(&self, event: AgentEvent) -> bool {
        if matches!(
            &event,
            AgentEvent::TaskStarted { .. } | AgentEvent::ChatTurnStarted { .. }
        ) {
            self.stream_truncated.store(false, Ordering::Relaxed);
        }
        self.tx.send(event).await.is_ok()
    }

    fn notice(&self, level: EventLevel, message: impl Into<String>) {
        self.emit(AgentEvent::Notice {
            level,
            message: message.into(),
        });
    }

    fn is_closed(&self) -> bool {
        self.tx.is_closed()
    }
}

impl EventSink for TuiEventSink {
    fn emit(&self, event: AgentEvent) {
        match event {
            AgentEvent::StreamDelta { .. } => {
                if let Err(TrySendError::Full(_)) = self.tx.try_send(event)
                    && !self.stream_truncated.swap(true, Ordering::Relaxed)
                {
                    spawn_send(self.tx.clone(), AgentEvent::StreamTruncated { dropped: 1 });
                }
            }
            event => match self.tx.try_send(event) {
                Ok(()) => {}
                Err(TrySendError::Full(event)) => spawn_send(self.tx.clone(), event),
                Err(TrySendError::Closed(_)) => {}
            },
        }
    }
}

fn spawn_send(tx: mpsc::Sender<AgentEvent>, event: AgentEvent) {
    if tokio::runtime::Handle::try_current().is_err() {
        return;
    }
    tokio::spawn(async move {
        let _ = tx.send(event).await;
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::AgentError;
    use crate::llm::VisibleChatOutcome;
    use crate::session::SessionConfig;

    struct FakeChatBackend {
        events: Vec<VisibleChatEvent>,
        outcome: Option<VisibleChatOutcome>,
        error: Option<String>,
        pending: bool,
        admission: Option<crate::llm::ContextAdmission>,
        calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    }

    impl FakeChatBackend {
        fn new(events: Vec<VisibleChatEvent>, outcome: VisibleChatOutcome) -> Self {
            Self {
                events,
                outcome: Some(outcome),
                error: None,
                pending: false,
                admission: None,
                calls: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            }
        }

        fn with_admission(mut self, admission: crate::llm::ContextAdmission) -> Self {
            self.admission = Some(admission);
            self
        }
    }

    #[async_trait::async_trait]
    impl VisibleChatBackend for FakeChatBackend {
        fn context_admission(&self) -> Option<crate::llm::ContextAdmission> {
            self.admission
                .or(Some(crate::llm::ContextAdmission::from_limits(
                    &crate::config::Limits::default(),
                )))
        }

        async fn stream_visible_chat(
            &self,
            _request: ChatRequest,
            event_tx: Option<&mpsc::UnboundedSender<VisibleChatEvent>>,
        ) -> crate::error::Result<VisibleChatOutcome> {
            self.calls
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if self.pending {
                tokio::time::sleep(Duration::from_secs(60)).await;
            }
            if let Some(event_tx) = event_tx {
                for event in &self.events {
                    let _ = event_tx.send(event.clone());
                }
            }
            if let Some(error) = &self.error {
                return Err(AgentError::Llm(error.clone()));
            }
            Ok(self.outcome.clone().expect("fake outcome"))
        }
    }

    fn outcome(empty_visible_response: bool) -> VisibleChatOutcome {
        VisibleChatOutcome {
            finish_reason: "stop".into(),
            empty_visible_response,
            hidden_reasoning_observed: false,
        }
    }

    fn drain_events(rx: &mut mpsc::Receiver<AgentEvent>) -> Vec<AgentEvent> {
        let mut events = Vec::new();
        while let Ok(event) = rx.try_recv() {
            events.push(event);
        }
        events
    }

    #[tokio::test]
    async fn critical_events_are_retried_when_the_channel_is_full() {
        let (tx, mut rx) = mpsc::channel(1);
        let first = AgentEvent::Notice {
            level: EventLevel::Info,
            message: "filler".into(),
        };
        tx.try_send(first.clone()).unwrap();
        let sink = TuiEventSink::new(tx);

        sink.emit(AgentEvent::Completed {
            summary: "done".into(),
        });

        assert_eq!(rx.recv().await, Some(first));
        let delivered = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("critical event should be retried");
        assert_eq!(
            delivered,
            Some(AgentEvent::Completed {
                summary: "done".into()
            })
        );
    }

    #[tokio::test]
    async fn dropped_stream_deltas_emit_one_truncation_notice() {
        let (tx, mut rx) = mpsc::channel(1);
        let first = AgentEvent::Notice {
            level: EventLevel::Info,
            message: "filler".into(),
        };
        tx.try_send(first.clone()).unwrap();
        let sink = TuiEventSink::new(tx);

        sink.emit(AgentEvent::StreamDelta { text: "one".into() });
        sink.emit(AgentEvent::StreamDelta { text: "two".into() });

        assert_eq!(rx.recv().await, Some(first));
        let notice = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .expect("truncation notice should be delivered");
        assert_eq!(notice, Some(AgentEvent::StreamTruncated { dropped: 1 }));
        assert!(
            tokio::time::timeout(Duration::from_millis(50), rx.recv())
                .await
                .is_err(),
            "only one truncation notice should be emitted"
        );
    }

    #[tokio::test]
    async fn closed_event_channel_is_not_fatal() {
        let (tx, rx) = mpsc::channel(1);
        drop(rx);
        let sink = TuiEventSink::new(tx);

        sink.emit(AgentEvent::StreamDelta {
            text: "ignored".into(),
        });
        sink.emit(AgentEvent::Completed {
            summary: "ignored".into(),
        });
        tokio::task::yield_now().await;
    }

    #[tokio::test]
    async fn command_channel_is_bounded() {
        let (tx, _rx) = mpsc::channel::<Command>(COMMAND_CHANNEL_CAPACITY);
        for _ in 0..COMMAND_CHANNEL_CAPACITY {
            tx.try_send(Command::Cancel).unwrap();
        }
        assert!(matches!(
            tx.try_send(Command::Quit),
            Err(TrySendError::Full(Command::Quit))
        ));
    }
    #[tokio::test]
    async fn chat_turn_streams_visible_text_and_updates_history() {
        let (tx, mut rx) = mpsc::channel(32);
        let (command_tx, mut command_rx) = mpsc::channel(4);
        let sink = TuiEventSink::new(tx);
        let backend = Arc::new(FakeChatBackend::new(
            vec![
                VisibleChatEvent::TextDelta("Hello".into()),
                VisibleChatEvent::Finish {
                    reason: "stop".into(),
                    empty_visible_response: false,
                },
            ],
            outcome(false),
        ));
        let mut history = vec![ChatMessage::system("system")];

        let keep_running = run_chat_turn(
            backend,
            &sink,
            &mut history,
            "hi".into(),
            &mut command_rx,
            &CancellationToken::new(),
        )
        .await;
        drop(command_tx);

        assert!(keep_running);
        let events = drain_events(&mut rx);
        assert_eq!(events.len(), 4);
        assert_eq!(
            events[0],
            AgentEvent::ChatTurnStarted {
                prompt: "hi".into()
            }
        );
        assert_eq!(
            events[1],
            AgentEvent::StreamDelta {
                text: "Hello".into()
            }
        );
        let AgentEvent::ChatTelemetry { telemetry } = &events[2] else {
            panic!("expected chat telemetry, got {:?}", events[2]);
        };
        assert_eq!(telemetry.estimated_prompt_tokens, 22);
        assert_eq!(telemetry.visible_output_chars, 5);
        assert_eq!(telemetry.visible_output_tokens, 2);
        assert!(telemetry.time_to_first_token_ms.is_some());
        assert_eq!(telemetry.finish_reason.as_deref(), Some("stop"));
        assert!(!telemetry.admission_rejected);
        assert_eq!(
            events[3],
            AgentEvent::ChatTurnFinished {
                finish_reason: "stop".into(),
                empty_visible_response: false,
            }
        );
        assert_eq!(history.len(), 3);
        assert_eq!(history[1].content.as_deref(), Some("hi"));
        assert_eq!(history[2].content.as_deref(), Some("Hello"));
    }

    #[tokio::test]
    async fn completed_chat_turn_is_persisted_for_restart() {
        let dir = std::env::temp_dir().join(format!(
            "rupi-worker-session-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let config = SessionConfig::new(dir.clone(), "local-model", 65_536);
        let mut session = SessionStore::create_new(&config).unwrap();
        let session_id = session.id().to_owned();
        let (tx, mut rx) = mpsc::channel(32);
        let (command_tx, mut command_rx) = mpsc::channel(4);
        let sink = TuiEventSink::new(tx);
        let backend = Arc::new(FakeChatBackend::new(
            vec![
                VisibleChatEvent::TextDelta("Hello".into()),
                VisibleChatEvent::Finish {
                    reason: "stop".into(),
                    empty_visible_response: false,
                },
            ],
            outcome(false),
        ));
        let mut history = vec![ChatMessage::system("system")];

        let keep_running = run_chat_turn_persisted(
            backend,
            &sink,
            &mut history,
            Some(&mut session),
            "hi".into(),
            &mut command_rx,
            &CancellationToken::new(),
        )
        .await;
        drop(command_tx);

        assert!(keep_running);
        let events = drain_events(&mut rx);
        assert!(events.iter().any(|event| matches!(
            event,
            AgentEvent::ChatTurnFinished {
                empty_visible_response: false,
                ..
            }
        )));

        let restored = SessionStore::resume(&config, &session_id).unwrap();
        assert_eq!(restored.messages().len(), 2);
        assert_eq!(restored.messages()[0].role, SessionRole::User);
        assert_eq!(restored.messages()[0].text, "hi");
        assert_eq!(restored.messages()[1].role, SessionRole::Assistant);
        assert_eq!(restored.messages()[1].text, "Hello");

        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn cancelled_stream_persists_no_assistant_completion() {
        let dir = std::env::temp_dir().join(format!(
            "rupi-worker-session-cancel-{}",
            uuid::Uuid::new_v4().simple()
        ));
        let config = SessionConfig::new(dir.clone(), "local-model", 65_536);
        let mut session = SessionStore::create_new(&config).unwrap();
        let session_id = session.id().to_owned();
        let (tx, mut rx) = mpsc::channel(32);
        let (command_tx, mut command_rx) = mpsc::channel(4);
        let sink = TuiEventSink::new(tx);
        let backend = Arc::new(FakeChatBackend {
            events: Vec::new(),
            outcome: None,
            error: None,
            pending: true,
            admission: None,
            calls: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        });
        let mut history = vec![ChatMessage::system("system")];
        let cancel = CancellationToken::new();
        let turn = run_chat_turn_persisted(
            backend,
            &sink,
            &mut history,
            Some(&mut session),
            "stream this".into(),
            &mut command_rx,
            &cancel,
        );
        tokio::pin!(turn);

        tokio::select! {
            result = &mut turn => panic!("turn should wait for cancellation: {result}"),
            _ = tokio::time::sleep(Duration::from_millis(20)) => {}
        }
        command_tx.send(Command::Cancel).await.unwrap();
        assert!(turn.await);
        drop(command_tx);

        let events = drain_events(&mut rx);
        assert!(
            events
                .iter()
                .any(|event| matches!(event, AgentEvent::ChatTurnCancelled { .. }))
        );
        let restored = SessionStore::resume(&config, &session_id).unwrap();
        assert_eq!(restored.messages().len(), 1);
        assert_eq!(restored.messages()[0].role, SessionRole::User);
        assert!(
            restored
                .messages()
                .iter()
                .all(|message| message.role != SessionRole::Assistant)
        );

        let _ = std::fs::remove_dir_all(dir);
    }

    #[tokio::test]
    async fn empty_chat_turn_does_not_add_assistant_history() {
        let (tx, mut rx) = mpsc::channel(32);
        let (command_tx, mut command_rx) = mpsc::channel(4);
        let sink = TuiEventSink::new(tx);
        let backend = Arc::new(FakeChatBackend::new(
            vec![
                VisibleChatEvent::Warning("empty visible response".into()),
                VisibleChatEvent::Finish {
                    reason: "stop".into(),
                    empty_visible_response: true,
                },
            ],
            outcome(true),
        ));
        let mut history = vec![ChatMessage::system("system")];

        run_chat_turn(
            backend,
            &sink,
            &mut history,
            "hi".into(),
            &mut command_rx,
            &CancellationToken::new(),
        )
        .await;
        drop(command_tx);

        assert_eq!(history.len(), 1);
        let events = drain_events(&mut rx);
        assert!(events.contains(&AgentEvent::ChatTurnFinished {
            finish_reason: "stop".into(),
            empty_visible_response: true,
        }));
        assert!(!events.iter().any(|event| {
            matches!(event, AgentEvent::Notice { message, .. } if message == "empty visible response")
        }));
    }

    #[tokio::test]
    async fn chat_failure_is_reported_once_and_not_added_to_history() {
        let (tx, mut rx) = mpsc::channel(32);
        let (command_tx, mut command_rx) = mpsc::channel(4);
        let sink = TuiEventSink::new(tx);
        let backend = Arc::new(FakeChatBackend {
            events: vec![VisibleChatEvent::Error("stream broke".into())],
            outcome: None,
            error: Some("stream broke".into()),
            pending: false,
            admission: None,
            calls: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        });
        let mut history = vec![ChatMessage::system("system")];

        run_chat_turn(
            backend,
            &sink,
            &mut history,
            "hi".into(),
            &mut command_rx,
            &CancellationToken::new(),
        )
        .await;
        drop(command_tx);

        assert_eq!(history.len(), 1);
        let events = drain_events(&mut rx);
        let failures = events
            .iter()
            .filter(|event| matches!(event, AgentEvent::ChatTurnFailed { .. }))
            .count();
        assert_eq!(failures, 1);
        let telemetry = events
            .iter()
            .find_map(|event| match event {
                AgentEvent::ChatTelemetry { telemetry } => Some(telemetry),
                _ => None,
            })
            .expect("failed turn should report telemetry");
        assert_eq!(
            telemetry.transport_error_category.as_deref(),
            Some("stream")
        );
        assert_eq!(telemetry.finish_reason, None);
        assert!(!telemetry.admission_rejected);
    }

    #[tokio::test]
    async fn chat_admission_rejects_before_backend_call_and_reports_telemetry() {
        let (tx, mut rx) = mpsc::channel(32);
        let (command_tx, mut command_rx) = mpsc::channel(4);
        let sink = TuiEventSink::new(tx);
        let backend = Arc::new(
            FakeChatBackend::new(
                vec![VisibleChatEvent::TextDelta("must not run".into())],
                outcome(false),
            )
            .with_admission(crate::llm::ContextAdmission::new(128, 32, 16)),
        );
        let observed = Arc::clone(&backend);
        let mut history = vec![ChatMessage::system("system")];

        let keep_running = run_chat_turn(
            backend,
            &sink,
            &mut history,
            "x".repeat(1_000),
            &mut command_rx,
            &CancellationToken::new(),
        )
        .await;
        drop(command_tx);

        assert!(keep_running);
        assert_eq!(
            observed.calls.load(std::sync::atomic::Ordering::Relaxed),
            0,
            "admission must reject before invoking the backend"
        );
        assert_eq!(history.len(), 1);
        let events = drain_events(&mut rx);
        assert_eq!(
            events[0],
            AgentEvent::ChatTurnStarted {
                prompt: "x".repeat(1_000)
            }
        );
        let AgentEvent::ChatTelemetry { telemetry } = &events[1] else {
            panic!("expected rejection telemetry");
        };
        assert!(telemetry.admission_rejected);
        assert_eq!(telemetry.effective_context_cap_tokens, 128);
        assert_eq!(telemetry.reserved_output_tokens, 32);
        assert_eq!(telemetry.safety_margin_tokens, 16);
        assert_eq!(
            telemetry.finish_reason.as_deref(),
            Some("admission_rejected")
        );
        assert_eq!(telemetry.context_utilization_percent, 100);
        assert!(matches!(
            &events[2],
            AgentEvent::ChatTurnFailed { message } if message.contains("context admission rejected")
        ));
    }

    #[tokio::test]
    async fn chat_success_telemetry_uses_admission_plan() {
        let (tx, mut rx) = mpsc::channel(32);
        let (command_tx, mut command_rx) = mpsc::channel(4);
        let sink = TuiEventSink::new(tx);
        let backend = Arc::new(
            FakeChatBackend::new(
                vec![
                    VisibleChatEvent::TextDelta("Hello".into()),
                    VisibleChatEvent::Finish {
                        reason: "stop".into(),
                        empty_visible_response: false,
                    },
                ],
                outcome(false),
            )
            .with_admission(crate::llm::ContextAdmission::new(1_000, 200, 100)),
        );
        let mut history = vec![ChatMessage::system("system")];

        run_chat_turn(
            backend,
            &sink,
            &mut history,
            "hi".into(),
            &mut command_rx,
            &CancellationToken::new(),
        )
        .await;
        drop(command_tx);

        let events = drain_events(&mut rx);
        let AgentEvent::ChatTelemetry { telemetry } = &events[2] else {
            panic!("expected chat telemetry");
        };
        assert_eq!(telemetry.estimated_prompt_tokens, 22);
        assert_eq!(telemetry.effective_context_cap_tokens, 1_000);
        assert_eq!(telemetry.reserved_output_tokens, 200);
        assert_eq!(telemetry.safety_margin_tokens, 100);
        assert_eq!(telemetry.context_utilization_percent, 2);
    }

    #[tokio::test]
    async fn chat_cancel_command_stops_the_pending_turn() {
        let (tx, mut rx) = mpsc::channel(32);
        let (command_tx, mut command_rx) = mpsc::channel(4);
        let sink = TuiEventSink::new(tx);
        let backend = Arc::new(FakeChatBackend {
            events: Vec::new(),
            outcome: None,
            error: None,
            pending: true,
            admission: None,
            calls: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        });
        let mut history = vec![ChatMessage::system("system")];
        let cancel = CancellationToken::new();

        let keep_running = {
            let turn = run_chat_turn(
                backend,
                &sink,
                &mut history,
                "hi".into(),
                &mut command_rx,
                &cancel,
            );
            tokio::pin!(turn);

            let first = tokio::select! {
                event = rx.recv() => event,
                _ = &mut turn => panic!("turn ended before it started"),
            };
            assert_eq!(
                first,
                Some(AgentEvent::ChatTurnStarted {
                    prompt: "hi".into()
                })
            );
            command_tx.send(Command::Cancel).await.unwrap();
            tokio::time::timeout(Duration::from_secs(1), &mut turn)
                .await
                .expect("cancelled chat turn should stop promptly")
        };
        assert!(keep_running);
        assert_eq!(history.len(), 1);
        assert!(
            drain_events(&mut rx).contains(&AgentEvent::ChatTurnCancelled {
                reason: "operator cancelled the active turn".into(),
            })
        );
    }
}
