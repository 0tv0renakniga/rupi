use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use local_agent_harness::mcp::client::{DEFAULT_MCP_PROTOCOL_VERSION, McpClient, McpClientOptions};
use local_agent_harness::mcp::process::{self, ProcessSpec};
use local_agent_harness::mcp::tools::ToolContent;
use serde_json::json;

const MOCK_SERVER: &str = env!("CARGO_BIN_EXE_mock_mcp_server");

fn mock_spec() -> ProcessSpec {
    let home = std::env::temp_dir().join("local-agent-harness-mcp-test-home");
    std::fs::create_dir_all(&home).expect("create controlled HOME");

    let mut env = BTreeMap::new();
    env.insert("HOME".to_owned(), home.to_string_lossy().into_owned());

    ProcessSpec {
        program: PathBuf::from(MOCK_SERVER),
        args: Vec::new(),
        cwd: Some(std::env::current_dir().expect("current directory")),
        env,
    }
}

async fn connect_with(options: McpClientOptions) -> McpClient {
    McpClient::connect("mock", &mock_spec(), options)
        .await
        .expect("mock MCP lifecycle should succeed")
}

#[tokio::test]
async fn initialize_lifecycle_succeeds() {
    let client = connect_with(McpClientOptions::default()).await;

    assert_eq!(client.protocol_version(), DEFAULT_MCP_PROTOCOL_VERSION);
    assert!(client.tools().iter().any(|tool| tool.name == "echo"));
    assert_eq!(client.pending_request_count().await, 0);
}

#[tokio::test]
async fn tools_list_returns_validated_metadata() {
    let client = connect_with(McpClientOptions::default()).await;

    let echo = client
        .tools()
        .iter()
        .find(|tool| tool.name == "echo")
        .expect("echo tool should be advertised");
    assert!(
        echo.description
            .as_deref()
            .is_some_and(|text| !text.is_empty())
    );
    assert!(echo.input_schema.is_some());
}

#[tokio::test]
async fn concurrent_requests_resolve_to_correct_callers() {
    let client = connect_with(McpClientOptions::default()).await;

    let (first, second, third) = tokio::join!(
        client.call_tool("echo", json!({"text": "alpha"})),
        client.call_tool("echo", json!({"text": "beta"})),
        client.call_tool("echo", json!({"text": "gamma"})),
    );

    for (outcome, expected) in [(first, "alpha"), (second, "beta"), (third, "gamma")] {
        assert!(!outcome.is_error, "{}", outcome.text());
        assert_eq!(outcome.tool_call_id, "echo");
        assert_eq!(outcome.text(), expected);
    }
    assert_eq!(client.pending_request_count().await, 0);
}

#[tokio::test]
async fn tools_call_maps_normal_result() {
    let client = connect_with(McpClientOptions::default()).await;
    let outcome = client.call_tool("echo", json!({"text": "hello"})).await;

    assert!(!outcome.is_error);
    assert_eq!(
        outcome.content,
        vec![ToolContent::Text {
            text: "hello".into()
        }]
    );
    assert_eq!(outcome.text(), "hello");
}

#[tokio::test]
async fn tools_call_maps_is_error_result() {
    let client = connect_with(McpClientOptions::default()).await;
    let outcome = client
        .call_tool("fail", json!({"message": "expected failure"}))
        .await;

    assert!(outcome.is_error);
    assert_eq!(outcome.text(), "expected failure");
}

#[tokio::test]
async fn timeout_removes_pending_state_and_returns_clear_error() {
    let options = McpClientOptions {
        request_timeout: Duration::from_millis(300),
        ..McpClientOptions::default()
    };
    let client = connect_with(options).await;

    let started = Instant::now();
    let error = client
        .request(
            "tools/call",
            json!({"name": "delay", "arguments": {"millis": 900}}),
        )
        .await
        .unwrap_err();

    assert!(error.to_string().contains("timed out"), "{error}");
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(client.pending_request_count().await, 0);

    // The late response uses now-unknown id 1; it must be ignored safely while
    // a subsequent request still works.
    tokio::time::sleep(Duration::from_millis(1_000)).await;
    let outcome = client
        .call_tool("echo", json!({"text": "after-timeout"}))
        .await;
    assert!(!outcome.is_error, "{}", outcome.text());
    assert_eq!(outcome.text(), "after-timeout");
}

#[tokio::test]
async fn server_exit_wakes_pending_requests_promptly() {
    let options = McpClientOptions {
        request_timeout: Duration::from_secs(10),
        ..McpClientOptions::default()
    };
    let client = connect_with(options).await;

    let started = Instant::now();
    let outcome = tokio::time::timeout(Duration::from_secs(3), client.call_tool("exit", json!({})))
        .await
        .expect("server exit must wake the pending request before the timeout");

    assert!(outcome.is_error);
    assert!(
        outcome.text().contains("stdout closed") || outcome.text().contains("stdout"),
        "{}",
        outcome.text()
    );
    assert!(started.elapsed() < Duration::from_secs(3));
    assert_eq!(client.pending_request_count().await, 0);
}

#[tokio::test]
async fn unknown_ids_and_notifications_do_not_panic() {
    let client = connect_with(McpClientOptions::default()).await;
    let outcome = client.call_tool("noise", json!({})).await;

    assert!(!outcome.is_error, "{}", outcome.text());
    assert_eq!(outcome.text(), "noise-ok");
    assert_eq!(client.pending_request_count().await, 0);
}

#[tokio::test]
async fn server_requests_receive_method_not_found() {
    let client = connect_with(McpClientOptions::default()).await;
    let outcome = client.call_tool("server_request", json!({})).await;

    assert!(!outcome.is_error, "{}", outcome.text());
    assert!(
        outcome.text().contains("-32601"),
        "mock reported: {}",
        outcome.text()
    );
}

#[tokio::test]
async fn malformed_server_output_is_a_protocol_error() {
    let client = connect_with(McpClientOptions::default()).await;
    let outcome = tokio::time::timeout(
        Duration::from_secs(2),
        client.call_tool("malformed", json!({})),
    )
    .await
    .expect("protocol error must resolve pending requests promptly");

    assert!(outcome.is_error);
    assert!(
        outcome.text().contains("protocol error"),
        "{}",
        outcome.text()
    );
    assert_eq!(client.pending_request_count().await, 0);
}

#[tokio::test]
async fn json_rpc_error_response_is_mapped_to_transport_error() {
    let client = connect_with(McpClientOptions::default()).await;
    let error = client
        .request("mock/unsupported", json!({}))
        .await
        .unwrap_err();

    assert!(error.to_string().contains("-32601"), "{error}");
    assert!(error.to_string().contains("Method not found"), "{error}");
    assert_eq!(client.pending_request_count().await, 0);
}

#[tokio::test]
async fn stderr_capture_is_bounded() {
    let mut spec = mock_spec();
    spec.env
        .insert("MOCK_STDERR_BYTES".to_owned(), "16384".to_owned());

    let mut process = process::spawn_with_stderr_capacity(&spec, 64)
        .await
        .expect("spawn mock server");

    let deadline = Instant::now() + Duration::from_secs(2);
    let mut snapshot = String::new();
    while Instant::now() < deadline {
        snapshot = process.stderr_snapshot();
        if !snapshot.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    assert!(!snapshot.is_empty(), "stderr should be captured");
    assert_eq!(process.stderr_capacity_bytes(), 64);
    assert!(
        snapshot.len() <= 64,
        "stderr snapshot was not bounded: {} bytes",
        snapshot.len()
    );

    process.kill().await.expect("kill mock server");
}
