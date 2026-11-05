use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use local_agent_harness::approval::{ApprovalAction, ApprovalRequest};
use local_agent_harness::events::{AgentEvent, ChatTurnTelemetry, Command, EventLevel};
use local_agent_harness::session::{SessionMessage, SessionRole};
use local_agent_harness::state_machine::{LoopState, TaskMode};
use local_agent_harness::tui::app::{MAX_TRANSCRIPT_ENTRIES, StreamState};
use local_agent_harness::tui::input::{handle_key, handle_paste};
use local_agent_harness::tui::render;
use local_agent_harness::tui::{
    AppState, FocusPane, InputMode, MainTab, TaskStatus, TranscriptKind, UiCommand,
};
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
    app.active_tab = MainTab::Output;
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
fn chat_tab_enter_submits_a_reusable_chat_turn() {
    let mut app = AppState::default();
    app.task_input = "explain the parser".into();

    assert_eq!(
        handle_key(&mut app, key(KeyCode::Enter)),
        Some(Command::Chat {
            prompt: "explain the parser".into(),
        })
    );
    assert_eq!(app.input_mode, InputMode::Running);
    assert_eq!(app.task_status, TaskStatus::Running);
    assert!(app.chat_active);
    assert!(app.task_input.is_empty());
}

#[test]
fn chat_session_commands_are_typed_without_a_model_turn() {
    let mut app = AppState::default();
    app.task_input = "/new".into();
    assert_eq!(
        handle_key(&mut app, key(KeyCode::Enter)),
        Some(Command::NewSession)
    );
    assert!(app.task_input.is_empty());
    assert!(!app.chat_active);
    assert_eq!(app.input_mode, InputMode::TaskEntry);

    app.task_input = "/resume abc123".into();
    assert_eq!(
        handle_key(&mut app, key(KeyCode::Enter)),
        Some(Command::ResumeSession {
            session_id: Some("abc123".into())
        })
    );
    assert!(app.task_input.is_empty());

    app.task_input = "/resume".into();
    assert_eq!(
        handle_key(&mut app, key(KeyCode::Enter)),
        Some(Command::ResumeSession { session_id: None })
    );

    app.task_input = "/unknown".into();
    assert_eq!(handle_key(&mut app, key(KeyCode::Enter)), None);
    assert!(app.task_input.is_empty());
    assert!(app.transcript.iter().any(|entry| {
        entry.kind == TranscriptKind::Warning && entry.text.contains("unknown slash command")
    }));
}

#[test]
fn restored_session_replaces_visible_chat_history() {
    let mut app = AppState::default();
    app.apply(AgentEvent::TaskStarted {
        summary: "old task".into(),
    });
    app.apply(AgentEvent::SessionLoaded {
        session_id: "abc123".into(),
        messages: vec![
            SessionMessage {
                role: SessionRole::User,
                text: "restored question".into(),
                timestamp: "2026-11-05T12:00:00.000Z".into(),
            },
            SessionMessage {
                role: SessionRole::Assistant,
                text: "restored answer".into(),
                timestamp: "2026-11-05T12:00:01.000Z".into(),
            },
        ],
    });

    assert_eq!(app.session, "abc123");
    assert_eq!(app.task_status, TaskStatus::Idle);
    assert_eq!(app.input_mode, InputMode::TaskEntry);
    assert!(!app.chat_active);
    assert_eq!(app.transcript.len(), 2);
    assert_eq!(app.transcript[0].kind, TranscriptKind::Operator);
    assert_eq!(app.transcript[0].text, "restored question");
    assert_eq!(app.transcript[1].kind, TranscriptKind::Assistant);
    assert_eq!(app.transcript[1].text, "restored answer");
}

