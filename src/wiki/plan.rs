//! Deterministic compilation of a bounded, durable implementation plan.
//!
//! The model may gather read-only evidence and propose a structured plan, but
//! the harness owns the artifact shape, verifier profile, bounds, redaction,
//! and completion decision. A missing or invalid model plan still produces a
//! deterministic fallback artifact; an unsupported verifier profile is always
//! recorded as a blocker and cannot produce a completed task.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use uuid::Uuid;

use crate::error::{AgentError, Result};
use crate::wiki::compile::{SessionTrace, WikiLimitsWithTrace, atomic_write, sanitize_field};
use crate::wiki::redact::redact;
use crate::wiki::{WIKI_DIR, truncate_utf8};

pub const PLAN_DIR: &str = "plans";
pub const PLAN_REQUIRED_HEADINGS: [&str; 9] = [
    "Goal",
    "Repository evidence used",
    "Likely files",
    "Ordered steps",
    "Assumptions",
    "Risks",
    "Expected verifier profile",
    "Blockers and missing evidence",
    "Confirmed facts and model inferences",
];

const SECTION_TRUNCATION_MARKER: &str = "\n...[section truncated]...\n";
const DRAFT_TRUNCATION_MARKER: &str = "\n...[plan draft truncated]...\n";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanLimits {
    pub max_plan_bytes: usize,
    pub max_section_bytes: usize,
    pub max_draft_bytes: usize,
    pub max_items: usize,
    pub max_field_bytes: usize,
}

impl Default for PlanLimits {
    fn default() -> Self {
        Self {
            max_plan_bytes: 32 * 1024,
            max_section_bytes: 4 * 1024,
            max_draft_bytes: 12 * 1024,
            max_items: 24,
            max_field_bytes: 512,
        }
    }
}

/// Harness-owned verifier information included in the plan artifact.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PlanVerifier {
    pub commands: Vec<String>,
    pub blocker: Option<String>,
}

impl PlanVerifier {
    pub fn supported(commands: Vec<String>) -> Self {
        Self {
            commands,
            blocker: None,
        }
    }

    pub fn unsupported(reason: impl Into<String>) -> Self {
        Self {
            commands: Vec::new(),
            blocker: Some(reason.into()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanReport {
    pub path: PathBuf,
    pub bytes: usize,
    pub fallback: bool,
    pub verifier_blocker: Option<String>,
}

pub fn plan_path(root: &Path, task_id: Uuid) -> PathBuf {
    root.join(WIKI_DIR)
        .join(PLAN_DIR)
        .join(format!("{task_id}.md"))
}

/// Return `Ok(())` only when a bounded model draft has every required,
/// non-empty section. This is the deterministic review gate before Compile.
pub fn review_draft(draft: Option<&str>) -> std::result::Result<(), String> {
    let limits = PlanLimits::default();
    let review = parse_draft(draft, &limits).map_err(|error| error.to_string())?;
    match review.reason {
        Some(reason) => Err(reason),
        None if review.sections.is_some() => Ok(()),
        None => Err("model plan review produced no usable sections".into()),
    }
}

pub async fn compile(
    root: &Path,
    task_id: Uuid,
    trace: &SessionTrace,
    draft: Option<&str>,
    verifier: &PlanVerifier,
    fallback_reason: Option<&str>,
    today: &str,
) -> Result<PlanReport> {
    compile_with_limits(
        root,
        PlanCompileInput {
            task_id,
            trace,
            draft,
            verifier,
            fallback_reason,
            today,
        },
        &PlanLimits::default(),
    )
    .await
}

struct PlanCompileInput<'a> {
    task_id: Uuid,
    trace: &'a SessionTrace,
    draft: Option<&'a str>,
    verifier: &'a PlanVerifier,
    fallback_reason: Option<&'a str>,
    today: &'a str,
}

async fn compile_with_limits(
    root: &Path,
    input: PlanCompileInput<'_>,
    limits: &PlanLimits,
) -> Result<PlanReport> {
    let PlanCompileInput {
        task_id,
        trace,
        draft,
        verifier,
        fallback_reason,
        today,
    } = input;
    let review = if let Some(reason) = fallback_reason {
        DraftReview {
            sections: None,
            reason: Some(reason.to_owned()),
        }
    } else {
        parse_draft(draft, limits)?
    };
    let fallback = review.sections.is_none();
    let blocker = verifier
        .blocker
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    let content = render_plan(
        task_id,
        trace,
        today,
        &review,
        verifier,
        blocker.as_deref(),
        limits,
    )?;
    if content.len() > limits.max_plan_bytes {
        return Err(AgentError::Wiki(format!(
            "plan artifact is {} bytes, exceeding the configured limit of {} bytes",
            content.len(),
            limits.max_plan_bytes
        )));
    }

    let path = plan_path(root, task_id);
    atomic_write(&path, content.as_bytes()).await?;
    Ok(PlanReport {
        path,
        bytes: content.len(),
        fallback,
        verifier_blocker: blocker,
    })
}

#[derive(Debug, Clone, Default)]
struct DraftReview {
    sections: Option<BTreeMap<&'static str, String>>,
    reason: Option<String>,
}

fn parse_draft(draft: Option<&str>, limits: &PlanLimits) -> Result<DraftReview> {
    let Some(raw) = draft.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(DraftReview {
            sections: None,
            reason: Some("no model plan was available for review".into()),
        });
    };

    let redacted = redact(raw)?;
    let bounded = truncate_utf8(&redacted, limits.max_draft_bytes, DRAFT_TRUNCATION_MARKER);
    let mut sections = BTreeMap::new();
    let mut current: Option<&'static str> = None;
    let mut content = String::new();

    for line in bounded.lines() {
        if let Some(heading) = line.strip_prefix("## ") {
            commit_section(&mut sections, current.take(), &mut content);
            current = canonical_heading(heading);
            continue;
        }
        if current.is_some() {
            content.push_str(line);
            content.push('\n');
        }
    }
    commit_section(&mut sections, current.take(), &mut content);

    let missing = PLAN_REQUIRED_HEADINGS
        .iter()
        .filter(|heading| {
            sections
                .get(**heading)
                .is_none_or(|content| content.trim().is_empty())
        })
        .copied()
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        return Ok(DraftReview {
            sections: None,
            reason: Some(format!(
                "model plan is missing or has empty required section(s): {}",
                missing.join(", ")
            )),
        });
    }

    Ok(DraftReview {
        sections: Some(sections),
        reason: None,
    })
}

fn commit_section(
    sections: &mut BTreeMap<&'static str, String>,
    heading: Option<&'static str>,
    content: &mut String,
) {
    if let Some(heading) = heading {
        sections.insert(heading, content.trim().to_owned());
    }
    content.clear();
}

fn canonical_heading(line: &str) -> Option<&'static str> {
    let normalized = line
        .trim()
        .trim_end_matches(':')
        .trim()
        .to_ascii_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");

