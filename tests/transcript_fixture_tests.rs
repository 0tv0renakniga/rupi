use local_agent_harness::events::{AgentEvent, EventLevel};
use local_agent_harness::tui::render;
use local_agent_harness::tui::{
    AppState, StreamState, TranscriptFixture, TranscriptFixtureCase, TranscriptKind,
};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

fn buffer_lines(terminal: &mut Terminal<TestBackend>) -> Vec<String> {
    let width = terminal.backend().buffer().area.width as usize;
    terminal
        .backend_mut()
        .buffer()
        .content()
        .chunks(width)
        .map(|row| row.iter().map(|cell| cell.symbol()).collect())
        .collect()
}

fn render_fixture(case: TranscriptFixtureCase, width: u16, height: u16) -> Vec<String> {
    let mut app = AppState::default();
    app.load_transcript_fixture(&TranscriptFixture::case(case));

    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| render::draw(frame, &app))
        .expect("fixture should render");
    buffer_lines(&mut terminal)
}

fn row_containing(lines: &[String], needle: &str) -> Option<usize> {
    lines.iter().position(|line| line.contains(needle))
}

fn content_segment(line: &str) -> &str {
    let Some(start) = line.find('│').map(|index| index + '│'.len_utf8()) else {
        return line;
    };
    let end = line[start..]
        .find('│')
        .map(|index| start + index)
        .unwrap_or(line.len());
    &line[start..end]
}

#[test]
fn normal_text_fixture_renders() {
    let lines = render_fixture(TranscriptFixtureCase::NormalText, 120, 30);
    let text = lines.join("\n");

    assert!(text.contains("INFO"), "{text}");
    assert!(text.contains("status: idle"), "{text}");
    assert!(text.contains("ASSIST"), "{text}");
    assert!(text.contains("Normal assistant text."), "{text}");
}

#[test]
fn multiline_markdown_fixture_renders_every_line() {
    let lines = render_fixture(TranscriptFixtureCase::MultilineMarkdown, 120, 30);
    let text = lines.join("\n");

    for expected in ["## Summary", "- first item", "- second item", "**Done.**"] {
        assert!(text.contains(expected), "missing {expected:?}: {text}");
    }
}

#[test]
fn code_block_fixture_renders_without_losing_indentation_or_lines() {
    let lines = render_fixture(TranscriptFixtureCase::CodeBlock, 120, 30);
    let text = lines.join("\n");

    for expected in ["```rust", "fn main() {", "    println!(\"hello\");", "```"] {
        assert!(text.contains(expected), "missing {expected:?}: {text}");
    }
}

#[test]
fn unicode_fixture_renders_without_corruption() {
    let lines = render_fixture(TranscriptFixtureCase::Unicode, 120, 30);
    let text = lines.join("\n");

    for expected in ["café", "naïve", "résumé"] {
        assert!(text.contains(expected), "missing {expected:?}: {text}");
    }
}

#[test]
fn blank_lines_fixture_preserves_separation() {
    let lines = render_fixture(TranscriptFixtureCase::BlankLines, 120, 30);
    let before = row_containing(&lines, "Before.").expect("before line");
    let after = row_containing(&lines, "After.").expect("after line");

    assert_eq!(after, before + 2, "{lines:#?}");
    assert!(
        content_segment(&lines[before + 1]).trim().is_empty(),
        "{lines:#?}"
    );
}

#[test]
fn long_unbroken_line_fixture_wraps_stably() {
    let first = render_fixture(TranscriptFixtureCase::LongUnbrokenLine, 120, 30);
    let second = render_fixture(TranscriptFixtureCase::LongUnbrokenLine, 120, 30);
    let text = first.join("\n");

    assert_eq!(first, second);
    assert!(text.contains("start-"), "{text}");
    assert!(text.contains("after long line"), "{text}");
    assert!(
        first.iter().filter(|line| line.contains("xxxx")).count() >= 3,
        "long line did not wrap: {first:#?}"
    );
}

#[test]
fn overlong_unbroken_fixture_is_bounded_and_marks_truncation() {
    let fixture = TranscriptFixture::new([(TranscriptKind::Assistant, "y".repeat(5_000))]);
    assert!(fixture.entries[0].text.len() < 5_000);
    assert!(fixture.entries[0].text.contains("...[truncated]"));

    let mut app = AppState::default();
    app.load_transcript_fixture(&fixture);
    let backend = TestBackend::new(120, 100);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| render::draw(frame, &app))
        .expect("bounded fixture should render");
    let text = buffer_lines(&mut terminal).join("\n");
    assert!(text.contains("...[truncated]"), "{text}");
}

