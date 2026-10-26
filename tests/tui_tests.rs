use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use local_agent_harness::approval::{ApprovalAction, ApprovalRequest};
use local_agent_harness::events::{AgentEvent, Command, EventLevel};
use local_agent_harness::state_machine::{LoopState, TaskMode};
use local_agent_harness::tui::app::{MAX_TRANSCRIPT_ENTRIES, StreamState};
use local_agent_harness::tui::input::{handle_key, handle_paste};
use local_agent_harness::tui::render;
use local_agent_harness::tui::{AppState, InputMode, TaskStatus, TranscriptKind};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn ctrl(character: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(character), KeyModifiers::CONTROL)
}

fn approval(id: &str) -> ApprovalRequest {
    ApprovalRequest {
        approval_id: id.into(),
        action: ApprovalAction::WriteFile,
        target: Some("src/main.rs".into()),
        program: None,
        arguments: None,
        reason: "workspace mutation requires approval".into(),
    }
}

fn start(app: &mut AppState) {
    app.apply(AgentEvent::TaskStarted {
        summary: "fix the failing test".into(),
    });
    app.apply(AgentEvent::PhaseChanged {
        from: LoopState::Boot,
        to: LoopState::Learn,
    });
}

fn buffer_text(terminal: &mut Terminal<TestBackend>) -> String {
    terminal
        .backend_mut()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect()
}

#[test]
fn starts_in_task_entry_mode() {
    let app = AppState::default();
    assert_eq!(app.input_mode, InputMode::TaskEntry);
    assert_eq!(app.task_status, TaskStatus::Idle);
    assert_eq!(app.phase, LoopState::Boot);
    assert!(app.transcript.is_empty());
}

#[test]
fn enter_submits_nonempty_task_and_ignores_empty_input() {
    let mut app = AppState::default();
    assert_eq!(handle_key(&mut app, key(KeyCode::Enter)), None);
    assert_eq!(app.input_mode, InputMode::TaskEntry);

    app.task_input = "fix the failing test".into();
    assert_eq!(
        handle_key(&mut app, key(KeyCode::Enter)),
        Some(Command::Submit {
            objective: "fix the failing test".into(),
            mode: TaskMode::Change,
        })
    );
    assert_eq!(app.input_mode, InputMode::Running);
    assert_eq!(app.task_status, TaskStatus::Running);
    assert!(app.task_input.is_empty());
}

#[test]
fn f2_toggles_mode_only_before_submission() {
    let mut app = AppState::default();
    assert_eq!(app.selected_mode, TaskMode::Change);

    assert_eq!(handle_key(&mut app, key(KeyCode::F(2))), None);
    assert_eq!(app.selected_mode, TaskMode::Investigate);

    app.task_input = "inspect the repository".into();
    assert_eq!(
        handle_key(&mut app, key(KeyCode::Enter)),
        Some(Command::Submit {
            objective: "inspect the repository".into(),
            mode: TaskMode::Investigate,
        })
    );

    assert_eq!(handle_key(&mut app, key(KeyCode::F(2))), None);
    assert_eq!(app.selected_mode, TaskMode::Investigate);
}

#[test]
fn f2_cycles_through_plan_mode_before_submission() {
    let mut app = AppState::default();
    assert_eq!(app.selected_mode, TaskMode::Change);

    assert_eq!(handle_key(&mut app, key(KeyCode::F(2))), None);
    assert_eq!(app.selected_mode, TaskMode::Investigate);

    assert_eq!(handle_key(&mut app, key(KeyCode::F(2))), None);
    assert_eq!(app.selected_mode, TaskMode::Plan);

    app.task_input = "plan the repository change".into();
    assert_eq!(
        handle_key(&mut app, key(KeyCode::Enter)),
        Some(Command::Submit {
            objective: "plan the repository change".into(),
            mode: TaskMode::Plan,
        })
    );
}

#[test]
fn change_mode_is_gated_when_write_scripts_are_unavailable() {
    let mut app = AppState::default();
    app.change_available = false;
    app.selected_mode = TaskMode::Plan;

    assert_eq!(handle_key(&mut app, key(KeyCode::F(2))), None);
    assert_eq!(app.selected_mode, TaskMode::Investigate);

    app.task_input = "modify the repository".into();
    app.selected_mode = TaskMode::Change;
    assert_eq!(handle_key(&mut app, key(KeyCode::Enter)), None);
    assert_eq!(app.input_mode, InputMode::TaskEntry);
    assert_eq!(app.task_input, "modify the repository");
}

