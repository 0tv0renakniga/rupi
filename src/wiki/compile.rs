use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

use crate::error::{AgentError, Result};
use crate::wiki::redact::redact;
use crate::wiki::{
    DEFAULT_SCHEMA_VERSION, DEFAULT_TITLE, REQUIRED_HEADINGS, WikiContext, WikiFrontmatter,
    WikiLimits, WikiStatus, log_path, render_frontmatter, session_record_path, sessions_dir,
    split_sections, state_path, truncate_utf8,
};

const PRIOR_TRUNCATION_MARKER: &str = "\n...[prior context truncated]...\n";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionSummary {
    pub tool: String,
    pub summary: String,
    pub ok: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerificationSummary {
    pub command: String,
    pub exit_code: Option<i32>,
    pub required: bool,
    #[serde(default)]
    pub summary: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SessionTrace {
    pub session_id: String,
    pub task_summary: String,
    #[serde(default)]
    pub mode: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub changed_files: BTreeSet<String>,
    #[serde(default)]
    pub inspected_files: BTreeSet<String>,
    #[serde(default)]
    pub git_state: Vec<String>,
    pub actions: Vec<ActionSummary>,
    pub verification: Vec<VerificationSummary>,
    pub policy_denials: Vec<String>,
    pub blockers: Vec<String>,
    pub diff_stat: Option<String>,
    pub decisions: Vec<String>,
    pub open_items: Vec<String>,
}

impl SessionTrace {
    pub fn new(
        session_id: impl Into<String>,
        task_summary: impl Into<String>,
        started_at: impl Into<String>,
    ) -> Self {
        Self {
            session_id: session_id.into(),
            task_summary: task_summary.into(),
            started_at: started_at.into(),
            ..Self::default()
        }
    }

    /// Bound and redact every trace-derived value before persistence.
    pub fn redacted(&self, limits: &WikiLimitsWithTrace) -> Result<Self> {
        let field = |value: &str| sanitize_field(value, limits.max_trace_field_bytes);

        let changed_files = self
            .changed_files
            .iter()
            .take(limits.max_trace_items)
            .map(|value| field(value))
            .collect::<Result<BTreeSet<_>>>()?;

        let inspected_files = self
            .inspected_files
            .iter()
            .take(limits.max_trace_items)
            .map(|value| field(value))
            .collect::<Result<BTreeSet<_>>>()?;

        let actions = self
            .actions
            .iter()
            .take(limits.max_trace_items)
            .map(|action| {
                Ok(ActionSummary {
                    tool: field(&action.tool)?,
                    summary: field(&action.summary)?,
                    ok: action.ok,
                })
            })
            .collect::<Result<Vec<_>>>()?;

        let verification = self
            .verification
            .iter()
            .take(limits.max_trace_items)
            .map(|check| {
                Ok(VerificationSummary {
                    command: field(&check.command)?,
                    exit_code: check.exit_code,
                    required: check.required,
                    summary: check.summary.as_deref().map(field).transpose()?,
                })
            })
            .collect::<Result<Vec<_>>>()?;

        let list = |values: &[String]| -> Result<Vec<String>> {
            values
                .iter()
                .take(limits.max_trace_items)
                .map(|value| field(value))
                .collect()
        };

        Ok(Self {
            session_id: field(&self.session_id)?,
            task_summary: field(&self.task_summary)?,
            mode: field(&self.mode)?,
            started_at: field(&self.started_at)?,
            finished_at: self.finished_at.as_deref().map(field).transpose()?,
            changed_files,
            inspected_files,
            git_state: list(&self.git_state)?,
            actions,
            verification,
            policy_denials: list(&self.policy_denials)?,
            blockers: list(&self.blockers)?,
            diff_stat: self.diff_stat.as_deref().map(field).transpose()?,
            decisions: list(&self.decisions)?,
            open_items: list(&self.open_items)?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WikiLimitsWithTrace {
    pub wiki: WikiLimits,
    pub max_prior_section_bytes: usize,
    pub max_trace_field_bytes: usize,
    pub max_trace_items: usize,
    pub max_log_entry_bytes: usize,
    pub max_session_record_bytes: usize,
}

impl Default for WikiLimitsWithTrace {
    fn default() -> Self {
        Self {
            wiki: WikiLimits::default(),
            max_prior_section_bytes: 2_048,
            max_trace_field_bytes: 256,
            max_trace_items: 24,
            max_log_entry_bytes: 512,
            max_session_record_bytes: 64 * 1024,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct WikiCompiler {
    pub limits: WikiLimitsWithTrace,
}

impl WikiCompiler {
    pub fn with_limits(limits: WikiLimitsWithTrace) -> Self {
        Self { limits }
    }

    pub async fn compile(
        &self,
        root: &Path,
        prior: &WikiContext,
        trace: &SessionTrace,
        today: &str,
    ) -> Result<CompileReport> {
        let trace = trace.redacted(&self.limits)?;
        let mut status = derive_status(&trace);
        if status == WikiStatus::Green && prior_has_open_items(prior) {
            status = WikiStatus::Blocked;
        }
        let session_count = prior.frontmatter.session_count.saturating_add(1);
        let today = sanitize_field(today, self.limits.max_trace_field_bytes)?;

        let title = if prior.frontmatter.title.trim().is_empty() {
            DEFAULT_TITLE.to_owned()
        } else {
            sanitize_field(&prior.frontmatter.title, self.limits.max_trace_field_bytes)?
        };
        let tags = prior
            .frontmatter
            .tags
            .iter()
            .take(self.limits.max_trace_items)
            .map(|tag| sanitize_field(tag, self.limits.max_trace_field_bytes))
            .collect::<Result<BTreeSet<_>>>()?
            .into_iter()
            .collect::<Vec<_>>();
        let risk_level = prior
            .frontmatter
            .risk_level
            .as_deref()
            .map(|value| sanitize_field(value, self.limits.max_trace_field_bytes))
            .transpose()?;
        let last_verified_commit = prior
            .frontmatter
            .last_verified_commit
            .as_deref()
            .map(|value| sanitize_field(value, self.limits.max_trace_field_bytes))
            .transpose()?;

        let frontmatter = WikiFrontmatter {
            schema_version: DEFAULT_SCHEMA_VERSION,
            title,
            tags,
            updated: today.clone(),
            session_count,
            status,
            risk_level,
            last_verified_commit,
        };

        let body = render_body(prior, &trace, status, &today, &self.limits)?;
        if body.len() > self.limits.wiki.max_body_bytes {
            return Err(AgentError::Wiki(format!(
                "compiled body is {} bytes, exceeding the configured limit of {} bytes",
                body.len(),
                self.limits.wiki.max_body_bytes
            )));
        }

        let content = format!("{}\n{}", render_frontmatter(&frontmatter), body);
        if content.len() > self.limits.wiki.max_state_bytes {
            return Err(AgentError::Wiki(format!(
                "compiled state is {} bytes, exceeding the configured limit of {} bytes",
                content.len(),
                self.limits.wiki.max_state_bytes
            )));
        }

        let sessions = sessions_dir(root);
        tokio::fs::create_dir_all(&sessions).await?;

        let session_record = session_record_path(root);
        let mut record = serde_json::to_vec(&trace)?;
        record.push(b'\n');
        if record.len() > self.limits.max_session_record_bytes {
            return Err(AgentError::Wiki(format!(
                "session record is {} bytes, exceeding the configured limit of {} bytes",
                record.len(),
                self.limits.max_session_record_bytes
            )));
        }
        append_bytes(&session_record, &record).await?;

        let log = log_path(root);
        let entry = bounded_log_entry(
            &format!(
                "- {} — {} — {} — {}",
                today,
                trace.session_id,
                trace.task_summary,
                status.as_str()
            ),
            self.limits.max_log_entry_bytes,
        );
        append_bytes(&log, entry.as_bytes()).await?;

        let state = state_path(root);
        atomic_write(&state, content.as_bytes()).await?;

        Ok(CompileReport {
            state_path: state,
            log_path: log,
            session_record_path: session_record,
            status,
            session_count,
        })
    }
}

/// Compile the next deterministic `state-of-the-world.md` with default limits.
pub async fn compile(
    root: &Path,
    prior: &WikiContext,
    trace: &SessionTrace,
    today: &str,
) -> Result<CompileReport> {
    WikiCompiler::default()
        .compile(root, prior, trace, today)
        .await
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompileReport {
    pub state_path: PathBuf,
    pub log_path: PathBuf,
    pub session_record_path: PathBuf,
    pub status: WikiStatus,
    pub session_count: u32,
}

/// Derive status only from recorded evidence, never from task text or claims.
pub fn derive_status(trace: &SessionTrace) -> WikiStatus {
    if trace
        .verification
        .iter()
        .any(|check| check.exit_code.is_some_and(|code| code != 0))
    {
        return WikiStatus::Red;
    }

    let required = trace
        .verification
        .iter()
        .filter(|check| check.required)
        .collect::<Vec<_>>();

    if required.is_empty()
        || required.iter().any(|check| check.exit_code != Some(0))
        || !trace.policy_denials.is_empty()
        || !trace.blockers.is_empty()
        || !trace.open_items.is_empty()
    {
        return WikiStatus::Blocked;
    }

    WikiStatus::Green
}

fn prior_has_open_items(prior: &WikiContext) -> bool {
    let Some(section) = prior.section("Open items") else {
        return false;
    };
    let current = section
        .content
        .split("\n### Prior context")
        .next()
        .unwrap_or_default()
        .trim();

    !current.is_empty() && current != "None recorded." && current != "- None recorded."
}

fn bounded_log_entry(entry: &str, max_bytes: usize) -> String {
    const MARKER: &str = "...[log entry truncated]...";

    if entry.len() < max_bytes {
        return format!("{entry}\n");
    }

    let budget = max_bytes.saturating_sub(MARKER.len() + 1);
    let mut bounded = truncate_utf8(entry, budget, MARKER);
    bounded.push('\n');
    bounded
}

pub(crate) fn sanitize_field(value: &str, max_bytes: usize) -> Result<String> {
    let redacted = redact(value)?;
    let single_line = redacted
        .replace("\r\n", " ")
        .replace(['\r', '\n'], " ")
        .trim()
        .to_owned();
    Ok(truncate_utf8(
        &single_line,
        max_bytes,
        "...[field truncated]...",
    ))
}

fn render_body(
    prior: &WikiContext,
    trace: &SessionTrace,
    status: WikiStatus,
    today: &str,
    limits: &WikiLimitsWithTrace,
) -> Result<String> {
    let prior_sections = if prior.sections.is_empty() {
        split_sections(&prior.body, &limits.wiki)
    } else {
        prior.sections.clone()
    };

    let mut output = String::new();

    output.push_str("## Current state\n\n");
    writeln!(output, "- Status: `{}`", status.as_str()).ok();
    writeln!(output, "- Session: `{}`", trace.session_id).ok();
    writeln!(
        output,
        "- Task: {}",
        if trace.task_summary.is_empty() {
            "not recorded"
        } else {
            &trace.task_summary
        }
    )
    .ok();
    writeln!(output, "- Updated: {}", today).ok();
    if !trace.started_at.is_empty() {
        writeln!(output, "- Started: {}", trace.started_at).ok();
    }
    if let Some(finished_at) = &trace.finished_at
        && !finished_at.is_empty()
    {
        writeln!(output, "- Finished: {finished_at}").ok();
    }

    write_list(
        &mut output,
        "Changed files",
        trace.changed_files.iter().map(String::as_str),
    );
    if trace.actions.is_empty() {
        output.push_str("- Actions: none recorded\n");
    } else {
        output.push_str("- Actions:\n");
        for action in &trace.actions {
            writeln!(
                output,
                "  - [{}] {}: {}",
                if action.ok { "ok" } else { "error" },
                action.tool,
                action.summary
            )
            .ok();
        }
    }
    write_list(
        &mut output,
        "Policy denials",
        trace.policy_denials.iter().map(String::as_str),
    );
    write_list(
        &mut output,
        "Blockers",
        trace.blockers.iter().map(String::as_str),
    );
    if let Some(diff_stat) = &trace.diff_stat {
        writeln!(output, "- Diff stat: {diff_stat}").ok();
    }
    append_prior(
        &mut output,
        prior_sections
            .iter()
            .find(|section| section.heading.eq_ignore_ascii_case("Current state"))
            .map(|section| section.content.as_str()),
        limits,
    )?;

    output.push_str("\n## Decisions\n\n");
    write_bullets(
        &mut output,
        trace.decisions.iter().map(String::as_str),
        "None recorded.",
    );
    append_prior(
        &mut output,
        prior_sections
            .iter()
            .find(|section| section.heading.eq_ignore_ascii_case("Decisions"))
            .map(|section| section.content.as_str()),
        limits,
    )?;

    output.push_str("\n## Open items\n\n");
    write_bullets(
        &mut output,
        trace.open_items.iter().map(String::as_str),
        "None recorded.",
    );
    append_prior(
        &mut output,
        prior_sections
            .iter()
            .find(|section| section.heading.eq_ignore_ascii_case("Open items"))
            .map(|section| section.content.as_str()),
        limits,
    )?;

    output.push_str("\n## How to verify\n\n");
    if trace.verification.is_empty() {
        output.push_str("- No verification evidence recorded.\n");
    } else {
        for check in &trace.verification {
            match check.exit_code {
                Some(0) => {
                    writeln!(output, "- PASS `{}`", check.command).ok();
                }
                Some(code) => {
                    writeln!(output, "- FAIL `{}` (exit {code})", check.command).ok();
                }
                None => {
                    writeln!(output, "- UNAVAILABLE `{}`", check.command).ok();
                }
            }
            if let Some(summary) = &check.summary {
                writeln!(output, "  - {summary}").ok();
            }
        }
    }
    append_prior(
        &mut output,
        prior_sections
            .iter()
            .find(|section| section.heading.eq_ignore_ascii_case("How to verify"))
            .map(|section| section.content.as_str()),
        limits,
    )?;

    if output.len() > limits.wiki.max_body_bytes {
        return Err(AgentError::Wiki(format!(
            "compiled body is {} bytes, exceeding the configured limit of {} bytes",
            output.len(),
            limits.wiki.max_body_bytes
        )));
    }
    if !REQUIRED_HEADINGS.iter().all(|heading| {
        output
            .lines()
            .any(|line| line.trim_end() == format!("## {heading}"))
    }) {
        return Err(AgentError::Wiki(
            "compiled state is missing a required level-two heading".into(),
        ));
    }

    Ok(output)
}

fn write_list<'a>(output: &mut String, label: &str, values: impl Iterator<Item = &'a str>) {
    let values = values.collect::<Vec<_>>();
    if values.is_empty() {
        writeln!(output, "- {label}: none recorded").ok();
    } else {
        writeln!(output, "- {label}:").ok();
        for value in values {
            writeln!(output, "  - {value}").ok();
        }
    }
}

fn write_bullets<'a>(output: &mut String, values: impl Iterator<Item = &'a str>, empty: &str) {
    let values = values.collect::<Vec<_>>();
    if values.is_empty() {
        writeln!(output, "- {empty}").ok();
    } else {
        for value in values {
            writeln!(output, "- {value}").ok();
        }
    }
}

fn append_prior(
    output: &mut String,
    prior: Option<&str>,
    limits: &WikiLimitsWithTrace,
) -> Result<()> {
    let Some(prior) = prior.filter(|value| !value.trim().is_empty()) else {
        return Ok(());
    };

    let redacted = redact(prior)?;
    let demoted = demote_level_two_headings(&redacted);
    let bounded = truncate_utf8(
        &demoted,
        limits.max_prior_section_bytes,
        PRIOR_TRUNCATION_MARKER,
    );

    output.push_str("\n### Prior context\n\n");
    output.push_str(&bounded);
    if !bounded.ends_with('\n') {
        output.push('\n');
    }
    Ok(())
}

fn demote_level_two_headings(input: &str) -> String {
    input
        .split_inclusive('\n')
        .map(|line| {
            let trimmed = line.trim_start();
            if trimmed.starts_with("## ") {
                line.replacen("## ", "### ", 1)
            } else {
                line.to_owned()
            }
        })
        .collect()
}

async fn append_bytes(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .await?;
    file.write_all(bytes).await?;
    file.flush().await?;
    file.sync_data().await?;
    Ok(())
}

pub(crate) async fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| AgentError::Wiki("state file path has no parent directory".into()))?;
    tokio::fs::create_dir_all(parent).await?;

    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("state-of-the-world.md");
    let temporary = parent.join(format!(".{file_name}.tmp-{}", Uuid::new_v4()));

    let result: Result<()> = async {
        let mut file = tokio::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .await?;
        file.write_all(bytes).await?;
        file.flush().await?;
        file.sync_all().await?;
        drop(file);
        tokio::fs::rename(&temporary, path).await?;
        Ok(())
    }
    .await;

    if result.is_err() {
        let _ = tokio::fs::remove_file(&temporary).await;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits() -> WikiLimitsWithTrace {
        WikiLimitsWithTrace::default()
    }

    #[test]
    fn status_requires_evidence_and_fails_closed() {
        let mut trace = SessionTrace::new("s1", "task", "2026-10-22");

        assert_eq!(derive_status(&trace), WikiStatus::Blocked);

        trace.verification.push(VerificationSummary {
            command: "cargo test".into(),
            exit_code: Some(0),
            required: true,
            summary: None,
        });
        assert_eq!(derive_status(&trace), WikiStatus::Green);

        trace.verification.push(VerificationSummary {
            command: "cargo clippy".into(),
            exit_code: Some(1),
            required: false,
            summary: None,
        });
        assert_eq!(derive_status(&trace), WikiStatus::Red);

        trace.verification.pop();
        trace.policy_denials.push("denied rm".into());
        assert_eq!(derive_status(&trace), WikiStatus::Blocked);

        trace.policy_denials.clear();
        trace.open_items.push("finish docs".into());
        assert_eq!(derive_status(&trace), WikiStatus::Blocked);

        trace.open_items.clear();
        trace.verification[0].exit_code = None;
        assert_eq!(derive_status(&trace), WikiStatus::Blocked);
    }

    #[test]
    fn trace_redaction_bounds_and_sanitizes_fields() {
        let mut trace = SessionTrace::new("session", "KEY=secret-value", "now");
        trace.decisions.push("Bearer abcdefghijklmnop".into());
        trace.changed_files.insert("src/lib.rs".into());
        trace.actions.push(ActionSummary {
            tool: "shell".into(),
            summary: "PASSWORD=hunter2".into(),
            ok: false,
        });

        let redacted = trace.redacted(&limits()).unwrap();
        assert!(!redacted.task_summary.contains("secret-value"));
        assert!(!redacted.decisions[0].contains("abcdefghijklmnop"));
        assert!(!redacted.actions[0].summary.contains("hunter2"));
        assert!(redacted.task_summary.contains("[REDACTED]"));
    }
}
