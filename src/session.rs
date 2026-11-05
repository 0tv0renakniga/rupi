//! Bounded append-only session persistence for visible Chat state.
//!
//! Session files live in the harness-owned data directory and use JSONL. Every
//! written record is redacted first, bounded by the configured event size, and
//! appended with a trailing newline. Recovery reads complete lines only, which
//! makes a partially written final line harmless after a crash.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::error::{AgentError, Result};
use crate::events::ChatTurnTelemetry;
use crate::wiki::redact::redact;

/// Version stored in each new session header.
pub const SESSION_SCHEMA_VERSION: u32 = 1;
/// File extension for append-only session records.
pub const SESSION_FILE_EXTENSION: &str = "jsonl";
/// Maximum accepted session identifier length.
pub const MAX_SESSION_ID_BYTES: usize = 64;

const TRUNCATION_MARKER: &str = "...[truncated]";
const MAX_RESTORED_MESSAGES: usize = 1_000;
const MAX_RESTORED_BYTES: usize = 512 * 1024;
const MAX_TELEMETRY_TEXT_BYTES: usize = 128;

/// Role of a restored visible Chat message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionRole {
    User,
    Assistant,
}

impl SessionRole {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
        }
    }
}

/// One visible message recovered from a session log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionMessage {
    pub role: SessionRole,
    pub text: String,
    pub timestamp: String,
}

/// Filesystem and bound configuration shared by all sessions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionConfig {
    pub data_dir: PathBuf,
    pub model: String,
    pub max_record_bytes: usize,
}

impl SessionConfig {
    pub fn new(data_dir: PathBuf, model: impl Into<String>, max_record_bytes: usize) -> Self {
        Self {
            data_dir,
            model: model.into(),
            max_record_bytes,
        }
    }

    fn validate(&self) -> Result<()> {
        if self.data_dir.as_os_str().is_empty() {
            return Err(AgentError::Session(
                "session data directory must not be empty".into(),
            ));
        }
        if self.max_record_bytes == 0 {
            return Err(AgentError::Session(
                "max_session_event_bytes must be greater than zero".into(),
            ));
        }
        Ok(())
    }
}

/// Open session log plus its bounded visible-message view.
#[derive(Debug, Clone)]
pub struct SessionStore {
    config: SessionConfig,
    id: String,
    messages: Vec<SessionMessage>,
}

impl SessionStore {
    /// Open the most recently modified valid session file, or create a new one.
    pub fn open_latest_or_new(config: &SessionConfig) -> Result<Self> {
        config.validate()?;
        fs::create_dir_all(&config.data_dir)?;
        match latest_session_path(&config.data_dir)? {
            Some(path) => Self::open_path(config.clone(), &path),
            None => Self::create_new(config),
        }
    }

    /// Open the most recently modified session file, if one exists.
    pub fn open_latest(config: &SessionConfig) -> Result<Option<Self>> {
        config.validate()?;
        fs::create_dir_all(&config.data_dir)?;
        latest_session_path(&config.data_dir)?
            .map(|path| Self::open_path(config.clone(), &path))
            .transpose()
    }

    /// Create a new empty session file with a header record.
    pub fn create_new(config: &SessionConfig) -> Result<Self> {
        config.validate()?;
        fs::create_dir_all(&config.data_dir)?;
        let id = Uuid::new_v4().simple().to_string();
        let store = Self {
            config: config.clone(),
            id,
            messages: Vec::new(),
        };
        store.append_session_header()?;
        Ok(store)
    }

    /// Resume an exact saved session id.
    pub fn resume(config: &SessionConfig, session_id: &str) -> Result<Self> {
        config.validate()?;
        validate_session_id(session_id)?;
        let path = session_path(&config.data_dir, session_id);
        if !path.is_file() {
            return Err(AgentError::Session(format!(
                "session `{session_id}` does not exist"
            )));
        }
        Self::open_path(config.clone(), &path)
    }

