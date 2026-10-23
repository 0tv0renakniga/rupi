use std::collections::{BTreeMap, VecDeque};
use std::path::PathBuf;
use std::process::{ExitStatus, Stdio};
use std::sync::{Arc, Mutex};

use tokio::io::AsyncReadExt;
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};
use tokio::task::JoinHandle;

use crate::error::{AgentError, Result};

/// Default bound for captured MCP server stderr, in bytes.
pub const DEFAULT_STDERR_CAPACITY_BYTES: usize = 16 * 1024;

/// Fixed process description for an allowlisted MCP server.
///
/// The executable path must be absolute and vetted by caller/configuration
/// code. Arguments are a vector, never a shell command string. `env` is the
/// complete child environment: it is not merged with the harness environment.
/// Callers that need `HOME` must set it to a harness-controlled directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessSpec {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub env: BTreeMap<String, String>,
}

#[derive(Debug)]
struct BoundedBuffer {
    bytes: VecDeque<u8>,
    capacity: usize,
}

impl BoundedBuffer {
    fn new(capacity: usize) -> Self {
        Self {
            bytes: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    fn push(&mut self, chunk: &[u8]) {
        if chunk.len() >= self.capacity {
            self.bytes.clear();
            let start = chunk.len() - self.capacity;
            self.bytes.extend(&chunk[start..]);
            return;
        }

        let overflow = (self.bytes.len() + chunk.len()).saturating_sub(self.capacity);
        if overflow > 0 {
            self.bytes.drain(..overflow);
        }
        self.bytes.extend(chunk);
    }

    fn snapshot(&self) -> String {
        let bytes: Vec<u8> = self.bytes.iter().copied().collect();
        String::from_utf8_lossy(&bytes).into_owned()
    }
}

#[derive(Debug, Clone)]
struct StderrCapture {
    buffer: Arc<Mutex<BoundedBuffer>>,
}

impl StderrCapture {
    fn new(capacity: usize) -> Self {
        Self {
            buffer: Arc::new(Mutex::new(BoundedBuffer::new(capacity))),
        }
    }

    fn push(&self, chunk: &[u8]) {
        let mut buffer = self
            .buffer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        buffer.push(chunk);
    }

    fn snapshot(&self) -> String {
        let buffer = self
            .buffer
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        buffer.snapshot()
    }
}

/// A spawned MCP server with owned child-process and stdio handles.
///
/// Dropping this value terminates the child because the underlying Tokio child
/// was configured with `kill_on_drop(true)`.
#[derive(Debug)]
pub struct McpProcess {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: Option<ChildStdout>,
    stderr: StderrCapture,
    stderr_task: Option<JoinHandle<()>>,
    stderr_capacity_bytes: usize,
}

impl McpProcess {
    pub fn take_stdin(&mut self) -> Result<ChildStdin> {
        self.stdin
            .take()
            .ok_or_else(|| AgentError::Mcp("MCP server did not expose a piped stdin handle".into()))
    }

    pub fn take_stdout(&mut self) -> Result<ChildStdout> {
        self.stdout.take().ok_or_else(|| {
            AgentError::Mcp("MCP server did not expose a piped stdout handle".into())
        })
    }

    pub fn stderr_snapshot(&self) -> String {
        self.stderr.snapshot()
    }

    pub fn stderr_capacity_bytes(&self) -> usize {
        self.stderr_capacity_bytes
    }

    pub async fn wait(&mut self) -> Result<ExitStatus> {
        self.child.wait().await.map_err(|error| {
            AgentError::Mcp(format!("failed to wait for MCP server process: {error}"))
        })
    }

    /// Terminate the server and reap it. Dropping the process also kills it.
    pub async fn kill(&mut self) -> Result<()> {
        if let Err(error) = self.child.start_kill() {
            return Err(AgentError::Mcp(format!(
                "failed to terminate MCP server process: {error}"
            )));
        }
        let _ = self.child.wait().await;
        Ok(())
    }
}

impl Drop for McpProcess {
    fn drop(&mut self) {
        if let Some(task) = self.stderr_task.take() {
            task.abort();
        }
    }
}

/// Spawn an MCP server with the default bounded stderr capacity.
pub async fn spawn(spec: &ProcessSpec) -> Result<McpProcess> {
    spawn_with_stderr_capacity(spec, DEFAULT_STDERR_CAPACITY_BYTES).await
}

/// Spawn an MCP server with a caller-selected bounded stderr capacity.
///
/// The child receives only the variables in `spec.env`; the harness
/// environment is cleared first. No shell interpreter is involved.
pub async fn spawn_with_stderr_capacity(
    spec: &ProcessSpec,
    stderr_capacity_bytes: usize,
) -> Result<McpProcess> {
    if spec.program.as_os_str().is_empty() {
        return Err(AgentError::Mcp(
            "MCP server executable path must not be empty".into(),
        ));
    }
    if !spec.program.is_absolute() {
        return Err(AgentError::Mcp(format!(
            "MCP server executable path must be absolute: {}",
            spec.program.display()
        )));
    }
    if stderr_capacity_bytes == 0 {
        return Err(AgentError::Mcp(
            "MCP stderr capacity must be greater than zero".into(),
        ));
    }

    let mut command = Command::new(&spec.program);
    command
        .args(&spec.args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .env_clear();

    if let Some(cwd) = &spec.cwd {
        command.current_dir(cwd);
    }
    for (key, value) in &spec.env {
        command.env(key, value);
    }

    let mut child = command.spawn().map_err(|error| {
        AgentError::Mcp(format!(
            "failed to spawn MCP server {}: {error}",
            spec.program.display()
        ))
    })?;

    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| AgentError::Mcp("MCP server did not provide stdin".into()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| AgentError::Mcp("MCP server did not provide stdout".into()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| AgentError::Mcp("MCP server did not provide stderr".into()))?;

    let capture = StderrCapture::new(stderr_capacity_bytes);
    let stderr_task = tokio::spawn(drain_stderr(stderr, capture.clone()));

    Ok(McpProcess {
        child,
        stdin: Some(stdin),
        stdout: Some(stdout),
        stderr: capture,
        stderr_task: Some(stderr_task),
        stderr_capacity_bytes,
    })
}

async fn drain_stderr(mut stderr: ChildStderr, capture: StderrCapture) {
    let mut buffer = [0u8; 4_096];
    loop {
        match stderr.read(&mut buffer).await {
            Ok(0) => break,
            Ok(read) => capture.push(&buffer[..read]),
            Err(_) => break,
        }
    }
}