    match normalized.as_str() {
        "goal" | "objective" => Some("Goal"),
        "repository evidence used" | "repository evidence" | "evidence used" => {
            Some("Repository evidence used")
        }
        "likely files" | "likely relevant files" | "relevant files" => Some("Likely files"),
        "ordered steps" | "implementation steps" | "steps" => Some("Ordered steps"),
        "assumptions" => Some("Assumptions"),
        "risks" | "risks and mitigations" => Some("Risks"),
        "expected verifier profile" | "verifier profile" | "verification profile" => {
            Some("Expected verifier profile")
        }
        "blockers and missing evidence"
        | "blockers"
        | "missing evidence"
        | "blockers or missing evidence" => Some("Blockers and missing evidence"),
        "confirmed facts and model inferences"
        | "confirmed facts vs model inferences"
        | "confirmed facts versus model inferences"
        | "facts and inferences" => Some("Confirmed facts and model inferences"),
        _ => None,
    }
}

fn render_plan(
    task_id: Uuid,
    trace: &SessionTrace,
    today: &str,
    review: &DraftReview,
    verifier: &PlanVerifier,
    blocker: Option<&str>,
    limits: &PlanLimits,
) -> Result<String> {
    let trace = trace.redacted(&WikiLimitsWithTrace::default())?;
    let started = field_or_unknown(&trace.started_at, limits)?;
    let finished = trace
        .finished_at
        .as_deref()
        .map(|value| sanitize_field(value, limits.max_field_bytes))
        .transpose()?
        .unwrap_or_else(|| "not recorded".into());
    let date = sanitize_field(today, limits.max_field_bytes)?;
    let status = if review.sections.is_some() {
        "reviewed model plan"
    } else {
        "deterministic compiler fallback"
    };

    let mut output = String::new();
    output.push_str("# Plan\n\n");
    writeln!(output, "- Task ID: `{task_id}`").ok();
    writeln!(output, "- Mode: `plan`").ok();
    writeln!(output, "- Date: {date}").ok();
    writeln!(output, "- Started: {started}").ok();
    writeln!(output, "- Finished: {finished}").ok();
    writeln!(output, "- Status: {status}").ok();
    writeln!(
        output,
        "- Provenance: bounded local harness evidence; hidden reasoning and raw provider responses are excluded"
    )
    .ok();
    writeln!(
        output,
        "- Authority: this plan is advisory; the harness selects verifiers and owns completion"
    )
    .ok();

    let sections = match &review.sections {
        Some(model) => model_sections(&trace, model, verifier, blocker, limits)?,
        None => fallback_sections(&trace, review.reason.as_deref(), verifier, blocker, limits)?,
    };

    let section_cap = limits
        .max_section_bytes
        .min(limits.max_plan_bytes / (PLAN_REQUIRED_HEADINGS.len() + 2))
        .max(256);
    for (heading, content) in sections {
        write_section(&mut output, heading, &content, section_cap);
    }

    Ok(output)
}