#[test]
fn chat_turn_streams_and_returns_to_prompt_without_terminal_state() {
    let mut app = AppState::default();
    app.apply(AgentEvent::ChatTurnStarted {
        prompt: "hello".into(),
    });
    app.apply(AgentEvent::StreamDelta {
        text: "visible ".into(),
    });
    app.apply(AgentEvent::StreamDelta {
        text: "answer".into(),
    });
    app.apply(AgentEvent::ChatTurnFinished {
        finish_reason: "stop".into(),
        empty_visible_response: false,
    });

    assert_eq!(app.task_status, TaskStatus::Idle);
    assert_eq!(app.input_mode, InputMode::TaskEntry);
    assert!(!app.chat_active);
    assert!(
        app.transcript.iter().any(|entry| {
            entry.kind == TranscriptKind::Operator && entry.text.contains("hello")
        })
    );
    assert!(app.transcript.iter().any(|entry| {
        entry.kind == TranscriptKind::Assistant && entry.text == "visible answer"
    }));
    assert_eq!(app.detail, "Chat response complete (finish: stop).");
}

#[test]
fn empty_chat_completion_stores_no_assistant_message() {
    let mut app = AppState::default();
    app.apply(AgentEvent::ChatTurnStarted {
        prompt: "hello".into(),
    });
    app.apply(AgentEvent::StreamDelta { text: "   ".into() });
    app.apply(AgentEvent::ChatTurnFinished {
        finish_reason: "stop".into(),
        empty_visible_response: true,
    });

    assert!(
        !app.transcript
            .iter()
            .any(|entry| entry.kind == TranscriptKind::Assistant)
    );
    assert!(app.transcript.iter().any(|entry| {
        entry.kind == TranscriptKind::EmptyResponse
            && entry.text.contains("no assistant message was stored")
    }));
    assert_eq!(app.input_mode, InputMode::TaskEntry);
}

#[test]
fn chat_failure_is_visible_and_returns_to_prompt() {
    let mut app = AppState::default();
    app.apply(AgentEvent::ChatTurnStarted {
        prompt: "hello".into(),
    });
    app.apply(AgentEvent::ChatTurnFailed {
        message: "llama-server could not connect".into(),
    });

    assert_eq!(app.task_status, TaskStatus::Idle);
    assert_eq!(app.input_mode, InputMode::TaskEntry);
    assert_eq!(
        app.last_error.as_deref(),
        Some("llama-server could not connect")
    );
    assert!(app.transcript.iter().any(|entry| {
        entry.kind == TranscriptKind::Error && entry.text.contains("could not connect")
    }));
}

#[test]
fn chat_telemetry_updates_context_and_header_fields() {
    let mut app = AppState::default();
    app.apply(AgentEvent::ChatTurnStarted {
        prompt: "hello".into(),
    });
    app.apply(AgentEvent::ChatTelemetry {
        telemetry: ChatTurnTelemetry {
            estimated_prompt_tokens: 2_200,
            reserved_output_tokens: 4_096,
            effective_context_cap_tokens: 16_384,
            safety_margin_tokens: 512,
            context_utilization_percent: 13,
            time_to_first_token_ms: Some(1_840),
            visible_output_chars: 50,
            visible_output_tokens: 17,
            finish_reason: Some("stop".into()),
            empty_visible_response: false,
            transport_error_category: None,
            admission_rejected: false,
        },
    });

    assert_eq!(app.context_summary, "13% / 16k");
    let telemetry = app.last_chat_telemetry.as_ref().expect("telemetry stored");
    assert_eq!(telemetry.estimated_prompt_tokens, 2_200);
    assert_eq!(telemetry.time_to_first_token_ms, Some(1_840));
    assert_eq!(telemetry.visible_output_tokens, 17);

    app.apply(AgentEvent::ChatTurnFinished {
        finish_reason: "stop".into(),
        empty_visible_response: false,
    });

    let backend = TestBackend::new(140, 30);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| render::draw(frame, &app))
        .expect("telemetry header should render");
    let text = buffer_text(&mut terminal);
    assert!(text.contains("ctx: 13% / 16k"), "missing context: {text}");
    assert!(text.contains("ttft: 1.8 s"), "missing TTFT: {text}");
    assert!(text.contains("out: 17"), "missing output estimate: {text}");
    assert!(
        text.contains("finish: stop"),
        "missing finish reason: {text}"
    );
}

