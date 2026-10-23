use std::collections::{BTreeSet, HashMap};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, ChildStdout};
use tokio::sync::{Mutex, oneshot};
use tokio::task::JoinHandle;

use crate::error::{AgentError, Result};
use crate::mcp::process::{self, McpProcess, ProcessSpec};
use crate::mcp::protocol::{
    self, InboundMessage, JsonRpcError, JsonRpcId, JsonRpcNotification, JsonRpcRequest,
    JsonRpcResponse,
};
use crate::mcp::tools::{ToolContent, ToolOutcome, ToolOutcomeKind};

/// Default MCP protocol revision assumed by this implementation.
///
/// This is a configuration default, not a claim that every deployed server
/// speaks this revision. Deployments must set
/// [`McpClientOptions::protocol_version`] to the revision implemented by their
/// vetted MCP servers.
pub const DEFAULT_MCP_PROTOCOL_VERSION: &str = "2025-06-18";

const METHOD_INITIALIZE: &str = "initialize";
const METHOD_INITIALIZED: &str = "notifications/initialized";
const METHOD_TOOLS_LIST: &str = "tools/list";
const METHOD_TOOLS_CALL: &str = "tools/call";

type PendingReply = std::result::Result<JsonRpcResponse, AgentError>;
type PendingMap = Arc<Mutex<HashMap<u64, oneshot::Sender<PendingReply>>>>;

#[derive(Debug, Clone)]
pub struct McpClientOptions {
    pub protocol_version: String,
    pub startup_timeout: Duration,
    pub request_timeout: Duration,
    pub stderr_capacity_bytes: usize,
}

impl Default for McpClientOptions {
    fn default() -> Self {
        Self {
            protocol_version: DEFAULT_MCP_PROTOCOL_VERSION.into(),
            startup_timeout: Duration::from_secs(30),
            request_timeout: Duration::from_secs(120),
            stderr_capacity_bytes: process::DEFAULT_STDERR_CAPACITY_BYTES,
        }
    }
}

/// Minimally validated tool metadata returned by `tools/list`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpToolInfo {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default, rename = "inputSchema")]
    pub input_schema: Option<Value>,
}

/// Concurrent newline-delimited JSON-RPC client for one MCP server.
///
/// Request IDs are generated monotonically. Writes to stdin are serialized by
/// a mutex while reads are handled by one dedicated task, allowing many
/// requests to be in flight at once.
pub struct McpClient {
    pub name: String,
    protocol_version: String,
    tools: Vec<McpToolInfo>,
    stdin: Arc<Mutex<ChildStdin>>,
    pending: PendingMap,
    next_id: AtomicU64,
    startup_timeout: Duration,
    request_timeout: Duration,
    process: McpProcess,
    reader_task: JoinHandle<()>,
    closed: bool,
}

impl McpClient {
    /// Spawn a configured MCP server, run the initialization handshake, and
    /// request its advertised tool list.
    pub async fn connect(
        name: impl Into<String>,
        spec: &ProcessSpec,
        options: McpClientOptions,
    ) -> Result<Self> {
        let name = name.into();
        let mut process =
            process::spawn_with_stderr_capacity(spec, options.stderr_capacity_bytes).await?;
        let stdin = process.take_stdin()?;
        let stdout = process.take_stdout()?;

        let stdin = Arc::new(Mutex::new(stdin));
        let pending: PendingMap = Arc::new(Mutex::new(HashMap::new()));
        let reader_task = tokio::spawn(read_loop(
            name.clone(),
            stdout,
            Arc::clone(&stdin),
            Arc::clone(&pending),
        ));

        let mut client = Self {
            name,
            protocol_version: options.protocol_version,
            tools: Vec::new(),
            stdin,
            pending,
            next_id: AtomicU64::new(1),
            startup_timeout: options.startup_timeout,
            request_timeout: options.request_timeout,
            process,
            reader_task,
            closed: false,
        };

        let startup = async {
            client.initialize().await?;
            client.notify(METHOD_INITIALIZED, None).await?;
            client.tools = client.list_tools().await?;
            Ok(())
        }
        .await;

        if let Err(error) = startup {
            let stderr = client.stderr_snapshot();
            let _ = client.shutdown(Duration::from_secs(2)).await;
            return Err(attach_startup_stderr(error, &stderr));
        }
        Ok(client)
    }

