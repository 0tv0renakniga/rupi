use std::time::Duration;

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};

use crate::tui::app::{
    AppState, DetailPane, InputMode, StreamState, TaskStatus, TranscriptEntry, TranscriptKind,
};

/// Render the complete UI from presentation state only. This function performs
/// no network, filesystem, or tool I/O.
pub fn draw(frame: &mut Frame, app: &AppState) {
    let area = frame.area();
    if area.width == 0 || area.height == 0 {
        return;
    }

    if area.width < 72 || area.height < 14 {
        draw_compact(frame, area, app);
    } else {
        draw_wide(frame, area, app);
    }

    if app.show_help {
        draw_help(frame, area, app);
    }
}

fn draw_wide(frame: &mut Frame, area: Rect, app: &AppState) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(3),
            Constraint::Length(3),
            Constraint::Length(1),
        ])
        .split(area);

    draw_header(frame, rows[0], app);

    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(62), Constraint::Percentage(38)])
        .split(rows[1]);
    draw_transcript(frame, body[0], app);
    draw_detail(frame, body[1], app);
    draw_input(frame, rows[2], app);
    draw_footer(frame, rows[3], app);
}

fn draw_compact(frame: &mut Frame, area: Rect, app: &AppState) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(2),
            Constraint::Length(1),
        ])
        .split(area);

    draw_header(frame, rows[0], app);
    draw_transcript(frame, rows[1], app);
    draw_input(frame, rows[2], app);
    draw_footer(frame, rows[3], app);
}