fn model_sections(
    trace: &SessionTrace,
    model: &BTreeMap<&'static str, String>,
    verifier: &PlanVerifier,
    blocker: Option<&str>,
    limits: &PlanLimits,
) -> Result<Vec<(&'static str, String)>> {
    let mut sections = Vec::new();
    for heading in PLAN_REQUIRED_HEADINGS {
        let model_content = model.get(heading).map(String::as_str).unwrap_or_default();
        let content = match heading {
            "Repository evidence used" => format!(
                "{}\n\nModel evidence notes (advisory):\n{}",
                harness_evidence(trace, limits)?,
                model_content
            ),
            "Expected verifier profile" => verifier_profile(verifier, limits)?,
            "Blockers and missing evidence" => blockers(trace, model_content, blocker, limits)?,
            "Confirmed facts and model inferences" => format!(
                "{}\n\nModel plan statements (advisory unless they match the facts above):\n{}",
                confirmed_facts(trace, limits)?,
                model_content
            ),
            _ => model_content.to_owned(),
        };
        sections.push((heading, content));
    }
    Ok(sections)
}

fn fallback_sections(
    trace: &SessionTrace,
    reason: Option<&str>,
    verifier: &PlanVerifier,
    blocker: Option<&str>,
    limits: &PlanLimits,
) -> Result<Vec<(&'static str, String)>> {
    let goal = if trace.task_summary.trim().is_empty() {
        "No task objective was recorded.".to_owned()
    } else {
        sanitize_field(&trace.task_summary, limits.max_field_bytes)?
    };
    let likely_files = if trace.inspected_files.is_empty() {
        "- No repository files were inspected; likely files require further evidence.".to_owned()
    } else {
        trace
            .inspected_files
            .iter()
            .take(limits.max_items)
            .map(|path| sanitize_field(&format!("- `{path}`"), limits.max_field_bytes))
            .collect::<Result<Vec<_>>>()?
            .join("\n")
    };
    let fallback_reason = reason.unwrap_or("the model plan did not pass deterministic review");
    let assumptions = format!(
        "- The model plan was unavailable or invalid: {}\n\
         - The ordered steps below are deterministic and may be less specific than a reviewed plan.\n\
         - A separate Change task is required before any implementation or verification run.",
        sanitize_field(fallback_reason, limits.max_field_bytes)?
    );
    let risks = "- The fallback plan has not been reviewed against the latest repository state.\n\
                 - Relevant files and dependencies may be incomplete because model planning was unavailable.\n\
                 - Implementing directly from this fallback without operator review may expand scope.";

    Ok(vec![
        ("Goal", goal),
        ("Repository evidence used", harness_evidence(trace, limits)?),
        ("Likely files", likely_files),
        (
            "Ordered steps",
            "1. Review the harness-recorded repository evidence and confirm the goal.\n\
             2. Identify the smallest relevant file set and any missing evidence.\n\
             3. Start a separate Change task; do not mutate the repository from Plan mode.\n\
             4. Add or update focused tests before implementation where practical.\n\
             5. Run the harness-selected verifier profile after implementation.\n\
             6. Review the final diff and documentation/configuration changes."
                .to_owned(),
        ),
        ("Assumptions", assumptions),
        ("Risks", risks.to_owned()),
        (
            "Expected verifier profile",
            verifier_profile(verifier, limits)?,
        ),
        (
            "Blockers and missing evidence",
            blockers(trace, "", blocker, limits)?,
        ),
        (
            "Confirmed facts and model inferences",
            format!(
                "{}\n\nModel plan statements (advisory):\n- None retained; deterministic fallback was used.",
                confirmed_facts(trace, limits)?
            ),
        ),
    ])
}