#[test]
fn running_mode_rejects_task_replacement_and_submission() {
    let mut app = AppState::default();
    start(&mut app);

    assert_eq!(handle_key(&mut app, key(KeyCode::Char('x'))), None);
    assert_eq!(handle_key(&mut app, key(KeyCode::Enter)), None);
    assert!(app.task_input.is_empty());
    assert_eq!(app.input_mode, InputMode::Running);
    assert_eq!(app.task_status, TaskStatus::Running);
}

#[test]
fn approval_mode_only_explicit_keys_decide() {
    let mut app = AppState::default();
    app.apply(AgentEvent::ApprovalRequired {
        request: approval("approval-1"),
    });
    assert_eq!(app.input_mode, InputMode::Approval);

    for character in "ok go ahead".chars() {
        assert_eq!(handle_key(&mut app, key(KeyCode::Char(character))), None);
    }
    assert_eq!(app.input_mode, InputMode::Approval);
    assert!(app.approval.is_some());

    assert_eq!(
        handle_key(&mut app, key(KeyCode::Char('y'))),
        Some(Command::ResolveApproval {
            approval_id: "approval-1".into(),
            approved: true,
        })
    );
    assert!(app.approval.is_none());

    app.apply(AgentEvent::ApprovalRequired {
        request: approval("approval-2"),
    });
    assert_eq!(
        handle_key(&mut app, key(KeyCode::Esc)),
        Some(Command::ResolveApproval {
            approval_id: "approval-2".into(),
            approved: false,
        })
    );
    assert!(app.approval.is_none());
}

#[test]
fn stale_approval_resolution_does_not_clear_newer_prompt() {
    let mut app = AppState::default();
    app.apply(AgentEvent::ApprovalRequired {
        request: approval("approval-new"),
    });

    app.apply(AgentEvent::ApprovalResolved {
        approval_id: "approval-old".into(),
        approved: true,
    });

    assert_eq!(app.input_mode, InputMode::Approval);
    assert_eq!(
        app.approval
            .as_ref()
            .map(|prompt| prompt.approval_id.as_str()),
        Some("approval-new")
    );
    assert!(app.transcript.iter().all(|entry| {
        entry.kind != TranscriptKind::Approval || !entry.text.contains("approval granted")
    }));
}

#[test]
fn approval_keys_do_nothing_without_a_pending_request() {
    let mut app = AppState::default();
    app.input_mode = InputMode::Approval;

    assert_eq!(handle_key(&mut app, key(KeyCode::Char('y'))), None);
    assert_eq!(handle_key(&mut app, key(KeyCode::Esc)), None);
    assert!(app.approval.is_none());
}

#[test]
fn ctrl_c_in_approval_mode_cancels_instead_of_approving() {
    let mut app = AppState::default();
    start(&mut app);
    app.apply(AgentEvent::ApprovalRequired {
        request: approval("approval-1"),
    });

    assert_eq!(handle_key(&mut app, ctrl('c')), Some(Command::Cancel));
    assert_eq!(app.task_status, TaskStatus::Cancelling);
    assert!(app.approval.is_some());
}

#[test]
fn model_text_does_not_affect_control_flow() {
    let mut app = AppState::default();
    start(&mut app);
    app.apply(AgentEvent::StreamDelta {
        text: "approved done yes".into(),
    });

    assert_eq!(app.task_status, TaskStatus::Running);
    assert_eq!(app.input_mode, InputMode::Running);
    assert!(app.approval.is_none());
    assert!(!app.should_quit);
}

