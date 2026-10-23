use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use serde_json::{Map, Value, json};
use thiserror::Error;

use crate::mcp::client::McpClient;
use crate::mcp::tools::{AgentTool, ToolContent, ToolOutcome};
use crate::policy::Policy;
use crate::state_machine::TaskBudget;

pub const DEFAULT_MAX_TOOL_OUTPUT_BYTES: usize = 4_000;

/// Every canonical capability owned by the fixed manifest.
pub const ALL_CAPABILITIES: [&str; 6] = [
    "read_file",
    "write_file",
    "apply_patch",
    "run_command",
    "git_status",
    "git_diff",
];

/// One fixed server/tool route owned by the harness (never by the model).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpRoute {
    pub server: String,
    pub tool: String,
}

impl McpRoute {
    pub fn new(server: impl Into<String>, tool: impl Into<String>) -> Self {
        Self {
            server: server.into(),
            tool: tool.into(),
        }
    }
}

/// Compile-time owned mapping from canonical tools to vetted MCP routes.
///
/// The model can choose a canonical capability, but it cannot choose which
/// server or MCP tool implements it. Extra tools advertised by a server are
/// unreachable because dispatch only uses these fixed routes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityManifest {
    pub read_file: McpRoute,
    pub write_file: McpRoute,
    pub apply_patch: McpRoute,
    pub run_command: McpRoute,
    pub git_status: McpRoute,
    pub git_diff: McpRoute,
}

impl Default for CapabilityManifest {
    fn default() -> Self {
        Self {
            read_file: McpRoute::new("filesystem", "read_file"),
            write_file: McpRoute::new("filesystem", "write_file"),
            apply_patch: McpRoute::new("filesystem", "apply_patch"),
            run_command: McpRoute::new("shell", "run"),
            git_status: McpRoute::new("git", "status"),
            git_diff: McpRoute::new("git", "diff"),
        }
    }
}

impl CapabilityManifest {
    pub fn route_for(&self, tool: &AgentTool) -> Option<&McpRoute> {
        match tool {
            AgentTool::ReadFile { .. } => Some(&self.read_file),
            AgentTool::WriteFile { .. } => Some(&self.write_file),
            AgentTool::ApplyPatch { .. } => Some(&self.apply_patch),
            AgentTool::RunCommand { .. } => Some(&self.run_command),
            AgentTool::GitStatus => Some(&self.git_status),
            AgentTool::GitDiff { .. } => Some(&self.git_diff),
            AgentTool::McpTool { .. } => None,
        }
    }

