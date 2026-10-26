//! Local, filesystem-only AI Wiki ingestion.
//!
//! Only `.ai-wiki/state-of-the-world.md` is eligible for prompt context.
//! Session traces and `log.md` are durable local history, but this module never
//! injects them into model prompts automatically.

pub mod compile;
pub mod evidence;
pub mod redact;

use std::path::{Path, PathBuf};

use gray_matter::Matter;
use gray_matter::engine::YAML;
use serde::{Deserialize, Serialize};

use crate::error::{AgentError, Result};

pub const WIKI_DIR: &str = ".ai-wiki";
pub const STATE_FILE: &str = "state-of-the-world.md";
pub const LOG_FILE: &str = "log.md";
pub const SESSIONS_DIR: &str = "sessions";
pub const SESSION_RECORD_FILE: &str = "session-log.jsonl";
pub const DEFAULT_SCHEMA_VERSION: u32 = 1;
pub const DEFAULT_TITLE: &str = "State of the world";

pub const REQUIRED_HEADINGS: [&str; 4] =
    ["Current state", "Decisions", "Open items", "How to verify"];

const SECTION_TRUNCATION_MARKER: &str = "\n...[section truncated]...\n";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WikiLimits {
    pub max_state_bytes: usize,
    pub max_body_bytes: usize,
    pub max_section_bytes: usize,
    pub max_sections: usize,
}

impl Default for WikiLimits {
    fn default() -> Self {
        Self {
            max_state_bytes: 64 * 1024,
            max_body_bytes: 48 * 1024,
            max_section_bytes: 8 * 1024,
            max_sections: 256,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum WikiStatus {
    Green,
    Red,
    #[default]
    Blocked,
}

impl WikiStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Green => "green",
            Self::Red => "red",
            Self::Blocked => "blocked",
        }
    }
}

fn default_schema_version() -> u32 {
    DEFAULT_SCHEMA_VERSION
}

fn default_title() -> String {
    DEFAULT_TITLE.to_owned()
}

fn default_status() -> WikiStatus {
    WikiStatus::Blocked
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WikiFrontmatter {
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    #[serde(default = "default_title")]
    pub title: String,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub updated: String,
    #[serde(default)]
    pub session_count: u32,
    #[serde(default = "default_status")]
    pub status: WikiStatus,
    #[serde(default)]
    pub risk_level: Option<String>,
    #[serde(default)]
    pub last_verified_commit: Option<String>,
}

impl Default for WikiFrontmatter {
    fn default() -> Self {
        Self {
            schema_version: DEFAULT_SCHEMA_VERSION,
            title: default_title(),
            tags: Vec::new(),
            updated: String::new(),
            session_count: 0,
            status: WikiStatus::Blocked,
            risk_level: None,
            last_verified_commit: None,
        }
    }
}

/// Backwards-compatible name for the typed frontmatter.
pub type Frontmatter = WikiFrontmatter;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WikiSection {
    pub heading: String,
    pub content: String,
    pub byte_len: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WikiContext {
    pub frontmatter: WikiFrontmatter,
    pub body: String,
    pub source_path: PathBuf,
    pub sections: Vec<WikiSection>,
}

impl WikiContext {
    pub fn empty(root: &Path) -> Self {
        Self {
            frontmatter: WikiFrontmatter::default(),
            body: String::new(),
            source_path: state_path(root),
            sections: Vec::new(),
        }
    }

    pub fn section(&self, heading: &str) -> Option<&WikiSection> {
        self.sections
            .iter()
            .find(|section| section.heading.eq_ignore_ascii_case(heading))
    }
}

pub fn state_path(root: &Path) -> PathBuf {
    root.join(WIKI_DIR).join(STATE_FILE)
}

pub fn log_path(root: &Path) -> PathBuf {
    root.join(WIKI_DIR).join(LOG_FILE)
}

pub fn sessions_dir(root: &Path) -> PathBuf {
    root.join(WIKI_DIR).join(SESSIONS_DIR)
}

pub fn session_record_path(root: &Path) -> PathBuf {
    sessions_dir(root).join(SESSION_RECORD_FILE)
}

/// Load `.ai-wiki/state-of-the-world.md` with default bounds.
pub async fn load(root: &Path) -> Result<WikiContext> {
    load_with_limits(root, &WikiLimits::default()).await
}

/// Load the state file with explicit bounds.
///
/// A missing state file is a valid empty context. Invalid frontmatter, invalid
/// UTF-8, an oversized file, or a directory at the state path is an error.
pub async fn load_with_limits(root: &Path, limits: &WikiLimits) -> Result<WikiContext> {
    let path = state_path(root);

    let metadata = match tokio::fs::metadata(&path).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(WikiContext::empty(root));
        }
        Err(error) => return Err(error.into()),
    };

    if metadata.len() > limits.max_state_bytes as u64 {
        return Err(AgentError::Wiki(format!(
            "state file is {} bytes, exceeding the configured limit of {} bytes",
            metadata.len(),
            limits.max_state_bytes
        )));
    }

    let content = tokio::fs::read_to_string(&path).await.map_err(|error| {
        if error.kind() == std::io::ErrorKind::InvalidData {
            AgentError::Wiki(format!("state file is not valid UTF-8: {error}"))
        } else {
            AgentError::Io(error)
        }
    })?;

    parse_context(&content, &path, limits)
}

