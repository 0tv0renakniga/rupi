use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use crate::events::Command;
use crate::state_machine::TaskMode;
use crate::tui::app::{AppState, InputMode, MAX_TASK_INPUT_BYTES, TaskStatus, TranscriptKind};

/// Translate one key press into an optional worker command. All other changes
/// are local presentation state; this function never performs I/O.
pub fn handle_key(app: &mut AppState, key: KeyEvent) -> Option<Command> {
    if key.kind == KeyEventKind::Release {
        return None;
    }

    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);

    if ctrl && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('C')) {
        return handle_ctrl_c(app);
    }

    if app.show_help {
        match key.code {
            KeyCode::Esc | KeyCode::F(1) | KeyCode::Char('?') => {
                app.show_help = false;
                return None;
            }
            _ => {}
        }
    }

    if key.code == KeyCode::F(1) {
        app.show_help = !app.show_help;
        return None;
    }

    match key.code {
        KeyCode::Up => {
            app.scroll_offset = app
                .scroll_offset
                .saturating_add(1)
                .min(app.transcript.len());
            return None;
        }
        KeyCode::Down => {
            app.scroll_offset = app.scroll_offset.saturating_sub(1);
            return None;
        }
        KeyCode::PageUp => {
            app.scroll_offset = app
                .scroll_offset
                .saturating_add(10)
                .min(app.transcript.len());
            return None;
        }
        KeyCode::PageDown => {
            app.scroll_offset = app.scroll_offset.saturating_sub(10);
            return None;
        }
        KeyCode::Tab => {
            app.selected_detail = match app.selected_detail {
                crate::tui::app::DetailPane::Transcript => crate::tui::app::DetailPane::Detail,
                crate::tui::app::DetailPane::Detail => crate::tui::app::DetailPane::Transcript,
            };
            return None;
        }
        _ => {}
    }

    if app.show_help {
        return None;
    }

    if key.code == KeyCode::Char('?') && app.input_mode != InputMode::TaskEntry {
        app.show_help = true;
        return None;
    }

    if key.code == KeyCode::Char('q') && !ctrl && !alt && app.input_mode != InputMode::TaskEntry {
        return handle_quit(app);
    }

    match app.input_mode {
        InputMode::TaskEntry => handle_task_entry(app, key, ctrl, alt),
        InputMode::Approval => handle_approval(app, key, ctrl, alt),
        InputMode::Running => handle_running(app, key),
        InputMode::Blocked => handle_blocked(app, key),
        InputMode::Completed | InputMode::Cancelled | InputMode::Failed => {
            if key.code == KeyCode::Enter {
                app.show_help = false;
            }
            None
        }
    }
}

/// Append a bracketed-paste payload in task-entry mode only.
pub fn handle_paste(app: &mut AppState, text: &str) {
    if app.input_mode != InputMode::TaskEntry || app.show_help {
        return;
    }
    append_input(app, text);
}

fn handle_ctrl_c(app: &mut AppState) -> Option<Command> {
    if app.task_status.is_active() {
        if app.task_status == TaskStatus::Cancelling && app.cancel_grace_elapsed {
            app.should_quit = true;
            return Some(Command::Quit);
        }
        app.request_cancel();
        return Some(Command::Cancel);
    }
    app.should_quit = true;
    Some(Command::Quit)
}

fn handle_quit(app: &mut AppState) -> Option<Command> {
    if app.task_status.is_active() {
        if app.task_status == TaskStatus::Cancelling && app.cancel_grace_elapsed {
            app.should_quit = true;
            return Some(Command::Quit);
        }
        app.request_cancel();
        return Some(Command::Cancel);
    }
    app.should_quit = true;
    Some(Command::Quit)
}

