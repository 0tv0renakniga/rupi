pub mod app;
pub mod input;
pub mod render;
pub mod worker;

pub use app::{
    AppState, ApprovalPrompt, DetailPane, InputMode, StreamState, TaskStatus, TranscriptEntry,
    TranscriptKind, VerificationView,
};

use std::io::Stdout;
use std::sync::Arc;
use std::time::Duration;

use crossterm::cursor::Show;
use crossterm::event::{
    DisableBracketedPaste, EnableBracketedPaste, Event, EventStream, KeyEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use futures_util::StreamExt;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use tokio::sync::mpsc::error::TrySendError;

use crate::approval::InteractiveApprovalGate;
use crate::audit::AuditLog;
use crate::config::Config;
use crate::error::{AgentError, Result};
use crate::events::{AgentEvent, Command, EventLevel};
use crate::jev::JevClient;
use crate::llm::OpenAiCompatClient;
use crate::orchestrator::{
    LocalVerificationEngine, LocalWikiStore, Orchestrator, OrchestratorLimits, ToolExecutor,
};
use crate::runtime;

const TICK_INTERVAL: Duration = Duration::from_millis(200);

type TuiTerminal = Terminal<CrosstermBackend<Stdout>>;

struct PreparedApp {
    app: AppState,
    orchestrator: Orchestrator,
    approvals: InteractiveApprovalGate,
}

/// Launch the single-task TUI. Configuration must already be validated by the
/// caller; terminal setup happens only after dependency construction succeeds.
pub async fn run(config: Config) -> Result<()> {
    let PreparedApp {
        mut app,
        orchestrator,
        approvals,
    } = prepare(&config).await?;

    let (mut terminal, guard) = setup_terminal()?;
    let change_available = app.change_available;
    let mut worker = worker::spawn(
        orchestrator,
        config.root.clone(),
        config.budget.clone(),
        approvals,
        change_available,
    );

    let result = event_loop(&mut terminal, &mut app, &mut worker).await;

    drop(terminal);
    drop(guard);
    worker.shutdown().await;
    result
}

async fn prepare(config: &Config) -> Result<PreparedApp> {
    let mut app = AppState::new();
    app.budget_summary = format!(
        "acts {}/tools {}/verify {}/wall {}s",
        config.budget.max_act_turns,
        config.budget.max_tool_calls,
        config.budget.max_verify_retries,
        config.budget.max_wall_time.as_secs()
    );
    app.endpoint_mode = config.endpoint.profile.as_str().to_owned();
    app.model = config.model.clone();

    let runtime = runtime::build(config)?;
    app.change_available = runtime.capabilities.change_ready();
    if !app.change_available {
        app.selected_mode = crate::state_machine::TaskMode::Investigate;
        app.push_notice(
            TranscriptKind::Warning,
            &format!(
                "Change mode is gated until the reviewed verifier script is installed: {}",
                runtime.capabilities.missing_file_names().join(", ")
            ),
        );
    }
    // Startup happens before terminal setup so failures remain printable on a
    // normal terminal and no task can run with partial tool capabilities.
    let approvals = InteractiveApprovalGate::new();
    let orchestrator = build_orchestrator(
        config,
        runtime.executor.clone(),
        runtime.audit.clone(),
        approvals.clone(),
    )?;

    Ok(PreparedApp {
        app,
        orchestrator,
        approvals,
    })
}

fn build_orchestrator(
    config: &Config,
    tools: Arc<dyn ToolExecutor>,
    audit: Arc<AuditLog>,
    approvals: InteractiveApprovalGate,
) -> Result<Orchestrator> {
    let mut llm = OpenAiCompatClient::new(config.llm_base.clone(), config.model.clone());
    if let Some(credential) = config.llm_credential()? {
        llm = llm.with_bearer_token(credential);
    }
    let llm = Arc::new(llm);
    let jev = Arc::new(JevClient::new(config.jev_base.clone()));
    let limits = OrchestratorLimits {
        min_choice_confidence: config.min_route_conf,
        min_review_confidence: config.min_route_conf,
        ..OrchestratorLimits::default()
    };

    Ok(Orchestrator::new(
        llm,
        jev,
        tools,
        Arc::new(LocalVerificationEngine),
        Arc::new(LocalWikiStore::default()),
    )
    .with_limits(limits)
    .with_approval_policy(config.approval.clone())
    .with_approval_gate(Arc::new(approvals))
    .with_audit(audit))
}

async fn event_loop(
    terminal: &mut TuiTerminal,
    app: &mut AppState,
    worker: &mut worker::Worker,
) -> Result<()> {
    let mut reader = EventStream::new();
    let mut ticker = tokio::time::interval(TICK_INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut worker_closed = false;

    terminal.draw(|frame| render::draw(frame, app))?;

    loop {
        tokio::select! {
            maybe_event = reader.next() => {
                match maybe_event {
                    Some(Ok(Event::Key(key))) if key.kind != KeyEventKind::Release => {
                        if let Some(command) = input::handle_key(app, key) {
                            send_command(app, worker, command);
                        }
                    }
                    Some(Ok(Event::Paste(text))) => input::handle_paste(app, &text),
                    Some(Ok(_)) => {}
                    Some(Err(error)) => return Err(AgentError::Io(error)),
                    None => {
                        return Err(AgentError::Io(std::io::Error::new(
                            std::io::ErrorKind::UnexpectedEof,
                            "terminal event stream ended",
                        )));
                    }
                }
            }
            event = worker.events.recv(), if !worker_closed => {
                match event {
                    Some(event) => app.apply(event),
                    None => {
                        worker_closed = true;
                        app.mark_worker_closed();
                    }
                }
            }
            _ = ticker.tick() => app.tick(),
        }

        terminal.draw(|frame| render::draw(frame, app))?;
        if app.should_quit {
            break;
        }
    }

    Ok(())
}

fn send_command(app: &mut AppState, worker: &worker::Worker, command: Command) {
    match worker.commands.try_send(command) {
        Ok(()) => {}
        Err(TrySendError::Full(command)) => {
            if matches!(command, Command::Cancel | Command::Quit) {
                worker.cancel.cancel();
            }
            if matches!(command, Command::Submit { .. }) {
                app.apply(AgentEvent::Failed {
                    reason: "command queue is full; task was not submitted".into(),
                });
            } else {
                app.apply(AgentEvent::Notice {
                    level: EventLevel::Warning,
                    message: "command queue is full; request was not delivered".into(),
                });
            }
            if matches!(command, Command::Quit) {
                app.should_quit = true;
            }
        }
        Err(TrySendError::Closed(_)) => {
            app.mark_worker_closed();
            app.apply(AgentEvent::Notice {
                level: EventLevel::Error,
                message: "worker command channel is closed".into(),
            });
        }
    }
}

fn setup_terminal() -> Result<(TuiTerminal, TerminalGuard)> {
    let guard = TerminalGuard::install();
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableBracketedPaste)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;
    terminal.hide_cursor()?;
    Ok((terminal, guard))
}

struct TerminalGuard;

impl TerminalGuard {
    fn install() -> Self {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore_terminal();
            previous(info);
        }));
        Self
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore_terminal();
    }
}