/// Parse already-loaded state content. This is the deterministic ingestion
/// boundary used by tests and by [`load_with_limits`].
pub fn parse_context(
    content: &str,
    source_path: &Path,
    limits: &WikiLimits,
) -> Result<WikiContext> {
    if content.len() > limits.max_state_bytes {
        return Err(AgentError::Wiki(format!(
            "state content is {} bytes, exceeding the configured limit of {} bytes",
            content.len(),
            limits.max_state_bytes
        )));
    }

    let (frontmatter_yaml, body) = split_frontmatter(content)?;

    let frontmatter = match frontmatter_yaml {
        None => WikiFrontmatter::default(),
        Some(yaml) if yaml.trim().is_empty() => WikiFrontmatter::default(),
        Some(yaml) => parse_frontmatter(yaml)?,
    };

    let body = body.to_owned();
    if body.len() > limits.max_body_bytes {
        return Err(AgentError::Wiki(format!(
            "state body is {} bytes, exceeding the configured limit of {} bytes",
            body.len(),
            limits.max_body_bytes
        )));
    }

    let sections = split_sections(&body, limits);

    Ok(WikiContext {
        frontmatter,
        body,
        source_path: source_path.to_path_buf(),
        sections,
    })
}

fn parse_frontmatter(yaml: &str) -> Result<WikiFrontmatter> {
    let mut wrapped = String::from("---\n");
    wrapped.push_str(yaml);
    if !yaml.ends_with('\n') {
        wrapped.push('\n');
    }
    wrapped.push_str("---\n");

    let parsed = Matter::<YAML>::new().parse(&wrapped);
    let data = parsed.data.ok_or_else(|| {
        AgentError::Wiki("frontmatter could not be parsed as a YAML mapping".into())
    })?;

    data.deserialize::<WikiFrontmatter>().map_err(|error| {
        AgentError::Wiki(format!(
            "frontmatter is invalid or has an unsupported shape: {error}"
        ))
    })
}

fn split_frontmatter(content: &str) -> Result<(Option<&str>, &str)> {
    let Some(first_line) = content.split_inclusive('\n').next() else {
        return Ok((None, content));
    };
    if first_line.trim_end() != "---" {
        return Ok((None, content));
    }

    let mut cursor = first_line.len();
    for line in content[first_line.len()..].split_inclusive('\n') {
        let line_start = cursor;
        cursor += line.len();
        if line.trim_end() == "---" {
            let yaml = &content[first_line.len()..line_start];
            let remaining = &content[cursor..];
            let body = remaining
                .strip_prefix("\r\n")
                .or_else(|| remaining.strip_prefix('\n'))
                .unwrap_or(remaining);
            return Ok((Some(yaml), body));
        }
    }

    Err(AgentError::Wiki(
        "frontmatter is missing its closing `---` delimiter".into(),
    ))
}

pub(crate) fn split_sections(body: &str, limits: &WikiLimits) -> Vec<WikiSection> {
    let mut sections = Vec::new();
    let mut heading: Option<String> = None;
    let mut content = String::new();

    for line in body.split_inclusive('\n') {
        let without_newline = line.trim_end_matches(['\r', '\n']);
        if let Some(raw_heading) = without_newline.strip_prefix("## ") {
            push_section(&mut sections, &mut heading, &mut content, limits);
            heading = Some(truncate_utf8(
                raw_heading.trim(),
                limits.max_section_bytes,
                "\n...[heading truncated]...\n",
            ));
            continue;
        }

        if heading.is_some() {
            content.push_str(line);
        }
    }

    push_section(&mut sections, &mut heading, &mut content, limits);
    sections
}