#[test]
fn reducer_handles_every_agent_event_variant() {
    let events = vec![
        AgentEvent::TaskStarted {
            summary: "task".into(),
        },
        AgentEvent::StreamDelta {
            text: "partial".into(),
        },
        AgentEvent::StreamTruncated { dropped: 2 },
        AgentEvent::ApprovalRequired {
            request: approval("approval-1"),
        },
        AgentEvent::ApprovalResolved {
            approval_id: "approval-1".into(),
            approved: true,
        },
        AgentEvent::Notice {
            level: EventLevel::Warning,
            message: "notice".into(),
        },
        AgentEvent::PolicyDenied {
            summary: "denied".into(),
        },
        AgentEvent::PhaseChanged {
            from: LoopState::Boot,
            to: LoopState::Learn,
        },
        AgentEvent::WikiLoaded {
            summary: "wiki".into(),
        },
        AgentEvent::PlanProduced {
            summary: "plan".into(),
            steps: 2,
        },
        AgentEvent::ToolRequested {
            tool_call_id: "call-1".into(),
            kind: "read_file".into(),
        },
        AgentEvent::ToolFinished {
            tool_call_id: "call-1".into(),
            is_error: false,
            summary: "read ok".into(),
        },
        AgentEvent::VerificationStarted,
        AgentEvent::VerificationFinished {
            passed: true,
            summary: "all tests passed".into(),
        },
        AgentEvent::ReviewVerdict {
            verdict: "accept".into(),
        },
        AgentEvent::ContextCompacted {
            removed_messages: 3,
        },
        AgentEvent::WikiCompiled {
            path: ".ai-wiki/index.md".into(),
        },
        AgentEvent::PlanCompiled {
            path: ".ai-wiki/plans/task.md".into(),
        },
        AgentEvent::Blocked {
            reason: "blocked".into(),
        },
        AgentEvent::Completed {
            summary: "done".into(),
        },
        AgentEvent::Cancelled {
            reason: "cancelled".into(),
        },
        AgentEvent::Failed {
            reason: "failed".into(),
        },
        AgentEvent::WorkerError {
            message: "worker failed".into(),
        },
    ];

    let mut app = AppState::default();
    for event in events {
        app.apply(event);
    }

    assert_eq!(app.task_status, TaskStatus::Failed);
    assert_eq!(app.input_mode, InputMode::Failed);
    assert!(app.worker_closed);
}

#[test]
fn phase_changes_update_phase_and_status() {
    let mut app = AppState::default();
    start(&mut app);
    assert_eq!(app.phase, LoopState::Learn);
    assert_eq!(app.task_status, TaskStatus::Running);
    assert_eq!(app.input_mode, InputMode::Running);

    app.apply(AgentEvent::PhaseChanged {
        from: LoopState::Learn,
        to: LoopState::Blocked,
    });
    assert_eq!(app.phase, LoopState::Blocked);
    assert_eq!(app.task_status, TaskStatus::Blocked);
    assert_eq!(app.input_mode, InputMode::Blocked);
}

#[test]
fn transcript_is_bounded_and_marks_truncation() {
    let mut app = AppState::default();
    for index in 0..(MAX_TRANSCRIPT_ENTRIES + 50) {
        app.apply(AgentEvent::Notice {
            level: EventLevel::Info,
            message: format!("event {index}"),
        });
    }
    assert!(app.transcript.len() <= MAX_TRANSCRIPT_ENTRIES);
    assert!(app.transcript_truncated);
}

#[test]
fn adjacent_stream_deltas_are_coalesced() {
    let mut app = AppState::default();
    start(&mut app);
    app.apply(AgentEvent::StreamDelta { text: "hel".into() });
    app.apply(AgentEvent::StreamDelta { text: "lo".into() });

    let assistant: Vec<_> = app
        .transcript
        .iter()
        .filter(|entry| entry.kind == TranscriptKind::Assistant)
        .collect();
    assert_eq!(assistant.len(), 1);
    assert_eq!(assistant[0].text, "hello");
    assert_eq!(app.stream_state, StreamState::Streaming);
}

#[test]
fn stream_truncation_is_visible() {
    let mut app = AppState::default();
    start(&mut app);
    app.apply(AgentEvent::StreamTruncated { dropped: 3 });

    assert!(app.stream_truncated);
    assert_eq!(app.stream_state, StreamState::Truncated);
    assert!(app.transcript.iter().any(|entry| {
        entry.kind == TranscriptKind::Warning && entry.text.contains("stream output truncated")
    }));
}

