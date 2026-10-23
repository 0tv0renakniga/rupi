use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use local_agent_harness::error::AgentError;
use local_agent_harness::llm::openai_compat::{LlmLimits, OpenAiCompatClient};
use local_agent_harness::llm::{
    ChatMessage, ChatRequest, FnCall, LlmBackend, LlmStreamEvent, ToolCall,
};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

#[derive(Debug, Clone)]
struct TestRequest {
    method: String,
    path: String,
    headers: BTreeMap<String, String>,
    body: String,
}

#[derive(Debug, Clone)]
struct TestResponse {
    status: u16,
    reason: &'static str,
    headers: Vec<(String, String)>,
    chunks: Vec<Vec<u8>>,
    delay: Duration,
    chunk_delay: Duration,
}

impl TestResponse {
    fn json(body: impl Into<String>) -> Self {
        Self {
            status: 200,
            reason: "OK",
            headers: vec![("Content-Type".into(), "application/json".into())],
            chunks: vec![body.into().into_bytes()],
            delay: Duration::ZERO,
            chunk_delay: Duration::ZERO,
        }
    }

    fn sse(body: impl Into<String>) -> Self {
        Self {
            status: 200,
            reason: "OK",
            headers: vec![("Content-Type".into(), "text/event-stream".into())],
            chunks: vec![body.into().into_bytes()],
            delay: Duration::ZERO,
            chunk_delay: Duration::ZERO,
        }
    }

    fn sse_chunks(chunks: Vec<Vec<u8>>) -> Self {
        Self {
            status: 200,
            reason: "OK",
            headers: vec![("Content-Type".into(), "text/event-stream".into())],
            chunks,
            delay: Duration::ZERO,
            chunk_delay: Duration::from_millis(2),
        }
    }

    fn status(status: u16, reason: &'static str, body: impl Into<String>) -> Self {
        Self {
            status,
            reason,
            headers: vec![("Content-Type".into(), "text/plain".into())],
            chunks: vec![body.into().into_bytes()],
            delay: Duration::ZERO,
            chunk_delay: Duration::ZERO,
        }
    }

    fn redirect(location: impl Into<String>) -> Self {
        Self {
            status: 302,
            reason: "Found",
            headers: vec![("Location".into(), location.into())],
            chunks: Vec::new(),
            delay: Duration::ZERO,
            chunk_delay: Duration::ZERO,
        }
    }

    fn delayed(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }

    fn content_length(&self) -> usize {
        self.chunks.iter().map(Vec::len).sum()
    }
}

type Responder = Arc<dyn Fn(&TestRequest) -> TestResponse + Send + Sync>;

struct TestServer {
    address: std::net::SocketAddr,
    requests: Arc<Mutex<Vec<TestRequest>>>,
    task: JoinHandle<()>,
}

impl TestServer {
    async fn start(
        responder: impl Fn(&TestRequest) -> TestResponse + Send + Sync + 'static,
    ) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind loopback test server");
        let address = listener.local_addr().expect("test server address");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let responder: Responder = Arc::new(responder);

        let task = {
            let requests = Arc::clone(&requests);
            tokio::spawn(async move {
                loop {
                    let Ok((stream, _)) = listener.accept().await else {
                        break;
                    };
                    let requests = Arc::clone(&requests);
                    let responder = Arc::clone(&responder);
                    tokio::spawn(async move {
                        let _ = handle_connection(stream, requests, responder).await;
                    });
                }
            })
        };

