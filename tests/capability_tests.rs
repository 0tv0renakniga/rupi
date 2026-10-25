use std::collections::BTreeMap;
use std::path::PathBuf;

use futures_util::future::join_all;
use local_agent_harness::mcp::{
    AgentTool, CapabilityError, CapabilityManifest, McpClient, McpClientOptions, McpPool,
    ProcessSpec, ToolDispatcher, ToolOutcome, ToolOutcomeKind,
};
use local_agent_harness::policy::Policy;
use serde_json::{Value, json};

const MOCK_SERVER: &str = env!("CARGO_BIN_EXE_mock_mcp_server");

fn mock_spec(extra: &[(&str, &str)]) -> ProcessSpec {
    let home = std::env::temp_dir().join("local-agent-harness-capability-test-home");
    std::fs::create_dir_all(&home).expect("create controlled HOME");

    let mut env = BTreeMap::new();
    env.insert("HOME".to_owned(), home.to_string_lossy().into_owned());
    for (key, value) in extra {
        env.insert((*key).to_owned(), (*value).to_owned());
    }

    ProcessSpec {
        program: PathBuf::from(MOCK_SERVER),
        args: Vec::new(),
        cwd: Some(std::env::current_dir().expect("current directory")),
        env,
    }
}

fn advertised_tools(server: &str) -> &'static str {
    match server {
        "filesystem" => "read_file,write_file,apply_patch",
        "shell" => "run",
        "git" => "status,diff",
        other => panic!("unknown mock capability server `{other}`"),
    }
}

async fn connect(name: &str, extra: &[(&str, &str)]) -> McpClient {
    McpClient::connect(name, &mock_spec(extra), McpClientOptions::default())
        .await
        .expect("mock MCP lifecycle should succeed")
}

async fn connect_capability(name: &str, extra: &[(&str, &str)]) -> McpClient {
    let mut env = vec![("MOCK_ADVERTISED_TOOLS", advertised_tools(name))];
    env.extend_from_slice(extra);
    connect(name, &env).await
}

fn policy() -> Policy {
    Policy::new(std::env::current_dir().expect("current directory")).expect("policy")
}

async fn full_dispatcher() -> ToolDispatcher {
    let capabilities = CapabilityManifest::default();
    let mut pool = McpPool::new();
    pool.install(&capabilities, connect_capability("filesystem", &[]).await)
        .expect("install filesystem mock");
    pool.install(&capabilities, connect_capability("shell", &[]).await)
        .expect("install shell mock");
    pool.install(&capabilities, connect_capability("git", &[]).await)
        .expect("install git mock");
    capabilities
        .validate_pool(&pool)
        .expect("pool must exactly match the fixed manifest");
    ToolDispatcher::new(policy(), capabilities, pool)
}

async fn dispatcher_with_env(server: &str, extra: &[(&str, &str)]) -> ToolDispatcher {
    let capabilities = CapabilityManifest::default();
    let mut pool = McpPool::new();

    for name in ["filesystem", "shell", "git"] {
        let env = if name == server { extra } else { &[] };
        pool.install(&capabilities, connect_capability(name, env).await)
            .expect("install mock");
    }

    ToolDispatcher::new(policy(), capabilities, pool)
}

fn routed_arguments(outcome: &ToolOutcome) -> Value {
    assert!(!outcome.is_error, "{}", outcome.text());
    let value: Value = serde_json::from_str(&outcome.text()).expect("mock route JSON");
    value["arguments"].clone()
}

#[tokio::test]
async fn dispatches_every_canonical_capability_to_its_fixed_route() {
    let dispatcher = full_dispatcher().await;

    let read = dispatcher
        .execute(
            "read-1",
            &AgentTool::ReadFile {
                path: "src/main.rs".into(),
                line_start: Some(3),
                line_end: Some(9),
            },
        )
        .await;
    assert_eq!(read.tool_call_id, "read-1");
    assert_eq!(read.kind, ToolOutcomeKind::Success);
    assert_eq!(
        routed_arguments(&read),
        json!({"path": "src/main.rs", "line_start": 3, "line_end": 9})
    );

    let write = dispatcher
        .execute(
            "write-1",
            &AgentTool::WriteFile {
                path: "notes.txt".into(),
                content: "hello\nworld".into(),
                expected_sha256: Some("abc123".into()),
            },
        )
        .await;
    assert_eq!(write.kind, ToolOutcomeKind::Success);
    assert_eq!(
        routed_arguments(&write),
        json!({
            "path": "notes.txt",
            "content": "hello\nworld",
            "expected_sha256": "abc123"
        })
    );

    let patch = dispatcher
        .execute(
            "patch-1",
            &AgentTool::ApplyPatch {
                path: "src/lib.rs".into(),
                unified_diff: "@@ -1 +1 @@\n-old\n+new\n".into(),
                expected_sha256: None,
            },
        )
        .await;
    assert_eq!(patch.kind, ToolOutcomeKind::Success);
    assert_eq!(
        routed_arguments(&patch),
        json!({"path": "src/lib.rs", "unified_diff": "@@ -1 +1 @@\n-old\n+new\n"})
    );

    let run = dispatcher
        .execute(
            "run-1",
            &AgentTool::RunCommand {
                program: "cargo".into(),
                args: vec!["test".into(), "--all-targets".into()],
                cwd: Some("src".into()),
                timeout_secs: Some(60),
            },
        )
        .await;
    assert_eq!(run.kind, ToolOutcomeKind::Success);
    assert_eq!(
        routed_arguments(&run),
        json!({
            "program": "cargo",
            "args": ["test", "--all-targets"],
            "cwd": "src",
            "timeout_secs": 60
        })
    );

    let status = dispatcher.execute("status-1", &AgentTool::GitStatus).await;
    assert_eq!(status.kind, ToolOutcomeKind::Success);
    assert_eq!(routed_arguments(&status), json!({}));
    assert!(!status.text().contains("staged"));

    let diff = dispatcher
        .execute("diff-1", &AgentTool::GitDiff { staged: true })
        .await;
    assert_eq!(diff.kind, ToolOutcomeKind::Success);
    assert_eq!(routed_arguments(&diff), json!({"staged": true}));
}