    pub fn capability_name(tool: &AgentTool) -> &'static str {
        match tool {
            AgentTool::ReadFile { .. } => "read_file",
            AgentTool::WriteFile { .. } => "write_file",
            AgentTool::ApplyPatch { .. } => "apply_patch",
            AgentTool::RunCommand { .. } => "run_command",
            AgentTool::GitStatus => "git_status",
            AgentTool::GitDiff { .. } => "git_diff",
            AgentTool::McpTool { .. } => "mcp_tool",
        }
    }

    /// Ensure a connected server advertises exactly the fixed routes it owns.
    ///
    /// Missing or unexpected advertised tools are setup errors and must
    /// prevent the server from being installed. This keeps one compromised or
    /// over-broad server from silently widening the harness's capability set.
    pub fn validate_client(&self, client: &McpClient) -> Result<(), CapabilityError> {
        self.validate_client_for(client, &ALL_CAPABILITIES)
    }

    /// Validate a server against the fixed manifest while requiring only the
    /// capabilities enabled by the active profile.
    ///
    /// Tools outside the fixed manifest remain fatal. A capability disabled by
    /// the profile may be absent or still advertised by a vetted server; it is
    /// never routable because the dispatcher also enforces the profile.
    pub fn validate_client_for(
        &self,
        client: &McpClient,
        allowed_capabilities: &[&str],
    ) -> Result<(), CapabilityError> {
        let expected = self
            .expected_routes()
            .into_iter()
            .filter(|(_, route)| route.server == client.name)
            .collect::<Vec<_>>();

        if expected.is_empty() {
            return Err(CapabilityError::UnexpectedServer {
                server: client.name.clone(),
            });
        }

        let advertised = client
            .tools()
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<BTreeSet<_>>();
        let expected_names = expected
            .iter()
            .map(|(_, route)| route.tool.as_str())
            .collect::<BTreeSet<_>>();

        for (capability, route) in &expected {
            if allowed_capabilities.contains(capability)
                && !advertised.contains(route.tool.as_str())
            {
                return Err(CapabilityError::MissingTool {
                    capability,
                    server: route.server.clone(),
                    tool: route.tool.clone(),
                });
            }
        }

        for tool in advertised {
            if !expected_names.contains(tool) {
                return Err(CapabilityError::UnexpectedTool {
                    server: client.name.clone(),
                    tool: tool.to_owned(),
                });
            }
        }

        Ok(())
    }

    /// Ensure a pool contains exactly the configured servers and tools.
    pub fn validate_pool(&self, pool: &McpPool) -> Result<(), CapabilityError> {
        let expected_servers = self
            .expected_routes()
            .into_iter()
            .map(|(_, route)| route.server.as_str())
            .collect::<BTreeSet<_>>();

        for server in pool.names() {
            if !expected_servers.contains(server) {
                return Err(CapabilityError::UnexpectedServer {
                    server: server.to_owned(),
                });
            }
        }

        for (capability, route) in self.expected_routes() {
            let Some(client) = pool.get(&route.server) else {
                return Err(CapabilityError::MissingServer {
                    capability,
                    server: route.server.clone(),
                });
            };
            self.validate_client(client)?;
        }
        Ok(())
    }

    fn expected_routes(&self) -> [(&'static str, &McpRoute); 6] {
        [
            ("read_file", &self.read_file),
            ("write_file", &self.write_file),
            ("apply_patch", &self.apply_patch),
            ("run_command", &self.run_command),
            ("git_status", &self.git_status),
            ("git_diff", &self.git_diff),
        ]
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CapabilityError {
    #[error("MCP server `{server}` is not configured for capability `{capability}`")]
    MissingServer {
        capability: &'static str,
        server: String,
    },

    #[error(
        "MCP server `{server}` does not advertise tool `{tool}`, required for capability `{capability}`"
    )]
    MissingTool {
        capability: &'static str,
        server: String,
        tool: String,
    },

    #[error("MCP server `{server}` advertises unexpected tool `{tool}`")]
    UnexpectedTool { server: String, tool: String },

    #[error("MCP server `{server}` is not part of the capability manifest")]
    UnexpectedServer { server: String },
}

/// Named collection of connected capability servers.
#[derive(Default)]
pub struct McpPool {
    clients: BTreeMap<String, McpClient>,
}

impl McpPool {
    pub fn new() -> Self {
        Self::default()
    }

    /// Validate the fixed routes owned by this client, then install it.
    pub fn install(
        &mut self,
        capabilities: &CapabilityManifest,
        client: McpClient,
    ) -> Result<(), CapabilityError> {
        capabilities.validate_client(&client)?;
        self.insert(client.name.clone(), client);
        Ok(())
    }

    pub fn insert(&mut self, name: impl Into<String>, client: McpClient) -> Option<McpClient> {
        self.clients.insert(name.into(), client)
    }

    pub fn get(&self, name: &str) -> Option<&McpClient> {
        self.clients.get(name)
    }

    pub fn contains(&self, name: &str) -> bool {
        self.clients.contains_key(name)
    }

    pub fn len(&self) -> usize {
        self.clients.len()
    }

    pub fn is_empty(&self) -> bool {
        self.clients.is_empty()
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.clients.keys().map(String::as_str)
    }

