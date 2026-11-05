use std::time::Duration;

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Tabs, Wrap};

use crate::tui::app::{
    AppState, FocusPane, InputMode, MAX_DISPLAY_TEXT_BYTES, MainTab, StreamState, TaskStatus,
    TranscriptEntry, TranscriptKind, bounded_display_text,
};

/// Render the complete UI from presentation state only. This function performs
/// no network, filesystem, or tool I/O.
pub fn draw(frame: &mut Frame, app: &AppState) {
    let area = frame.area();
    if area.width < 8 || area.height < 5 {
        return;
    }

    if area.width >= 72 && area.height >= 14 {
        draw_wide(frame, area, app);
    } else {
        draw_compact(frame, area, app);
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
            Constraint::Length(1),
            Constraint::Length(3),
            Constraint::Min(3),
            Constraint::Length(3),
            Constraint::Length(1),
        ])
        .split(area);

    draw_header(frame, rows[0], app);
    draw_header_details(frame, rows[1], app);
    draw_tabs(frame, rows[2], app, true);

    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(72), Constraint::Percentage(28)])
        .split(rows[3]);
    draw_content(frame, body[0], app);
    draw_explorer(frame, body[1], app);
    draw_input(frame, rows[4], app);
    draw_footer(frame, rows[5], app);
}

fn draw_compact(frame: &mut Frame, area: Rect, app: &AppState) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Min(2),
            Constraint::Length(3),
            Constraint::Length(1),
        ])
        .split(area);

    draw_header_compact(frame, rows[0], app);
    draw_tabs(frame, rows[1], app, false);

    let explorer_width = area
        .width
        .saturating_div(3)
        .clamp(10, 24)
        .min(area.width.saturating_sub(1));
    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(1), Constraint::Length(explorer_width)])
        .split(rows[2]);
    draw_content(frame, body[0], app);
    draw_explorer(frame, body[1], app);
    draw_input(frame, rows[3], app);
    draw_footer(frame, rows[4], app);
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
    ];
    push_field(
        &mut spans,
        "endpoint",
        bounded_display_text(&app.endpoint_mode, 64),
    );
    push_field(&mut spans, "mode", app.selected_mode.as_str().to_owned());
    push_field(
        &mut spans,
        "tools",
        format!(
            "{} ({})",
            bounded_display_text(&app.tool_indicator, 64),
            app.tool_count
        ),
    );

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

fn draw_header_details(frame: &mut Frame, area: Rect, app: &AppState) {
    let mut spans = Vec::new();
    push_field(
        &mut spans,
        "project",
        bounded_display_text(&app.project, MAX_DISPLAY_TEXT_BYTES),
    );
    push_field(&mut spans, "model", bounded_display_text(&app.model, 128));
    push_field(
        &mut spans,
        "ctx",
        bounded_display_text(&app.context_summary, 64),
    );
    push_field(
        &mut spans,
        "session",
        bounded_display_text(&app.session, 64),
    );
    push_field(&mut spans, "focus", app.focus.label().to_owned());

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_header_compact(frame: &mut Frame, area: Rect, app: &AppState) {
    let status = task_status_text(app);
    let mut spans = vec![
        Span::styled(
            " Local Agent Harness ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" | "),
        Span::styled(
            format!("{:?}", app.phase),
            Style::default()
                .fg(phase_color(app.phase))
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" | "),
        Span::styled(status, Style::default().fg(status_color(app.task_status))),
    ];
    push_field(
        &mut spans,
        "project",
        bounded_display_text(&app.project, 128),
    );
    push_field(&mut spans, "mode", app.selected_mode.as_str().to_owned());
    push_field(
        &mut spans,
        "tools",
        bounded_display_text(&app.tool_indicator, 64),
    );
    push_field(
        &mut spans,
        "ctx",
        bounded_display_text(&app.context_summary, 64),
    );

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn push_field(spans: &mut Vec<Span<'static>>, label: &str, value: String) {
    if !spans.is_empty() {
        spans.push(Span::raw(" | "));
    }
    spans.push(Span::styled(
        format!("{label}: "),
        Style::default().fg(Color::DarkGray),
    ));
    spans.push(Span::raw(value));
}

fn draw_tabs(frame: &mut Frame, area: Rect, app: &AppState, framed: bool) {
    let titles = MainTab::ALL
        .iter()
        .map(|tab| {
            let style = if *tab == app.active_tab {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::Gray)
            };
            Line::from(Span::styled(format!(" {} ", tab.label()), style))
        })
        .collect::<Vec<_>>();
    let highlight = Style::default()
        .fg(Color::Black)
        .bg(Color::Cyan)
        .add_modifier(Modifier::BOLD);
    let selected = app.active_tab.index();
    let tabs = Tabs::new(titles)
        .select(selected)
        .divider(Span::raw("|"))
        .highlight_style(highlight);

    if framed {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(if app.focus == FocusPane::Tabs {
                Style::default().fg(Color::Cyan)
            } else {
                Style::default().fg(Color::DarkGray)
            })
            .title(" Tabs ");
        frame.render_widget(tabs.block(block), area);
    } else {
        frame.render_widget(tabs, area);
    }
}

fn draw_content(frame: &mut Frame, area: Rect, app: &AppState) {
    let focused = app.focus == FocusPane::Content;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(if focused {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default().fg(Color::DarkGray)
        })
        .title(format!(" {} ", app.active_tab.label()));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let lines = content_lines(app);
    let height = inner.height as usize;
    if height == 0 {
        return;
    }
    let total = lines.len();
    let scroll = if app.active_tab == MainTab::Chat {
        app.scroll_offset
    } else {
        app.content_scroll
    };
    let start = total.saturating_sub(height.saturating_add(scroll));
    let end = start.saturating_add(height).min(total);
    let visible = lines[start..end].to_vec();
    frame.render_widget(Paragraph::new(visible).wrap(Wrap { trim: true }), inner);
}

fn content_lines(app: &AppState) -> Vec<Line<'static>> {
    match app.active_tab {
        MainTab::Chat if !app.transcript.is_empty() => transcript_lines(app),
        MainTab::Output => {
            let mut lines = fixture_lines(app.active_tab_lines());
            if !app.detail.is_empty() {
                lines.push(Line::from(""));
                lines.push(Line::from(vec![
                    Span::styled("live: ", Style::default().fg(Color::Cyan)),
                    Span::raw(bounded_display_text(&app.detail, MAX_DISPLAY_TEXT_BYTES)),
                ]));
            }
            lines
        }
        _ => fixture_lines(app.active_tab_lines()),
    }
}

fn fixture_lines(lines: &[String]) -> Vec<Line<'static>> {
    if lines.is_empty() {
        return vec![Line::from(Span::styled(
            "No fixture content.",
            Style::default().fg(Color::DarkGray),
        ))];
    }
    lines
        .iter()
        .map(|line| Line::from(bounded_display_text(line, MAX_DISPLAY_TEXT_BYTES)))
        .collect()
}