        Self {
            address,
            requests,
            task,
        }
    }

    fn url(&self) -> String {
        format!("http://{}", self.address)
    }

    fn client(&self) -> OpenAiCompatClient {
        OpenAiCompatClient::new(self.url(), "local-model")
    }

    fn client_with(&self, limits: LlmLimits) -> OpenAiCompatClient {
        self.client().with_limits(limits)
    }

    fn requests(&self) -> Vec<TestRequest> {
        self.requests.lock().expect("request lock").clone()
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn handle_connection(
    mut stream: TcpStream,
    requests: Arc<Mutex<Vec<TestRequest>>>,
    responder: Responder,
) -> std::io::Result<()> {
    let mut buffer = Vec::new();
    let header_end = loop {
        let mut chunk = [0_u8; 4096];
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            return Ok(());
        }
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(index) = find_bytes(&buffer, b"\r\n\r\n") {
            break index;
        }
        if buffer.len() > 128 * 1024 {
            return Ok(());
        }
    };

    let head = String::from_utf8_lossy(&buffer[..header_end]);
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or_default();
    let mut request_parts = request_line.split_whitespace();
    let method = request_parts.next().unwrap_or_default().to_owned();
    let path = request_parts.next().unwrap_or_default().to_owned();

    let mut headers = BTreeMap::new();
    for (name, value) in lines.filter_map(|line| line.split_once(':')) {
        headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_owned());
    }
    let content_length = headers
        .get("content-length")
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);

    let body_start = header_end + 4;
    while buffer.len() < body_start + content_length {
        let mut chunk = [0_u8; 4096];
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..read]);
    }

    let body_end = (body_start + content_length).min(buffer.len());
    let body = String::from_utf8_lossy(&buffer[body_start..body_end]).into_owned();
    let request = TestRequest {
        method,
        path,
        headers,
        body,
    };
    requests.lock().expect("request lock").push(request.clone());

    let response = responder(&request);
    if !response.delay.is_zero() {
        tokio::time::sleep(response.delay).await;
    }

    let mut head = format!(
        "HTTP/1.1 {} {}\r\nContent-Length: {}\r\nConnection: close\r\n",
        response.status,
        response.reason,
        response.content_length()
    );
    for (name, value) in response.headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");

    stream.write_all(head.as_bytes()).await?;
    for chunk in response.chunks {
        if !chunk.is_empty() {
            stream.write_all(&chunk).await?;
            stream.flush().await?;
        }
        if !response.chunk_delay.is_zero() {
            tokio::time::sleep(response.chunk_delay).await;
        }
    }
    Ok(())
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn parse_body(request: &TestRequest) -> Value {
    serde_json::from_str(&request.body).expect("recorded request body is JSON")
}

fn data(json: &str) -> String {
    format!("data: {json}\n\n")
}

fn done() -> String {
    "data: [DONE]\n\n".to_owned()
}

fn split_text(text: &str, positions: &[usize]) -> Vec<Vec<u8>> {
    let bytes = text.as_bytes();
    let mut points = positions.to_vec();
    points.push(0);
    points.push(bytes.len());
    points.sort_unstable();
    points.dedup();

    points
        .windows(2)
        .map(|window| bytes[window[0]..window[1]].to_vec())
        .collect()
}

fn user_request(text: &str) -> ChatRequest {
    ChatRequest::new(vec![ChatMessage::user(text)])
}

#[tokio::test]
async fn streams_text_and_reassembles_fragmented_tool_calls() {
    let body = [
        data(r#"{"choices":[{"delta":{"content":"Hel"}}]}"#),
        data(r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_a","type":"function","function":{"name":"read_","arguments":"{\"pa"}}]}}]}"#),
        data(r#"{"choices":[{"delta":{"content":"lo"}}]}"#),
        data(r#"{"choices":[{"delta":{"tool_calls":[{"index":1,"id":"call_b","function":{"name":"list","arguments":"{}"}}]}}]}"#),
        data(r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"name":"file","arguments":"th\":\"src\"}"}}]}}]}"#),
        data(r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#),
        done(),
    ]
    .concat();
    let server = TestServer::start(move |_| TestResponse::sse(body.clone())).await;

    let turn = server
        .client()
        .stream_chat(user_request("inspect"), None)
        .await
        .expect("stream should succeed");

    assert_eq!(turn.text, "Hello");
    assert_eq!(turn.finish, "tool_calls");
    assert_eq!(turn.calls.len(), 2);
    assert_eq!(turn.calls[0].id, "call_a");
    assert_eq!(turn.calls[0].kind, "function");
    assert_eq!(turn.calls[0].function.name, "read_file");
    assert_eq!(turn.calls[0].function.arguments, r#"{"path":"src"}"#);
    assert_eq!(turn.calls[1].id, "call_b");
    assert_eq!(turn.calls[1].function.name, "list");
    assert_eq!(turn.calls[1].function.arguments, "{}");

    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "POST");
    assert_eq!(requests[0].path, "/v1/chat/completions");
    let request = parse_body(&requests[0]);
    assert_eq!(request["model"], "local-model");
    assert_eq!(request["stream"], true);
}