    pub fn tools(&self) -> &[McpToolInfo] {
        &self.tools
    }

    pub fn protocol_version(&self) -> &str {
        &self.protocol_version
    }

    pub fn stderr_snapshot(&self) -> String {
        self.process.stderr_snapshot()
    }

    pub fn stderr_capacity_bytes(&self) -> usize {
        self.process.stderr_capacity_bytes()
    }

    pub async fn pending_request_count(&self) -> usize {
        self.pending.lock().await.len()
    }

    /// Send a request and wait for its correlated response.
    pub async fn request(&self, method: &str, params: Value) -> Result<Value> {
        self.request_with_timeout(method, params, self.request_timeout)
            .await
    }

    async fn request_with_timeout(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (sender, receiver) = oneshot::channel();

        self.pending.lock().await.insert(id, sender);
        let mut guard = PendingGuard::new(id, Arc::clone(&self.pending));

        let request = JsonRpcRequest::new(id, method, params);
        if let Err(error) = write_message(&self.stdin, &request).await {
            guard.remove_pending().await;
            return Err(AgentError::Mcp(format!(
                "failed to send MCP request `{method}`: {error}"
            )));
        }

        let response = match tokio::time::timeout(timeout, receiver).await {
            Ok(Ok(Ok(response))) => Ok(response),
            Ok(Ok(Err(error))) => Err(error),
            Ok(Err(_)) => Err(AgentError::Mcp(format!(
                "MCP request `{method}` was cancelled before completion"
            ))),
            Err(_) => {
                guard.remove_pending().await;
                return Err(AgentError::Mcp(format!(
                    "MCP request `{method}` timed out after {timeout:?}"
                )));
            }
        };

        guard.disarm();
        let response = response?;
        if let Some(error) = response.error {
            return Err(mcp_rpc_error(method, &error));
        }
        Ok(response.result.unwrap_or(Value::Null))
    }

    /// Send a notification that has no response.
    pub async fn notify(&self, method: &str, params: Option<Value>) -> Result<()> {
        let notification = match params {
            Some(params) => JsonRpcNotification::with_params(method, params),
            None => JsonRpcNotification::new(method),
        };

        write_message(&self.stdin, &notification)
            .await
            .map_err(|error| {
                AgentError::Mcp(format!(
                    "failed to send MCP notification `{method}`: {error}"
                ))
            })
    }

    /// Transport-level `tools/call` adapter.
    ///
    /// Transport and server failures become model-readable error outcomes.
    /// This method does not route tools into filesystem, Git, or shell
    /// capabilities; that belongs to a later milestone.
    pub async fn call_tool(&self, tool_name: &str, arguments: Value) -> ToolOutcome {
        let params = json!({
            "name": tool_name,
            "arguments": arguments,
        });

        match self.request(METHOD_TOOLS_CALL, params).await {
            Ok(result) => match map_tools_call_result(tool_name, result) {
                Ok(outcome) => outcome,
                Err(message) => ToolOutcome::error(
                    tool_name,
                    format!(
                        "MCP tools/call returned an invalid result for `{tool_name}`: {message}"
                    ),
                ),
            },
            Err(error) => ToolOutcome::error(
                tool_name,
                format!("MCP tools/call failed for `{tool_name}`: {error}"),
            ),
        }
    }