fn push_section(
    sections: &mut Vec<WikiSection>,
    heading: &mut Option<String>,
    content: &mut String,
    limits: &WikiLimits,
) {
    if sections.len() >= limits.max_sections {
        *heading = None;
        content.clear();
        return;
    }

    if let Some(heading) = heading.take() {
        let bounded = truncate_utf8(
            content.trim_end_matches('\n'),
            limits.max_section_bytes,
            SECTION_TRUNCATION_MARKER,
        );
        sections.push(WikiSection {
            heading,
            byte_len: bounded.len(),
            content: bounded,
        });
    }
    content.clear();
}

pub(crate) fn truncate_utf8(input: &str, max_bytes: usize, marker: &str) -> String {
    if input.len() <= max_bytes {
        return input.to_owned();
    }
    if max_bytes == 0 {
        return String::new();
    }

    if marker.len() >= max_bytes {
        let mut end = max_bytes;
        while end > 0 && !marker.is_char_boundary(end) {
            end -= 1;
        }
        return marker[..end].to_owned();
    }

    let mut end = max_bytes - marker.len();
    while end > 0 && !input.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{}", &input[..end], marker)
}

pub(crate) fn render_frontmatter(frontmatter: &WikiFrontmatter) -> String {
    let mut output = String::from("---\n");
    output.push_str(&format!("schema_version: {}\n", frontmatter.schema_version));
    output.push_str(&format!("title: {}\n", yaml_string(&frontmatter.title)));
    if frontmatter.tags.is_empty() {
        output.push_str("tags: []\n");
    } else {
        output.push_str("tags:\n");
        for tag in &frontmatter.tags {
            output.push_str(&format!("  - {}\n", yaml_string(tag)));
        }
    }
    output.push_str(&format!("updated: {}\n", yaml_string(&frontmatter.updated)));
    output.push_str(&format!("session_count: {}\n", frontmatter.session_count));
    output.push_str(&format!("status: {}\n", frontmatter.status.as_str()));
    if let Some(risk_level) = &frontmatter.risk_level {
        output.push_str(&format!("risk_level: {}\n", yaml_string(risk_level)));
    }
    if let Some(commit) = &frontmatter.last_verified_commit {
        output.push_str(&format!("last_verified_commit: {}\n", yaml_string(commit)));
    }
    output.push_str("---\n");
    output
}

fn yaml_string(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_frontmatter_and_sections() {
        let input = "---\nschema_version: 1\ntitle: World\ntags:\n  - rust\nupdated: 2026-10-22\nsession_count: 4\nstatus: green\n---\n# Intro\n\n## Current state\nready\n## Decisions\nchosen\n";
        let context = parse_context(input, Path::new("state.md"), &WikiLimits::default())
            .expect("valid context");

        assert_eq!(context.frontmatter.title, "World");
        assert_eq!(context.frontmatter.tags, vec!["rust"]);
        assert_eq!(context.frontmatter.session_count, 4);
        assert_eq!(context.frontmatter.status, WikiStatus::Green);
        assert_eq!(context.sections.len(), 2);
        assert_eq!(context.sections[0].heading, "Current state");
        assert_eq!(context.sections[0].content, "ready");
    }

    #[test]
    fn missing_frontmatter_is_a_valid_default_context() {
        let context = parse_context(
            "## Current state\nbody\n",
            Path::new("state.md"),
            &WikiLimits::default(),
        )
        .expect("body-only state is valid");
        assert_eq!(context.frontmatter, WikiFrontmatter::default());
        assert_eq!(context.body, "## Current state\nbody\n");
    }

    #[test]
    fn malformed_frontmatter_is_a_wiki_error() {
        let error = parse_context(
            "---\ntitle: [unterminated\n---\nbody\n",
            Path::new("state.md"),
            &WikiLimits::default(),
        )
        .unwrap_err();
        assert!(matches!(error, AgentError::Wiki(_)), "{error}");
    }

    #[test]
    fn unclosed_frontmatter_is_a_wiki_error() {
        let error = parse_context(
            "---\ntitle: World\nbody\n",
            Path::new("state.md"),
            &WikiLimits::default(),
        )
        .unwrap_err();
        assert!(matches!(error, AgentError::Wiki(_)), "{error}");
    }

    #[test]
    fn section_limits_are_applied_without_splitting_utf8() {
        let input = "---\ntitle: World\n---\n## Current state\nééééééééééééééééééééééééééééééé\n";
        let limits = WikiLimits {
            max_section_bytes: 40,
            ..WikiLimits::default()
        };
        let context = parse_context(input, Path::new("state.md"), &limits).unwrap();
        assert!(context.sections[0].byte_len <= 40);
        assert!(context.sections[0].content.contains("truncated"));
        assert!(context.sections[0].content.is_char_boundary(0));
    }
}