fn draw_explorer(frame: &mut Frame, area: Rect, app: &AppState) {
    let focused = app.focus == FocusPane::Explorer;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(if focused {
            Style::default().fg(Color::Cyan)
        } else {
            Style::default().fg(Color::DarkGray)
        })
        .title("Explorer");
    let items = app
        .fixtures
        .explorer
        .iter()
        .map(|line| {
            ListItem::new(Line::from(bounded_display_text(
                line,
                MAX_DISPLAY_TEXT_BYTES,
            )))
        })
        .collect::<Vec<_>>();
    let selected = if items.is_empty() {
        None
    } else {
        Some(app.explorer_selected.min(items.len() - 1))
    };
    let list = List::new(items)
        .block(block)
        .highlight_style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("> ");
    let mut state = ListState::default();
    state.select(selected);
    frame.render_stateful_widget(list, area, &mut state);
}

fn draw_input(frame: &mut Frame, area: Rect, app: &AppState) {
    let title = match app.input_mode {
        InputMode::TaskEntry => " Prompt ",
        InputMode::Running => " Running ",
        InputMode::Approval => " Approval ",
        InputMode::Blocked => " Blocked ",
        InputMode::Completed => " Completed ",
        InputMode::Cancelled => " Cancelled ",
        InputMode::Failed => " Failed ",
    };
    let border = if app.focus == FocusPane::Prompt {
        input_color(app.input_mode)
    } else {
        Color::DarkGray
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border))
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
        format!(
            "Esc/? close help | focus: {} | tab: {} | Ctrl-C cancel | q quit",
            app.focus.label(),
            app.active_tab.label()
        )
    } else if app.focus == FocusPane::Tabs {
        format!(
            "focus: tabs | Left/Right select {} | Tab next | Shift-Tab previous | ? help",
            app.active_tab.label()
        )
    } else if app.focus == FocusPane::Content {
        "focus: content | Up/Down scroll | PgUp/PgDn page | Tab next | ? help".to_owned()
    } else if app.focus == FocusPane::Explorer {
        "focus: explorer | Up/Down select | PgUp/PgDn page | Tab next | ? help".to_owned()
    } else if app.input_mode == InputMode::TaskEntry {
        "focus: prompt | Enter submit | F2 cycle mode | Tab focus | F1 help | Ctrl-C quit"
            .to_owned()
    } else if app.input_mode == InputMode::Approval {
        "y/Enter approve | n/Esc deny | Ctrl-C cancel | Tab focus | ? help".to_owned()
    } else if app.input_mode.is_terminal() {
        "q quit | Ctrl-C quit | Up/Down scroll | Tab focus | ? help".to_owned()
    } else {
        "Ctrl-C cancel | Esc interrupt (unsupported) | Up/Down scroll | Tab focus | ? help | q cancel"
            .to_owned()
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
            "F2           cycle Change/Investigate/Plan mode before submission"
        } else {
            "F2           cycle Investigate/Plan; Change is gated until the verifier script exists"
        }),
        Line::from("Esc          deny approval; close help; interrupt is unsupported"),
        Line::from("Ctrl-C       cancel active task; quit when idle"),
        Line::from("q            quit; requests cancellation first while active"),
        Line::from("Tab          cycle focus: prompt, tabs, content, explorer"),
        Line::from("Shift-Tab    cycle focus in reverse"),
        Line::from("Left/Right   switch tabs when the tab bar has focus"),
        Line::from("Up/Down      scroll content or move the Explorer selection"),
        Line::from("PgUp/PgDn    page the focused content or Explorer"),
        Line::from("? / F1       toggle this help"),
        Line::from(""),
        Line::from("The shell renders bounded fixtures and typed presentation state."),
        Line::from("Drawing never performs network, filesystem, or tool I/O."),
        Line::from("The TUI only observes the orchestrator and sends typed commands."),
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
