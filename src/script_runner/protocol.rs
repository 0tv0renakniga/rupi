//! Versioned one-request/one-response protocol for fixed helper scripts.
//!
//! The runner writes exactly one bounded JSON request to stdin and accepts
//! exactly one bounded JSON response on stdout. Stdout is protocol data only;
//! stderr is bounded and redacted diagnostic text. Runtime timeout, cancellation,
//! minimal environment, and audit metadata are enforced by the runner.

use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use uuid::Uuid;

use crate::audit::{AuditKind, AuditRecord};
use crate::error::{AgentError, Result};
use crate::wiki::redact::redact;

use super::actions::ActionId;

/// Current script protocol version. Requests and responses must agree.
pub const SCRIPT_PROTOCOL_VERSION: u32 = 1;

/// Maximum serialized request body written to a helper script's stdin.
pub const MAX_REQUEST_BYTES: usize = 64 * 1024;

/// Maximum protocol response accepted from a helper script's stdout.
pub const MAX_RESPONSE_BYTES: usize = 1024 * 1024;

/// Maximum diagnostic bytes retained from a helper script's stderr.
pub const MAX_STDERR_BYTES: usize = 16 * 1024;

/// Maximum bytes retained for a helper error code.
pub const MAX_ERROR_CODE_BYTES: usize = 64;

/// Maximum bytes retained for a helper error message.
pub const MAX_ERROR_MESSAGE_BYTES: usize = 1024;

const MAX_DIAGNOSTIC_BYTES: usize = 2 * 1024;
const TRUNCATION_MARKER: &str = "...[truncated]";

/// Stable harness-side protocol failure codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ProtocolErrorCode {
    RequestTooLarge,
    RequestWriteFailed,
    ResponseTooLarge,
    InvalidResponseEncoding,
    InvalidProtocolJson,
    MissingProtocolResponse,
    TrailingProtocolData,
    InvalidProtocolResponse,
    ProtocolVersionMismatch,
    ScriptExitNonzero,
    ScriptTimeout,
    ScriptCancelled,
    StreamReadFailed,
    ChildWaitFailed,
    ChildTerminationFailed,
}

impl ProtocolErrorCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RequestTooLarge => "REQUEST_TOO_LARGE",
            Self::RequestWriteFailed => "REQUEST_WRITE_FAILED",
            Self::ResponseTooLarge => "RESPONSE_TOO_LARGE",
            Self::InvalidResponseEncoding => "INVALID_RESPONSE_ENCODING",
            Self::InvalidProtocolJson => "INVALID_PROTOCOL_JSON",
            Self::MissingProtocolResponse => "MISSING_PROTOCOL_RESPONSE",
            Self::TrailingProtocolData => "TRAILING_PROTOCOL_DATA",
            Self::InvalidProtocolResponse => "INVALID_PROTOCOL_RESPONSE",
            Self::ProtocolVersionMismatch => "PROTOCOL_VERSION_MISMATCH",
            Self::ScriptExitNonzero => "SCRIPT_EXIT_NONZERO",
            Self::ScriptTimeout => "SCRIPT_TIMEOUT",
            Self::ScriptCancelled => "SCRIPT_CANCELLED",
            Self::StreamReadFailed => "STREAM_READ_FAILED",
            Self::ChildWaitFailed => "CHILD_WAIT_FAILED",
            Self::ChildTerminationFailed => "CHILD_TERMINATION_FAILED",
        }
    }

    pub const fn message(self) -> &'static str {
        match self {
            Self::RequestTooLarge => "helper script request exceeded the configured size bound",
            Self::RequestWriteFailed => "helper script request could not be delivered",
            Self::ResponseTooLarge => "helper script response exceeded the configured size bound",
            Self::InvalidResponseEncoding => "helper script response was not valid UTF-8",
            Self::InvalidProtocolJson => "helper script response was not valid JSON",
            Self::MissingProtocolResponse => "helper script produced no protocol response",
            Self::TrailingProtocolData => "helper script produced more than one protocol response",
            Self::InvalidProtocolResponse => "helper script response did not match the protocol",
            Self::ProtocolVersionMismatch => "helper script protocol version is not supported",
            Self::ScriptExitNonzero => "helper script exited with a nonzero status",
            Self::ScriptTimeout => "helper script exceeded the configured timeout",
            Self::ScriptCancelled => "helper script execution was cancelled",
            Self::StreamReadFailed => "helper script output could not be read",
            Self::ChildWaitFailed => "helper script exit status could not be collected",
            Self::ChildTerminationFailed => "helper script could not be terminated cleanly",
        }
    }
}

