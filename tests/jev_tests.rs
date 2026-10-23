use std::sync::{Arc, Mutex};
use std::time::Duration;

use local_agent_harness::jev::{JevClient, JevLimits, Route, Verdict};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

#[derive(Debug, Clone)]
struct TestRequest {
    method: String,
    path: String,
    body: String,
}

#[derive(Debug, Clone)]
struct TestResponse {
    status: u16,
    reason: &'static str,
    headers: Vec<(String, String)>,
    body: String,
    delay: Duration,
}

impl TestResponse {
    fn json(body: impl Into<String>) -> Self {
        Self {
            status: 200,
            reason: "OK",
            headers: vec![("Content-Type".into(), "application/json".into())],
            body: body.into(),
            delay: Duration::ZERO,
        }
    }

    fn status(status: u16, reason: &'static str, body: impl Into<String>) -> Self {
        Self {
            status,
            reason,
            headers: vec![("Content-Type".into(), "text/plain".into())],
            body: body.into(),
            delay: Duration::ZERO,
        }
    }

    fn redirect(location: impl Into<String>) -> Self {
        Self {
            status: 302,
            reason: "Found",
            headers: vec![("Location".into(), location.into())],
            body: String::new(),
            delay: Duration::ZERO,
        }
    }

    fn delayed(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
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

    fn client(&self) -> JevClient {
        JevClient::new(self.url())
    }

    fn client_with(&self, limits: JevLimits) -> JevClient {
        JevClient::with_limits(self.url(), limits)
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

    let content_length = lines
        .filter_map(|line| line.split_once(':'))
        .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, value)| value.trim().parse::<usize>().ok())
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
    let request = TestRequest { method, path, body };
    requests.lock().expect("request lock").push(request.clone());

    let response = responder(&request);
    if !response.delay.is_zero() {
        tokio::time::sleep(response.delay).await;
    }

    let mut head = format!(
        "HTTP/1.1 {} {}\r\nContent-Length: {}\r\nConnection: close\r\n",
        response.status,
        response.reason,
        response.body.len()
    );
    for (name, value) in response.headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");