#[test]
fn tool_output_is_bounded_and_secrets_are_redacted() {
    let mut app = AppState::default();
    let secret = format!("token=supersecret {}", "x".repeat(5_000));
    app.apply(AgentEvent::ToolFinished {
        tool_call_id: "call-1".into(),
        is_error: false,
        summary: secret,
    });

    let rendered: String = app
        .transcript
        .iter()
        .map(|entry| entry.text.as_str())
        .collect();
    assert!(!rendered.contains("supersecret"));
    assert!(rendered.contains("[REDACTED]"));
    assert!(app.transcript.back().unwrap().text.len() < 3_000);
}

#[test]
fn verification_pass_and_fail_are_distinct() {
    let mut app = AppState::default();
    app.apply(AgentEvent::VerificationFinished {
        passed: true,
        summary: "ok".into(),
    });
    assert!(app.verification.as_ref().unwrap().passed);
    assert!(
        app.transcript
            .iter()
            .any(|entry| entry.kind == TranscriptKind::VerificationPassed)
    );

    app.apply(AgentEvent::VerificationStarted);
    assert!(app.verification.is_none());
    app.apply(AgentEvent::VerificationFinished {
        passed: false,
        summary: "failed".into(),
    });
    assert!(!app.verification.as_ref().unwrap().passed);
    assert!(
        app.transcript
            .iter()
            .any(|entry| entry.kind == TranscriptKind::VerificationFailed)
    );
}

#[test]
fn terminal_states_preserve_final_summary() {
    let cases = [
        (
            AgentEvent::Completed {
                summary: "done".into(),
            },
            TaskStatus::Completed,
            InputMode::Completed,
            LoopState::Completed,
        ),
        (
            AgentEvent::Blocked {
                reason: "blocked".into(),
            },
            TaskStatus::Blocked,
            InputMode::Blocked,
            LoopState::Blocked,
        ),
        (
            AgentEvent::Cancelled {
                reason: "cancelled".into(),
            },
            TaskStatus::Cancelled,
            InputMode::Cancelled,
            LoopState::Cancelled,
        ),
        (
            AgentEvent::Failed {
                reason: "failed".into(),
            },
            TaskStatus::Failed,
            InputMode::Failed,
            LoopState::Failed,
        ),
    ];

    for (event, status, mode, phase) in cases {
        let mut app = AppState::default();
        start(&mut app);
        app.apply(event);
        assert_eq!(app.task_status, status);
        assert_eq!(app.input_mode, mode);
        assert_eq!(app.phase, phase);
        assert!(app.completion_summary.is_some());
    }
}

#[test]
fn worker_errors_are_visible_and_fail_the_task() {
    let mut app = AppState::default();
    start(&mut app);
    app.apply(AgentEvent::WorkerError {
        message: "worker panicked".into(),
    });

    assert_eq!(app.task_status, TaskStatus::Failed);
    assert_eq!(app.input_mode, InputMode::Failed);
    assert!(app.worker_closed);
    assert_eq!(app.completion_summary.as_deref(), Some("worker panicked"));
    assert!(
        app.transcript
            .iter()
            .any(|entry| entry.kind == TranscriptKind::Error)
    );
}

#[test]
fn closed_event_channel_marks_worker_closed() {
    let mut app = AppState::default();
    start(&mut app);
    app.mark_worker_closed();

    assert!(app.worker_closed);
    assert_eq!(app.task_status, TaskStatus::Failed);
    assert_eq!(app.input_mode, InputMode::Failed);
}

#[test]
fn ctrl_c_requests_cancel_then_exits_after_grace() {
    let mut app = AppState::default();
    start(&mut app);

    assert_eq!(handle_key(&mut app, ctrl('c')), Some(Command::Cancel));
    assert_eq!(app.task_status, TaskStatus::Cancelling);
    assert!(!app.should_quit);

    app.cancel_grace_elapsed = true;
    assert_eq!(handle_key(&mut app, ctrl('c')), Some(Command::Quit));
    assert!(app.should_quit);
}

