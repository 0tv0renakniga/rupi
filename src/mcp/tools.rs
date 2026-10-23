use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Canonical, strict tool representation used everywhere inside the harness.
///
/// The only tolerant parsing is [`AgentTool::parse_tolerant`] and
/// [`AgentTool::parse_model_call`], which sit at the model boundary. Tool names
/// and aliases never influence policy after this point.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentTool {
    ReadFile {
        path: String,
        line_start: Option<u32>,
        line_end: Option<u32>,
    },
    WriteFile {
        path: String,
        content: String,
        expected_sha256: Option<String>,
    },
    ApplyPatch {
        path: String,
        unified_diff: String,
        expected_sha256: Option<String>,
    },
    RunCommand {
        program: String,
        args: Vec<String>,
        cwd: Option<String>,
        timeout_secs: Option<u64>,
    },
    GitStatus,
    GitDiff {
        staged: bool,
    },
    McpTool {
        server: String,
        name: String,
        arguments: Value,
    },
}

impl AgentTool {
    /// Parse a model-provided tool call at the narrow model boundary.
    pub fn parse_model_call(name: &str, arguments: &str) -> Result<Self, String> {
        let arguments: Value = serde_json::from_str(arguments)
            .map_err(|error| format!("arguments are not valid JSON: {error}"))?;
        Self::parse_tolerant(name, &arguments)
    }

    /// Tolerant name/argument matching for local-model output.
    ///
    /// The returned value is always the strict canonical enum. Errors are
    /// written as teaching text so the next model turn can correct itself.
    pub fn parse_tolerant(name: &str, arguments: &Value) -> Result<Self, String> {
        if !arguments.is_object() {
            return Err("tool arguments must be a JSON object".into());
        }

        match normalize_name(name).as_str() {
            "readfile" | "read" | "cat" => Ok(Self::ReadFile {
                path: string_arg(arguments, &["path", "file_path", "filename"])
                    .ok_or("read_file requires string field `path`")?
                    .to_owned(),
                line_start: optional_u32(arguments, "line_start")?,
                line_end: optional_u32(arguments, "line_end")?,
            }),

            "writefile" | "write" | "createfile" => Ok(Self::WriteFile {
                path: string_arg(arguments, &["path", "file_path", "filename"])
                    .ok_or("write_file requires string field `path`")?
                    .to_owned(),
                content: string_arg(arguments, &["content", "text", "contents"])
                    .ok_or("write_file requires string field `content`")?
                    .to_owned(),
                expected_sha256: string_arg(arguments, &["expected_sha256"]).map(str::to_owned),
            }),

            "applypatch" | "patch" | "editfile" => Ok(Self::ApplyPatch {
                path: string_arg(arguments, &["path", "file_path", "filename"])
                    .ok_or("apply_patch requires string field `path`")?
                    .to_owned(),
                unified_diff: string_arg(arguments, &["unified_diff", "diff", "patch"])
                    .ok_or("apply_patch requires string field `unified_diff`")?
                    .to_owned(),
                expected_sha256: string_arg(arguments, &["expected_sha256"]).map(str::to_owned),
            }),

            "runcommand" | "run" | "exec" => {
                let program = string_arg(arguments, &["program", "executable", "cmd"]).ok_or(
                    "run_command requires string field `program`; shell command strings are not accepted",
                )?;

                let args = arguments
                    .get("args")
                    .and_then(Value::as_array)
                    .ok_or("run_command requires array field `args`")?
                    .iter()
                    .map(|value| {
                        value
                            .as_str()
                            .map(str::to_owned)
                            .ok_or("run_command args must all be strings")
                    })
                    .collect::<Result<Vec<_>, _>>()?;

                Ok(Self::RunCommand {
                    program: program.to_owned(),
                    args,
                    cwd: string_arg(arguments, &["cwd", "dir"]).map(str::to_owned),
                    timeout_secs: arguments
                        .get("timeout_secs")
                        .and_then(Value::as_u64)
                        .map(|seconds| seconds.min(900)),
                })
            }

            "gitstatus" => Ok(Self::GitStatus),

            "gitdiff" => Ok(Self::GitDiff {
                staged: arguments
                    .get("staged")
                    .map(|value| {
                        value
                            .as_bool()
                            .ok_or("git_diff field `staged` must be a boolean")
                    })
                    .transpose()?
                    .unwrap_or(false),
            }),

            "mcptool" => Ok(Self::McpTool {
                server: string_arg(arguments, &["server"])
                    .ok_or("mcp_tool requires string field `server`")?
                    .to_owned(),
                name: string_arg(arguments, &["name", "tool"])
                    .ok_or("mcp_tool requires string field `name`")?
                    .to_owned(),
                arguments: arguments
                    .get("arguments")
                    .cloned()
                    .unwrap_or_else(|| Value::Object(Default::default())),
            }),

            other => Err(format!(
                "unknown tool `{other}`. Valid tools: read_file, write_file, apply_patch, \
                 run_command, git_status, git_diff, mcp_tool."
            )),
        }
    }

