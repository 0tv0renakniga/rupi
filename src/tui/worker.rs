use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TrySendError;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::approval::{ApprovalResolveError, InteractiveApprovalGate};
use crate::events::{AgentEvent, Command, EventLevel};
use crate::orchestrator::{EventSink, Orchestrator, TaskRequest};
use crate::state_machine::{TaskBudget, TaskMode};

/// Bounded command queue. The UI uses `try_send` so a stalled worker can never
/// block rendering or terminal restoration indefinitely.
pub const COMMAND_CHANNEL_CAPACITY: usize = 16;
/// Bounded event queue. Critical transitions are retried by a dedicated task;
/// best-effort stream deltas are dropped and explicitly marked truncated.
pub const EVENT_CHANNEL_CAPACITY: usize = 256;
const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

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
}

async fn run_worker(
    orchestrator: Arc<Orchestrator>,
    settings: WorkerSettings,
    sink: TuiEventSink,
    approvals: InteractiveApprovalGate,
    mut commands: mpsc::Receiver<Command>,
    cancel: CancellationToken,
) {
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
                if mode == TaskMode::Change && !settings.change_available {
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

                let request = TaskRequest::new(objective, settings.repo_root.clone())
                    .with_mode(mode)
                    .with_budget(settings.budget.clone());
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
            Command::Cancel => {
                sink.notice(EventLevel::Info, "no active task to cancel");
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
        if matches!(&event, AgentEvent::TaskStarted { .. }) {
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
}
