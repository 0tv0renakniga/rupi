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
use crate::mcp::{McpRuntime, ToolDispatcher};
use crate::orchestrator::{
    LocalVerificationEngine, LocalWikiStore, Orchestrator, OrchestratorLimits,
};
use crate::policy::Policy;

const TICK_INTERVAL: Duration = Duration::from_millis(200);
const MCP_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);

type TuiTerminal = Terminal<CrosstermBackend<Stdout>>;

struct PreparedApp {
    app: AppState,
    runtime: McpRuntime,
    orchestrator: Orchestrator,
    approvals: InteractiveApprovalGate,
}

/// Launch the single-task TUI. Configuration must already be validated by the
/// caller; terminal setup happens only after dependency construction succeeds.
pub async fn run(config: Config) -> Result<()> {
    let PreparedApp {
        mut app,
        runtime,
        orchestrator,
        approvals,
    } = prepare(&config).await?;

    let (mut terminal, guard) = match setup_terminal() {
        Ok(value) => value,
        Err(error) => {
            let _ = runtime.shutdown(MCP_SHUTDOWN_TIMEOUT).await;
            return Err(error);
        }
    };
    let mut worker = worker::spawn(
        orchestrator,
        config.root.clone(),
        config.budget.clone(),
        approvals,
    );

    let result = event_loop(&mut terminal, &mut app, &mut worker).await;

    drop(terminal);
    drop(guard);
    worker.shutdown().await;
    let shutdown = runtime.shutdown(MCP_SHUTDOWN_TIMEOUT).await;

    match (result, shutdown) {
        (Err(error), _) => Err(error),
        (Ok(()), Err(error)) => Err(error),
        (Ok(()), Ok(())) => Ok(()),
    }
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

    let policy = Policy::new(config.root.clone()).map_err(|error| {
        AgentError::Config(format!("repository path policy is invalid: {error}"))
    })?;
    let runtime = McpRuntime::start(
        &config.mcp,
        &config.root,
        policy,
        config.budget.max_tool_output_bytes,
    )
    .await?;
    for warning in runtime.warnings() {
        app.push_notice(TranscriptKind::Warning, warning);
    }
    // Startup happens before terminal setup so failures remain printable on a
    // normal terminal and no task can run with partial tool capabilities.
    let approvals = InteractiveApprovalGate::new();
    let orchestrator = build_orchestrator(
        config,
        runtime.dispatcher(),
        runtime.provenance().to_vec(),
        approvals.clone(),
    )?;

    Ok(PreparedApp {
        app,
        runtime,
        orchestrator,
        approvals,
    })
}

fn build_orchestrator(
    config: &Config,
    tools: Arc<ToolDispatcher>,
    mcp_provenance: Vec<crate::mcp::McpProvenance>,
    approvals: InteractiveApprovalGate,
) -> Result<Orchestrator> {
    let audit = Arc::new(AuditLog::new(config.audit_path.clone()));
    audit.prepare()?;

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
    .with_audit(audit)
    .with_mcp_provenance(mcp_provenance))
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
            if matches!(command, Command::Submit(_)) {
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
    use crate::config::{EndpointConfig, McpProfile, McpRuntimeConfig};
    use crate::state_machine::TaskBudget;
    use std::path::PathBuf;

    fn config() -> Config {
        Config {
            root: PathBuf::from("."),
            audit_path: PathBuf::from(".harness-audit/audit.jsonl"),
            llm_base: "http://127.0.0.1:8080".into(),
            model: "local-model".into(),
            jev_base: "http://127.0.0.1:9090".into(),
            endpoint: EndpointConfig::default(),
            min_route_conf: 0.85,
            budget: TaskBudget::default(),
            approval: ApprovalPolicy::default(),
            mcp: McpRuntimeConfig {
                profile: McpProfile::ReadOnly,
                ..McpRuntimeConfig::default()
            },
        }
    }

    #[tokio::test]
    async fn invalid_audit_path_is_returned_before_terminal_setup() {
        let mut config = config();
        config.mcp = McpRuntimeConfig::default();
        config.audit_path = std::env::temp_dir();

        let error = prepare(&config)
            .await
            .err()
            .expect("directory audit path must fail startup");
        assert!(error.to_string().contains("audit path"), "{error}");
    }

    #[tokio::test]
    async fn startup_configuration_error_is_returned_before_terminal_setup() {
        let error = prepare(&config())
            .await
            .err()
            .expect("invalid MCP profile must fail startup");
        assert!(error.to_string().contains("requires an enabled"));
    }
}