impl std::fmt::Display for ProtocolErrorCode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Strict request sent to a fixed helper script.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ScriptRequest {
    protocol_version: u32,
    action: ActionId,
    project_root: String,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    params: Map<String, Value>,
}

impl ScriptRequest {
    pub fn new(action: ActionId, project_root: &Path, params: Map<String, Value>) -> Result<Self> {
        let project_root = project_root.to_str().ok_or_else(|| {
            AgentError::Script("project root is not valid UTF-8 for the script protocol".to_owned())
        })?;

        Ok(Self {
            protocol_version: SCRIPT_PROTOCOL_VERSION,
            action,
            project_root: project_root.to_owned(),
            params,
        })
    }

    pub fn protocol_version(&self) -> u32 {
        self.protocol_version
    }

    pub fn action(&self) -> ActionId {
        self.action
    }

    pub fn project_root(&self) -> &str {
        &self.project_root
    }

    pub fn params(&self) -> &Map<String, Value> {
        &self.params
    }

    pub fn to_json_bytes(&self) -> Result<Vec<u8>> {
        Ok(serde_json::to_vec(self)?)
    }
}

/// Wire response accepted from a helper script.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScriptResponse {
    pub protocol_version: u32,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ScriptError>,
}

impl ScriptResponse {
    pub(crate) fn parse_stdout(stdout: &[u8]) -> std::result::Result<Self, ProtocolErrorCode> {
        let text =
            std::str::from_utf8(stdout).map_err(|_| ProtocolErrorCode::InvalidResponseEncoding)?;
        if text.trim().is_empty() {
            return Err(ProtocolErrorCode::MissingProtocolResponse);
        }

        let mut values = serde_json::Deserializer::from_str(text).into_iter::<Value>();
        let first = match values.next() {
            None => return Err(ProtocolErrorCode::MissingProtocolResponse),
            Some(Ok(value)) => value,
            Some(Err(_)) => return Err(ProtocolErrorCode::InvalidProtocolJson),
        };
        if values.next().is_some() {
            return Err(ProtocolErrorCode::TrailingProtocolData);
        }

        let response: Self = serde_json::from_value(first)
            .map_err(|_| ProtocolErrorCode::InvalidProtocolResponse)?;
        response.validate()?;
        Ok(response)
    }

    fn validate(&self) -> std::result::Result<(), ProtocolErrorCode> {
        if self.protocol_version != SCRIPT_PROTOCOL_VERSION {
            return Err(ProtocolErrorCode::ProtocolVersionMismatch);
        }

        match (self.ok, self.data.as_ref(), self.error.as_ref()) {
            (true, Some(_), None) => Ok(()),
            (false, None, Some(error)) if is_valid_error_code(&error.code) => Ok(()),
            _ => Err(ProtocolErrorCode::InvalidProtocolResponse),
        }
    }
}

/// Helper-declared error response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScriptError {
    pub code: String,
    pub message: String,
}

/// Structured, bounded outcome of one script action.
#[derive(Clone, PartialEq)]
pub struct ScriptOutcome {
    action: ActionId,
    protocol_version: u32,
    ok: bool,
    data: Option<Value>,
    error: Option<ScriptOutcomeError>,
    diagnostic: Option<String>,
    exit_code: Option<i32>,
    script: Option<PathBuf>,
    duration: Duration,
}

impl ScriptOutcome {
    pub fn action(&self) -> ActionId {
        self.action
    }

    pub fn protocol_version(&self) -> u32 {
        self.protocol_version
    }

    pub fn is_ok(&self) -> bool {
        self.ok
    }

    pub fn data(&self) -> Option<&Value> {
        self.data.as_ref()
    }

    pub fn error(&self) -> Option<&ScriptOutcomeError> {
        self.error.as_ref()
    }

    pub fn diagnostic(&self) -> Option<&str> {
        self.diagnostic.as_deref()
    }

    pub fn exit_code(&self) -> Option<i32> {
        self.exit_code
    }

    /// Canonical helper-script path when launch succeeded.
    pub fn script(&self) -> Option<&Path> {
        self.script.as_deref()
    }

