//! Adapter from canonical model actions to fixed helper scripts.
//!
//! This module recognizes the accepted read-only actions plus the narrow
//! new-file `write_file` action. `write_file` does not overwrite existing
//! targets. Other mutation-capable actions are rejected before execution. The
//! adapter owns no shell, executable, or script selection: [`ActionId`]
//! resolves every helper from the harness-owned script inventory.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Map, Value};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::audit::AuditSink;
use crate::error::{AgentError, Result};
use crate::mcp::{AgentTool, ToolOutcome};
use crate::orchestrator::{TaskId, ToolExecutor};
use crate::policy::Policy;
use crate::script_runner::{ActionId, ScriptOutcome, ScriptRunner};

/// Script-backed executor for the fixed read-only and new-file action inventory.
#[derive(Clone)]
pub struct ScriptToolExecutor {
    runner: ScriptRunner,
    policy: Policy,
    audit: Option<Arc<dyn AuditSink>>,
}

impl std::fmt::Debug for ScriptToolExecutor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ScriptToolExecutor")
            .field("runner", &self.runner)
            .field("policy_root", &self.policy.repo_root)
            .field("audit", &self.audit.is_some())
            .finish()
    }
}

impl ScriptToolExecutor {
    pub fn new(runner: ScriptRunner, policy: Policy) -> Result<Self> {
        Self::with_audit(runner, policy, None)
    }

    pub fn with_audit(
        runner: ScriptRunner,
        policy: Policy,
        audit: Option<Arc<dyn AuditSink>>,
    ) -> Result<Self> {
        if runner.project_root() != policy.repo_root {
            return Err(AgentError::Config(
                "script runner and policy must share the canonical project root".into(),
            ));
        }
        Ok(Self {
            runner,
            policy,
            audit,
        })
    }

    pub fn runner(&self) -> &ScriptRunner {
        &self.runner
    }

    async fn execute_inner(
        &self,
        task_id: Option<TaskId>,
        tool_call_id: String,
        tool: &AgentTool,
        cancel: &CancellationToken,
    ) -> ToolOutcome {
        if let Err(violation) = self.authorize(tool) {
            return ToolOutcome::policy_denied(
                tool_call_id,
                format!("policy denied action: {violation}"),
            );
        }
        if let Err(reason) = self.check_capability(tool) {
            return ToolOutcome::capability_unavailable(tool_call_id, reason);
        }

        let (action, params) =
            action_and_params(tool).expect("capability preflight verified supported action");
        let result = match task_id {
            Some(task_id) => {
                self.runner
                    .execute_for_task(task_id, action, params, cancel, self.audit.as_deref())
                    .await
            }
            None => {
                self.runner
                    .execute_with_cancel(action, params, cancel)
                    .await
            }
        };

        match result {
            Ok(outcome) if outcome.is_ok() => match render_success(&outcome) {
                Ok(text) => ToolOutcome::success(tool_call_id, text),
                Err(message) => ToolOutcome::server_error(tool_call_id, message),
            },
            Ok(outcome) => {
                ToolOutcome::server_error(tool_call_id, script_failure_message(&outcome))
            }
            Err(error) => ToolOutcome::error(
                tool_call_id,
                format!(
                    "script action `{}` could not be executed: {error}",
                    action.as_str()
                ),
            ),
        }
    }
}

#[async_trait]
impl ToolExecutor for ScriptToolExecutor {
    fn authorize(&self, tool: &AgentTool) -> std::result::Result<(), String> {
        if action_and_params(tool).is_none() {
            return Err("the script executor cannot dispatch this action".to_owned());
        }
        self.policy
            .authorize(tool)
            .map_err(|error| error.to_string())
    }

    fn check_capability(&self, tool: &AgentTool) -> std::result::Result<(), String> {
        let Some((action, _)) = action_and_params(tool) else {
            return Err(format!(
                "capability unavailable: `{}` is not a supported script action",
                crate::mcp::CapabilityManifest::capability_name(tool)
            ));
        };
        self.runner
            .invocation(action)
            .map(|_| ())
            .map_err(|error| format!("capability unavailable: {error}"))
    }

    async fn execute(&self, tool_call_id: String, tool: &AgentTool) -> ToolOutcome {
        self.execute_inner(None, tool_call_id, tool, &CancellationToken::new())
            .await
    }

    async fn execute_for_task(
        &self,
        task_id: Uuid,
        tool_call_id: String,
        tool: &AgentTool,
        cancel: &CancellationToken,
    ) -> ToolOutcome {
        self.execute_inner(Some(task_id), tool_call_id, tool, cancel)
            .await
    }
}