    let _ = stream.write_all(head.as_bytes()).await;
    let _ = stream.write_all(response.body.as_bytes()).await;
    let _ = stream.flush().await;
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

async fn settle() {
    tokio::time::sleep(Duration::from_millis(80)).await;
}

#[tokio::test]
async fn valid_choice_returns_pick() {
    let server =
        TestServer::start(|_| TestResponse::json(r#"{"index":1,"confidence":0.94}"#)).await;
    let route = server
        .client()
        .choice("pick one", &["first", "second"], 0.8)
        .await;
    assert_eq!(route, Route::Pick(1));

    let request = server.requests().pop().expect("one request");
    assert_eq!(request.method, "POST");
    let body = parse_body(&request);
    assert_eq!(body["prompt"], "pick one");
    assert_eq!(body["options"], serde_json::json!(["first", "second"]));
}

#[tokio::test]
async fn low_confidence_choice_escalates() {
    let server =
        TestServer::start(|_| TestResponse::json(r#"{"index":0,"confidence":0.42}"#)).await;
    assert_eq!(
        server.client().choice("prompt", &["a", "b"], 0.8).await,
        Route::Escalate
    );
}

#[tokio::test]
async fn invalid_choice_indices_escalate() {
    for index in ["-1", "2", "999999999999999999999999"] {
        let server = TestServer::start(move |_| {
            TestResponse::json(format!(r#"{{"index":{index},"confidence":0.99}}"#))
        })
        .await;
        assert_eq!(
            server.client().choice("prompt", &["a", "b"], 0.8).await,
            Route::Escalate,
            "index {index} must not be accepted"
        );
    }
}

#[tokio::test]
async fn invalid_choice_confidences_escalate() {
    for confidence in ["-0.1", "1.1", "NaN", "Infinity", "\"high\""] {
        let server = TestServer::start(move |_| {
            TestResponse::json(format!(r#"{{"index":0,"confidence":{confidence}}}"#))
        })
        .await;
        assert_eq!(
            server.client().choice("prompt", &["a", "b"], 0.8).await,
            Route::Escalate,
            "confidence {confidence} must not be accepted"
        );
    }
}

#[tokio::test]
async fn choice_timeout_escalates_without_retry() {
    let server = TestServer::start(|_| {
        TestResponse::json(r#"{"index":0,"confidence":0.99}"#).delayed(Duration::from_millis(250))
    })
    .await;
    let limits = JevLimits {
        connect_timeout: Duration::from_millis(40),
        request_timeout: Duration::from_millis(40),
        ..JevLimits::default()
    };
    assert_eq!(
        server
            .client_with(limits)
            .choice("prompt", &["a"], 0.8)
            .await,
        Route::Escalate
    );
    settle().await;
    assert_eq!(server.requests().len(), 1, "Jev must not be retried");
}

#[tokio::test]
async fn http_errors_and_malformed_json_escalate() {
    let error_server =
        TestServer::start(|_| TestResponse::status(500, "Internal Server Error", "no")).await;
    assert_eq!(
        error_server.client().choice("prompt", &["a"], 0.8).await,
        Route::Escalate
    );
    assert_eq!(
        error_server.client().noul("claim", "evidence", 0.8).await,
        Verdict::Uncertain
    );
    assert_eq!(
        error_server
            .client()
            .score("query", &["doc".to_owned()])
            .await,
        None
    );

    let malformed = TestServer::start(|_| TestResponse::json("not-json")).await;
    assert_eq!(
        malformed.client().choice("prompt", &["a"], 0.8).await,
        Route::Escalate
    );
}

#[tokio::test]
async fn empty_or_invalid_choice_input_avoids_http() {
    let server =
        TestServer::start(|_| TestResponse::json(r#"{"index":0,"confidence":0.99}"#)).await;
    let client = server.client();

    assert_eq!(client.choice("prompt", &[], 0.8).await, Route::Escalate);
    for invalid in [f32::NAN, f32::INFINITY, -0.1, 1.1] {
        assert_eq!(
            client.choice("prompt", &["a"], invalid).await,
            Route::Escalate
        );
        assert_eq!(
            client.noul("claim", "evidence", invalid).await,
            Verdict::Uncertain
        );
    }

    settle().await;
    assert!(server.requests().is_empty(), "no request may be sent");
}

#[tokio::test]
async fn high_confidence_noul_pass_and_fail_map_correctly() {
    let pass_server =
        TestServer::start(|_| TestResponse::json(r#"{"pass":true,"confidence":0.91}"#)).await;
    assert_eq!(
        pass_server.client().noul("claim", "evidence", 0.8).await,
        Verdict::Pass(0.91)
    );

    let fail_server =
        TestServer::start(|_| TestResponse::json(r#"{"pass":false,"confidence":0.92}"#)).await;
    assert_eq!(
        fail_server.client().noul("claim", "evidence", 0.8).await,
        Verdict::Fail(0.92)
    );
}

#[tokio::test]
async fn low_confidence_and_malformed_noul_are_uncertain() {
    let low = TestServer::start(|_| TestResponse::json(r#"{"pass":true,"confidence":0.5}"#)).await;
    assert_eq!(
        low.client().noul("claim", "evidence", 0.8).await,
        Verdict::Uncertain
    );

    let malformed =
        TestServer::start(|_| TestResponse::json(r#"{"pass":"yes","confidence":0.9}"#)).await;
    assert_eq!(
        malformed.client().noul("claim", "evidence", 0.8).await,
        Verdict::Uncertain
    );
}

#[tokio::test]
async fn valid_score_vector_maps_without_reordering() {
    let server = TestServer::start(|_| TestResponse::json(r#"{"scores":[0.91,0.62,0.0]}"#)).await;
    let docs = vec!["first".to_owned(), "second".to_owned(), "third".to_owned()];
    assert_eq!(
        server.client().score("query", &docs).await,
        Some(vec![0.91, 0.62, 0.0])
    );

    let body = parse_body(&server.requests().pop().expect("one request"));
    assert_eq!(
        body["docs"],
        serde_json::json!(["first", "second", "third"])
    );
}

#[tokio::test]
async fn invalid_score_vectors_return_none() {
    for response in [
        r#"{"scores":[0.9]}"#,
        r#"{"scores":[0.9,0.8,0.7,0.6]}"#,
        r#"{"scores":[0.9,NaN,0.7]}"#,
        r#"{"scores":[0.9,Infinity,0.7]}"#,
        r#"{"scores":[0.9,1.1,0.7]}"#,
        r#"{"scores":[0.9,-0.1,0.7]}"#,
    ] {
        let server = TestServer::start(move |_| TestResponse::json(response)).await;
        let docs = vec!["a".to_owned(), "b".to_owned(), "c".to_owned()];
        assert_eq!(
            server.client().score("query", &docs).await,
            None,
            "response {response} must be rejected"
        );
    }
}

#[tokio::test]
async fn arbitrary_finite_scores_require_explicit_configuration() {
    let server = TestServer::start(|_| TestResponse::json(r#"{"scores":[-2.5,3.5]}"#)).await;
    let docs = vec!["a".to_owned(), "b".to_owned()];
    let limits = JevLimits {
        require_unit_scores: false,
        ..JevLimits::default()
    };
    assert_eq!(
        server.client_with(limits).score("query", &docs).await,
        Some(vec![-2.5, 3.5])
    );
}

#[tokio::test]
async fn empty_docs_avoid_http_and_return_empty_vector() {
    let server = TestServer::start(|_| TestResponse::json(r#"{"scores":[]}"#)).await;
    assert_eq!(server.client().score("query", &[]).await, Some(Vec::new()));
    settle().await;
    assert!(server.requests().is_empty(), "no request may be sent");
}

#[tokio::test]
async fn fixed_endpoint_paths_are_used_exactly() {
    let server = TestServer::start(|request| match request.path.as_str() {
        "/v1/choice" => TestResponse::json(r#"{"index":0,"confidence":0.9}"#),
        "/v1/noul" => TestResponse::json(r#"{"pass":true,"confidence":0.9}"#),
        "/v1/score" => TestResponse::json(r#"{"scores":[0.5]}"#),
        _ => TestResponse::status(404, "Not Found", "wrong path"),
    })
    .await;
    let client = JevClient::new(format!("{}/", server.url()));

    assert_eq!(client.choice("p", &["a"], 0.5).await, Route::Pick(0));
    assert_eq!(client.noul("c", "e", 0.5).await, Verdict::Pass(0.9));
    assert_eq!(
        client.score("q", &["doc".to_owned()]).await,
        Some(vec![0.5])
    );

    let paths = server
        .requests()
        .into_iter()
        .map(|request| request.path)
        .collect::<Vec<_>>();
    assert_eq!(paths, ["/v1/choice", "/v1/noul", "/v1/score"]);
}

#[tokio::test]
async fn redirects_are_not_followed() {
    let server = TestServer::start(|request| {
        if request.path == "/followed" {
            TestResponse::json(r#"{"index":0,"confidence":0.99}"#)
        } else {
            TestResponse::redirect("/followed")
        }
    })
    .await;

    assert_eq!(
        server.client().choice("prompt", &["a"], 0.8).await,
        Route::Escalate
    );
    settle().await;
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].path, "/v1/choice");
}

#[tokio::test]
async fn unavailable_jev_escalates_without_another_host() {
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("reserve loopback port");
    let address = listener.local_addr().expect("reserved address");
    drop(listener);

    let client = JevClient::new(format!("http://{address}"));
    assert_eq!(client.choice("prompt", &["a"], 0.8).await, Route::Escalate);
    assert_eq!(
        client.noul("claim", "evidence", 0.8).await,
        Verdict::Uncertain
    );
    assert_eq!(client.score("query", &["doc".to_owned()]).await, None);
}

#[tokio::test]
async fn request_limits_truncate_utf8_safely_and_reject_oversized_bodies() {
    let server = TestServer::start(|request| match request.path.as_str() {
        "/v1/choice" => TestResponse::json(r#"{"index":1,"confidence":0.9}"#),
        "/v1/score" => TestResponse::json(r#"{"scores":[0.5,0.4]}"#),
        _ => TestResponse::status(404, "Not Found", "wrong path"),
    })
    .await;

    let limits = JevLimits {
        max_prompt_bytes: 21,
        max_option_bytes: 12,
        max_query_bytes: 21,
        max_doc_bytes: 21,
        max_docs: 2,
        ..JevLimits::default()
    };
    let client = server.client_with(limits);
    let long_prompt = "é".repeat(100);
    let long_option = "é".repeat(100);

    assert_eq!(
        client
            .choice(&long_prompt, &[long_option.as_str(), "ok"], 0.5)
            .await,
        Route::Pick(1)
    );
    let choice_body = parse_body(&server.requests().pop().expect("choice request"));
    let prompt = choice_body["prompt"].as_str().expect("prompt string");
    assert!(prompt.len() <= 21);
    assert!(prompt.ends_with("...[truncated]"));
    assert!(choice_body["options"][0].as_str().unwrap().len() <= 12);

    let docs = vec!["é".repeat(100), "short".to_owned()];
    assert_eq!(
        client.score(&long_prompt, &docs).await,
        Some(vec![0.5, 0.4])
    );
    let score_body = parse_body(&server.requests().pop().expect("score request"));
    assert!(score_body["query"].as_str().unwrap().len() <= 21);
    assert!(score_body["docs"][0].as_str().unwrap().len() <= 21);

    let too_many = vec!["a".to_owned(), "b".to_owned(), "c".to_owned()];
    assert_eq!(client.score("query", &too_many).await, None);

    let tiny_limits = JevLimits {
        max_request_bytes: 32,
        ..JevLimits::default()
    };
    let tiny = server.client_with(tiny_limits);
    assert_eq!(tiny.choice("prompt", &["a"], 0.5).await, Route::Escalate);
    settle().await;
    assert_eq!(server.requests().len(), 2, "rejected inputs send no HTTP");
}

#[tokio::test]
async fn debug_output_does_not_expose_request_content() {
    let secret = "PROMPT-SECRET-7f21c4";
    let server = TestServer::start(|_| TestResponse::json("not-json")).await;
    let client = server.client();

    assert_eq!(client.choice(secret, &["a"], 0.8).await, Route::Escalate);
    let request = server.requests().pop().expect("request was sent");
    assert!(request.body.contains(secret));

    let debug = format!("{client:?}");
    assert!(!debug.contains(secret));
}
