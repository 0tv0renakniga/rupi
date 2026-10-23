use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use local_agent_harness::config::{
    McpProfile, McpRuntimeConfig, McpSandboxConfig, McpServerConfig, McpServerKind,
};
use local_agent_harness::mcp::client::DEFAULT_MCP_PROTOCOL_VERSION;
use local_agent_harness::mcp::process::DEFAULT_STDERR_CAPACITY_BYTES;
use local_agent_harness::mcp::{AgentTool, McpRuntime, ToolOutcomeKind};
use local_agent_harness::policy::Policy;
use serde_json::{Value, json};

const MOCK_SERVER: &str = env!("CARGO_BIN_EXE_mock_mcp_server");
const HARNESS_BIN: &str = env!("CARGO_BIN_EXE_local-agent-harness");
static TEST_SEQUENCE: AtomicU64 = AtomicU64::new(1);

fn test_root(label: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "local-agent-harness-runtime-{label}-{}-{}",
        std::process::id(),
        TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&path).expect("create test root");
    path
}

fn mock_server(root: &Path, tools: &str, extra: &[&str]) -> McpServerConfig {
    let mut args = vec!["--tools".to_owned(), tools.to_owned()];
    args.extend(extra.iter().map(|arg| (*arg).to_owned()));
    McpServerConfig {
        enabled: true,
        executable: PathBuf::from(MOCK_SERVER),
        args,
        working_directory: root.to_path_buf(),
        protocol_version: DEFAULT_MCP_PROTOCOL_VERSION.into(),
        startup_timeout: Duration::from_secs(5),
        request_timeout: Duration::from_secs(5),
        stderr_capacity_bytes: DEFAULT_STDERR_CAPACITY_BYTES,
        sandbox: McpSandboxConfig::None,
    }
}

fn workspace_config(root: &Path, pids: &[(&str, &Path)]) -> McpRuntimeConfig {
    let pid = |name: &str| {
        pids.iter()
            .find(|(slot, _)| *slot == name)
            .map(|(_, path)| path.to_str().unwrap())
            .expect("pid path for slot")
    };

    McpRuntimeConfig {
        profile: McpProfile::WorkspaceMutation,
        filesystem: Some(mock_server(
            root,
            "read_file,write_file,apply_patch",
            &["--pid-file", pid("filesystem")],
        )),
        git: Some(mock_server(
            root,
            "status,diff",
            &["--pid-file", pid("git")],
        )),
        shell: Some(mock_server(root, "run", &["--pid-file", pid("shell")])),
        path: None,
    }
}

async fn runtime(
    root: &Path,
    config: &McpRuntimeConfig,
) -> local_agent_harness::error::Result<McpRuntime> {
    McpRuntime::start(
        config,
        root,
        Policy::default_for_root(root.to_path_buf()),
        4_000,
    )
    .await
}

fn routed_arguments(outcome: &local_agent_harness::mcp::ToolOutcome) -> Value {
    assert!(!outcome.is_error, "{}", outcome.text());
    let value: Value = serde_json::from_str(&outcome.text()).expect("mock route JSON");
    value["arguments"].clone()
}