    /// Whether a successful call changes repository state and therefore
    /// invalidates prior verification evidence.
    pub fn requires_mutation_permission(&self) -> bool {
        matches!(
            self,
            Self::WriteFile { .. }
                | Self::ApplyPatch { .. }
                | Self::RunCommand { .. }
                | Self::McpTool { .. }
        )
    }
}

fn normalize_name(value: &str) -> String {
    value
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .map(|character| character.to_ascii_lowercase())
        .collect()
}

fn string_arg<'a>(arguments: &'a Value, aliases: &[&str]) -> Option<&'a str> {
    aliases
        .iter()
        .find_map(|key| arguments.get(*key).and_then(Value::as_str))
}

fn optional_u32(arguments: &Value, key: &str) -> Result<Option<u32>, String> {
    let Some(value) = arguments.get(key) else {
        return Ok(None);
    };
    let raw = value
        .as_u64()
        .ok_or_else(|| format!("field `{key}` must be a non-negative integer"))?;
    u32::try_from(raw)
        .map(Some)
        .map_err(|_| format!("field `{key}` exceeds the supported range"))
}

/// Coarse outcome class used for tracing, verification, and model feedback.
///
/// `is_error` remains the MCP-compatible success flag. This enum adds the
/// harness-specific distinction between policy denials, unavailable
/// capabilities, transport failures, and errors declared by a server.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ToolOutcomeKind {
    #[default]
    Success,
    PolicyDenied,
    ApprovalDenied,
    ApprovalExpired,
    CapabilityUnavailable,
    TransportFailure,
    ServerError,
    InputError,
}

/// Tool result passed back to the model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolOutcome {
    pub tool_call_id: String,
    pub is_error: bool,
    #[serde(default)]
    pub kind: ToolOutcomeKind,
    pub content: Vec<ToolContent>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolContent {
    Text { text: String },
    Json { value: Value },
}

