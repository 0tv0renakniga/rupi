use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::error::Result;

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
    VerificationStarted,
    VerificationFinished,
    ReviewVerdict,
    ContextCompacted,
    PolicyDenied,
    WikiCompiled,
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