#[test]
fn streaming_chunk_fixture_renders_accumulated_text() {
    let lines = render_fixture(TranscriptFixtureCase::StreamingChunks, 120, 30);
    let text = lines.join("\n");

    assert!(text.contains("Hello world"), "{text}");
    assert!(text.contains("second line"), "{text}");
}

#[test]
fn streaming_chunks_render_incrementally_without_corrupting_previous_text() {
    let mut app = AppState::default();
    app.apply(AgentEvent::TaskStarted {
        summary: "stream fixture".into(),
    });

    let chunks = ["Hel", "lo", " world", "\nsecond line"];
    let mut expected = String::new();
    for chunk in chunks {
        expected.push_str(chunk);
        app.apply(AgentEvent::StreamDelta { text: chunk.into() });

        let backend = TestBackend::new(120, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|frame| render::draw(frame, &app))
            .expect("streaming fixture should render");
        let lines = buffer_lines(&mut terminal);
        let text = lines.join("\n");

        for fragment in expected.split('\n') {
            assert!(
                text.contains(fragment),
                "missing accumulated fragment {fragment:?} after {chunk:?}: {text}"
            );
        }
    }

    let assistant: Vec<_> = app
        .transcript
        .iter()
        .filter(|entry| entry.kind == TranscriptKind::Assistant)
        .collect();
    assert_eq!(assistant.len(), 1);
    assert_eq!(assistant[0].text, "Hello world\nsecond line");
    assert_eq!(app.stream_state, StreamState::Streaming);
}

#[test]
fn error_and_status_messages_have_distinct_render_paths() {
    let lines = render_fixture(TranscriptFixtureCase::ErrorStatus, 120, 30);
    let text = lines.join("\n");

    assert!(text.contains("INFO"), "{text}");
    assert!(text.contains("status: connected"), "{text}");
    assert!(text.contains("ERROR"), "{text}");
    assert!(
        text.contains("endpoint unavailable: connection refused"),
        "{text}"
    );
    assert!(
        row_containing(&lines, "status: connected")
            < row_containing(&lines, "endpoint unavailable: connection refused"),
        "{lines:#?}"
    );
}

#[test]
fn empty_response_fixture_has_a_distinct_render_path() {
    let lines = render_fixture(TranscriptFixtureCase::EmptyResponse, 120, 30);
    let text = lines.join("\n");

    assert!(text.contains("EMPTY"), "{text}");
    assert!(text.contains("visible output = 0"), "{text}");
    assert!(!text.contains("ASSIST"), "{text}");

    let mut app = AppState::default();
    app.apply(AgentEvent::TaskStarted {
        summary: "empty fixture".into(),
    });
    app.apply(AgentEvent::StreamDelta {
        text: "\n\n".into(),
    });
    app.apply(AgentEvent::Notice {
        level: EventLevel::Warning,
        message: "   ".into(),
    });
    assert!(
        app.transcript
            .iter()
            .all(|entry| entry.kind != TranscriptKind::Assistant)
    );
}

#[test]
fn truncation_fixture_renders_both_indicators() {
    let lines = render_fixture(TranscriptFixtureCase::TruncationIndicator, 120, 30);
    let text = lines.join("\n");

    assert!(text.contains("transcript truncated"), "{text}");
    assert!(text.contains("stream output truncated"), "{text}");
    assert!(text.contains("TRUNCATED"), "{text}");
}

#[test]
fn provider_frames_render_as_opaque_normalized_text() {
    let fixture = TranscriptFixture::new([(
        TranscriptKind::Assistant,
        "data: {\"choices\":[{\"delta\":{\"content\":\"visible\"}}]}",
    )]);
    let mut app = AppState::default();
    app.load_transcript_fixture(&fixture);

    let backend = TestBackend::new(120, 30);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|frame| render::draw(frame, &app))
        .expect("opaque fixture should render");
    let text = buffer_lines(&mut terminal).join("\n");

    assert!(text.contains("data:"), "{text}");
    assert!(text.contains("choices"), "{text}");
    assert!(text.contains("visible"), "{text}");
}

#[test]
fn every_fixture_case_has_content() {
    assert_eq!(TranscriptFixtureCase::ALL.len(), 10);
    for case in TranscriptFixtureCase::ALL {
        let fixture = TranscriptFixture::case(case);
        assert!(
            !fixture.entries.is_empty(),
            "{} has no entries",
            case.label()
        );
    }
}
