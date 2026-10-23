use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::time::Duration;

use local_agent_harness::config::{
    McpProfile, McpRuntimeConfig, McpSandboxConfig, McpServerConfig,
};
use local_agent_harness::mcp::client::DEFAULT_MCP_PROTOCOL_VERSION;
use local_agent_harness::mcp::fs_server::PROTOCOL_VERSION;
use local_agent_harness::mcp::process::DEFAULT_STDERR_CAPACITY_BYTES;
use local_agent_harness::mcp::{AgentTool, McpRuntime};
use local_agent_harness::policy::Policy;
use serde_json::{Value, json};
use uuid::Uuid;

const FS_SERVER: &str = env!("CARGO_BIN_EXE_filesystem_mcp");
const MOCK_SERVER: &str = env!("CARGO_BIN_EXE_mock_mcp_server");

struct TempRepo {
    path: PathBuf,
}

impl TempRepo {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!("rupi-fs-mcp-{label}-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&path).expect("create temp repository");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn write(&self, relative: &str, content: &str) -> PathBuf {
        let path = self.path.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create parent");
        }
        std::fs::write(&path, content).expect("write fixture");
        path
    }
}

impl Drop for TempRepo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

struct ServerProcess {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<std::process::ChildStdout>,
}

impl ServerProcess {
    fn start(root: &Path) -> Self {
        let mut child = Command::new(FS_SERVER)
            .arg("--root")
            .arg(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn filesystem MCP server");
        let stdin = child.stdin.take().expect("server stdin");
        let stdout = BufReader::new(child.stdout.take().expect("server stdout"));
        Self {
            child,
            stdin,
            stdout,
        }
    }

    fn request(&mut self, request: Value) -> Value {
        writeln!(self.stdin, "{request}").expect("write protocol request");
        self.stdin.flush().expect("flush protocol request");
        let mut line = String::new();
        self.stdout
            .read_line(&mut line)
            .expect("read protocol response");
        serde_json::from_str(&line).expect("parse protocol response")
    }

    fn finish(mut self) {
        drop(self.stdin);
        let status = self.child.wait().expect("wait for server exit");
        assert!(status.success(), "server exited with {status}");
    }
}

#[test]
fn binary_speaks_the_mcp_contract_and_is_read_only() {
    let repo = TempRepo::new("protocol");
    let path = repo.write("README.md", "hello\nworld\n");
    let before = std::fs::read(&path).expect("read before");

    let mut server = ServerProcess::start(repo.path());

    let initialized = server.request(json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": {"name": "filesystem-mcp-test", "version": "0"}
        }
    }));
    assert_eq!(initialized["result"]["protocolVersion"], PROTOCOL_VERSION);
    assert_eq!(
        initialized["result"]["serverInfo"]["name"],
        "filesystem-mcp"
    );

    writeln!(
        server.stdin,
        "{}",
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"})
    )
    .expect("write initialized notification");
    server.stdin.flush().expect("flush notification");

    let tools = server.request(json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/list",
        "params": {}
    }));
    let advertised = tools["result"]["tools"].as_array().expect("tools array");
    assert_eq!(advertised.len(), 1);
    assert_eq!(advertised[0]["name"], "read_file");

    let read = server.request(json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/call",
        "params": {
            "name": "read_file",
            "arguments": {"path": "README.md", "line_start": 2, "line_end": 2}
        }
    }));
    assert_eq!(read["result"]["isError"], false);
    assert_eq!(read["result"]["content"][0]["text"], "world\n");

    let traversal = server.request(json!({
        "jsonrpc": "2.0",
        "id": 4,
        "method": "tools/call",
        "params": {
            "name": "read_file",
            "arguments": {"path": "../outside"}
        }
    }));
    assert_eq!(traversal["result"]["isError"], true);
    assert!(
        traversal["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("traversal")
    );

    let write = server.request(json!({
        "jsonrpc": "2.0",
        "id": 5,
        "method": "tools/call",
        "params": {
            "name": "write_file",
            "arguments": {"path": "README.md", "content": "changed"}
        }
    }));
    assert_eq!(write["result"]["isError"], true);
    assert!(
        write["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("unknown tool")
    );

    server.finish();
    assert_eq!(std::fs::read(&path).expect("read after"), before);
}

#[cfg(unix)]
#[test]
fn binary_rejects_symlink_escape() {
    use std::os::unix::fs::symlink;

    let repo = TempRepo::new("symlink");
    let outside = TempRepo::new("outside");
    let outside_file = outside.write("secret.txt", "outside\n");
    symlink(&outside_file, repo.path().join("escape.txt")).expect("create escape symlink");

    let mut server = ServerProcess::start(repo.path());
    let response = server.request(json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "read_file",
            "arguments": {"path": "escape.txt"}
        }
    }));
    assert_eq!(response["result"]["isError"], true);
    assert!(
        response["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("escapes the repository root")
    );
    server.finish();
}