    fn open_path(config: SessionConfig, path: &Path) -> Result<Self> {
        let id = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .ok_or_else(|| AgentError::Session("session file has no valid identifier".into()))?;
        validate_session_id(id)?;
        let messages = read_messages(path)?;
        Ok(Self {
            config,
            id: id.to_owned(),
            messages,
        })
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn path(&self) -> PathBuf {
        session_path(&self.config.data_dir, &self.id)
    }

    pub fn messages(&self) -> &[SessionMessage] {
        &self.messages
    }

    pub fn config(&self) -> &SessionConfig {
        &self.config
    }

    pub fn model(&self) -> &str {
        &self.config.model
    }

    /// Redact and persist one visible user message.
    pub fn append_user_message(&mut self, text: &str) -> Result<()> {
        self.append_message(SessionRole::User, text)
    }

    /// Redact and persist one visible assistant message.
    pub fn append_assistant_message(&mut self, text: &str) -> Result<()> {
        self.append_message(SessionRole::Assistant, text)
    }

    /// Persist a slash-command identity without accepting arbitrary text.
    pub fn append_command(&mut self, command: &str) -> Result<()> {
        let command = command.trim();
        if command.is_empty()
            || command.len() > 64
            || !command
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
        {
            return Err(AgentError::Session(
                "session command identity is invalid".into(),
            ));
        }
        let redacted = redact(command)?;
        self.append_record(&SessionRecord::Command {
            timestamp: now_timestamp(),
            command: redacted,
        })
    }

    /// Persist bounded context telemetry. Oversized records are skipped.
    pub fn append_telemetry(&mut self, telemetry: &ChatTurnTelemetry) -> Result<()> {
        let record = SessionRecord::Telemetry {
            timestamp: now_timestamp(),
            estimated_prompt_tokens: telemetry.estimated_prompt_tokens,
            reserved_output_tokens: telemetry.reserved_output_tokens,
            effective_context_cap_tokens: telemetry.effective_context_cap_tokens,
            safety_margin_tokens: telemetry.safety_margin_tokens,
            context_utilization_percent: telemetry.context_utilization_percent,
            time_to_first_token_ms: telemetry.time_to_first_token_ms,
            visible_output_chars: telemetry.visible_output_chars,
            visible_output_tokens: telemetry.visible_output_tokens,
            finish_reason: bounded_optional(telemetry.finish_reason.as_deref())?,
            empty_visible_response: telemetry.empty_visible_response,
            transport_error_category: bounded_optional(
                telemetry.transport_error_category.as_deref(),
            )?,
            admission_rejected: telemetry.admission_rejected,
        };
        let line = serde_json::to_vec(&record)?;
        if line.len().saturating_add(1) > self.config.max_record_bytes {
            return Ok(());
        }
        self.append_line(&line)
    }

    fn append_session_header(&self) -> Result<()> {
        let created_at = now_timestamp();
        let mut model = redact(&self.config.model)?;
        for _ in 0..32 {
            let line = serde_json::to_vec(&SessionRecord::Session {
                schema_version: SESSION_SCHEMA_VERSION,
                session_id: self.id.clone(),
                created_at: created_at.clone(),
                model: model.clone(),
            })?;
            let required = line.len().saturating_add(1);
            if required <= self.config.max_record_bytes {
                return self.append_line(&line);
            }
            if model.is_empty() {
                break;
            }
            let over = required.saturating_sub(self.config.max_record_bytes).max(1);
            let target = model.len().saturating_sub(over);
            let next = truncate_record_text(&model, target);
            if next.len() >= model.len() {
                model = model
                    .char_indices()
                    .next_back()
                    .map(|(index, _)| model[..index].to_owned())
                    .unwrap_or_default();
            } else {
                model = next;
            }
        }
        Err(AgentError::Session(
            "session header cannot fit within max_session_event_bytes".into(),
        ))
    }

    fn append_message(&mut self, role: SessionRole, text: &str) -> Result<()> {
        let timestamp = now_timestamp();
        let persisted = self.fit_message_text(role, &timestamp, text)?;
        self.append_record(&SessionRecord::Message {
            timestamp: timestamp.clone(),
            role,
            text: persisted.clone(),
        })?;
        self.messages.push(SessionMessage {
            role,
            text: persisted,
            timestamp,
        });
        bound_restored_messages(&mut self.messages);
        Ok(())
    }

    fn fit_message_text(&self, role: SessionRole, timestamp: &str, text: &str) -> Result<String> {
        let redacted = redact(text)?;
        let mut candidate = redacted;
        for _ in 0..32 {
            let line = serde_json::to_vec(&SessionRecord::Message {
                timestamp: timestamp.to_owned(),
                role,
                text: candidate.clone(),
            })?;
            let required = line.len().saturating_add(1);
            if required <= self.config.max_record_bytes {
                return Ok(candidate);
            }
            if candidate.is_empty() {
                break;
            }
            let over = required.saturating_sub(self.config.max_record_bytes).max(1);
            let target = candidate.len().saturating_sub(over);
            let next = truncate_record_text(&candidate, target);
            if next.len() >= candidate.len() {
                candidate = candidate
                    .char_indices()
                    .next_back()
                    .map(|(index, _)| candidate[..index].to_owned())
                    .unwrap_or_default();
            } else {
                candidate = next;
            }
        }
        Err(AgentError::Session(
            "session message cannot fit within max_session_event_bytes".into(),
        ))
    }

    fn append_record(&self, record: &SessionRecord) -> Result<()> {
        let line = serde_json::to_vec(record)?;
        if line.len().saturating_add(1) > self.config.max_record_bytes {
            return Err(AgentError::Session(
                "session record exceeds max_session_event_bytes".into(),
            ));
        }
        self.append_line(&line)
    }

    fn append_line(&self, line: &[u8]) -> Result<()> {
        let path = self.path();
        let mut options = OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(path)?;
        file.write_all(line)?;
        file.write_all(b"\n")?;
        file.flush()?;
        file.sync_data()?;
        Ok(())
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum SessionRecord {
    Session {
        schema_version: u32,
        session_id: String,
        created_at: String,
        model: String,
    },
    Message {
        timestamp: String,
        role: SessionRole,
        text: String,
    },
    Telemetry {
        timestamp: String,
        estimated_prompt_tokens: u32,
        reserved_output_tokens: u32,
        effective_context_cap_tokens: u32,
        safety_margin_tokens: u32,
        context_utilization_percent: u32,
        time_to_first_token_ms: Option<u64>,
        visible_output_chars: usize,
        visible_output_tokens: u32,
        finish_reason: Option<String>,
        empty_visible_response: bool,
        transport_error_category: Option<String>,
        admission_rejected: bool,
    },
    Command {
        timestamp: String,
        command: String,
    },
}

fn now_timestamp() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn bounded_optional(value: Option<&str>) -> Result<Option<String>> {
    value
        .map(redact)
        .transpose()
        .map(|value| value.map(|text| truncate_record_text(&text, MAX_TELEMETRY_TEXT_BYTES)))
}

fn read_messages(path: &Path) -> Result<Vec<SessionMessage>> {
    let data = fs::read(path)?;
    let complete_end = if data.last() == Some(&b'\n') {
        data.len()
    } else {
        data.iter()
            .rposition(|byte| *byte == b'\n')
            .map(|index| index + 1)
            .unwrap_or(0)
    };

    let mut messages = Vec::new();
    for raw in data[..complete_end].split(|byte| *byte == b'\n') {
        if raw.is_empty() {
            continue;
        }
        let Ok(line) = std::str::from_utf8(raw) else {
            continue;
        };
        let line = line.trim_end_matches('\r');
        let Ok(SessionRecord::Message {
            timestamp,
            role,
            text,
        }) = serde_json::from_str::<SessionRecord>(line)
        else {
            continue;
        };
        messages.push(SessionMessage {
            role,
            text,
            timestamp,
        });
        bound_restored_messages(&mut messages);
    }
    Ok(messages)
}

fn bound_restored_messages(messages: &mut Vec<SessionMessage>) {
    while messages.len() > MAX_RESTORED_MESSAGES {
        messages.remove(0);
    }
    while messages.len() > 1
        && messages
            .iter()
            .map(|message| message.text.len())
            .sum::<usize>()
            > MAX_RESTORED_BYTES
    {
        messages.remove(0);
    }
}

fn latest_session_path(dir: &Path) -> Result<Option<PathBuf>> {
    let mut latest: Option<(SystemTime, PathBuf)> = None;
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some(SESSION_FILE_EXTENSION) {
            continue;
        }
        if !entry.file_type()?.is_file() {
            continue;
        }
        let Some(id) = path.file_stem().and_then(|stem| stem.to_str()) else {
            continue;
        };
        if validate_session_id(id).is_err() {
            continue;
        }
        let modified = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        if latest
            .as_ref()
            .is_none_or(|(latest_modified, _)| modified >= *latest_modified)
        {
            latest = Some((modified, path));
        }
    }
    Ok(latest.map(|(_, path)| path))
}

fn session_path(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}.{SESSION_FILE_EXTENSION}"))
}

fn validate_session_id(id: &str) -> Result<()> {
    if id.is_empty()
        || id.len() > MAX_SESSION_ID_BYTES
        || id.starts_with('.')
        || !id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
    {
        return Err(AgentError::Session(
            "session id must be a constrained identifier".into(),
        ));
    }
    Ok(())
}

fn truncate_record_text(text: &str, max_bytes: usize) -> String {
    if text.len() <= max_bytes {
        return text.to_owned();
    }
    if max_bytes <= TRUNCATION_MARKER.len() {
        let mut end = max_bytes.min(TRUNCATION_MARKER.len());
        while end > 0 && !TRUNCATION_MARKER.is_char_boundary(end) {
            end -= 1;
        }
        return TRUNCATION_MARKER[..end].to_owned();
    }
    let mut end = max_bytes - TRUNCATION_MARKER.len();
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{TRUNCATION_MARKER}", &text[..end])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    struct TempSessionDir(PathBuf);

    impl TempSessionDir {
        fn new(label: &str) -> Self {
            Self(
                std::env::temp_dir()
                    .join(format!("rupi-session-{label}-{}", Uuid::new_v4().simple())),
            )
        }

        fn config(&self, max_record_bytes: usize) -> SessionConfig {
            SessionConfig::new(self.0.clone(), "local-model", max_record_bytes)
        }
    }

    impl Drop for TempSessionDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn round_trip_restores_visible_messages_and_latest_session() {
        let dir = TempSessionDir::new("round-trip");
        let config = dir.config(65_536);
        let mut first = SessionStore::create_new(&config).unwrap();
        let first_id = first.id().to_owned();
        first.append_user_message("hello").unwrap();
        first.append_assistant_message("world").unwrap();

        std::thread::sleep(Duration::from_millis(10));
        let mut second = SessionStore::create_new(&config).unwrap();
        let second_id = second.id().to_owned();
        second.append_user_message("new session").unwrap();

        let restored = SessionStore::open_latest_or_new(&config).unwrap();
        assert_eq!(restored.id(), second_id);
        assert_eq!(restored.messages().len(), 1);
        assert_eq!(restored.messages()[0].text, "new session");

        let first = SessionStore::resume(&config, &first_id).unwrap();
        assert_eq!(first.messages().len(), 2);
        assert_eq!(first.messages()[0].text, "hello");
        assert_eq!(first.messages()[1].text, "world");
    }

    #[test]
    fn recovery_ignores_a_trailing_partial_line() {
        let dir = TempSessionDir::new("partial");
        let config = dir.config(65_536);
        let mut session = SessionStore::create_new(&config).unwrap();
        session.append_user_message("before crash").unwrap();
        let path = session.path();

        let mut file = OpenOptions::new().append(true).open(path).unwrap();
        file.write_all(b"{\"kind\":\"message\",\"timestamp\":\"now\",\"role\":\"assistant\"")
            .unwrap();

        let restored = SessionStore::resume(&config, session.id()).unwrap();
        assert_eq!(restored.messages().len(), 1);
        assert_eq!(restored.messages()[0].text, "before crash");
        assert_eq!(restored.messages()[0].role, SessionRole::User);
    }

    #[test]
    fn crash_during_streaming_has_no_false_assistant_completion() {
        let dir = TempSessionDir::new("streaming-crash");
        let config = dir.config(65_536);
        let mut session = SessionStore::create_new(&config).unwrap();
        session.append_user_message("stream this").unwrap();

        let restored = SessionStore::open_latest_or_new(&config).unwrap();
        assert_eq!(restored.messages().len(), 1);
        assert_eq!(restored.messages()[0].role, SessionRole::User);
        assert!(
            restored
                .messages()
                .iter()
                .all(|message| message.role != SessionRole::Assistant)
        );
    }

    #[test]
    fn redacts_secrets_before_writing() {
        let dir = TempSessionDir::new("redaction");
        let config = dir.config(65_536);
        let mut session = SessionStore::create_new(&config).unwrap();
        session
            .append_user_message("api_key=supersecret token=abcdef")
            .unwrap();
        session
            .append_assistant_message("Authorization: Bearer topsecret")
            .unwrap();

        let raw = fs::read_to_string(session.path()).unwrap();
        assert!(!raw.contains("supersecret"));
        assert!(!raw.contains("abcdef"));
        assert!(!raw.contains("topsecret"));
        assert!(raw.contains("[REDACTED]"));

        let restored = SessionStore::resume(&config, session.id()).unwrap();
        assert!(restored.messages()[0].text.contains("[REDACTED]"));
        assert!(restored.messages()[1].text.contains("[REDACTED]"));
    }

    #[test]
    fn records_are_bounded_before_append() {
        let dir = TempSessionDir::new("bounded");
        let config = dir.config(512);
        let mut session = SessionStore::create_new(&config).unwrap();
        session
            .append_assistant_message(&"x".repeat(20_000))
            .unwrap();

        let raw = fs::read_to_string(session.path()).unwrap();
        for line in raw.lines() {
            assert!(line.len() <= 512, "record exceeded bound: {}", line.len());
            serde_json::from_str::<serde_json::Value>(line).expect("valid JSON line");
        }
        let restored = SessionStore::resume(&config, session.id()).unwrap();
        assert!(restored.messages()[0].text.contains(TRUNCATION_MARKER));
    }

    #[test]
    fn persisted_records_contain_only_allowed_field_names() {
        let dir = TempSessionDir::new("fields");
        let config = dir.config(65_536);
        let mut session = SessionStore::create_new(&config).unwrap();
        session.append_user_message("visible question").unwrap();
        session.append_assistant_message("visible answer").unwrap();
        session.append_command("resume").unwrap();
        session
            .append_telemetry(&ChatTurnTelemetry {
                estimated_prompt_tokens: 10,
                reserved_output_tokens: 5,
                effective_context_cap_tokens: 100,
                safety_margin_tokens: 2,
                context_utilization_percent: 10,
                time_to_first_token_ms: Some(12),
                visible_output_chars: 4,
                visible_output_tokens: 1,
                finish_reason: Some("stop".into()),
                empty_visible_response: false,
                transport_error_category: None,
                admission_rejected: false,
            })
            .unwrap();

        let raw = fs::read_to_string(session.path()).unwrap();
        let mut keys = Vec::new();
        for line in raw.lines() {
            let value: serde_json::Value = serde_json::from_str(line).unwrap();
            collect_keys(&value, &mut keys);
        }
        for forbidden in [
            "reasoning",
            "reasoning_content",
            "raw_sse",
            "sse_frame",
            "raw_response",
            "provider_response",
            "credentials",
            "credential_ref",
            "credential_reference",
            "tool_output",
            "raw_payload",
        ] {
            assert!(
                !keys.iter().any(|key| key == forbidden),
                "forbidden field persisted: {forbidden}"
            );
        }
        assert!(!raw.contains("hidden_reasoning"));
    }

    #[test]
    fn rejects_path_like_session_ids() {
        let dir = TempSessionDir::new("ids");
        let config = dir.config(65_536);
        for id in ["", "../escape", "a/b", ".hidden", "session id"] {
            assert!(SessionStore::resume(&config, id).is_err(), "{id:?}");
        }
    }

    fn collect_keys(value: &serde_json::Value, keys: &mut Vec<String>) {
        match value {
            serde_json::Value::Object(map) => {
                for (key, value) in map {
                    keys.push(key.clone());
                    collect_keys(value, keys);
                }
            }
            serde_json::Value::Array(values) => {
                for value in values {
                    collect_keys(value, keys);
                }
            }
            _ => {}
        }
    }
}