fn draw_header(frame: &mut Frame, area: Rect, app: &AppState) {
    let status = task_status_text(app);
    let elapsed = format_elapsed(app.elapsed);
    let mut spans = vec![
        Span::styled(
            " Local Agent Harness ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("| "),
        Span::styled(
            format!("{:?}", app.phase),
            Style::default()
                .fg(phase_color(app.phase))
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" | "),
        Span::styled(status, Style::default().fg(status_color(app.task_status))),
        Span::raw(" | "),
        Span::raw(elapsed),
        Span::raw(" | mode "),
        Span::raw(app.selected_mode.as_str()),
        Span::raw(" | tools "),
        Span::raw(app.tool_count.to_string()),
    ];

    if !app.model.is_empty() {
        spans.push(Span::raw(" | model "));
        spans.push(Span::raw(crate::tui::app::bounded_display_text(
            &app.model, 128,
        )));
    }
    if !app.endpoint_mode.is_empty() {
        spans.push(Span::raw(" | "));
        spans.push(Span::raw(app.endpoint_mode.clone()));
    }
    if !app.budget_summary.is_empty() {
        spans.push(Span::raw(" | "));
        spans.push(Span::raw(app.budget_summary.clone()));
    }
    if app.stream_truncated || app.transcript_truncated {
        spans.push(Span::styled(
            " | TRUNCATED",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
    }
    if app.worker_closed {
        spans.push(Span::styled(
            " | worker closed",
            Style::default().fg(Color::Red),
        ));
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_transcript(frame: &mut Frame, area: Rect, app: &AppState) {
    let focused = app.selected_detail == DetailPane::Transcript;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(if focused {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default().fg(Color::DarkGray)
        })
        .title(" Activity ");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let lines = transcript_lines(app);
    let height = inner.height as usize;
    if height == 0 {
        return;
    }
    let total = lines.len();
    let start = total.saturating_sub(height.saturating_add(app.scroll_offset));
    let end = start.saturating_add(height).min(total);
    let visible = lines[start..end].to_vec();
    frame.render_widget(Paragraph::new(visible).wrap(Wrap { trim: true }), inner);
}

fn draw_detail(frame: &mut Frame, area: Rect, app: &AppState) {
    let focused = app.selected_detail == DetailPane::Detail;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(if focused {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default().fg(Color::DarkGray)
        })
        .title(" Detail ");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let text = if app.detail.is_empty() {
        "Detail appears here. Tab switches focus.".to_owned()
    } else {
        app.detail.clone()
    };
    let style = if app.last_error.is_some() && app.task_status == TaskStatus::Failed {
        Style::default().fg(Color::Red)
    } else {
        Style::default().fg(Color::Gray)
    };
    frame.render_widget(
        Paragraph::new(text).style(style).wrap(Wrap { trim: true }),
        inner,
    );
}

fn draw_input(frame: &mut Frame, area: Rect, app: &AppState) {
    let title = match app.input_mode {
        InputMode::TaskEntry => " Task ",
        InputMode::Running => " Running ",
        InputMode::Approval => " Approval ",
        InputMode::Blocked => " Blocked ",
        InputMode::Completed => " Completed ",
        InputMode::Cancelled => " Cancelled ",
        InputMode::Failed => " Failed ",
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(input_color(app.input_mode)))
        .title(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let (text, style) = input_text(app);
    frame.render_widget(
        Paragraph::new(text).style(style).wrap(Wrap { trim: true }),
        inner,
    );
}

fn draw_footer(frame: &mut Frame, area: Rect, app: &AppState) {
    let text = if app.show_help {
        "Esc/? close help | Ctrl-C cancel | q quit"
    } else if app.input_mode == InputMode::TaskEntry {
        "Enter submit | F2 mode | F1 help | Ctrl-C quit | Up/Down scroll | Tab pane"
    } else if app.input_mode == InputMode::Approval {
        "y/Enter approve | n/Esc deny | Ctrl-C cancel | ? help"
    } else if app.input_mode.is_terminal() {
        "q quit | Ctrl-C quit | Up/Down scroll | Tab pane | ? help"
    } else {
        "Ctrl-C cancel | Esc interrupt (unsupported) | Up/Down scroll | Tab pane | ? help | q cancel"
    };
    frame.render_widget(
        Paragraph::new(text).style(Style::default().fg(Color::DarkGray)),
        area,
    );
}

fn draw_help(frame: &mut Frame, area: Rect, app: &AppState) {
    let popup = centered_rect(area, 74, 72);
    frame.render_widget(Clear, popup);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(" Keys ");
    let text = vec![
        Line::from("Enter        submit task; approve in approval mode"),
        Line::from(if app.change_available {
            "F2           toggle Change/Investigate mode before submission"
        } else {
            "F2           Investigate selected; Change is gated until write scripts exist"
        }),
        Line::from("Esc          deny approval; close help; interrupt is unsupported"),
        Line::from("Ctrl-C       cancel active task; quit when idle"),
        Line::from("q            quit; requests cancellation first while active"),
        Line::from("Up/Down      scroll activity one line"),
        Line::from("PgUp/PgDn    scroll activity ten lines"),
        Line::from("Tab          switch focused pane"),
        Line::from("? / F1       toggle this help"),
        Line::from(""),
        Line::from("The TUI only observes the orchestrator and sends typed commands."),
        Line::from("It never executes tools, calls endpoints, or marks a task complete."),
    ];
    frame.render_widget(
        Paragraph::new(text)
            .block(block)
            .style(Style::default().fg(Color::Gray))
            .wrap(Wrap { trim: true }),
        popup,
    );
}

fn transcript_lines(app: &AppState) -> Vec<Line<'static>> {
    if app.transcript.is_empty() {
        return vec![Line::from(Span::styled(
            "No activity yet.",
            Style::default().fg(Color::DarkGray),
        ))];
    }

    app.transcript
        .iter()
        .map(|entry| transcript_line(entry))
        .collect()
}

fn transcript_line(entry: &TranscriptEntry) -> Line<'static> {
    let style = transcript_style(entry.kind);
    Line::from(vec![
        Span::styled(
            format!("{} ", entry.timestamp),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(
            format!("{:<9}", entry.kind.label()),
            style.add_modifier(Modifier::BOLD),
        ),
        Span::styled(entry.text.clone(), style),
    ])
}

fn transcript_style(kind: TranscriptKind) -> Style {
    match kind {
        TranscriptKind::Task => Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
        TranscriptKind::Info => Style::default().fg(Color::Gray),
        TranscriptKind::Assistant => Style::default().fg(Color::White),
        TranscriptKind::Phase => Style::default().fg(Color::Cyan),
        TranscriptKind::Plan => Style::default().fg(Color::Blue),
        TranscriptKind::Tool => Style::default().fg(Color::Yellow),
        TranscriptKind::ToolError => Style::default().fg(Color::Red),
        TranscriptKind::PolicyDenied => {
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
        }
        TranscriptKind::VerificationPassed => Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD),
        TranscriptKind::VerificationFailed => {
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
        }
        TranscriptKind::Review => Style::default().fg(Color::Magenta),
        TranscriptKind::Context => Style::default().fg(Color::DarkGray),
        TranscriptKind::Wiki => Style::default().fg(Color::Blue),
        TranscriptKind::Evidence => Style::default()
            .fg(Color::LightBlue)
            .add_modifier(Modifier::BOLD),
        TranscriptKind::Warning => Style::default().fg(Color::Yellow),
        TranscriptKind::Error => Style::default().fg(Color::Red),
        TranscriptKind::Approval => Style::default().fg(Color::Magenta),
        TranscriptKind::Completed => Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD),
        TranscriptKind::Blocked => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        TranscriptKind::Cancelled => Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
        TranscriptKind::Failed => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
    }
}

fn input_text(app: &AppState) -> (String, Style) {
    match app.input_mode {
        InputMode::TaskEntry => {
            if app.task_input.is_empty() {
                (
                    format!(
                        "> [{}] Describe a task and press Enter",
                        app.selected_mode.as_str()
                    ),
                    Style::default().fg(Color::DarkGray),
                )
            } else {
                (
                    format!(
                        "> [{}] {}",
                        app.selected_mode.as_str(),
                        crate::tui::app::bounded_display_text(
                            &app.task_input,
                            crate::tui::app::MAX_DISPLAY_TEXT_BYTES
                        )
                    ),
                    Style::default().fg(Color::White),
                )
            }
        }
        InputMode::Running => {
            let spinner = ["|", "/", "-", "\\"][app.spinner_tick % 4];
            let suffix = if app.stream_state == StreamState::Streaming {
                " receiving model stream"
            } else if app.stream_state == StreamState::Truncated {
                " stream display truncated"
            } else {
                ""
            };
            (
                format!(
                    "{spinner} Task running{suffix}. Ctrl-C cancels; q also requests cancellation."
                ),
                Style::default().fg(Color::Cyan),
            )
        }
        InputMode::Approval => {
            let (id, summary) = app
                .approval
                .as_ref()
                .map(|approval| {
                    let id = approval.approval_id.chars().take(8).collect::<String>();
                    (id, approval.summary.as_str())
                })
                .unwrap_or_else(|| ("?".into(), "approval requested"));
            (
                format!("Approval required [{id}]: {summary}  [y/Enter] approve  [n/Esc] deny"),
                Style::default().fg(Color::Magenta),
            )
        }
        InputMode::Blocked => (
            format!(
                "Blocked: {}. Enter acknowledges; q quits. The task was not resumed.",
                app.completion_summary
                    .as_deref()
                    .unwrap_or("no reason supplied")
            ),
            Style::default().fg(Color::Red),
        ),
        InputMode::Completed => (
            format!(
                "Completed: {}. Press q to quit.",
                app.completion_summary.as_deref().unwrap_or("no summary")
            ),
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        ),
        InputMode::Cancelled => (
            format!(
                "Cancelled: {}. Press q to quit.",
                app.completion_summary
                    .as_deref()
                    .unwrap_or("task cancelled")
            ),
            Style::default().fg(Color::Yellow),
        ),
        InputMode::Failed => (
            format!(
                "Failed: {}. Press q to quit.",
                app.completion_summary
                    .as_deref()
                    .or(app.last_error.as_deref())
                    .unwrap_or("task failed")
            ),
            Style::default().fg(Color::Red),
        ),
    }
}

fn task_status_text(app: &AppState) -> &'static str {
    match app.task_status {
        TaskStatus::Idle => "idle",
        TaskStatus::Running => "running",
        TaskStatus::Cancelling => "cancelling",
        TaskStatus::Blocked => "blocked",
        TaskStatus::Completed => "completed",
        TaskStatus::Cancelled => "cancelled",
        TaskStatus::Failed => "failed",
    }
}

fn status_color(status: TaskStatus) -> Color {
    match status {
        TaskStatus::Idle => Color::DarkGray,
        TaskStatus::Running | TaskStatus::Cancelling => Color::Cyan,
        TaskStatus::Blocked | TaskStatus::Failed => Color::Red,
        TaskStatus::Completed => Color::Green,
        TaskStatus::Cancelled => Color::Yellow,
    }
}

fn phase_color(phase: crate::state_machine::LoopState) -> Color {
    use crate::state_machine::LoopState;
    match phase {
        LoopState::Completed => Color::Green,
        LoopState::Blocked | LoopState::Failed => Color::Red,
        LoopState::Cancelled => Color::Yellow,
        LoopState::Boot => Color::DarkGray,
        _ => Color::Cyan,
    }
}

fn input_color(mode: InputMode) -> Color {
    match mode {
        InputMode::TaskEntry => Color::Cyan,
        InputMode::Running => Color::Blue,
        InputMode::Approval => Color::Magenta,
        InputMode::Blocked | InputMode::Failed => Color::Red,
        InputMode::Completed => Color::Green,
        InputMode::Cancelled => Color::Yellow,
    }
}

fn format_elapsed(elapsed: Duration) -> String {
    let total = elapsed.as_secs();
    let hours = total / 3_600;
    let minutes = (total % 3_600) / 60;
    let seconds = total % 60;
    if hours > 0 {
        format!("{hours:02}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes:02}:{seconds:02}")
    }
}

fn centered_rect(area: Rect, percent_x: u16, percent_y: u16) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);
    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1]);
    horizontal[1]
}