#[tokio::test]
async fn reasoning_content_is_transient_separate_and_unknown_fields_are_ignored() {
    let body = [
        data(r#"{"id":"chunk","object":"chat.completion.chunk","system_fingerprint":"fp","timings":{"x":1},"choices":[{"index":0,"logprobs":null,"delta":{"role":"assistant","reasoning_content":"think","content":"answer","unknown_delta":{"a":1}}}]}"#),
        data(r#"{"choices":[{"index":0,"delta":{"reasoning_content":"ing"},"unknown_choice":true}]}"#),
        data(r#"{"choices":[{"delta":{},"finish_reason":"stop"}],"usage":{"total_tokens":3}}"#),
        done(),
    ]
    .concat();
    let server = TestServer::start(move |_| TestResponse::sse(body.clone())).await;

    let turn = server
        .client()
        .stream_chat(user_request("reason"), None)
        .await
        .expect("unknown extension fields should be ignored");

    assert_eq!(turn.text, "answer");
    assert_eq!(turn.reasoning_content.as_deref(), Some("thinking"));
    assert_eq!(turn.finish, "stop");

    let message = serde_json::to_value(turn.as_message()).expect("message JSON");
    assert_eq!(message["content"], "answer");
    assert!(!message.to_string().contains("thinking"));
    assert!(!format!("{turn:?}").contains("thinking"));
}

#[tokio::test]
async fn reasoning_only_output_is_not_a_completion() {
    let server = TestServer::start(|_| {
        TestResponse::json(
            r#"{"choices":[{"message":{"role":"assistant","content":"","reasoning_content":"only reasoning"}}]}"#,
        )
    })
    .await;

    let error = server
        .client()
        .complete_text("system", "user")
        .await
        .expect_err("reasoning-only output must not satisfy complete_text");
    assert!(matches!(error, AgentError::Llm(message) if message.contains("empty")));
}

#[tokio::test]
async fn rejects_oversized_reasoning_content() {
    let server = TestServer::start(|_| {
        TestResponse::sse(
            [
                data(r#"{"choices":[{"delta":{"reasoning_content":"12345"}}]}"#),
                done(),
            ]
            .concat(),
        )
    })
    .await;
    let limits = LlmLimits {
        max_reasoning_bytes: 4,
        ..LlmLimits::default()
    };

    let error = server
        .client_with(limits)
        .stream_chat(user_request("reason"), None)
        .await
        .expect_err("reasoning content must be bounded");
    assert!(matches!(error, AgentError::Llm(message) if message.contains("reasoning")));
}

#[tokio::test]
async fn handles_arbitrary_chunks_and_utf8_split_across_chunks() {
    let body = [
        data(r#"{"choices":[{"delta":{"content":"héllo"}}]}"#),
        data(r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#),
        done(),
    ]
    .concat();
    let marker_start = body.find("héllo").expect("marker present");
    let chunks = split_text(&body, &[5, marker_start + 2, marker_start + 3]);
    let server = TestServer::start(move |_| TestResponse::sse_chunks(chunks.clone())).await;

    let turn = server
        .client()
        .stream_chat(user_request("say hello"), None)
        .await
        .expect("stream should survive arbitrary byte boundaries");

    assert_eq!(turn.text, "héllo");
    assert_eq!(turn.finish, "stop");
}

#[tokio::test]
async fn accepts_crlf_and_ignores_comments_and_non_data_fields() {
    let body = [
        ": keepalive\n\n".to_owned(),
        "event: message\n".to_owned(),
        "id: 1\n".to_owned(),
        "retry: 10\n".to_owned(),
        data(r#"{"choices":[{"delta":{"content":"ok"}}]}"#),
        data(r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#),
        done(),
    ]
    .concat()
    .replace('\n', "\r\n");
    let server = TestServer::start(move |_| TestResponse::sse(body.clone())).await;

    let turn = server
        .client()
        .stream_chat(user_request("test"), None)
        .await
        .expect("CRLF SSE should parse");

    assert_eq!(turn.text, "ok");
    assert_eq!(turn.finish, "stop");
}

#[tokio::test]
async fn accepts_done_without_a_trailing_newline() {
    let body = [
        data(r#"{"choices":[{"delta":{"content":"ok"}}]}"#),
        "data: [DONE]".to_owned(),
    ]
    .concat();
    let server = TestServer::start(move |_| TestResponse::sse(body.clone())).await;

    let turn = server
        .client()
        .stream_chat(user_request("test"), None)
        .await
        .expect("a final [DONE] line without a newline should be accepted");

    assert_eq!(turn.text, "ok");
}

#[tokio::test]
async fn accepts_late_ids_and_synthesizes_missing_ids() {
    let body = [
        data(r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"name":"a","arguments":"{}"}}]}}]}"#),
        data(r#"{"choices":[{"delta":{"tool_calls":[{"index":1,"id":"late_b","function":{"name":"b","arguments":"{}"}}]}}]}"#),
        data(r#"{"choices":[{"delta":{"tool_calls":[{"index":2,"function":{"name":"c","arguments":"{}"}}]}}]}"#),
        data(r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"late_a"}]}}]}"#),
        data(r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#),
        done(),
    ]
    .concat();
    let server = TestServer::start(move |_| TestResponse::sse(body.clone())).await;

    let turn = server
        .client()
        .stream_chat(user_request("tools"), None)
        .await
        .expect("late and missing ids should be handled");

    assert_eq!(turn.calls.len(), 3);
    assert_eq!(turn.calls[0].id, "late_a");
    assert_eq!(turn.calls[1].id, "late_b");
    assert_eq!(turn.calls[2].id, "call_2");
    assert_eq!(turn.calls[2].function.name, "c");
}

#[tokio::test]
async fn rejects_duplicate_final_tool_call_ids() {
    let body = [
        data(r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"dup","function":{"name":"a","arguments":"{}"}}]}}]}"#),
        data(r#"{"choices":[{"delta":{"tool_calls":[{"index":1,"id":"dup","function":{"name":"b","arguments":"{}"}}]}}]}"#),
        done(),
    ]
    .concat();
    let server = TestServer::start(move |_| TestResponse::sse(body.clone())).await;

    let error = server
        .client()
        .stream_chat(user_request("tools"), None)
        .await
        .expect_err("duplicate ids must be rejected");

    assert!(matches!(error, AgentError::Llm(message) if message.contains("duplicate")));
}