    async fn initialize(&self) -> Result<()> {
        let params = json!({
            "protocolVersion": self.protocol_version,
            "capabilities": {},
            "clientInfo": {
                "name": "local-agent-harness",
                "version": env!("CARGO_PKG_VERSION"),
            },
        });

        let result = self
            .request_with_timeout(METHOD_INITIALIZE, params, self.startup_timeout)
            .await?;
        let object = result
            .as_object()
            .ok_or_else(|| AgentError::Mcp("initialize result must be a JSON object".into()))?;

        let version = object
            .get("protocolVersion")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                AgentError::Mcp(
                    "initialize result is missing a string `protocolVersion` field".into(),
                )
            })?;
        if version != self.protocol_version {
            return Err(AgentError::Mcp(format!(
                "MCP protocol version mismatch: configured `{}`, server `{version}`. \
                 Configure the revision that matches the deployed server.",
                self.protocol_version
            )));
        }

        if let Some(capabilities) = object.get("capabilities")
            && !capabilities.is_object()
        {
            return Err(AgentError::Mcp(
                "initialize result `capabilities` must be an object".into(),
            ));
        }
        if let Some(server_info) = object.get("serverInfo")
            && !server_info.is_object()
        {
            return Err(AgentError::Mcp(
                "initialize result `serverInfo` must be an object".into(),
            ));
        }
        Ok(())
    }

    async fn list_tools(&self) -> Result<Vec<McpToolInfo>> {
        let result = self
            .request_with_timeout(METHOD_TOOLS_LIST, json!({}), self.startup_timeout)
            .await?;
        let object = result
            .as_object()
            .ok_or_else(|| AgentError::Mcp("tools/list result must be a JSON object".into()))?;
        let tools = object
            .get("tools")
            .and_then(Value::as_array)
            .ok_or_else(|| AgentError::Mcp("tools/list result is missing array `tools`".into()))?;

        let mut seen = BTreeSet::new();
        let mut advertised = Vec::with_capacity(tools.len());
        for tool in tools {
            let info: McpToolInfo = serde_json::from_value(tool.clone()).map_err(|error| {
                AgentError::Mcp(format!(
                    "tools/list contained an invalid tool entry: {error}"
                ))
            })?;
            if info.name.trim().is_empty() {
                return Err(AgentError::Mcp(
                    "tools/list contained a tool with an empty name".into(),
                ));
            }
            if !seen.insert(info.name.clone()) {
                return Err(AgentError::Mcp(format!(
                    "tools/list contained duplicate tool `{}`",
                    info.name
                )));
            }
            advertised.push(info);
        }
        Ok(advertised)
    }

    /// Stop the server and reap its process within `timeout`.
    pub async fn shutdown(&mut self, timeout: Duration) -> Result<()> {
        if self.closed {
            return Ok(());
        }
        self.closed = true;
        self.reader_task.abort();

        match tokio::time::timeout(timeout, self.process.kill()).await {
            Ok(result) => result,
            Err(_) => Err(AgentError::Mcp(format!(
                "MCP server `{}` did not shut down within {timeout:?}",
                self.name
            ))),
        }
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        self.reader_task.abort();
    }
}

struct PendingGuard {
    id: u64,
    pending: PendingMap,
    armed: bool,
}

impl PendingGuard {
    fn new(id: u64, pending: PendingMap) -> Self {
        Self {
            id,
            pending,
            armed: true,
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }

    async fn remove_pending(&mut self) {
        if self.armed {
            self.pending.lock().await.remove(&self.id);
            self.armed = false;
        }
    }
}

impl Drop for PendingGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        self.armed = false;

        // A dropped request future cannot await. Schedule explicit removal so
        // cancelled callers do not leave stale pending entries behind.
        let pending = Arc::clone(&self.pending);
        let id = self.id;
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                pending.lock().await.remove(&id);
            });
        }
    }
}

async fn read_loop(
    name: String,
    stdout: ChildStdout,
    stdin: Arc<Mutex<ChildStdin>>,
    pending: PendingMap,
) {
    let mut lines = BufReader::new(stdout).lines();

    loop {
        match lines.next_line().await {
            Ok(None) => {
                fail_all(
                    &pending,
                    &format!("MCP server `{name}` stdout closed unexpectedly"),
                )
                .await;
                break;
            }
            Ok(Some(line)) => {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }

                match protocol::parse_inbound(line) {
                    Ok(InboundMessage::Response(response)) => {
                        route_response(&name, &pending, response).await;
                    }
                    Ok(InboundMessage::Notification(notification)) => {
                        tracing::debug!(
                            server = %name,
                            method = %notification.method,
                            "ignored MCP server notification"
                        );
                    }
                    Ok(InboundMessage::Request(request)) => {
                        let id = request.id.clone();
                        let response = JsonRpcResponse::error_response(
                            id,
                            protocol::METHOD_NOT_FOUND_CODE,
                            "Method not found",
                        );
                        if let Err(error) = write_message(&stdin, &response).await {
                            fail_all(
                                &pending,
                                &format!(
                                    "MCP server `{name}` request could not be answered: {error}"
                                ),
                            )
                            .await;
                            break;
                        }
                        tracing::warn!(
                            server = %name,
                            method = %request.method,
                            "rejected unsupported MCP server request with -32601"
                        );
                    }
                    Err(error) => {
                        fail_all(
                            &pending,
                            &format!("MCP protocol error from server `{name}`: {error}"),
                        )
                        .await;
                        break;
                    }
                }
            }
            Err(error) => {
                fail_all(
                    &pending,
                    &format!("failed reading MCP server `{name}` stdout: {error}"),
                )
                .await;
                break;
            }
        }
    }
}

