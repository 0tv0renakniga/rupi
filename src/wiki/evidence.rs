//! Deterministic compilation of a bounded, durable evidence brief.
//!
//! Facts come from harness-recorded successful read-only actions and Git
//! observations. Model-supplied findings are always treated as unverified
//! hypotheses. If the model produced no usable findings, the compiler falls
//! back to the recorded trace so completion still has a durable artifact.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use uuid::Uuid;

use crate::error::{AgentError, Result};
use crate::wiki::compile::{SessionTrace, WikiLimitsWithTrace, atomic_write, sanitize_field};
use crate::wiki::{WIKI_DIR, truncate_utf8};

pub const EVIDENCE_DIR: &str = "evidence";
pub const EVIDENCE_REQUIRED_HEADINGS: [&str; 6] = [
    "Confirmed facts",
    "Hypotheses",
    "Missing evidence and uncertainty",
    "Inspected files",
    "Git state",
    "Recommended next action",
];

const HEADER_TRUNCATION_MARKER: &str = "\n...[brief truncated]...\n";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceLimits {
    pub max_brief_bytes: usize,
    pub max_section_bytes: usize,
    pub max_items: usize,
    pub max_field_bytes: usize,
    pub max_findings_bytes: usize,
}

impl Default for EvidenceLimits {
    fn default() -> Self {
        Self {
            max_brief_bytes: 32 * 1024,
            max_section_bytes: 4 * 1024,
            max_items: 24,
            max_field_bytes: 512,
            max_findings_bytes: 8 * 1024,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceReport {
    pub path: PathBuf,
    pub bytes: usize,
    pub fallback: bool,
}

pub fn evidence_path(root: &Path, task_id: Uuid) -> PathBuf {
    root.join(WIKI_DIR)
        .join(EVIDENCE_DIR)
        .join(format!("{task_id}.md"))
}

/// Compile the durable evidence brief for one Investigate task.
pub async fn compile(
    root: &Path,
    task_id: Uuid,
    trace: &SessionTrace,
    findings: Option<&str>,
    today: &str,
) -> Result<EvidenceReport> {
    compile_with_limits(
        root,
        task_id,
        trace,
        findings,
        today,
        &EvidenceLimits::default(),
    )
    .await
}

pub async fn compile_with_limits(
    root: &Path,
    task_id: Uuid,
    trace: &SessionTrace,
    findings: Option<&str>,
    today: &str,
    limits: &EvidenceLimits,
) -> Result<EvidenceReport> {
    let parsed = parse_findings(findings, limits)?;
    let fallback = !parsed.structured;
    let content = render_brief(task_id, trace, today, &parsed, limits)?;
    if content.len() > limits.max_brief_bytes {
        return Err(AgentError::Wiki(format!(
            "evidence brief is {} bytes, exceeding the configured limit of {} bytes",
            content.len(),
            limits.max_brief_bytes
        )));
    }

    let path = evidence_path(root, task_id);
    atomic_write(&path, content.as_bytes()).await?;
    Ok(EvidenceReport {
        path,
        bytes: content.len(),
        fallback,
    })
}

#[derive(Debug, Clone, Default)]
struct ParsedFindings {
    hypotheses: Vec<String>,
    missing: Vec<String>,
    recommendation: Option<String>,
    structured: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    Confirmed,
    Hypotheses,
    Missing,
    Recommendation,
}

fn parse_findings(findings: Option<&str>, limits: &EvidenceLimits) -> Result<ParsedFindings> {
    let Some(raw) = findings.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(ParsedFindings::default());
    };
    let bounded = truncate_utf8(
        raw,
        limits.max_findings_bytes,
        "\n...[findings truncated]...\n",
    );
    let mut parsed = ParsedFindings::default();
    let mut section: Option<Section> = None;

    for line in bounded.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        if let Some(heading) = finding_heading(trimmed) {
            parsed.structured = true;
            section = Some(heading);
            continue;
        }

        let value = strip_bullet(trimmed);
        if value.is_empty() {
            continue;
        }
        let value = sanitize_field(value, limits.max_field_bytes)?;
        if value.is_empty() {
            continue;
        }

        match section {
            Some(Section::Confirmed) => parsed
                .hypotheses
                .push(format!("Model-reported observation (unconfirmed): {value}")),
            Some(Section::Hypotheses) | None => parsed.hypotheses.push(value),
            Some(Section::Missing) => parsed.missing.push(value),
            Some(Section::Recommendation) => {
                if parsed.recommendation.is_none() {
                    parsed.recommendation = Some(value);
                } else if let Some(recommendation) = &mut parsed.recommendation {
                    let separator = if recommendation.len() + value.len() < limits.max_field_bytes {
                        " "
                    } else {
                        ""
                    };
                    if !separator.is_empty() {
                        recommendation.push(' ');
                        recommendation.push_str(&value);
                    }
                }
            }
        }
    }

    parsed.hypotheses.truncate(limits.max_items);
    parsed.missing.truncate(limits.max_items);
    Ok(parsed)
}

fn finding_heading(line: &str) -> Option<Section> {
    let normalized = line
        .trim_start_matches('#')
        .trim()
        .trim_end_matches(':')
        .trim()
        .to_ascii_uppercase();

    if normalized.starts_with("CONFIRMED FACTS") || normalized == "FACTS" {
        Some(Section::Confirmed)
    } else if normalized.starts_with("HYPOTHESES") || normalized.starts_with("INFERENCES") {
        Some(Section::Hypotheses)
    } else if normalized.starts_with("MISSING EVIDENCE")
        || normalized.starts_with("UNCERTAINTY")
        || normalized.starts_with("UNCERTAINTIES")
    {
        Some(Section::Missing)
    } else if normalized.starts_with("RECOMMENDED NEXT ACTION")
        || normalized == "NEXT ACTION"
        || normalized.starts_with("RECOMMENDATION")
    {
        Some(Section::Recommendation)
    } else {
        None
    }
}

fn strip_bullet(line: &str) -> &str {
    line.trim_start_matches(['-', '*', '+'])
        .trim_start()
        .trim_start_matches(|character: char| character.is_ascii_digit())
        .trim_start_matches(['.', ')', ' '])
        .trim()
}

fn render_brief(
    task_id: Uuid,
    trace: &SessionTrace,
    today: &str,
    parsed: &ParsedFindings,
    limits: &EvidenceLimits,
) -> Result<String> {
    let trace = trace.redacted(&WikiLimitsWithTrace::default())?;
    let mode = if trace.mode.trim().is_empty() {
        "investigate"
    } else {
        &trace.mode
    };
    let started = field_or_unknown(&trace.started_at, limits)?;
    let finished = trace
        .finished_at
        .as_deref()
        .map(|value| sanitize_field(value, limits.max_field_bytes))
        .transpose()?
        .unwrap_or_else(|| "not recorded".into());
    let date = sanitize_field(today, limits.max_field_bytes)?;

    let mut output = String::new();
    output.push_str("# Evidence Brief\n\n");
    writeln!(output, "- Task ID: `{task_id}`").ok();
    writeln!(output, "- Mode: `{}`", sanitize_field(mode, 64)?).ok();
    writeln!(output, "- Skills: `read_file`, `git_status`, `git_diff`").ok();
    writeln!(output, "- Date: {date}").ok();
    writeln!(output, "- Started: {started}").ok();
    writeln!(output, "- Finished: {finished}").ok();
    writeln!(
        output,
        "- Provenance: local harness action trace; hidden reasoning and raw provider responses are excluded"
    )
    .ok();

    let facts = confirmed_facts(&trace, limits)?;
    write_section(&mut output, "Confirmed facts", &facts, limits);
    write_section(&mut output, "Hypotheses", &parsed.hypotheses, limits);
    write_section(
        &mut output,
        "Missing evidence and uncertainty",
        &missing_evidence(&trace, parsed, limits)?,
        limits,
    );
    let files = if trace.inspected_files.is_empty() {
        vec!["No repository files were inspected.".into()]
    } else {
        trace
            .inspected_files
            .iter()
            .take(limits.max_items)
            .map(|path| sanitize_field(&format!("`{path}`"), limits.max_field_bytes))
            .collect::<Result<Vec<_>>>()?
    };
    write_section(&mut output, "Inspected files", &files, limits);
    let git = if trace.git_state.is_empty() {
        vec!["No Git state was observed.".into()]
    } else {
        trace
            .git_state
            .iter()
            .take(limits.max_items)
            .map(|value| sanitize_field(value, limits.max_field_bytes))
            .collect::<Result<Vec<_>>>()?
    };
    write_section(&mut output, "Git state", &git, limits);
    let recommendation = parsed
        .recommendation
        .clone()
        .unwrap_or_else(|| fallback_recommendation(&trace));
    write_section(
        &mut output,
        "Recommended next action",
        &[recommendation],
        limits,
    );

    for heading in EVIDENCE_REQUIRED_HEADINGS {
        if !output
            .lines()
            .any(|line| line.trim_end() == format!("## {heading}"))
        {
            return Err(AgentError::Wiki(format!(
                "evidence brief is missing required heading `{heading}`"
            )));
        }
    }

    if output.len() > limits.max_brief_bytes {
        let body_limit = limits
            .max_brief_bytes
            .saturating_sub(HEADER_TRUNCATION_MARKER.len());
        output = truncate_utf8(&output, body_limit, HEADER_TRUNCATION_MARKER);
    }
    Ok(output)
}

fn field_or_unknown(value: &str, limits: &EvidenceLimits) -> Result<String> {
    if value.trim().is_empty() {
        Ok("not recorded".into())
    } else {
        sanitize_field(value, limits.max_field_bytes)
    }
}

fn confirmed_facts(trace: &SessionTrace, limits: &EvidenceLimits) -> Result<Vec<String>> {
    let mut facts = Vec::new();

    let mut by_tool: BTreeMap<&str, usize> = BTreeMap::new();
    for action in &trace.actions {
        if action.ok {
            *by_tool.entry(action.tool.as_str()).or_default() += 1;
        }
    }
    for (tool, count) in &by_tool {
        if matches!(*tool, "read_file" | "git_status" | "git_diff") {
            facts.push(sanitize_field(
                &format!("`{tool}` completed successfully {count} time(s)."),
                limits.max_field_bytes,
            )?);
        }
    }
    for path in trace.inspected_files.iter().take(limits.max_items) {
        facts.push(sanitize_field(
            &format!("Repository file `{path}` was inspected with `read_file`."),
            limits.max_field_bytes,
        )?);
    }
    for state in trace.git_state.iter().take(limits.max_items) {
        facts.push(sanitize_field(
            &format!("Git observation: {state}"),
            limits.max_field_bytes,
        )?);
    }

    if facts.is_empty() {
        facts.push("No successful read-only action was recorded.".into());
    }
    Ok(facts)
}

fn missing_evidence(
    trace: &SessionTrace,
    parsed: &ParsedFindings,
    limits: &EvidenceLimits,
) -> Result<Vec<String>> {
    let mut missing = parsed.missing.clone();
    for denial in trace.policy_denials.iter().take(limits.max_items) {
        missing.push(sanitize_field(
            &format!("Policy denial: {denial}"),
            limits.max_field_bytes,
        )?);
    }
    for blocker in trace.blockers.iter().take(limits.max_items) {
        missing.push(sanitize_field(
            &format!("Recorded blocker: {blocker}"),
            limits.max_field_bytes,
        )?);
    }
    if missing.is_empty() {
        missing.push(
            "No missing evidence was recorded. Absence of recorded gaps does not prove the investigation was exhaustive."
                .into(),
        );
    }
    missing.truncate(limits.max_items);
    Ok(missing)
}

fn fallback_recommendation(trace: &SessionTrace) -> String {
    if trace.policy_denials.is_empty() && trace.blockers.is_empty() {
        "Operator should review this evidence brief and decide whether a separate Change task is warranted."
            .into()
    } else {
        "Operator should resolve the recorded policy denials or blockers before starting any follow-up task."
            .into()
    }
}

fn write_section(output: &mut String, heading: &str, items: &[String], limits: &EvidenceLimits) {
    output.push_str(&format!("\n## {heading}\n\n"));
    let mut section = String::new();
    if items.is_empty() {
        section.push_str("- None recorded.\n");
    } else {
        for item in items.iter().take(limits.max_items) {
            writeln!(section, "- {item}").ok();
        }
    }
    let section = truncate_utf8(
        &section,
        limits.max_section_bytes,
        "\n...[section truncated]...\n",
    );
    output.push_str(&section);
    if !output.ends_with('\n') {
        output.push('\n');
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wiki::compile::ActionSummary;

    fn trace() -> SessionTrace {
        let mut trace = SessionTrace::new("task-1", "inspect fixture", "2026-10-25T00:00:00Z");
        trace.mode = "investigate".into();
        trace.finished_at = Some("2026-10-25T00:01:00Z".into());
        trace.inspected_files.insert("src/lib.rs".into());
        trace.actions.push(ActionSummary {
            tool: "read_file".into(),
            summary: "read_file completed".into(),
            ok: true,
        });
        trace.git_state.push("git_status clean=false".into());
        trace
    }

    #[tokio::test]
    async fn compiles_a_bounded_brief_and_falls_back_without_model_findings() {
        let root = std::env::temp_dir().join(format!("rupi-evidence-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let task_id = Uuid::new_v4();

        let report = compile(&root, task_id, &trace(), None, "2026-10-25")
            .await
            .expect("compile fallback");
        assert!(report.fallback);
        assert_eq!(report.path, evidence_path(&root, task_id));
        let content = std::fs::read_to_string(&report.path).expect("read brief");
        assert!(content.contains(&task_id.to_string()));
        assert!(content.contains("## Confirmed facts"));
        assert!(content.contains("src/lib.rs"));
        assert!(content.contains("git_status clean=false"));
        assert!(content.contains("Operator should review this evidence brief"));
        assert!(content.len() <= EvidenceLimits::default().max_brief_bytes);

        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn model_findings_remain_hypotheses_and_are_redacted() {
        let root = std::env::temp_dir().join(format!("rupi-evidence-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let task_id = Uuid::new_v4();
        let findings = "CONFIRMED FACTS:\n- the file is here\nHYPOTHESES:\n- API_KEY=secret-value\nMISSING EVIDENCE:\n- no tests were inspected\nRECOMMENDED NEXT ACTION:\nOpen a Change task.";

        let report = compile(&root, task_id, &trace(), Some(findings), "2026-10-25")
            .await
            .expect("compile findings");
        assert!(!report.fallback);
        let content = std::fs::read_to_string(&report.path).expect("read brief");
        assert!(content.contains("## Confirmed facts"));
        assert!(content.contains("Model-reported observation (unconfirmed): the file is here"));
        assert!(content.contains("## Hypotheses"));
        assert!(!content.contains("secret-value"));
        assert!(content.contains("[REDACTED]"));
        assert!(content.contains("Open a Change task."));

        let _ = std::fs::remove_dir_all(root);
    }
}