#[tokio::test]
async fn rejects_malformed_sse_and_eof_before_done() {
    let malformed = TestServer::start(|_| TestResponse::sse("data: {not json}\n\n")).await;
    let error = malformed
        .client()
        .stream_chat(user_request("bad"), None)
        .await
        .expect_err("malformed JSON must fail");
    assert!(matches!(error, AgentError::Llm(message) if message.contains("malformed")));

    let unfinished = TestServer::start(|_| {
        TestResponse::sse(data(
            r#"{"choices":[{"delta":{"content":"partial"},"finish_reason":"stop"}]}"#,
        ))
    })
    .await;
    let error = unfinished
        .client()
        .stream_chat(user_request("unfinished"), None)
        .await
        .expect_err("EOF before [DONE] must fail");
    assert!(matches!(error, AgentError::Llm(message) if message.contains("[DONE]")));
}

#[tokio::test]
async fn rejects_stream_bounds_and_oversized_tool_payloads() {
    let oversized_line = TestServer::start(|_| {
        TestResponse::sse(data(
            r#"{"choices":[{"delta":{"content":"this line is too long"}}]}"#,
        ))
    })
    .await;
    let limits = LlmLimits {
        max_sse_line_bytes: 16,
        ..LlmLimits::default()
    };
    let error = oversized_line
        .client_with(limits)
        .stream_chat(user_request("line"), None)
        .await
        .expect_err("oversized SSE line must fail");
    assert!(matches!(error, AgentError::Llm(message) if message.contains("SSE line")));

    let oversized_stream = TestServer::start(|_| {
        TestResponse::sse(data(r#"{"choices":[{"delta":{"content":"too much"}}]}"#))
    })
    .await;
    let limits = LlmLimits {
        max_stream_bytes: 16,
        ..LlmLimits::default()
    };
    let error = oversized_stream
        .client_with(limits)
        .stream_chat(user_request("stream"), None)
        .await
        .expect_err("oversized stream must fail");
    assert!(matches!(error, AgentError::Llm(message) if message.contains("streamed response")));

    let oversized_arguments = TestServer::start(|_| {
        TestResponse::sse(
            [
                data(r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"name":"a","arguments":"123456789"}}]}}]}"#),
                done(),
            ]
            .concat(),
        )
    })
    .await;
    let limits = LlmLimits {
        max_tool_arguments_bytes: 4,
        ..LlmLimits::default()
    };
    let error = oversized_arguments
        .client_with(limits)
        .stream_chat(user_request("args"), None)
        .await
        .expect_err("oversized arguments must fail");
    assert!(matches!(error, AgentError::Llm(message) if message.contains("arguments")));

    let too_many_calls = TestServer::start(|_| {
        TestResponse::sse(
            [
                data(r#"{"choices":[{"delta":{"tool_calls":[{"index":1,"function":{"name":"b","arguments":"{}"}}]}}]}"#),
                done(),
            ]
            .concat(),
        )
    })
    .await;
    let limits = LlmLimits {
        max_tool_calls: 1,
        ..LlmLimits::default()
    };
    let error = too_many_calls
        .client_with(limits)
        .stream_chat(user_request("calls"), None)
        .await
        .expect_err("tool-call index over the limit must fail");
    assert!(matches!(error, AgentError::Llm(message) if message.contains("tool-call")));
}