    /// Elapsed wall time from execution start through cleanup.
    pub fn duration(&self) -> Duration {
        self.duration
    }

    /// Stable result classification: `OK` or the bounded error code.
    pub fn result_code(&self) -> &str {
        self.error
            .as_ref()
            .map_or("OK", |error| error.code.as_str())
    }

    /// Bounded audit metadata safe to persist outside model context.
    ///
    /// This intentionally excludes request parameters, stdout data, and stderr
    /// diagnostics. It includes only fixed action/script identity, outcome
    /// classification, duration, and exit status.
    pub fn audit_metadata(&self) -> Value {
        let script = self
            .script
            .as_deref()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.action.script_file_name().to_owned());
        let mut metadata = json!({
            "action": self.action.as_str(),
            "script": script,
            "script_file": self.action.script_file_name(),
            "ok": self.ok,
            "result": self.result_code(),
            "protocol_version": self.protocol_version,
            "duration_ms": u64::try_from(self.duration.as_millis()).unwrap_or(u64::MAX),
            "exit_code": self.exit_code,
        });

        if self.action == ActionId::WriteFile
            && self.ok
            && let Some(data) = self.data.as_ref()
        {
            if let Some(path) = data.get("path").and_then(Value::as_str) {
                metadata["path"] = Value::String(bounded_audit_text(path, 256));
            }
            if let Some(bytes_written) = data.get("bytes_written").and_then(Value::as_u64) {
                metadata["bytes_written"] = Value::from(bytes_written);
            }
            for key in ["sha256", "previous_sha256"] {
                if let Some(digest) = data.get(key).and_then(Value::as_str)
                    && digest.len() == 64
                    && digest
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
                {
                    metadata[key] = Value::String(digest.to_owned());
                }
            }
            if let Some(created) = data.get("created").and_then(Value::as_bool) {
                metadata["created"] = Value::Bool(created);
            }
        }

        metadata
    }

    /// Build one bounded audit record for this action.
    pub fn audit_record(&self, task_id: Uuid) -> AuditRecord {
        AuditRecord::new(
            task_id,
            AuditKind::ScriptFinished,
            format!(
                "script action {} {}",
                self.action.as_str(),
                self.result_code()
            ),
            self.audit_metadata(),
        )
    }

    pub(crate) fn set_duration(&mut self, duration: Duration) {
        self.duration = duration;
    }

    pub(crate) fn success(
        action: ActionId,
        script: Option<PathBuf>,
        data: Value,
        diagnostic: Option<String>,
        exit_code: Option<i32>,
    ) -> Self {
        Self {
            action,
            protocol_version: SCRIPT_PROTOCOL_VERSION,
            ok: true,
            data: Some(data),
            error: None,
            diagnostic,
            exit_code,
            script,
            duration: Duration::ZERO,
        }
    }

    pub(crate) fn helper_failure(
        action: ActionId,
        script: Option<PathBuf>,
        error: ScriptError,
        diagnostic: Option<String>,
        exit_code: Option<i32>,
    ) -> Self {
        Self {
            action,
            protocol_version: SCRIPT_PROTOCOL_VERSION,
            ok: false,
            data: None,
            error: Some(ScriptOutcomeError {
                code: error.code,
                message: sanitize_message(&error.message),
            }),
            diagnostic,
            exit_code,
            script,
            duration: Duration::ZERO,
        }
    }

    pub(crate) fn protocol_failure(
        action: ActionId,
        script: Option<PathBuf>,
        code: ProtocolErrorCode,
        diagnostic: Option<String>,
        exit_code: Option<i32>,
    ) -> Self {
        Self {
            action,
            protocol_version: SCRIPT_PROTOCOL_VERSION,
            ok: false,
            data: None,
            error: Some(ScriptOutcomeError {
                code: code.as_str().to_owned(),
                message: code.message().to_owned(),
            }),
            diagnostic,
            exit_code,
            script,
            duration: Duration::ZERO,
        }
    }
}

impl fmt::Debug for ScriptOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ScriptOutcome")
            .field("action", &self.action)
            .field("protocol_version", &self.protocol_version)
            .field("ok", &self.ok)
            .field("error", &self.error)
            .field("diagnostic", &self.diagnostic)
            .field("exit_code", &self.exit_code)
            .field("script", &self.script)
            .field("duration", &self.duration)
            .field("data_present", &self.data.is_some())
            .finish()
    }
}