#[test]
fn chat_telemetry_strings_are_bounded() {
    let mut app = AppState::default();
    app.apply(AgentEvent::ChatTelemetry {
        telemetry: ChatTurnTelemetry {
            finish_reason: Some("f".repeat(1_000)),
            transport_error_category: Some("t".repeat(1_000)),
            ..ChatTurnTelemetry::default()
        },
    });

    let telemetry = app.last_chat_telemetry.as_ref().expect("telemetry stored");
    assert!(telemetry.finish_reason.as_ref().unwrap().len() <= 128 + 14);
    assert!(telemetry.transport_error_category.as_ref().unwrap().len() <= 32 + 14);
}

#[test]
fn esc_cancels_an_active_chat_turn() {
    let mut app = AppState::default();
    app.apply(AgentEvent::ChatTurnStarted {
        prompt: "hello".into(),
    });

    assert_eq!(
        handle_key(&mut app, key(KeyCode::Esc)),
        Some(Command::Cancel)
    );
    assert_eq!(app.task_status, TaskStatus::Cancelling);

    app.apply(AgentEvent::ChatTurnCancelled {
        reason: "operator cancelled the active turn".into(),
    });
    assert_eq!(app.task_status, TaskStatus::Idle);
    assert_eq!(app.input_mode, InputMode::TaskEntry);
    assert!(!app.chat_active);
    assert!(app.transcript.iter().any(|entry| {
        entry.kind == TranscriptKind::Cancelled && entry.text.contains("chat turn cancelled")
    }));
}

#[test]
fn f2_toggles_mode_only_before_submission() {
    let mut app = AppState::default();
    app.active_tab = MainTab::Output;
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
    app.active_tab = MainTab::Output;
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
fn change_mode_is_gated_when_verifier_script_is_unavailable() {
    let mut app = AppState::default();
    app.active_tab = MainTab::Output;
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
        AgentEvent::ChatTurnStarted {
            prompt: "chat".into(),
        },
        AgentEvent::StreamDelta {
            text: "chat response".into(),
        },
        AgentEvent::ChatTurnFinished {
            finish_reason: "stop".into(),
            empty_visible_response: false,
        },
        AgentEvent::ChatTurnCancelled {
            reason: "cancelled".into(),
        },
        AgentEvent::ChatTurnFailed {
            message: "failed".into(),
        },
        AgentEvent::ChatTelemetry {
            telemetry: ChatTurnTelemetry::default(),
        },
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
fn wide_terminal_renders_header_tabs_and_explorer() {
    let mut app = AppState::default();
    app.project = "/srv/harness/workspace".into();
    app.model = "local-model".into();
    app.endpoint_mode = "local".into();
    app.context_summary = "57% / 16k".into();
    app.session = "12ab".into();
    app.budget_summary = "acts 40/tools 120/verify 3/wall 3600s".into();
    start(&mut app);

    let backend = TestBackend::new(120, 40);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| render::draw(frame, &app))
        .expect("wide layout should render");

    let text = buffer_text(&mut terminal);
    assert!(text.contains("Local Agent Harness"));
    assert!(text.contains("project:"));
    assert!(text.contains("/srv/harness/workspace"));
    assert!(text.contains("model:"));
    assert!(text.contains("local-model"));
    assert!(text.contains("ctx:"));
    assert!(text.contains("session:"));
    assert!(text.contains("endpoint:"));
    assert!(text.contains("tools:"));
    for label in ["Chat", "Context", "Diff", "Output", "Session"] {
        assert!(text.contains(label), "missing tab {label}: {text}");
    }
    assert!(text.contains("Explorer"));
    assert!(text.contains("Read-only placeholder"));
}

#[test]
fn all_tabs_render_static_fixture_content() {
    for (tab, needle) in [
        (MainTab::Chat, "Chat transcript (fixture)"),
        (MainTab::Context, "Global context (fixture)"),
        (MainTab::Diff, "No session changes yet."),
        (MainTab::Output, "Diagnostics (fixture)"),
        (MainTab::Session, "Session metadata (fixture)"),
    ] {
        let mut app = AppState::default();
        app.active_tab = tab;

        let backend = TestBackend::new(120, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render::draw(frame, &app))
            .expect("fixture tab should render");

        let text = buffer_text(&mut terminal);
        assert!(
            text.contains(needle),
            "tab {tab:?} did not render {needle:?}: {text}"
        );
    }
}

#[test]
fn explorer_is_persistent_across_responsive_layouts() {
    for (width, height) in [(120, 40), (80, 24), (40, 12), (20, 8)] {
        let app = AppState::default();
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render::draw(frame, &app))
            .expect("responsive layout should render");

        let text = buffer_text(&mut terminal);
        assert!(
            text.contains("Explorer"),
            "Explorer missing at {width}x{height}: {text}"
        );
    }
}