#[test]
fn q_quits_only_outside_task_entry() {
    let mut app = AppState::default();
    assert_eq!(handle_key(&mut app, key(KeyCode::Char('q'))), None);
    assert_eq!(app.task_input, "q");

    let mut terminal_app = AppState::default();
    start(&mut terminal_app);
    terminal_app.apply(AgentEvent::Completed {
        summary: "done".into(),
    });
    assert_eq!(
        handle_key(&mut terminal_app, key(KeyCode::Char('q'))),
        Some(Command::Quit)
    );
    assert!(terminal_app.should_quit);
}

#[test]
fn scroll_keys_update_bounded_offset() {
    let mut app = AppState::default();
    start(&mut app);
    for index in 0..5 {
        app.apply(AgentEvent::Notice {
            level: EventLevel::Info,
            message: format!("line {index}"),
        });
    }

    handle_key(&mut app, key(KeyCode::Up));
    assert_eq!(app.scroll_offset, 1);
    handle_key(&mut app, key(KeyCode::PageUp));
    assert_eq!(app.scroll_offset, app.transcript.len());
    handle_key(&mut app, key(KeyCode::Down));
    assert_eq!(app.scroll_offset, app.transcript.len() - 1);
    handle_key(&mut app, key(KeyCode::PageDown));
    assert_eq!(app.scroll_offset, 0);
}

#[test]
fn paste_is_accepted_only_in_task_entry() {
    let mut app = AppState::default();
    handle_paste(&mut app, "hello");
    assert_eq!(app.task_input, "hello");

    start(&mut app);
    handle_paste(&mut app, "ignored");
    assert!(app.task_input.is_empty());
}

#[test]
fn narrow_terminal_renders_without_panic() {
    let mut app = AppState::default();
    start(&mut app);
    app.apply(AgentEvent::PlanProduced {
        summary: "read files, patch, verify".into(),
        steps: 3,
    });
    app.apply(AgentEvent::ToolFinished {
        tool_call_id: "call-1".into(),
        is_error: false,
        summary: "read src/main.rs".into(),
    });

    let backend = TestBackend::new(20, 8);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| render::draw(frame, &app))
        .expect("narrow layout should render");
}

#[test]
fn wide_terminal_renders_header_and_activity() {
    let mut app = AppState::default();
    app.model = "local-model".into();
    app.endpoint_mode = "air-gapped".into();
    app.budget_summary = "acts 40/tools 120/verify 3/wall 3600s".into();
    start(&mut app);

    let backend = TestBackend::new(120, 40);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| render::draw(frame, &app))
        .expect("wide layout should render");

    let text = buffer_text(&mut terminal);
    assert!(text.contains("Local Agent Harness"));
    assert!(text.contains("Activity"));
    assert!(text.contains("Detail"));
}

#[test]
fn help_overlay_renders() {
    let mut app = AppState::default();
    app.show_help = true;

    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| render::draw(frame, &app))
        .expect("help overlay should render");

    let text = buffer_text(&mut terminal);
    assert!(text.contains("Keys"));
    assert!(text.contains("Ctrl-C"));
}

#[test]
fn task_input_display_is_redacted_and_bounded() {
    let mut app = AppState::default();
    app.task_input = format!("token=supersecret {}", "x".repeat(5_000));

    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| render::draw(frame, &app))
        .expect("task entry should render");

    let text = buffer_text(&mut terminal);
    assert!(!text.contains("supersecret"));
    assert!(text.contains("[REDACTED]"));
}

#[test]
fn tiny_terminal_renders_without_panic() {
    let mut app = AppState::default();
    app.show_help = true;
    let backend = TestBackend::new(1, 1);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| render::draw(frame, &app))
        .expect("tiny layout should render");
}

#[test]
fn worker_error_before_final_summary_overrides_terminal_phase() {
    let mut app = AppState::default();
    start(&mut app);
    app.apply(AgentEvent::PhaseChanged {
        from: LoopState::Compile,
        to: LoopState::Completed,
    });
    assert_eq!(app.task_status, TaskStatus::Completed);
    assert!(app.completion_summary.is_none());

    app.apply(AgentEvent::WorkerError {
        message: "audit write failed".into(),
    });
    assert_eq!(app.task_status, TaskStatus::Failed);
    assert_eq!(app.input_mode, InputMode::Failed);
    assert_eq!(
        app.completion_summary.as_deref(),
        Some("audit write failed")
    );
}