/// Bounded, UI-safe action error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptOutcomeError {
    code: String,
    message: String,
}

impl ScriptOutcomeError {
    pub fn code(&self) -> &str {
        &self.code
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

pub(crate) fn sanitize_diagnostic(bytes: &[u8], truncated: bool) -> Option<String> {
    if bytes.is_empty() && !truncated {
        return None;
    }

    let text = String::from_utf8_lossy(bytes);
    let redacted = redact(&text).unwrap_or_else(|_| text.into_owned());
    let trimmed = redacted.trim();
    if trimmed.is_empty() && !truncated {
        return None;
    }

    let mut source = trimmed.to_owned();
    if truncated {
        source.insert_str(0, "[stderr truncated]\n");
    }
    Some(bound_text(&source, MAX_DIAGNOSTIC_BYTES))
}

fn sanitize_message(raw: &str) -> String {
    let redacted = redact(raw).unwrap_or_else(|_| raw.to_owned());
    let trimmed = redacted.trim();
    if trimmed.is_empty() {
        return "helper script reported an error without a message".to_owned();
    }
    bound_text(trimmed, MAX_ERROR_MESSAGE_BYTES)
}

fn bounded_audit_text(text: &str, cap: usize) -> String {
    let sanitized = text
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect::<String>();
    bound_text(&sanitized, cap)
}

fn bound_text(text: &str, cap: usize) -> String {
    if text.len() <= cap {
        return text.to_owned();
    }
    if cap <= TRUNCATION_MARKER.len() {
        let mut end = cap.min(text.len());
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        return text[..end].to_owned();
    }

    let limit = cap - TRUNCATION_MARKER.len();
    let mut end = limit.min(text.len());
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{}", &text[..end], TRUNCATION_MARKER)
}

fn is_valid_error_code(code: &str) -> bool {
    !code.is_empty()
        && code.len() <= MAX_ERROR_CODE_BYTES
        && code
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn response_bytes(value: Value) -> Vec<u8> {
        serde_json::to_vec(&value).expect("serialize test response")
    }

    #[test]
    fn request_is_versioned_and_strict() {
        let mut params = Map::new();
        params.insert("path".into(), json!("src/lib.rs"));
        let request = ScriptRequest::new(
            ActionId::ReadFile,
            Path::new("/tmp/project"),
            params.clone(),
        )
        .expect("request");
        assert_eq!(request.protocol_version(), SCRIPT_PROTOCOL_VERSION);
        assert_eq!(request.action(), ActionId::ReadFile);
        assert_eq!(request.project_root(), "/tmp/project");
        assert_eq!(request.params(), &params);

        let wire: Value =
            serde_json::from_slice(&request.to_json_bytes().expect("request JSON")).expect("JSON");
        assert_eq!(wire["protocol_version"], SCRIPT_PROTOCOL_VERSION);
        assert_eq!(wire["action"], "read_file");
        assert_eq!(wire["project_root"], "/tmp/project");
        assert_eq!(wire["params"]["path"], "src/lib.rs");
    }

    #[test]
    fn parses_valid_success_and_helper_error_responses() {
        let success = ScriptResponse::parse_stdout(&response_bytes(json!({
            "protocol_version": SCRIPT_PROTOCOL_VERSION,
            "ok": true,
            "data": {"content": "hello"}
        })))
        .expect("success response");
        assert!(success.ok);
        assert_eq!(success.data.expect("data")["content"], "hello");

        let failure = ScriptResponse::parse_stdout(&response_bytes(json!({
            "protocol_version": SCRIPT_PROTOCOL_VERSION,
            "ok": false,
            "error": {
                "code": "PATH_OUTSIDE_PROJECT_ROOT",
                "message": "The requested path is not permitted."
            }
        })))
        .expect("error response");
        assert!(!failure.ok);
        assert_eq!(
            failure.error.expect("error").code,
            "PATH_OUTSIDE_PROJECT_ROOT"
        );
    }

    #[test]
    fn rejects_invalid_protocol_shapes_and_versions() {
        let cases = [
            (
                json!({"ok": true, "data": {}}),
                ProtocolErrorCode::InvalidProtocolResponse,
            ),
            (
                json!({"protocol_version": 2, "ok": true, "data": {}}),
                ProtocolErrorCode::ProtocolVersionMismatch,
            ),
            (
                json!({"protocol_version": SCRIPT_PROTOCOL_VERSION, "ok": true}),
                ProtocolErrorCode::InvalidProtocolResponse,
            ),
            (
                json!({"protocol_version": SCRIPT_PROTOCOL_VERSION, "ok": false}),
                ProtocolErrorCode::InvalidProtocolResponse,
            ),
            (
                json!({
                    "protocol_version": SCRIPT_PROTOCOL_VERSION,
                    "ok": false,
                    "error": {"code": "lowercase", "message": "bad"}
                }),
                ProtocolErrorCode::InvalidProtocolResponse,
            ),
            (
                json!({
                    "protocol_version": SCRIPT_PROTOCOL_VERSION,
                    "ok": true,
                    "data": {},
                    "error": {"code": "BAD", "message": "bad"}
                }),
                ProtocolErrorCode::InvalidProtocolResponse,
            ),
        ];

        for (value, expected) in cases {
            assert_eq!(
                ScriptResponse::parse_stdout(&response_bytes(value)),
                Err(expected)
            );
        }
    }

    #[test]
    fn detects_invalid_encoding_json_trailing_data_and_empty_output() {
        assert_eq!(
            ScriptResponse::parse_stdout(&[0xff, 0xfe]),
            Err(ProtocolErrorCode::InvalidResponseEncoding)
        );
        assert_eq!(
            ScriptResponse::parse_stdout(b"{not json"),
            Err(ProtocolErrorCode::InvalidProtocolJson)
        );
        assert_eq!(
            ScriptResponse::parse_stdout(b"   \n\t"),
            Err(ProtocolErrorCode::MissingProtocolResponse)
        );

        let first = response_bytes(json!({
            "protocol_version": SCRIPT_PROTOCOL_VERSION,
            "ok": true,
            "data": {}
        }));
        let mut multiple = first.clone();
        multiple.extend_from_slice(&first);
        assert_eq!(
            ScriptResponse::parse_stdout(&multiple),
            Err(ProtocolErrorCode::TrailingProtocolData)
        );
    }

    #[test]
    fn write_audit_metadata_includes_only_safe_fields() {
        let digest = "ab".repeat(32);
        let outcome = ScriptOutcome::success(
            ActionId::WriteFile,
            None,
            json!({
                "path": "src/new\nfile.txt",
                "created": true,
                "bytes_written": 12,
                "sha256": digest,
                "previous_sha256": "cd".repeat(32),
                "content": "WRITE_CONTENT_SECRET"
            }),
            None,
            Some(0),
        );
        let metadata = outcome.audit_metadata();
        assert_eq!(metadata["action"], "write_file");
        assert_eq!(metadata["path"], "src/new file.txt");
        assert_eq!(metadata["created"], true);
        assert_eq!(metadata["bytes_written"], 12);
        assert_eq!(metadata["sha256"], digest);
        assert_eq!(metadata["previous_sha256"], "cd".repeat(32));
        assert!(metadata.get("content").is_none());
        assert!(!metadata.to_string().contains("WRITE_CONTENT_SECRET"));

        let read = ScriptOutcome::success(
            ActionId::ReadFile,
            None,
            json!({"path": "src/lib.rs", "content": "READ_CONTENT_SECRET"}),
            None,
            Some(0),
        );
        assert!(
            !read
                .audit_metadata()
                .to_string()
                .contains("READ_CONTENT_SECRET")
        );
    }

    #[test]
    fn error_messages_and_diagnostics_are_bounded_and_redacted() {
        let message = sanitize_message(&format!(
            "authorization: Bearer SUPER_SECRET\n{}",
            "x".repeat(MAX_ERROR_MESSAGE_BYTES * 2)
        ));
        assert!(message.len() <= MAX_ERROR_MESSAGE_BYTES);
        assert!(message.contains(TRUNCATION_MARKER));
        assert!(!message.contains("SUPER_SECRET"));

        let diagnostic = sanitize_diagnostic(
            format!(
                "authorization: Bearer SUPER_SECRET\n{}",
                "y".repeat(MAX_STDERR_BYTES * 2)
            )
            .as_bytes(),
            true,
        )
        .expect("diagnostic");
        assert!(diagnostic.len() <= MAX_DIAGNOSTIC_BYTES);
        assert!(diagnostic.contains("truncated"));
        assert!(!diagnostic.contains("SUPER_SECRET"));
    }
}