#[tokio::test]
async fn missing_server_is_a_clean_unavailable_outcome() {
    let dispatcher = ToolDispatcher::new(policy(), CapabilityManifest::default(), McpPool::new());

    let outcome = dispatcher
        .execute(
            "read-missing",
            &AgentTool::ReadFile {
                path: "src/main.rs".into(),
                line_start: None,
                line_end: None,
            },
        )
        .await;

    assert!(outcome.is_error);
    assert_eq!(outcome.kind, ToolOutcomeKind::CapabilityUnavailable);
    assert!(outcome.text().contains("capability unavailable"));
    assert!(outcome.text().contains("filesystem"));
}

#[tokio::test]
async fn advertised_tool_mismatch_is_refused_at_setup_and_at_dispatch() {
    let capabilities = CapabilityManifest::default();
    let client = connect("filesystem", &[("MOCK_ADVERTISED_TOOLS", "echo,fail")]).await;
    let mut pool = McpPool::new();

    let error = pool
        .install(&capabilities, client)
        .expect_err("server without required routes must be refused");
    assert!(matches!(
        error,
        CapabilityError::MissingTool {
            capability: "read_file",
            ..
        }
    ));
    assert!(pool.is_empty());

    let over_broad = connect(
        "filesystem",
        &[(
            "MOCK_ADVERTISED_TOOLS",
            "read_file,write_file,apply_patch,echo",
        )],
    )
    .await;
    let error = McpPool::new()
        .install(&capabilities, over_broad)
        .expect_err("unexpected advertised tools must be refused");
    assert!(matches!(
        error,
        CapabilityError::UnexpectedTool {
            server,
            tool,
        } if server == "filesystem" && tool == "echo"
    ));

    let mut pool = McpPool::new();
    pool.insert(
        "filesystem",
        connect("filesystem", &[("MOCK_ADVERTISED_TOOLS", "echo,fail")]).await,
    );
    let dispatcher = ToolDispatcher::new(policy(), capabilities, pool);
    let outcome = dispatcher
        .execute(
            "read-mismatch",
            &AgentTool::ReadFile {
                path: "src/main.rs".into(),
                line_start: None,
                line_end: None,
            },
        )
        .await;

    assert!(outcome.is_error);
    assert_eq!(outcome.kind, ToolOutcomeKind::CapabilityUnavailable);
    assert!(outcome.text().contains("does not advertise"));
}

#[tokio::test]
async fn policy_denial_happens_before_pool_lookup_or_mcp_request() {
    let dispatcher = ToolDispatcher::new(policy(), CapabilityManifest::default(), McpPool::new());

    for path in [".env", "../outside.txt", "/etc/passwd"] {
        let outcome = dispatcher
            .execute(
                "read-denied",
                &AgentTool::ReadFile {
                    path: path.into(),
                    line_start: None,
                    line_end: None,
                },
            )
            .await;

        assert!(outcome.is_error, "{path}");
        assert_eq!(outcome.kind, ToolOutcomeKind::PolicyDenied, "{path}");
        assert!(outcome.text().contains("policy denied action"), "{path}");
        assert!(
            !outcome.text().contains("capability unavailable"),
            "policy must be checked before routing: {}",
            outcome.text()
        );
    }
}