/// Exact canonical action to script action/parameter mapping.
///
/// Only accepted forms have a mapping. Optional fields are omitted when absent
/// rather than sent as JSON null.
fn action_and_params(tool: &AgentTool) -> Option<(ActionId, Map<String, Value>)> {
    match tool {
        AgentTool::ReadFile {
            path,
            line_start,
            line_end,
        } => {
            let mut params = Map::new();
            params.insert("path".into(), Value::String(path.clone()));
            if let Some(line_start) = line_start {
                params.insert("line_start".into(), Value::from(*line_start));
            }
            if let Some(line_end) = line_end {
                params.insert("line_end".into(), Value::from(*line_end));
            }
            Some((ActionId::ReadFile, params))
        }
        AgentTool::GitStatus => Some((ActionId::GitStatus, Map::new())),
        AgentTool::GitDiff { staged } => {
            let mut params = Map::new();
            params.insert("staged".into(), Value::Bool(*staged));
            Some((ActionId::GitDiff, params))
        }
        AgentTool::WriteFile {
            path,
            content,
            expected_sha256,
        } => {
            let mut params = Map::new();
            params.insert("path".into(), Value::String(path.clone()));
            params.insert("content".into(), Value::String(content.clone()));
            if let Some(expected_sha256) = expected_sha256 {
                params.insert(
                    "expected_sha256".into(),
                    Value::String(expected_sha256.clone()),
                );
            }
            Some((ActionId::WriteFile, params))
        }
        AgentTool::ApplyPatch { .. } | AgentTool::RunCommand { .. } | AgentTool::McpTool { .. } => {
            None
        }
    }
}

fn script_failure_message(outcome: &ScriptOutcome) -> String {
    let action = outcome.action().as_str();
    match outcome.error() {
        Some(error) => format!(
            "script action `{action}` failed [{}]: {}",
            error.code(),
            error.message()
        ),
        None => format!("script action `{action}` failed without an error code"),
    }
}

fn render_success(outcome: &ScriptOutcome) -> std::result::Result<String, String> {
    let action = outcome.action();
    let data = outcome
        .data()
        .ok_or_else(|| format!("script action `{}` succeeded without data", action.as_str()))?;

    match action {
        ActionId::ReadFile => {
            let path = string_field(data, "path")?;
            let content = string_field(data, "content")?;
            let truncated = bool_field(data, "truncated")?;
            let partial_line = bool_field(data, "partial_line")?;
            let line_start = optional_number_field(data, "line_start")?;
            let line_end = optional_number_field(data, "line_end")?;
            let next_line = optional_number_field(data, "next_line")?;
            Ok(format!(
                "read_file path={path} line_start={line_start} line_end={line_end} \
                 next_line={next_line} truncated={truncated} partial_line={partial_line}\n\n{content}"
            ))
        }
        ActionId::GitStatus => {
            let clean = bool_field(data, "clean")?;
            let status = string_field(data, "status")?;
            let truncated = bool_field(data, "truncated")?;
            Ok(format!(
                "git_status clean={clean} truncated={truncated}\n\n{status}"
            ))
        }
        ActionId::GitDiff => {
            let staged = bool_field(data, "staged")?;
            let diff = string_field(data, "diff")?;
            let truncated = bool_field(data, "truncated")?;
            Ok(format!(
                "git_diff staged={staged} truncated={truncated}\n\n{diff}"
            ))
        }
        ActionId::WriteFile => {
            let path = string_field(data, "path")?;
            let created = bool_field(data, "created")?;
            let bytes_written = number_field(data, "bytes_written")?;
            let sha256 = string_field(data, "sha256")?;
            Ok(format!(
                "write_file path={path} created={created} bytes_written={bytes_written} sha256={sha256}"
            ))
        }
        ActionId::RunVerifier => Err(format!(
            "script action `{}` is not a supported action",
            action.as_str()
        )),
    }
}

fn string_field<'a>(data: &'a Value, key: &str) -> std::result::Result<&'a str, String> {
    data.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("helper returned malformed data: `{key}` must be a string"))
}

fn bool_field(data: &Value, key: &str) -> std::result::Result<bool, String> {
    data.get(key)
        .and_then(Value::as_bool)
        .ok_or_else(|| format!("helper returned malformed data: `{key}` must be a boolean"))
}

fn number_field(data: &Value, key: &str) -> std::result::Result<u64, String> {
    data.get(key)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("helper returned malformed data: `{key}` must be a number"))
}

fn optional_number_field(data: &Value, key: &str) -> std::result::Result<String, String> {
    match data.get(key) {
        None | Some(Value::Null) => Ok("none".into()),
        Some(Value::Number(number)) => Ok(number.to_string()),
        Some(_) => Err(format!(
            "helper returned malformed data: `{key}` must be a number or null"
        )),
    }
}
