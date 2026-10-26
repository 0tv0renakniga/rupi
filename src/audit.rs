use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::error::{AgentError, Result};

/// Maximum serialized size of one audit record, including the trailing newline.
pub const MAX_AUDIT_RECORD_BYTES: usize = 16 * 1024;

/// Structured, append-only audit events. Audit files live outside `.ai-wiki/`
/// so raw evidence is not mixed into compiled memory and prompt context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditKind {
    TaskStarted,
    PhaseChanged,
    WikiLoaded,
    PlanProduced,
    ToolRequested,
    ApprovalRequested,
    ApprovalResolved,
    ToolFinished,
    ScriptFinished,
    VerificationStarted,
    VerificationFinished,
    ReviewVerdict,
    ContextCompacted,
    PolicyDenied,
    WikiCompiled,
    EvidenceCompiled,
    TaskFinished,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuditRecord {
    pub timestamp: DateTime<Utc>,
    pub task_id: Uuid,
    pub kind: AuditKind,
    pub message: String,
    #[serde(default)]
    pub metadata: Value,
}

impl AuditRecord {
    pub fn new(
        task_id: Uuid,
        kind: AuditKind,
        message: impl Into<String>,
        metadata: Value,
    ) -> Self {
        Self {
            timestamp: Utc::now(),
            task_id,
            kind,
            message: message.into(),
            metadata,
        }
    }
}

#[derive(Debug, Clone)]
pub struct AuditLog {
    path: PathBuf,
}

impl AuditLog {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Validate the destination and create its parent directory before a task
    /// starts. This keeps a bad audit path from surfacing after work begins.
    pub fn prepare(&self) -> Result<()> {
        if self.path.is_dir() {
            return Err(AgentError::Config(
                "audit path must not be an existing directory".into(),
            ));
        }
        if let Some(parent) = self.path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)?;
        }
        Ok(())
    }

    /// Append one JSON line. The file is opened with append semantics so
    /// existing evidence is never rewritten.
    pub async fn append(&self, record: &AuditRecord) -> Result<()> {
        use tokio::io::AsyncWriteExt;

        if let Some(parent) = self.path.parent()
            && !parent.as_os_str().is_empty()
        {
            tokio::fs::create_dir_all(parent).await?;
        }

        let mut line = serde_json::to_vec(record)?;
        line.push(b'\n');
        if line.len() > MAX_AUDIT_RECORD_BYTES {
            return Err(AgentError::Config(format!(
                "audit record exceeded the {MAX_AUDIT_RECORD_BYTES}-byte bound"
            )));
        }

        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .await?;
        file.write_all(&line).await?;
        file.flush().await?;
        Ok(())
    }
}

#[async_trait::async_trait]
pub trait AuditSink: Send + Sync {
    async fn record(&self, record: &AuditRecord) -> Result<()>;
}

#[async_trait::async_trait]
impl AuditSink for AuditLog {
    async fn record(&self, record: &AuditRecord) -> Result<()> {
        self.append(record).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use uuid::Uuid;

    #[tokio::test]
    async fn prepares_parent_and_rejects_oversized_records() {
        let root = std::env::temp_dir().join(format!("rupi-audit-{}", Uuid::new_v4()));
        let path = root.join("nested/audit.jsonl");
        let log = AuditLog::new(&path);
        log.prepare().expect("prepare audit path");
        assert!(root.join("nested").is_dir());

        let record = AuditRecord::new(
            Uuid::new_v4(),
            AuditKind::TaskStarted,
            "x".repeat(MAX_AUDIT_RECORD_BYTES),
            json!({}),
        );
        let error = log.append(&record).await.expect_err("oversized record");
        assert!(error.to_string().contains("byte bound"), "{error}");
        assert!(!path.exists(), "oversized record must not be written");

        let _ = std::fs::remove_dir_all(root);
    }
}