async fn route_response(name: &str, pending: &PendingMap, response: JsonRpcResponse) {
    let Some(id) = response.id.as_ref().and_then(JsonRpcId::as_u64) else {
        tracing::warn!(
            server = %name,
            id = ?response.id,
            "ignored MCP response with a non-numeric or missing id"
        );
        return;
    };

    let sender = pending.lock().await.remove(&id);
    match sender {
        Some(sender) => {
            if sender.send(Ok(response)).is_err() {
                tracing::debug!(
                    server = %name,
                    id,
                    "MCP response receiver was already dropped"
                );
            }
        }
        None => {
            tracing::warn!(
                server = %name,
                id,
                "ignored MCP response for an unknown or duplicate request id"
            );
        }
    }
}

async fn fail_all(pending: &PendingMap, message: &str) {
    let entries = {
        let mut pending = pending.lock().await;
        std::mem::take(&mut *pending)
    };

    for (id, sender) in entries {
        let error = AgentError::Mcp(format!("{message} (request {id})"));
        let _ = sender.send(Err(error));
    }
}

async fn write_message<T: Serialize>(stdin: &Arc<Mutex<ChildStdin>>, message: &T) -> Result<()> {
    let mut line = serde_json::to_string(message)
        .map_err(|error| AgentError::Mcp(format!("failed to serialize MCP message: {error}")))?;
    line.push('\n');

    let mut writer = stdin.lock().await;
    writer
        .write_all(line.as_bytes())
        .await
        .map_err(|error| AgentError::Mcp(format!("failed to write MCP message: {error}")))?;
    writer
        .flush()
        .await
        .map_err(|error| AgentError::Mcp(format!("failed to flush MCP message: {error}")))?;
    Ok(())
}

fn attach_startup_stderr(error: AgentError, stderr: &str) -> AgentError {
    if stderr.trim().is_empty() {
        return error;
    }
    let stderr = crate::wiki::redact::redact(stderr).unwrap_or_else(|_| stderr.to_owned());
    AgentError::Mcp(format!(
        "{error}\nserver stderr (bounded): {}",
        stderr.trim()
    ))
}

fn mcp_rpc_error(method: &str, error: &JsonRpcError) -> AgentError {
    AgentError::Mcp(format!(
        "MCP request `{method}` failed with code {}: {}",
        error.code, error.message
    ))
}

fn map_tools_call_result(
    tool_name: &str,
    result: Value,
) -> std::result::Result<ToolOutcome, String> {
    let object = result
        .as_object()
        .ok_or_else(|| "result must be a JSON object".to_owned())?;

    let is_error = match object.get("isError") {
        Some(Value::Bool(value)) => *value,
        Some(_) => return Err("`isError` must be a boolean".into()),
        None => false,
    };

    let mut content = Vec::new();
    match object.get("content") {
        Some(Value::Array(items)) => {
            for item in items {
                match item {
                    Value::Object(fields)
                        if fields.get("type").and_then(Value::as_str) == Some("text") =>
                    {
                        let text = fields.get("text").and_then(Value::as_str).ok_or_else(|| {
                            "text content item requires string field `text`".to_owned()
                        })?;
                        content.push(ToolContent::Text {
                            text: text.to_owned(),
                        });
                    }
                    other => content.push(ToolContent::Json {
                        value: other.clone(),
                    }),
                }
            }
        }
        Some(_) => return Err("`content` must be an array".into()),
        None => {}
    }

    Ok(ToolOutcome {
        // The transport adapter does not receive a model tool-call id, so the
        // tool name is used as the diagnostic correlation value. The routing
        // dispatcher replaces this with the caller's tool-call id.
        tool_call_id: tool_name.to_owned(),
        is_error,
        kind: if is_error {
            ToolOutcomeKind::ServerError
        } else {
            ToolOutcomeKind::Success
        },
        content,
    })
}