fn harness_evidence(trace: &SessionTrace, limits: &PlanLimits) -> Result<String> {
    let mut lines = vec!["Harness-recorded read-only evidence:".to_owned()];
    let successful = trace
        .actions
        .iter()
        .filter(|action| {
            action.ok
                && matches!(
                    action.tool.as_str(),
                    "read_file" | "git_status" | "git_diff"
                )
        })
        .count();
    if successful == 0 {
        lines.push("- No successful read-only action was recorded.".into());
    } else {
        lines.push(format!(
            "- {successful} successful read-only action(s) were recorded."
        ));
    }
    for path in trace.inspected_files.iter().take(limits.max_items) {
        lines.push(sanitize_field(
            &format!("- Inspected file: `{path}`"),
            limits.max_field_bytes,
        )?);
    }
    for state in trace.git_state.iter().take(limits.max_items) {
        lines.push(sanitize_field(
            &format!("- Git observation: {state}"),
            limits.max_field_bytes,
        )?);
    }
    if !trace.policy_denials.is_empty() {
        lines.push("- Policy denials were recorded; see blockers.".into());
    }
    Ok(lines.join("\n"))
}

fn confirmed_facts(trace: &SessionTrace, limits: &PlanLimits) -> Result<String> {
    let mut facts = vec!["Harness-confirmed facts:".to_owned()];
    let mut by_tool: BTreeMap<&str, usize> = BTreeMap::new();
    for action in &trace.actions {
        if action.ok {
            *by_tool.entry(action.tool.as_str()).or_default() += 1;
        }
    }
    for (tool, count) in &by_tool {
        if matches!(*tool, "read_file" | "git_status" | "git_diff") {
            facts.push(sanitize_field(
                &format!("- `{tool}` completed successfully {count} time(s)."),
                limits.max_field_bytes,
            )?);
        }
    }
    for path in trace.inspected_files.iter().take(limits.max_items) {
        facts.push(sanitize_field(
            &format!("- Repository file `{path}` was inspected."),
            limits.max_field_bytes,
        )?);
    }
    for state in trace.git_state.iter().take(limits.max_items) {
        facts.push(sanitize_field(
            &format!("- Git observation: {state}"),
            limits.max_field_bytes,
        )?);
    }
    if facts.len() == 1 {
        facts.push("- No successful read-only action was recorded.".into());
    }
    Ok(facts.join("\n"))
}

fn verifier_profile(verifier: &PlanVerifier, limits: &PlanLimits) -> Result<String> {
    if let Some(blocker) = verifier.blocker.as_deref() {
        return Ok(format!(
            "No supported verifier profile is available. The harness will not invent verification commands.\n\n- Blocker: {}",
            sanitize_field(blocker, limits.max_field_bytes)?
        ));
    }
    if verifier.commands.is_empty() {
        return Ok(
            "No supported verifier profile is available. The harness will not invent verification commands."
                .into(),
        );
    }

    let mut lines = vec![
        "Harness-selected verifier profile (authoritative; the model cannot add or replace commands):"
            .to_owned(),
        String::new(),
    ];
    for command in verifier.commands.iter().take(limits.max_items) {
        lines.push(sanitize_field(
            &format!("- `{command}`"),
            limits.max_field_bytes,
        )?);
    }
    Ok(lines.join("\n"))
}

fn blockers(
    trace: &SessionTrace,
    model_content: &str,
    verifier_blocker: Option<&str>,
    limits: &PlanLimits,
) -> Result<String> {
    let mut lines = Vec::new();
    if !model_content.trim().is_empty() {
        lines.push(model_content.trim().to_owned());
    }
    let mut recorded = Vec::new();
    for denial in trace.policy_denials.iter().take(limits.max_items) {
        recorded.push(format!("Policy denial: {denial}"));
    }
    for blocker in trace.blockers.iter().take(limits.max_items) {
        recorded.push(format!("Recorded blocker: {blocker}"));
    }
    if let Some(blocker) = verifier_blocker {
        recorded.push(format!("Verifier blocker: {blocker}"));
    }
    if recorded.is_empty() {
        lines.push("- No blockers were recorded.".to_owned());
    } else {
        lines.push("Harness-recorded blockers:".to_owned());
        for item in recorded {
            lines.push(sanitize_field(
                &format!("- {item}"),
                limits.max_field_bytes,
            )?);
        }
    }
    Ok(lines.join("\n"))
}

fn field_or_unknown(value: &str, limits: &PlanLimits) -> Result<String> {
    if value.trim().is_empty() {
        Ok("not recorded".into())
    } else {
        sanitize_field(value, limits.max_field_bytes)
    }
}