    /// Stop every connected server, reaping each child within `timeout`.
    pub async fn shutdown(&mut self, timeout: Duration) -> crate::error::Result<()> {
        let clients = std::mem::take(&mut self.clients);
        let mut first_error = None;
        for (_, mut client) in clients {
            if let Err(error) = client.shutdown(timeout).await
                && first_error.is_none()
            {
                first_error = Some(error);
            }
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

/// Policy-enforced router from canonical `AgentTool` values to MCP calls.
pub struct ToolDispatcher {
    pub policy: Policy,
    pub capabilities: CapabilityManifest,
    pub pool: McpPool,
    pub max_tool_output_bytes: usize,
    allowed_capabilities: BTreeSet<String>,
}

impl ToolDispatcher {
    pub fn new(policy: Policy, capabilities: CapabilityManifest, pool: McpPool) -> Self {
        let allowed_capabilities = ALL_CAPABILITIES.into_iter().map(str::to_owned).collect();
        Self {
            policy,
            capabilities,
            pool,
            max_tool_output_bytes: DEFAULT_MAX_TOOL_OUTPUT_BYTES,
            allowed_capabilities,
        }
    }

    pub fn with_tool_output_limit(mut self, max_tool_output_bytes: usize) -> Self {
        self.max_tool_output_bytes = max_tool_output_bytes;
        self
    }

    pub fn with_allowed_capabilities<I, S>(mut self, capabilities: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.allowed_capabilities = capabilities.into_iter().map(Into::into).collect();
        self
    }

    pub fn allows_capability(&self, capability: &str) -> bool {
        self.allowed_capabilities.contains(capability)
    }

    /// Stop all connected servers, reaping each child within `timeout`.
    pub async fn shutdown(&mut self, timeout: Duration) -> crate::error::Result<()> {
        self.pool.shutdown(timeout).await
    }

    /// Verify the fixed capability route is enabled and reachable.
    ///
    /// This is a synchronous, side-effect-free preflight used before approval
    /// prompts are created. It does not perform the MCP call.
    pub fn check_capability(&self, tool: &AgentTool) -> std::result::Result<(), String> {
        let capability = CapabilityManifest::capability_name(tool);
        if !self.allowed_capabilities.contains(capability) {
            return Err(format!(
                "capability unavailable: `{capability}` is not enabled by the configured MCP profile"
            ));
        }

        let Some(route) = self.capabilities.route_for(tool) else {
            return Err(format!(
                "capability unavailable: `{capability}` has no fixed MCP route; \
                 direct MCP tool dispatch is not permitted"
            ));
        };

        let Some(client) = self.pool.get(&route.server) else {
            return Err(format!(
                "capability unavailable: MCP server `{}` is not configured for `{capability}`",
                route.server
            ));
        };

        if !client
            .tools()
            .iter()
            .any(|advertised| advertised.name == route.tool)
        {
            return Err(format!(
                "capability unavailable: MCP server `{}` does not advertise required tool `{}` for `{capability}`",
                route.server, route.tool
            ));
        }

        if tool_arguments(tool).is_none() {
            return Err(format!(
                "capability unavailable: `{capability}` has no fixed MCP argument mapping"
            ));
        }

        Ok(())
    }

    /// Authorize first, then route to the fixed capability and invoke it.
    ///
    /// The returned outcome always carries the caller's `tool_call_id`, is
    /// bounded, and is safe to return verbatim to the model.
    pub async fn execute(&self, tool_call_id: impl Into<String>, tool: &AgentTool) -> ToolOutcome {
        let tool_call_id = tool_call_id.into();

        if let Err(violation) = self.policy.authorize(tool) {
            return self.bound(ToolOutcome::policy_denied(
                tool_call_id,
                format!("policy denied action: {violation}"),
            ));
        }

        if let Err(reason) = self.check_capability(tool) {
            return self.bound(ToolOutcome::capability_unavailable(tool_call_id, reason));
        }

        let route = self
            .capabilities
            .route_for(tool)
            .expect("capability preflight verified route");
        let client = self
            .pool
            .get(&route.server)
            .expect("capability preflight verified client");
        let arguments = tool_arguments(tool).expect("capability preflight verified arguments");

        let mut outcome = client.call_tool(&route.tool, arguments).await;
        outcome.tool_call_id = tool_call_id;
        self.bound(outcome)
    }

    fn bound(&self, mut outcome: ToolOutcome) -> ToolOutcome {
        let cap = self.max_tool_output_bytes.max(1);
        let text = outcome.text();
        if text.len() > cap {
            let budget = TaskBudget {
                max_tool_output_bytes: cap,
                ..TaskBudget::default()
            };
            outcome.content = vec![ToolContent::Text {
                text: budget.bound_tool_output(&text),
            }];
        }
        outcome
    }
}

/// Exact canonical-tool-to-MCP-arguments mapping.
///
/// Absent optional fields are omitted rather than serialized as `null`, so
/// servers see the same intent the harness policy checked.
pub fn tool_arguments(tool: &AgentTool) -> Option<Value> {
    match tool {
        AgentTool::ReadFile {
            path,
            line_start,
            line_end,
        } => {
            let mut arguments = Map::new();
            arguments.insert("path".into(), Value::String(path.clone()));
            if let Some(line_start) = line_start {
                arguments.insert("line_start".into(), json!(line_start));
            }
            if let Some(line_end) = line_end {
                arguments.insert("line_end".into(), json!(line_end));
            }
            Some(Value::Object(arguments))
        }
        AgentTool::WriteFile {
            path,
            content,
            expected_sha256,
        } => {
            let mut arguments = Map::new();
            arguments.insert("path".into(), Value::String(path.clone()));
            arguments.insert("content".into(), Value::String(content.clone()));
            if let Some(expected_sha256) = expected_sha256 {
                arguments.insert(
                    "expected_sha256".into(),
                    Value::String(expected_sha256.clone()),
                );
            }
            Some(Value::Object(arguments))
        }
        AgentTool::ApplyPatch {
            path,
            unified_diff,
            expected_sha256,
        } => {
            let mut arguments = Map::new();
            arguments.insert("path".into(), Value::String(path.clone()));
            arguments.insert("unified_diff".into(), Value::String(unified_diff.clone()));
            if let Some(expected_sha256) = expected_sha256 {
                arguments.insert(
                    "expected_sha256".into(),
                    Value::String(expected_sha256.clone()),
                );
            }
            Some(Value::Object(arguments))
        }
        AgentTool::RunCommand {
            program,
            args,
            cwd,
            timeout_secs,
        } => {
            let mut arguments = Map::new();
            arguments.insert("program".into(), Value::String(program.clone()));
            arguments.insert("args".into(), json!(args));
            if let Some(cwd) = cwd {
                arguments.insert("cwd".into(), Value::String(cwd.clone()));
            }
            if let Some(timeout_secs) = timeout_secs {
                arguments.insert("timeout_secs".into(), json!(timeout_secs));
            }
            Some(Value::Object(arguments))
        }
        AgentTool::GitStatus => Some(json!({})),
        AgentTool::GitDiff { staged } => Some(json!({ "staged": staged })),
        AgentTool::McpTool { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_manifest_uses_fixed_routes() {
        let manifest = CapabilityManifest::default();
        assert_eq!(manifest.read_file, McpRoute::new("filesystem", "read_file"));
        assert_eq!(
            manifest.write_file,
            McpRoute::new("filesystem", "write_file")
        );
        assert_eq!(
            manifest.apply_patch,
            McpRoute::new("filesystem", "apply_patch")
        );
        assert_eq!(manifest.run_command, McpRoute::new("shell", "run"));
        assert_eq!(manifest.git_status, McpRoute::new("git", "status"));
        assert_eq!(manifest.git_diff, McpRoute::new("git", "diff"));
    }

    #[test]
    fn maps_read_file_and_omits_absent_ranges() {
        let tool = AgentTool::ReadFile {
            path: "src/main.rs".into(),
            line_start: None,
            line_end: None,
        };
        assert_eq!(
            tool_arguments(&tool).unwrap(),
            json!({"path": "src/main.rs"})
        );

        let tool = AgentTool::ReadFile {
            path: "src/main.rs".into(),
            line_start: Some(2),
            line_end: Some(8),
        };
        assert_eq!(
            tool_arguments(&tool).unwrap(),
            json!({"path": "src/main.rs", "line_start": 2, "line_end": 8})
        );
    }

    #[test]
    fn maps_mutations_and_omits_absent_hashes() {
        let write = AgentTool::WriteFile {
            path: "src/lib.rs".into(),
            content: "hello".into(),
            expected_sha256: None,
        };
        assert_eq!(
            tool_arguments(&write).unwrap(),
            json!({"path": "src/lib.rs", "content": "hello"})
        );

        let patch = AgentTool::ApplyPatch {
            path: "src/lib.rs".into(),
            unified_diff: "@@ -1 +1 @@".into(),
            expected_sha256: Some("abc".into()),
        };
        assert_eq!(
            tool_arguments(&patch).unwrap(),
            json!({
                "path": "src/lib.rs",
                "unified_diff": "@@ -1 +1 @@",
                "expected_sha256": "abc"
            })
        );
    }

    #[test]
    fn maps_command_and_git_tools_exactly() {
        let command = AgentTool::RunCommand {
            program: "cargo".into(),
            args: vec!["test".into()],
            cwd: None,
            timeout_secs: None,
        };
        assert_eq!(
            tool_arguments(&command).unwrap(),
            json!({"program": "cargo", "args": ["test"]})
        );

        let command = AgentTool::RunCommand {
            program: "cargo".into(),
            args: vec!["test".into()],
            cwd: Some("crates/core".into()),
            timeout_secs: Some(60),
        };
        assert_eq!(
            tool_arguments(&command).unwrap(),
            json!({
                "program": "cargo",
                "args": ["test"],
                "cwd": "crates/core",
                "timeout_secs": 60
            })
        );

        assert_eq!(tool_arguments(&AgentTool::GitStatus).unwrap(), json!({}));
        assert_eq!(
            tool_arguments(&AgentTool::GitDiff { staged: true }).unwrap(),
            json!({"staged": true})
        );
        assert!(
            tool_arguments(&AgentTool::McpTool {
                server: "filesystem".into(),
                name: "read_file".into(),
                arguments: json!({})
            })
            .is_none()
        );
    }
}