fn find_bwrap() -> Option<PathBuf> {
    ["/usr/bin/bwrap", "/bin/bwrap"]
        .into_iter()
        .map(PathBuf::from)
        .find(|path| path.is_file())
}

fn require_bwrap_or_skip(test: &str) -> Option<PathBuf> {
    match find_bwrap() {
        Some(bwrap) => Some(bwrap),
        None if std::env::var_os("HARNESS_REQUIRE_BWRAP").is_some() => {
            panic!("HARNESS_REQUIRE_BWRAP is set but bubblewrap is not installed")
        }
        None => {
            eprintln!(
                "SKIPPED: {test} not executed because bwrap is unavailable; set HARNESS_REQUIRE_BWRAP=1 to fail instead"
            );
            None
        }
    }
}

fn server_config(
    root: &Path,
    executable: &str,
    args: Vec<String>,
    bwrap: &Path,
) -> McpServerConfig {
    McpServerConfig {
        enabled: true,
        executable: PathBuf::from(executable),
        args,
        working_directory: root.to_path_buf(),
        protocol_version: DEFAULT_MCP_PROTOCOL_VERSION.into(),
        startup_timeout: Duration::from_secs(10),
        request_timeout: Duration::from_secs(10),
        stderr_capacity_bytes: DEFAULT_STDERR_CAPACITY_BYTES,
        sandbox: McpSandboxConfig::Bubblewrap {
            executable: bwrap.to_path_buf(),
        },
    }
}

#[tokio::test]
async fn read_only_filesystem_mcp_runs_under_bubblewrap() {
    let Some(bwrap) = require_bwrap_or_skip("read-only filesystem MCP Bubblewrap E2E") else {
        return;
    };

    let repo = TempRepo::new("bubblewrap");
    let path = repo.write("README.md", "contained\n");
    let before = std::fs::read(&path).expect("read before");

    let config = McpRuntimeConfig {
        profile: McpProfile::ReadOnly,
        filesystem: Some(server_config(
            repo.path(),
            FS_SERVER,
            vec!["--root".into(), repo.path().to_string_lossy().into_owned()],
            &bwrap,
        )),
        git: Some(server_config(
            repo.path(),
            MOCK_SERVER,
            vec!["--tools".into(), "status,diff".into()],
            &bwrap,
        )),
        shell: None,
        path: None,
        allow_unsandboxed: false,
        digest_allowlist: None,
    };

    let runtime = McpRuntime::start(
        &config,
        repo.path(),
        Policy::default_for_root(repo.path()),
        4_000,
    )
    .await
    .expect("start Bubblewrap-contained read-only runtime");
    assert!(
        runtime
            .warnings()
            .iter()
            .any(|warning| warning.contains("bubblewrap network namespaces"))
    );

    let outcome = runtime
        .dispatcher()
        .execute(
            "call-1",
            &AgentTool::ReadFile {
                path: "README.md".into(),
                line_start: None,
                line_end: None,
            },
        )
        .await;
    assert!(!outcome.is_error, "{}", outcome.text());
    assert_eq!(outcome.text(), "contained\n");

    runtime
        .shutdown(Duration::from_secs(2))
        .await
        .expect("shutdown runtime");
    assert_eq!(std::fs::read(&path).expect("read after"), before);
}