fn write_section(output: &mut String, heading: &str, content: &str, section_cap: usize) {
    output.push_str(&format!("\n## {heading}\n\n"));
    let redacted = redact(content).unwrap_or_else(|_| "[redaction unavailable]".to_owned());
    let bounded = truncate_utf8(&redacted, section_cap, SECTION_TRUNCATION_MARKER);
    output.push_str(bounded.trim_end());
    output.push('\n');
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wiki::compile::ActionSummary;
    use uuid::Uuid;

    fn trace() -> SessionTrace {
        let mut trace =
            SessionTrace::new("task-1", "plan a fixture change", "2026-10-25T00:00:00Z");
        trace.mode = "plan".into();
        trace.finished_at = Some("2026-10-25T00:01:00Z".into());
        trace.inspected_files.insert("src/lib.rs".into());
        trace.actions.push(ActionSummary {
            tool: "read_file".into(),
            summary: "read_file completed".into(),
            ok: true,
        });
        trace
    }

    fn draft() -> String {
        PLAN_REQUIRED_HEADINGS
            .iter()
            .map(|heading| format!("## {heading}\n- fixture content for {heading}\n"))
            .collect()
    }

    #[tokio::test]
    async fn compiles_a_reviewed_plan_with_harness_verifier_profile() {
        let root = std::env::temp_dir().join(format!("rupi-plan-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let task_id = Uuid::new_v4();
        let verifier = PlanVerifier::supported(vec!["cargo test".into()]);

        let report = compile(
            &root,
            task_id,
            &trace(),
            Some(&draft()),
            &verifier,
            None,
            "2026-10-25",
        )
        .await
        .expect("compile plan");
        assert!(!report.fallback);
        assert_eq!(report.path, plan_path(&root, task_id));
        assert!(report.verifier_blocker.is_none());
        let content = std::fs::read_to_string(&report.path).expect("read plan");
        for heading in PLAN_REQUIRED_HEADINGS {
            assert!(content.contains(&format!("## {heading}")), "{heading}");
        }
        assert!(content.contains("cargo test"));
        assert!(content.contains("src/lib.rs"));

        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn invalid_plan_uses_a_bounded_redacted_fallback() {
        let root = std::env::temp_dir().join(format!("rupi-plan-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let task_id = Uuid::new_v4();
        let verifier = PlanVerifier::supported(vec!["cargo test".into()]);

        let report = compile(
            &root,
            task_id,
            &trace(),
            Some("API_KEY=secret-value\nnot a structured plan"),
            &verifier,
            None,
            "2026-10-25",
        )
        .await
        .expect("compile fallback");
        assert!(report.fallback);
        let content = std::fs::read_to_string(&report.path).expect("read plan");
        for heading in PLAN_REQUIRED_HEADINGS {
            assert!(content.contains(&format!("## {heading}")), "{heading}");
        }
        assert!(!content.contains("secret-value"));
        assert!(content.contains("Deterministic fallback") || content.contains("deterministic"));
        assert!(content.len() <= PlanLimits::default().max_plan_bytes);

        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn unsupported_verifier_is_a_recorded_blocker() {
        let root = std::env::temp_dir().join(format!("rupi-plan-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let task_id = Uuid::new_v4();
        let verifier = PlanVerifier::unsupported("no supported verifier profile detected");

        let report = compile(
            &root,
            task_id,
            &trace(),
            Some(&draft()),
            &verifier,
            None,
            "2026-10-25",
        )
        .await
        .expect("compile blocked plan");
        assert_eq!(
            report.verifier_blocker.as_deref(),
            Some("no supported verifier profile detected")
        );
        let content = std::fs::read_to_string(&report.path).expect("read plan");
        assert!(content.contains("will not invent verification commands"));
        assert!(content.contains("Verifier blocker"));

        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn model_verifier_commands_cannot_override_the_harness_profile() {
        let root = std::env::temp_dir().join(format!("rupi-plan-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let task_id = Uuid::new_v4();
        let verifier = PlanVerifier::supported(vec!["cargo test".into()]);
        let draft = draft().replace(
            "fixture content for Expected verifier profile",
            "run `rm -rf /` instead",
        );

        let report = compile(
            &root,
            task_id,
            &trace(),
            Some(&draft),
            &verifier,
            None,
            "2026-10-25",
        )
        .await
        .expect("compile plan");
        assert!(!report.fallback);
        let content = std::fs::read_to_string(&report.path).expect("read plan");
        assert!(content.contains("cargo test"));
        assert!(!content.contains("rm -rf"));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn review_requires_every_nonempty_section() {
        assert!(review_draft(Some(&draft())).is_ok());
        assert!(review_draft(None).is_err());
        assert!(review_draft(Some("## Goal\nonly one section")).is_err());
    }
}