#[test]
fn focus_cycles_through_typed_ui_commands_and_tab_key() {
    let mut app = AppState::default();
    assert_eq!(app.focus, FocusPane::Prompt);

    app.apply_ui_command(UiCommand::FocusNext);
    assert_eq!(app.focus, FocusPane::Tabs);
    app.apply_ui_command(UiCommand::FocusNext);
    assert_eq!(app.focus, FocusPane::Content);
    app.apply_ui_command(UiCommand::FocusNext);
    assert_eq!(app.focus, FocusPane::Explorer);
    app.apply_ui_command(UiCommand::FocusNext);
    assert_eq!(app.focus, FocusPane::Prompt);

    handle_key(&mut app, key(KeyCode::Tab));
    assert_eq!(app.focus, FocusPane::Tabs);
    handle_key(&mut app, key(KeyCode::BackTab));
    assert_eq!(app.focus, FocusPane::Prompt);
}

#[test]
fn focused_tabs_switch_with_arrow_keys_and_typed_commands() {
    let mut app = AppState::default();
    handle_key(&mut app, key(KeyCode::Tab));
    assert_eq!(app.focus, FocusPane::Tabs);
    assert_eq!(app.active_tab, MainTab::Chat);

    handle_key(&mut app, key(KeyCode::Right));
    assert_eq!(app.active_tab, MainTab::Context);
    handle_key(&mut app, key(KeyCode::Left));
    assert_eq!(app.active_tab, MainTab::Chat);

    app.apply_ui_command(UiCommand::NextTab);
    assert_eq!(app.active_tab, MainTab::Context);
    app.apply_ui_command(UiCommand::PreviousTab);
    assert_eq!(app.active_tab, MainTab::Chat);
    app.apply_ui_command(UiCommand::SelectTab(MainTab::Output));
    assert_eq!(app.active_tab, MainTab::Output);
    assert_eq!(app.focus, FocusPane::Tabs);
}

#[test]
fn explorer_focus_moves_selection_within_bounds() {
    let mut app = AppState::default();
    app.focus = FocusPane::Explorer;
    assert_eq!(app.explorer_selected, 0);

    handle_key(&mut app, key(KeyCode::Down));
    assert_eq!(app.explorer_selected, 1);
    handle_key(&mut app, key(KeyCode::Up));
    assert_eq!(app.explorer_selected, 0);
    handle_key(&mut app, key(KeyCode::Up));
    assert_eq!(app.explorer_selected, 0);
    handle_key(&mut app, key(KeyCode::PageDown));
    assert_eq!(
        app.explorer_selected,
        app.fixtures.explorer.len().saturating_sub(1)
    );
}

#[test]
fn non_prompt_focus_does_not_type_or_submit() {
    let mut app = AppState::default();
    handle_key(&mut app, key(KeyCode::Tab));
    assert_eq!(app.focus, FocusPane::Tabs);

    handle_key(&mut app, key(KeyCode::Char('x')));
    handle_key(&mut app, key(KeyCode::Enter));
    assert!(app.task_input.is_empty());
    assert_eq!(app.input_mode, InputMode::TaskEntry);

    handle_paste(&mut app, "ignored");
    assert!(app.task_input.is_empty());
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