impl ToolOutcome {
    pub fn success(tool_call_id: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            tool_call_id: tool_call_id.into(),
            is_error: false,
            kind: ToolOutcomeKind::Success,
            content: vec![ToolContent::Text { text: text.into() }],
        }
    }

    pub fn error(tool_call_id: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            tool_call_id: tool_call_id.into(),
            is_error: true,
            kind: ToolOutcomeKind::TransportFailure,
            content: vec![ToolContent::Text { text: text.into() }],
        }
    }

    pub fn policy_denied(tool_call_id: impl Into<String>, message: impl Into<String>) -> Self {
        Self::teaching_error(tool_call_id, message).with_kind(ToolOutcomeKind::PolicyDenied)
    }

    pub fn approval_denied(tool_call_id: impl Into<String>, message: impl Into<String>) -> Self {
        Self::error(tool_call_id, message).with_kind(ToolOutcomeKind::ApprovalDenied)
    }

    pub fn approval_expired(tool_call_id: impl Into<String>, message: impl Into<String>) -> Self {
        Self::error(tool_call_id, message).with_kind(ToolOutcomeKind::ApprovalExpired)
    }

    pub fn capability_unavailable(
        tool_call_id: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self::error(tool_call_id, message).with_kind(ToolOutcomeKind::CapabilityUnavailable)
    }

    pub fn server_error(tool_call_id: impl Into<String>, text: impl Into<String>) -> Self {
        Self::error(tool_call_id, text).with_kind(ToolOutcomeKind::ServerError)
    }

    /// Structured feedback for malformed model input.
    pub fn teaching_error(tool_call_id: impl Into<String>, message: impl Into<String>) -> Self {
        Self::error(
            tool_call_id,
            format!(
                "TOOL_INPUT_ERROR: {}. Correct the request and try again.",
                message.into()
            ),
        )
        .with_kind(ToolOutcomeKind::InputError)
    }

    pub fn with_kind(mut self, kind: ToolOutcomeKind) -> Self {
        self.kind = kind;
        self
    }

    pub fn text(&self) -> String {
        self.content
            .iter()
            .map(|item| match item {
                ToolContent::Text { text } => text.clone(),
                ToolContent::Json { value } => value.to_string(),
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn normalizes_tool_names_and_aliases() {
        let tool =
            AgentTool::parse_tolerant("Read_File", &json!({"file_path": "src/main.rs"})).unwrap();
        assert_eq!(
            tool,
            AgentTool::ReadFile {
                path: "src/main.rs".into(),
                line_start: None,
                line_end: None,
            }
        );

        let tool = AgentTool::parse_tolerant("READFILE", &json!({"path": "README.md"})).unwrap();
        assert!(matches!(tool, AgentTool::ReadFile { .. }));

        let tool = AgentTool::parse_tolerant("cat", &json!({"filename": "Cargo.toml"})).unwrap();
        assert!(matches!(tool, AgentTool::ReadFile { .. }));

        let tool = AgentTool::parse_tolerant(
            "create_file",
            &json!({
                "filename": "notes.txt",
                "contents": "hello"
            }),
        )
        .unwrap();
        assert_eq!(
            tool,
            AgentTool::WriteFile {
                path: "notes.txt".into(),
                content: "hello".into(),
                expected_sha256: None,
            }
        );

        let tool = AgentTool::parse_tolerant(
            "Patch",
            &json!({
                "file_path": "src/lib.rs",
                "diff": "--- a\n+++ b\n"
            }),
        )
        .unwrap();
        assert!(matches!(tool, AgentTool::ApplyPatch { .. }));

        let tool = AgentTool::parse_tolerant(
            "RUN",
            &json!({
                "program": "cargo",
                "args": ["test", "--all-targets"],
                "cwd": "src",
                "timeout_secs": 5000
            }),
        )
        .unwrap();
        assert_eq!(
            tool,
            AgentTool::RunCommand {
                program: "cargo".into(),
                args: vec!["test".into(), "--all-targets".into()],
                cwd: Some("src".into()),
                timeout_secs: Some(900),
            }
        );

        assert_eq!(
            AgentTool::parse_tolerant("Git_Status", &json!({})).unwrap(),
            AgentTool::GitStatus
        );

        assert_eq!(
            AgentTool::parse_tolerant("git-diff", &json!({"staged": true})).unwrap(),
            AgentTool::GitDiff { staged: true }
        );

        let tool = AgentTool::parse_tolerant(
            "MCPTool",
            &json!({
                "server": "fs",
                "tool": "read_file",
                "arguments": {"path": "README.md"}
            }),
        )
        .unwrap();
        assert!(matches!(tool, AgentTool::McpTool { .. }));
    }

    #[test]
    fn malformed_arguments_produce_teaching_errors() {
        let error = AgentTool::parse_tolerant("read_file", &json!({})).unwrap_err();
        assert!(error.contains("read_file requires string field `path`"));

        let error = AgentTool::parse_tolerant("write_file", &json!({"path": "x"})).unwrap_err();
        assert!(error.contains("write_file requires string field `content`"));

        let error = AgentTool::parse_tolerant(
            "run_command",
            &json!({
                "command": "cargo test"
            }),
        )
        .unwrap_err();
        assert!(error.contains("shell command strings are not accepted"));

        let error = AgentTool::parse_tolerant(
            "run_command",
            &json!({
                "program": "cargo",
                "args": "test"
            }),
        )
        .unwrap_err();
        assert!(error.contains("requires array field `args`"));

        let error = AgentTool::parse_tolerant(
            "run_command",
            &json!({
                "program": "cargo",
                "args": ["test", 3]
            }),
        )
        .unwrap_err();
        assert!(error.contains("args must all be strings"));

        let error = AgentTool::parse_tolerant("read_file", &json!("src/main.rs")).unwrap_err();
        assert!(error.contains("JSON object"));

        let error = AgentTool::parse_model_call("read_file", "{not json").unwrap_err();
        assert!(error.contains("not valid JSON"));

        let error = AgentTool::parse_tolerant("git_diff", &json!({"staged": "yes"})).unwrap_err();
        assert!(error.contains("must be a boolean"));
    }

    #[test]
    fn unknown_tools_list_the_valid_set() {
        let error = AgentTool::parse_tolerant("delete_everything", &json!({})).unwrap_err();
        assert!(error.contains("unknown tool"));
        assert!(error.contains("read_file"));
        assert!(error.contains("run_command"));
    }

    #[test]
    fn teaching_errors_are_structured_and_explicit() {
        let outcome = ToolOutcome::teaching_error("call_1", "missing `path`");
        assert!(outcome.is_error);
        assert_eq!(outcome.tool_call_id, "call_1");
        assert!(outcome.text().starts_with("TOOL_INPUT_ERROR:"));
        assert!(outcome.text().contains("Correct the request"));
    }
}