fn restore_terminal() {
    let _ = disable_raw_mode();
    let mut stdout = std::io::stdout();
    let _ = execute!(stdout, DisableBracketedPaste, LeaveAlternateScreen, Show);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::approval::ApprovalPolicy;
    use crate::config::{EndpointConfig, ScriptConfig};
    use crate::state_machine::{TaskBudget, TaskMode};
    use std::path::{Path, PathBuf};
    use uuid::Uuid;

    struct Fixture {
        base: PathBuf,
        project: PathBuf,
        scripts: PathBuf,
        config: Config,
    }

    impl Fixture {
        fn new(label: &str) -> Self {
            let base = std::env::temp_dir().join(format!("rupi-tui-{label}-{}", Uuid::new_v4()));
            let project = base.join("project");
            let scripts = base.join("scripts");
            std::fs::create_dir_all(&project).expect("create project");
            std::fs::create_dir_all(&scripts).expect("create scripts");
            let config = Config {
                root: project.clone(),
                audit_path: project.join(".harness-audit/audit.jsonl"),
                llm_base: "http://127.0.0.1:8080".into(),
                model: "local-model".into(),
                jev_base: "http://127.0.0.1:9090".into(),
                endpoint: EndpointConfig::default(),
                min_route_conf: 0.85,
                budget: TaskBudget::default(),
                approval: ApprovalPolicy::default(),
                scripts: ScriptConfig {
                    root: PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts"),
                    interpreter: python_interpreter(),
                    timeout: crate::script_runner::DEFAULT_SCRIPT_TIMEOUT,
                },
            };
            Self {
                base,
                project,
                scripts,
                config,
            }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.base);
        }
    }

    fn python_interpreter() -> PathBuf {
        for candidate in [
            "/usr/bin/python3",
            "/usr/local/bin/python3",
            "/bin/python3",
            "/usr/bin/python",
        ] {
            let path = Path::new(candidate);
            if path.is_file() {
                return path.to_path_buf();
            }
        }
        panic!("python3 is required for TUI runtime tests");
    }

    #[tokio::test]
    async fn prepare_uses_script_runtime_and_gates_change_without_mcp() {
        let fixture = Fixture::new("script-runtime");
        let prepared = prepare(&fixture.config)
            .await
            .expect("script runtime startup");

        assert_eq!(prepared.app.selected_mode, TaskMode::Investigate);
        assert!(!prepared.app.change_available);
        assert!(fixture.project.join(".harness-audit").is_dir());
        let warnings = prepared
            .app
            .transcript
            .iter()
            .map(|entry| entry.text.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(warnings.contains("run_verifier.py"), "{warnings}");
        assert!(!warnings.contains("write_file.py"), "{warnings}");
    }

    #[tokio::test]
    async fn invalid_audit_path_is_returned_before_terminal_setup() {
        let mut fixture = Fixture::new("invalid-audit");
        fixture.config.audit_path = std::env::temp_dir();

        let error = prepare(&fixture.config)
            .await
            .err()
            .expect("directory audit path must fail startup");
        assert!(error.to_string().contains("audit path"), "{error}");
    }

    #[tokio::test]
    async fn incomplete_read_only_scripts_fail_before_terminal_setup() {
        let mut fixture = Fixture::new("missing-scripts");
        fixture.config.scripts.root = fixture.scripts.clone();

        let error = prepare(&fixture.config)
            .await
            .err()
            .expect("missing read-only scripts must fail startup");
        assert!(
            error
                .to_string()
                .contains("read-only script profile is incomplete"),
            "{error}"
        );
        assert!(error.to_string().contains("read_file.py"), "{error}");
    }
}