#[tokio::test]
async fn policy_rejects_unsafe_commands_and_unbounded_timeouts() {
    let dispatcher = full_dispatcher().await;

    let cases = [
        AgentTool::RunCommand {
            program: "rm".into(),
            args: vec!["-rf".into(), "target".into()],
            cwd: None,
            timeout_secs: None,
        },
        AgentTool::RunCommand {
            program: "sudo".into(),
            args: vec!["cargo".into(), "test".into()],
            cwd: None,
            timeout_secs: None,
        },
        AgentTool::RunCommand {
            program: "cargo".into(),
            args: vec!["test; rm -rf /".into()],
            cwd: None,
            timeout_secs: None,
        },
        AgentTool::RunCommand {
            program: "cargo".into(),
            args: vec!["test".into()],
            cwd: Some("../outside".into()),
            timeout_secs: None,
        },
        AgentTool::RunCommand {
            program: "cargo".into(),
            args: vec!["test".into()],
            cwd: None,
            timeout_secs: Some(901),
        },
        AgentTool::RunCommand {
            program: "cargo".into(),
            args: vec!["test".into()],
            cwd: None,
            timeout_secs: Some(0),
        },
    ];

    for tool in cases {
        let outcome = dispatcher.execute("run-denied", &tool).await;
        assert!(outcome.is_error, "{tool:?}");
        assert_eq!(outcome.kind, ToolOutcomeKind::PolicyDenied, "{tool:?}");
    }
}

#[tokio::test]
async fn direct_mcp_tool_cannot_bypass_capability_routing() {
    let dispatcher = full_dispatcher().await;
    let outcome = dispatcher
        .execute(
            "bypass-1",
            &AgentTool::McpTool {
                server: "filesystem".into(),
                name: "read_file".into(),
                arguments: json!({"path": "src/main.rs"}),
            },
        )
        .await;

    assert!(outcome.is_error);
    assert_eq!(outcome.kind, ToolOutcomeKind::PolicyDenied);
    assert!(outcome.text().contains("direct MCP tool"));
    assert!(!outcome.text().contains("capability unavailable"));
}

#[tokio::test]
async fn transport_failure_is_readable_and_classified() {
    let dispatcher = dispatcher_with_env("filesystem", &[("MOCK_EXIT_TOOLS", "read_file")]).await;
    let outcome = dispatcher
        .execute(
            "read-exit",
            &AgentTool::ReadFile {
                path: "src/main.rs".into(),
                line_start: None,
                line_end: None,
            },
        )
        .await;

    assert!(outcome.is_error);
    assert_eq!(outcome.kind, ToolOutcomeKind::TransportFailure);
    assert!(
        outcome.text().contains("MCP tools/call failed"),
        "{}",
        outcome.text()
    );
}

#[tokio::test]
async fn server_declared_is_error_is_classified_as_server_error() {
    let dispatcher = dispatcher_with_env("shell", &[("MOCK_ERROR_TOOLS", "run")]).await;
    let outcome = dispatcher
        .execute(
            "run-fail",
            &AgentTool::RunCommand {
                program: "cargo".into(),
                args: vec!["test".into()],
                cwd: None,
                timeout_secs: None,
            },
        )
        .await;

    assert!(outcome.is_error);
    assert_eq!(outcome.kind, ToolOutcomeKind::ServerError);
    assert!(outcome.text().contains("server-declared failure"));
    assert!(outcome.text().contains("\"program\":\"cargo\""));
    assert!(outcome.text().contains("\"args\":[\"test\"]"));
}

#[tokio::test]
async fn concurrent_dispatch_does_not_cross_talk() {
    let dispatcher = full_dispatcher().await;
    let tools = (0..12)
        .map(|index| AgentTool::ReadFile {
            path: format!("src/file-{index}.rs"),
            line_start: None,
            line_end: None,
        })
        .collect::<Vec<_>>();

    let futures = tools
        .iter()
        .enumerate()
        .map(|(index, tool)| dispatcher.execute(format!("call-{index}"), tool));
    let outcomes = join_all(futures).await;

    for (index, outcome) in outcomes.iter().enumerate() {
        assert_eq!(outcome.tool_call_id, format!("call-{index}"));
        assert!(!outcome.is_error, "{}", outcome.text());
        assert_eq!(outcome.kind, ToolOutcomeKind::Success);
        assert_eq!(
            routed_arguments(outcome),
            json!({"path": format!("src/file-{index}.rs")})
        );
    }
}

#[tokio::test]
async fn dispatcher_output_is_bounded_with_head_and_tail() {
    let dispatcher = dispatcher_with_env("filesystem", &[("MOCK_TOOL_OUTPUT_BYTES", "20000")])
        .await
        .with_tool_output_limit(512);

    let outcome = dispatcher
        .execute(
            "read-large",
            &AgentTool::ReadFile {
                path: "src/main.rs".into(),
                line_start: None,
                line_end: None,
            },
        )
        .await;

    assert!(!outcome.is_error, "{}", outcome.text());
    let text = outcome.text();
    assert!(
        text.contains("bytes omitted"),
        "{}",
        &text[..text.len().min(200)]
    );
    assert!(text.starts_with('{'), "{text}");
    assert!(text.contains("\"tool\":\"read_file\""), "{text}");
    assert!(text.ends_with('X'), "{text}");
    assert!(
        text.len() < 2_000,
        "bounded output was {} bytes",
        text.len()
    );
}