fn handle_task_entry(app: &mut AppState, key: KeyEvent, ctrl: bool, alt: bool) -> Option<Command> {
    match key.code {
        KeyCode::Enter => submit_task(app),
        KeyCode::F(2) => {
            app.selected_mode = match app.selected_mode {
                TaskMode::Change => TaskMode::Investigate,
                TaskMode::Investigate => TaskMode::Plan,
                TaskMode::Plan if app.change_available => TaskMode::Change,
                TaskMode::Plan => {
                    app.push_notice(
                        TranscriptKind::Warning,
                        "Change mode is gated until reviewed write and verifier scripts are installed",
                    );
                    TaskMode::Investigate
                }
            };
            app.push_notice(
                TranscriptKind::Info,
                &format!("task mode selected: {}", app.selected_mode.as_str()),
            );
            None
        }
        KeyCode::Backspace => {
            app.task_input.pop();
            None
        }
        KeyCode::Char(character) if !ctrl && !alt => {
            append_input(app, &character.to_string());
            None
        }
        _ => None,
    }
}

fn handle_approval(app: &mut AppState, key: KeyEvent, ctrl: bool, alt: bool) -> Option<Command> {
    let approved = match key.code {
        KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') if !ctrl && !alt => true,
        KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') if !ctrl && !alt => false,
        _ => return None,
    };
    resolve_approval(app, approved)
}

fn handle_running(app: &mut AppState, key: KeyEvent) -> Option<Command> {
    if key.code == KeyCode::Esc {
        app.push_notice(
            TranscriptKind::Warning,
            "interrupting a model turn is not supported in this build; use Ctrl-C to cancel the task",
        );
    }
    None
}

fn handle_blocked(app: &mut AppState, key: KeyEvent) -> Option<Command> {
    if key.code == KeyCode::Enter {
        app.detail = "Blocked state acknowledged. Press q to quit.".into();
        app.push_notice(
            TranscriptKind::Info,
            "blocked state acknowledged; the task was not resumed",
        );
    }
    None
}

fn submit_task(app: &mut AppState) -> Option<Command> {
    if app.task_status.is_active() {
        return None;
    }
    let objective = app.task_input.trim().to_owned();
    if objective.is_empty() {
        return None;
    }
    if app.selected_mode == TaskMode::Change && !app.change_available {
        app.push_notice(
            TranscriptKind::Warning,
            "Change mode is gated until reviewed write and verifier scripts are installed",
        );
        return None;
    }

    app.task_input.clear();
    app.task_status = TaskStatus::Running;
    app.input_mode = InputMode::Running;
    app.elapsed = std::time::Duration::ZERO;
    app.completion_summary = None;
    app.last_error = None;
    app.approval = None;
    app.verification = None;
    app.stream_state = crate::tui::app::StreamState::Idle;
    app.stream_truncated = false;
    app.worker_closed = false;
    app.scroll_offset = 0;
    app.detail = "Submitting task...".into();
    app.push_entry(
        TranscriptKind::Task,
        format!(
            "submitting {} task: {objective}",
            app.selected_mode.as_str()
        ),
    );
    Some(Command::Submit {
        objective,
        mode: app.selected_mode,
    })
}

fn resolve_approval(app: &mut AppState, approved: bool) -> Option<Command> {
    let approval_id = app.approval.as_ref()?.approval_id.clone();
    app.approval = None;
    app.input_mode = InputMode::Running;
    app.detail = if approved {
        "Approval granted; waiting for the worker.".into()
    } else {
        "Approval denied; waiting for the worker.".into()
    };
    app.push_entry(
        TranscriptKind::Approval,
        if approved {
            "approval granted; awaiting worker confirmation"
        } else {
            "approval denied; awaiting worker confirmation"
        },
    );
    Some(Command::ResolveApproval {
        approval_id,
        approved,
    })
}

fn append_input(app: &mut AppState, text: &str) {
    if app.task_input.len().saturating_add(text.len()) > MAX_TASK_INPUT_BYTES {
        return;
    }
    app.task_input.push_str(text);
}
