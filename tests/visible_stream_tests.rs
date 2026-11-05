use local_agent_harness::llm::{
    LlmLimits, VisibleChatEvent, VisibleStreamNormalizer, normalize_sse, normalize_sse_bytes,
};

fn data(json: &str) -> String {
    format!("data: {json}\n\n")
}

fn done() -> String {
    "data: [DONE]\n\n".to_owned()
}

fn finish_stop() -> String {
    data(r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#)
}

fn finished(reason: &str, empty: bool) -> VisibleChatEvent {
    VisibleChatEvent::Finish {
        reason: reason.into(),
        empty_visible_response: empty,
    }
}

#[test]
fn normalizes_split_utf8_and_split_sse_lines() {
    let body = [
        data(r#"{"choices":[{"delta":{"content":"héllo"}}]}"#),
        data(r#"{"choices":[{"delta":{"content":" world"}}]}"#),
        finish_stop(),
        done(),
    ]
    .concat();

    let mut normalizer = VisibleStreamNormalizer::default();
    let mut events = Vec::new();
    for byte in body.as_bytes() {
        events.extend(normalizer.push_chunk(&[*byte]));
    }
    events.extend(normalizer.finish());

    assert_eq!(
        events,
        vec![
            VisibleChatEvent::TextDelta("héllo".into()),
            VisibleChatEvent::TextDelta(" world".into()),
            finished("stop", false),
        ]
    );
    assert!(normalizer.is_done());
    assert!(!normalizer.is_failed());
    assert_eq!(normalizer.finish_reason(), Some("stop"));
}

#[test]
fn accepts_crlf_and_done_without_a_trailing_newline() {
    let body = [
        "data: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\r\n\r\n",
        "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\r\n\r\n",
        "data: [DONE]",
    ]
    .concat();

    let events = normalize_sse(body.as_bytes());

    assert_eq!(
        events,
        vec![
            VisibleChatEvent::TextDelta("ok".into()),
            finished("stop", false)
        ]
    );
}

#[test]
fn malformed_frames_are_protocol_errors() {
    let events = normalize_sse(b"data: {not json}\n\n");

    assert_eq!(
        events,
        vec![VisibleChatEvent::Error("malformed SSE JSON".into())]
    );
}

#[test]
fn malformed_utf8_lines_are_protocol_errors() {
    let events = normalize_sse(b"data: \xff\n\n");

    assert_eq!(
        events,
        vec![VisibleChatEvent::Error(
            "SSE line was not valid UTF-8".into()
        )]
    );
}

#[test]
fn hidden_reasoning_is_never_emitted_and_reasoning_only_is_empty() {
    let body = [
        data(
            r#"{"choices":[{"delta":{"reasoning_content":"secret-a","reasoning":"secret-b","thinking":"secret-c","analysis":"secret-d"}}]}"#,
        ),
        finish_stop(),
        done(),
    ]
    .concat();

    let mut normalizer = VisibleStreamNormalizer::default();
    let mut events = normalizer.push_chunk(body.as_bytes());
    events.extend(normalizer.finish());

    assert_eq!(
        events,
        vec![
            VisibleChatEvent::Warning("empty visible response".into()),
            finished("stop", true),
        ]
    );
    assert!(normalizer.hidden_reasoning_observed());
    assert!(
        !events
            .iter()
            .any(|event| format!("{event:?}").contains("secret-"))
    );
}

#[test]
fn hidden_reasoning_does_not_hide_visible_content() {
    let body = [
        data(
            r#"{"choices":[{"delta":{"reasoning_content":"hidden","content":"answer","thinking":"also hidden"}}]}"#,
        ),
        finish_stop(),
        done(),
    ]
    .concat();

    let mut normalizer = VisibleStreamNormalizer::default();
    let mut events = normalizer.push_chunk(body.as_bytes());
    events.extend(normalizer.finish());

    assert_eq!(
        events,
        vec![
            VisibleChatEvent::TextDelta("answer".into()),
            finished("stop", false),
        ]
    );
    assert!(normalizer.hidden_reasoning_observed());
}

#[test]
fn empty_and_whitespace_visible_completions_are_warnings() {
    let no_text = [finish_stop(), done()].concat();
    assert_eq!(
        normalize_sse(no_text.as_bytes()),
        vec![
            VisibleChatEvent::Warning("empty visible response".into()),
            finished("stop", true),
        ]
    );

    let whitespace = [
        data(r#"{"choices":[{"delta":{"content":"   \n\t"}}]}"#),
        finish_stop(),
        done(),
    ]
    .concat();
    assert_eq!(
        normalize_sse(whitespace.as_bytes()),
        vec![
            VisibleChatEvent::TextDelta("   \n\t".into()),
            VisibleChatEvent::Warning("empty visible response".into()),
            finished("stop", true),
        ]
    );
}

#[test]
fn early_disconnect_is_classified_as_an_error() {
    let body = [
        data(r#"{"choices":[{"delta":{"content":"partial"}}]}"#),
        finish_stop(),
    ]
    .concat();

    let mut normalizer = VisibleStreamNormalizer::default();
    let mut events = normalizer.push_chunk(body.as_bytes());
    events.extend(normalizer.finish());

    assert_eq!(
        events,
        vec![
            VisibleChatEvent::TextDelta("partial".into()),
            VisibleChatEvent::Error("stream ended before [DONE]".into()),
        ]
    );
    assert!(normalizer.is_failed());
}

#[test]
fn enforces_sse_line_and_stream_bounds() {
    let line_limits = LlmLimits {
        max_sse_line_bytes: 4,
        ..LlmLimits::default()
    };
    assert_eq!(
        normalize_sse_bytes(b"data: {}\n\n", &line_limits),
        vec![VisibleChatEvent::Error(
            "SSE line exceeded the size limit".into()
        )]
    );

    let stream_limits = LlmLimits {
        max_stream_bytes: 8,
        ..LlmLimits::default()
    };
    assert_eq!(
        normalize_sse_bytes(b"data: {}\n\n", &stream_limits),
        vec![VisibleChatEvent::Error(
            "streamed response exceeded the size limit".into()
        )]
    );
}

#[test]
fn bounds_hidden_reasoning_without_emitting_it() {
    let limits = LlmLimits {
        max_reasoning_bytes: 4,
        ..LlmLimits::default()
    };
    let body = [
        data(r#"{"choices":[{"delta":{"reasoning":"12345"}}]}"#),
        done(),
    ]
    .concat();

    assert_eq!(
        normalize_sse_bytes(body.as_bytes(), &limits),
        vec![VisibleChatEvent::Error(
            "reasoning content exceeded the size limit".into()
        )]
    );
}

#[test]
fn usage_and_comment_frames_are_ignored() {
    let body = [
        ": keep-alive\n\n".to_owned(),
        data(r#"{"usage":{"total_tokens":3}}"#),
        data(r#"{"choices":[{"delta":{"content":"ok"}}]}"#),
        finish_stop(),
        done(),
    ]
    .concat();

    assert_eq!(
        normalize_sse(body.as_bytes()),
        vec![
            VisibleChatEvent::TextDelta("ok".into()),
            finished("stop", false),
        ]
    );
}