#[tokio::test]
async fn emits_stream_events_and_survives_a_dropped_receiver() {
    let body = [
        data(r#"{"choices":[{"delta":{"content":"Hi"}}]}"#),
        data(r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#),
        done(),
    ]
    .concat();
    let server = TestServer::start(move |_| TestResponse::sse(body.clone())).await;
    let (tx, mut rx) = mpsc::unbounded_channel();

    let turn = server
        .client()
        .stream_chat(user_request("events"), Some(&tx))
        .await
        .expect("stream should succeed with events");
    drop(tx);
    assert_eq!(turn.text, "Hi");

    let mut events = Vec::new();
    while let Ok(event) = rx.try_recv() {
        events.push(event);
    }
    assert!(events.contains(&LlmStreamEvent::TextDelta("Hi".into())));
    assert!(events.contains(&LlmStreamEvent::Finished {
        finish_reason: Some("stop".into())
    }));

    let (tx, rx) = mpsc::unbounded_channel();
    drop(rx);
    let turn = server
        .client()
        .stream_chat(user_request("dropped"), Some(&tx))
        .await
        .expect("dropped event receiver must not cancel the request");
    assert_eq!(turn.text, "Hi");
}

#[tokio::test]
async fn complete_text_uses_non_streaming_mode_and_rejects_empty_or_tool_output() {
    let ok = TestServer::start(|_| {
        TestResponse::json(r#"{"choices":[{"message":{"role":"assistant","content":"done"}}]}"#)
    })
    .await;
    let text = ok
        .client()
        .complete_text("system prompt", "user prompt")
        .await
        .expect("completion should succeed");
    assert_eq!(text, "done");

    let requests = ok.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].path, "/v1/chat/completions");
    let request = parse_body(&requests[0]);
    assert_eq!(request["stream"], false);
    assert_eq!(request["model"], "local-model");
    assert!(request.get("tools").is_none());
    assert_eq!(request["messages"][0]["role"], "system");
    assert_eq!(request["messages"][0]["content"], "system prompt");
    assert_eq!(request["messages"][1]["role"], "user");
    assert_eq!(request["messages"][1]["content"], "user prompt");

    let empty =
        TestServer::start(|_| TestResponse::json(r#"{"choices":[{"message":{"content":""}}]}"#))
            .await;
    let error = empty
        .client()
        .complete_text("system", "user")
        .await
        .expect_err("empty completion must fail");
    assert!(matches!(error, AgentError::Llm(message) if message.contains("empty")));

    let tool_output = TestServer::start(|_| {
        TestResponse::json(
            r#"{"choices":[{"message":{"content":"text","tool_calls":[{"id":"x"}]}}]}"#,
        )
    })
    .await;
    let error = tool_output
        .client()
        .complete_text("system", "user")
        .await
        .expect_err("tool output must not be accepted as text");
    assert!(matches!(error, AgentError::Llm(message) if message.contains("tool calls")));
}

#[tokio::test]
async fn http_errors_redirects_timeouts_and_unavailable_endpoints_do_not_retry() {
    let failure =
        TestServer::start(|_| TestResponse::status(500, "Internal Server Error", "boom")).await;
    let error = failure
        .client()
        .stream_chat(user_request("fail"), None)
        .await
        .expect_err("HTTP 500 must fail");
    assert!(matches!(error, AgentError::Llm(message) if message.contains("500")));
    assert_eq!(failure.requests().len(), 1);

    let redirect =
        TestServer::start(|_| TestResponse::redirect("http://127.0.0.1:1/elsewhere")).await;
    let error = redirect
        .client()
        .stream_chat(user_request("redirect"), None)
        .await
        .expect_err("redirects must not be followed");
    assert!(matches!(error, AgentError::Llm(message) if message.contains("302")));
    assert_eq!(redirect.requests().len(), 1);

    let slow =
        TestServer::start(|_| TestResponse::sse(done()).delayed(Duration::from_millis(300))).await;
    let limits = LlmLimits {
        connect_timeout: Duration::from_millis(100),
        request_timeout: Duration::from_millis(60),
        ..LlmLimits::default()
    };
    let error = slow
        .client_with(limits)
        .stream_chat(user_request("timeout"), None)
        .await
        .expect_err("timeout must fail");
    assert!(matches!(error, AgentError::Llm(message) if message.contains("timed out")));
    assert_eq!(slow.requests().len(), 1);

    let unavailable =
        OpenAiCompatClient::new("http://127.0.0.1:1", "local-model").with_limits(LlmLimits {
            connect_timeout: Duration::from_millis(50),
            request_timeout: Duration::from_millis(200),
            ..LlmLimits::default()
        });
    let error = unavailable
        .stream_chat(user_request("unavailable"), None)
        .await
        .expect_err("unavailable endpoint must fail");
    assert!(matches!(error, AgentError::Llm(_)));
}

#[tokio::test]
async fn rejects_unsafe_or_oversized_requests_before_contacting_the_server() {
    let server =
        TestServer::start(|_| TestResponse::sse([data(r#"{"choices":[]}"#), done()].concat()))
            .await;

    let too_few_messages = LlmLimits {
        max_messages: 1,
        ..LlmLimits::default()
    };
    let error = server
        .client_with(too_few_messages)
        .stream_chat(
            ChatRequest::new(vec![
                ChatMessage::user("one"),
                ChatMessage::assistant("two"),
            ]),
            None,
        )
        .await
        .expect_err("message count must be bounded");
    assert!(matches!(error, AgentError::Llm(message) if message.contains("message count")));

    let short_content = LlmLimits {
        max_message_content_bytes: 4,
        ..LlmLimits::default()
    };
    let error = server
        .client_with(short_content)
        .stream_chat(user_request("hello"), None)
        .await
        .expect_err("message content must be bounded");
    assert!(matches!(error, AgentError::Llm(message) if message.contains("content")));

    let few_tools = LlmLimits {
        max_tool_definitions: 1,
        ..LlmLimits::default()
    };
    let error = server
        .client_with(few_tools)
        .stream_chat(
            user_request("tools").with_tools(json!([
                {"type": "function", "function": {"name": "a"}},
                {"type": "function", "function": {"name": "b"}}
            ])),
            None,
        )
        .await
        .expect_err("tool definition count must be bounded");
    assert!(matches!(error, AgentError::Llm(message) if message.contains("tool definitions")));

    let small_tool = LlmLimits {
        max_tool_definition_bytes: 4,
        ..LlmLimits::default()
    };
    let error = server
        .client_with(small_tool)
        .stream_chat(
            user_request("tool").with_tools(json!([{"type": "function"}])),
            None,
        )
        .await
        .expect_err("tool definition size must be bounded");
    assert!(matches!(error, AgentError::Llm(message) if message.contains("tool definition")));

    let small_request = LlmLimits {
        max_request_bytes: 10,
        ..LlmLimits::default()
    };
    let error = server
        .client_with(small_request)
        .stream_chat(user_request("request"), None)
        .await
        .expect_err("request body must be bounded");
    assert!(matches!(error, AgentError::Llm(message) if message.contains("request body")));

    let error = server
        .client()
        .stream_chat(user_request("tools").with_tools(json!({})), None)
        .await
        .expect_err("tools must be an array");
    assert!(matches!(error, AgentError::Llm(message) if message.contains("tools")));

    let mut bad_role = user_request("bad role");
    bad_role.messages[0].role = "robot".into();
    let error = server
        .client()
        .stream_chat(bad_role, None)
        .await
        .expect_err("unknown roles must be rejected");
    assert!(matches!(error, AgentError::Llm(message) if message.contains("role")));

    for base in [
        format!("{}?token=secret", server.url()),
        format!("{}#fragment", server.url()),
    ] {
        let client = OpenAiCompatClient::new(base, "local-model");
        let error = client
            .stream_chat(user_request("unsafe"), None)
            .await
            .expect_err("query and fragment must be rejected");
        assert!(matches!(error, AgentError::Llm(_)));
    }

    let mut messages = user_request("missing id");
    messages.messages.push(ChatMessage {
        role: "tool".into(),
        content: Some("result".into()),
        tool_calls: None,
        tool_call_id: None,
    });
    let error = server
        .client()
        .stream_chat(messages, None)
        .await
        .expect_err("tool messages require an id");
    assert!(matches!(error, AgentError::Llm(message) if message.contains("tool_call_id")));

    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(server.requests().is_empty());
}

#[tokio::test]
async fn uses_a_fixed_path_and_clamps_generation_parameters() {
    let server = TestServer::start(|_| {
        TestResponse::sse(
            [
                data(r#"{"choices":[{"delta":{"content":"ok"}}]}"#),
                data(r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#),
                done(),
            ]
            .concat(),
        )
    })
    .await;
    let client = OpenAiCompatClient::new(format!("{}/prefix/", server.url()), "local-model");
    let request = user_request("shape")
        .with_tools(json!([{"type": "function", "function": {"name": "a"}}]))
        .with_temperature(9.0)
        .with_max_tokens(0);

    client
        .stream_chat(request, None)
        .await
        .expect("clamped request should succeed");

    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].path, "/prefix/v1/chat/completions");
    let body = parse_body(&requests[0]);
    assert_eq!(body["temperature"], 2.0);
    assert_eq!(body["max_tokens"], 1);
    assert_eq!(body["tools"][0]["function"]["name"], "a");
    assert_eq!(body["messages"][0]["content"], "shape");
}

#[tokio::test]
async fn debug_and_errors_do_not_leak_prompt_or_tool_result_contents() {
    let server = TestServer::start(|_| TestResponse::sse("data: {oops}\n\n")).await;
    let client =
        OpenAiCompatClient::new(format!("{}/secret-base-path", server.url()), "local-model");
    let request = ChatRequest::new(vec![
        ChatMessage::system("system"),
        ChatMessage::user("PROMPT_SECRET_123"),
        ChatMessage::assistant_with_tool_calls(
            "",
            vec![ToolCall {
                id: "call_1".into(),
                kind: "function".into(),
                function: FnCall {
                    name: "read".into(),
                    arguments: "{}".into(),
                },
            }],
        ),
        ChatMessage::tool("call_1".into(), "TOOL_SECRET_456".into()),
    ]);

    let error = client
        .stream_chat(request, None)
        .await
        .expect_err("malformed response should fail");
    let error_text = error.to_string();
    let debug_text = format!("{client:?}");

    for secret in ["PROMPT_SECRET_123", "TOOL_SECRET_456", "secret-base-path"] {
        assert!(!error_text.contains(secret), "error leaked {secret}");
        assert!(!debug_text.contains(secret), "debug leaked {secret}");
    }
    assert!(debug_text.contains("http://127.0.0.1"));
}

/// Manual smoke test. Start a local llama-server, choose `LLM_MODEL` from the
/// ID returned by `GET /v1/models`, optionally set `LLM_BASE` (default
/// `http://127.0.0.1:8080`), then run this ignored test. Tool-call support
/// depends on the selected model, chat template, llama.cpp version, and server
/// configuration; this test only checks transport and text streaming.
#[tokio::test]
#[ignore = "requires a manually started local llama-server and explicit LLM_MODEL"]
async fn real_llama_server_smoke_test() {
    let base = std::env::var("LLM_BASE").unwrap_or_else(|_| "http://127.0.0.1:8080".to_owned());
    let model = std::env::var("LLM_MODEL").expect("set LLM_MODEL to the server's model name");
    let client = OpenAiCompatClient::new(base, model);
    let request = ChatRequest::new(vec![
        ChatMessage::system("You are a terse test assistant."),
        ChatMessage::user("Reply with exactly: ok"),
    ]);

    let turn = client
        .stream_chat(request, None)
        .await
        .expect("local llama-server smoke request should succeed");
    assert!(
        !turn.text.trim().is_empty(),
        "local llama-server returned no text"
    );
}

#[tokio::test]
async fn bearer_token_is_sent_to_both_completion_endpoints_and_not_debugged() {
    let server = TestServer::start(|request| {
        if request.path.ends_with("/chat/completions") {
            if request.body.contains("\"stream\":true") {
                TestResponse::sse(format!(
                    "{}{}",
                    data(r#"{"choices":[{"delta":{"content":"ok"}}]}"#),
                    done()
                ))
            } else {
                TestResponse::json(r#"{"choices":[{"message":{"content":"ok"}}]}"#)
            }
        } else {
            TestResponse::status(404, "Not Found", "unexpected path")
        }
    })
    .await;
    let client = server.client().with_bearer_token("test-secret-token");

    let streamed = client
        .stream_chat(user_request("stream"), None)
        .await
        .expect("streamed completion");
    assert_eq!(streamed.text, "ok");
    let completed = client
        .complete_text("system", "user")
        .await
        .expect("completion");
    assert_eq!(completed, "ok");

    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    for request in requests {
        assert_eq!(
            request.headers.get("authorization").map(String::as_str),
            Some("Bearer test-secret-token")
        );
    }

    let debug = format!("{client:?}");
    assert!(debug.contains("credential_configured: true"));
    assert!(!debug.contains("test-secret-token"));
}