async fn wait_for_process_exit(pid_file: &Path) -> bool {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let raw = std::fs::read_to_string(pid_file).expect("pid file");
        let pid = raw.trim();
        let proc_path = PathBuf::from(format!("/proc/{pid}"));
        if !proc_path.exists() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn starts_all_fixed_servers_routes_canonical_calls_and_shuts_down() {
    let root = test_root("workspace");
    let fs_pid = root.join("fs.pid");
    let git_pid = root.join("git.pid");
    let shell_pid = root.join("shell.pid");
    let config = workspace_config(
        &root,
        &[
            ("filesystem", &fs_pid),
            ("git", &git_pid),
            ("shell", &shell_pid),
        ],
    );

    let runtime = runtime(&root, &config).await.expect("runtime starts");
    assert_eq!(runtime.profile(), McpProfile::WorkspaceMutation);
    assert_eq!(
        runtime.started_servers(),
        &[
            McpServerKind::Filesystem,
            McpServerKind::Git,
            McpServerKind::Shell
        ]
    );
    assert!(
        runtime
            .warnings()
            .iter()
            .any(|warning| warning.contains("network isolation is not provided"))
    );
    let dispatcher = runtime.dispatcher();

    let read = dispatcher
        .execute(
            "read",
            &AgentTool::ReadFile {
                path: "src/main.rs".into(),
                line_start: None,
                line_end: None,
            },
        )
        .await;
    assert_eq!(read.kind, ToolOutcomeKind::Success, "{}", read.text());
    assert_eq!(routed_arguments(&read), json!({"path": "src/main.rs"}));

    let write = dispatcher
        .execute(
            "write",
            &AgentTool::WriteFile {
                path: "notes.txt".into(),
                content: "hello".into(),
                expected_sha256: None,
            },
        )
        .await;
    assert_eq!(write.kind, ToolOutcomeKind::Success, "{}", write.text());

    let patch = dispatcher
        .execute(
            "patch",
            &AgentTool::ApplyPatch {
                path: "notes.txt".into(),
                unified_diff: "@@ -1 +1 @@".into(),
                expected_sha256: None,
            },
        )
        .await;
    assert_eq!(patch.kind, ToolOutcomeKind::Success, "{}", patch.text());

    let status = dispatcher.execute("status", &AgentTool::GitStatus).await;
    assert_eq!(status.kind, ToolOutcomeKind::Success, "{}", status.text());
    assert!(status.text().contains("\"tool\":\"status\""));

    let diff = dispatcher
        .execute("diff", &AgentTool::GitDiff { staged: true })
        .await;
    assert_eq!(diff.kind, ToolOutcomeKind::Success, "{}", diff.text());
    assert_eq!(routed_arguments(&diff), json!({"staged": true}));

    let command = dispatcher
        .execute(
            "run",
            &AgentTool::RunCommand {
                program: "cargo".into(),
                args: vec!["test".into(), "--quiet".into()],
                cwd: Some("crates/core".into()),
                timeout_secs: Some(30),
            },
        )
        .await;
    assert_eq!(command.kind, ToolOutcomeKind::Success, "{}", command.text());
    assert_eq!(
        routed_arguments(&command),
        json!({
            "program": "cargo",
            "args": ["test", "--quiet"],
            "cwd": "crates/core",
            "timeout_secs": 30
        })
    );

    let direct = dispatcher
        .execute(
            "direct",
            &AgentTool::McpTool {
                server: "git".into(),
                name: "status".into(),
                arguments: json!({}),
            },
        )
        .await;
    assert_eq!(direct.kind, ToolOutcomeKind::PolicyDenied);

    drop(dispatcher);
    runtime
        .shutdown(Duration::from_secs(2))
        .await
        .expect("runtime shuts down");

    assert!(
        wait_for_process_exit(&fs_pid).await,
        "filesystem still running"
    );
    assert!(wait_for_process_exit(&git_pid).await, "git still running");
    assert!(
        wait_for_process_exit(&shell_pid).await,
        "shell still running"
    );
}

#[tokio::test]
async fn read_only_profile_denies_mutation_and_disabled_capabilities() {
    let root = test_root("read-only");
    let config = McpRuntimeConfig {
        profile: McpProfile::ReadOnly,
        filesystem: Some(mock_server(&root, "read_file,write_file,apply_patch", &[])),
        git: Some(mock_server(&root, "status,diff", &[])),
        shell: None,
        path: None,
    };
    let runtime = runtime(&root, &config).await.expect("runtime starts");
    let dispatcher = runtime.dispatcher();

    let read = dispatcher
        .execute(
            "read",
            &AgentTool::ReadFile {
                path: "src/main.rs".into(),
                line_start: None,
                line_end: None,
            },
        )
        .await;
    assert_eq!(read.kind, ToolOutcomeKind::Success, "{}", read.text());

    let write = dispatcher
        .execute(
            "write",
            &AgentTool::WriteFile {
                path: "notes.txt".into(),
                content: "no".into(),
                expected_sha256: None,
            },
        )
        .await;
    assert_eq!(write.kind, ToolOutcomeKind::CapabilityUnavailable);
    assert!(
        write
            .text()
            .contains("not enabled by the configured MCP profile")
    );

    let command = dispatcher
        .execute(
            "run",
            &AgentTool::RunCommand {
                program: "cargo".into(),
                args: vec!["test".into()],
                cwd: None,
                timeout_secs: None,
            },
        )
        .await;
    assert_eq!(command.kind, ToolOutcomeKind::CapabilityUnavailable);

    drop(dispatcher);
    runtime.shutdown(Duration::from_secs(2)).await.unwrap();
}

#[tokio::test]
async fn child_environment_is_sanitized_and_not_inherited() {
    let root = test_root("environment");
    let env_file = root.join("child-env.json");
    let fs_args = [
        "--env-file",
        env_file.to_str().unwrap(),
        "--env-names",
        "HOME,PATH,CARGO_PKG_NAME,HTTP_PROXY,HARNESS_PARENT_ONLY",
    ];
    let config = McpRuntimeConfig {
        profile: McpProfile::ReadOnly,
        filesystem: Some(mock_server(&root, "read_file", &fs_args)),
        git: Some(mock_server(&root, "status,diff", &[])),
        shell: None,
        path: None,
    };

    let runtime = runtime(&root, &config).await.expect("runtime starts");
    let values: Value =
        serde_json::from_str(&std::fs::read_to_string(&env_file).expect("env file"))
            .expect("env JSON");
    assert!(values["HOME"].is_string());
    assert_ne!(values["HOME"], Value::Null);
    assert_eq!(values["PATH"], expected_child_path());
    assert_eq!(values["CARGO_PKG_NAME"], Value::Null);
    assert_eq!(values["HTTP_PROXY"], Value::Null);
    assert_eq!(values["HARNESS_PARENT_ONLY"], Value::Null);

    runtime.shutdown(Duration::from_secs(2)).await.unwrap();
}

fn expected_child_path() -> Value {
    if cfg!(windows) {
        Value::String("C:\\Windows\\System32;C:\\Windows".into())
    } else {
        Value::String("/usr/local/bin:/usr/bin:/bin".into())
    }
}

#[tokio::test]
async fn startup_timeout_is_bounded_and_kills_the_child() {
    let root = test_root("startup-timeout");
    let git_pid = root.join("git.pid");
    let mut git = mock_server(
        &root,
        "status,diff",
        &[
            "--pid-file",
            git_pid.to_str().unwrap(),
            "--initialize-delay-ms",
            "5000",
            "--startup-stderr-bytes",
            "0",
            "--startup-stderr-text",
            "token=STARTUP_SECRET",
        ],
    );
    git.startup_timeout = Duration::from_millis(100);
    let config = McpRuntimeConfig {
        profile: McpProfile::ReadOnly,
        filesystem: Some(mock_server(&root, "read_file", &[])),
        git: Some(git),
        shell: None,
        path: None,
    };

    let error = runtime(&root, &config)
        .await
        .err()
        .expect("startup timeout must fail startup");
    assert!(error.to_string().contains("timed out"), "{error}");
    assert!(
        wait_for_process_exit(&git_pid).await,
        "timed-out child leaked"
    );
}

#[tokio::test]
async fn capability_mismatch_fails_closed_and_kills_every_started_child() {
    let root = test_root("mismatch");
    let fs_pid = root.join("fs.pid");
    let git_pid = root.join("git.pid");
    let config = McpRuntimeConfig {
        profile: McpProfile::ReadOnly,
        filesystem: Some(mock_server(
            &root,
            "read_file",
            &["--pid-file", fs_pid.to_str().unwrap()],
        )),
        git: Some(mock_server(
            &root,
            "status,diff,echo",
            &["--pid-file", git_pid.to_str().unwrap()],
        )),
        shell: None,
        path: None,
    };

    let error = runtime(&root, &config)
        .await
        .err()
        .expect("unexpected advertised tool must fail startup");
    assert!(error.to_string().contains("unexpected tool"), "{error}");
    assert!(
        wait_for_process_exit(&fs_pid).await,
        "filesystem child leaked"
    );
    assert!(wait_for_process_exit(&git_pid).await, "git child leaked");
}

#[tokio::test]
async fn startup_diagnostics_are_bounded_and_redacted() {
    let root = test_root("stderr");
    let mut git = mock_server(
        &root,
        "status,diff,echo",
        &[
            "--startup-stderr-bytes",
            "100000",
            "--startup-stderr-text",
            "token=SUPER_SECRET",
        ],
    );
    git.stderr_capacity_bytes = 200_000;
    let config = McpRuntimeConfig {
        profile: McpProfile::ReadOnly,
        filesystem: Some(mock_server(&root, "read_file", &[])),
        git: Some(git),
        shell: None,
        path: None,
    };

    let error = runtime(&root, &config)
        .await
        .err()
        .expect("capability mismatch must fail");
    let message = error.to_string();
    assert!(!message.contains("SUPER_SECRET"), "{message}");
    assert!(message.contains("[REDACTED]"), "{message}");
    assert!(
        message.len() < 4_096,
        "diagnostic grew to {}",
        message.len()
    );
}

#[test]
fn headless_startup_checks_mcp_without_model_or_jev_services() {
    let output = std::process::Command::new(HARNESS_BIN)
        .arg("--headless")
        .env_clear()
        .output()
        .expect("run harness headless");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("mcp startup ok: profile=none servers=0"),
        "stdout: {stdout}"
    );
}
