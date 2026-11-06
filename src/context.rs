//! Bounded loading of operator-owned global context.
//!
//! Global trusted context comes only from fixed paths inside the trusted
//! configuration directory. The target repository is never scanned, and loaded
//! content is captured at startup so model or repository activity cannot alter
//! it.

use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::config::Limits;

/// Fixed global context files loaded directly from the trusted config directory.
pub const GLOBAL_CONTEXT_FILES: [&str; 2] = ["AGENTS.md", "CLAUDE.md"];
/// Directory containing trusted prompt templates (`prompts/*.md`).
pub const GLOBAL_CONTEXT_PROMPTS_DIR: &str = "prompts";
/// Maximum number of global context sources retained in one load.
pub const MAX_GLOBAL_CONTEXT_SOURCES: usize = 64;

/// One trusted global context source and its provenance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlobalContextSource {
    /// Path relative to the trusted configuration directory.
    pub relative_path: String,
    /// Bounded, validated UTF-8 content.
    pub content: String,
    /// Number of retained content bytes.
    pub bytes: usize,
    /// Whether the source was shortened by a configured bound.
    pub truncated: bool,
}

/// Immutable snapshot of global trusted context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlobalContext {
    /// Trusted configuration directory used for provenance.
    pub directory: PathBuf,
    pub sources: Vec<GlobalContextSource>,
    pub total_bytes: usize,
    pub truncated: bool,
    pub warnings: Vec<String>,
}

impl GlobalContext {
    pub fn empty(directory: impl Into<PathBuf>) -> Self {
        Self {
            directory: directory.into(),
            sources: Vec::new(),
            total_bytes: 0,
            truncated: false,
            warnings: Vec::new(),
        }
    }

    /// Load global context from fixed paths under `directory`.
    ///
    /// Missing files and directories are normal. Unreadable, non-regular, or
    /// non-UTF-8 sources are skipped with a bounded warning. Per-source content
    /// is limited by `limits.file_preview_bytes` and the aggregate is limited by
    /// `limits.max_global_context_bytes`.
    pub fn load(directory: &Path, limits: &Limits) -> Self {
        let mut context = Self::empty(directory);
        let per_source_limit = limits.file_preview_bytes.max(1);
        let total_limit = limits.max_global_context_bytes.max(1);

        for name in GLOBAL_CONTEXT_FILES {
            context.load_source(directory, name, name, per_source_limit, total_limit);
        }

        context.load_prompt_files(directory, per_source_limit, total_limit);
        context
    }

    /// Context-tab lines with a provenance label for every loaded source.
    pub fn display_lines(&self) -> Vec<String> {
        let mut lines = vec![
            "Global trusted context (operator-owned)".to_owned(),
            format!("directory: {}", self.directory.display()),
            format!(
                "sources: {} loaded, {} bytes{}",
                self.sources.len(),
                self.total_bytes,
                if self.truncated { ", truncated" } else { "" }
            ),
        ];

        if self.sources.is_empty() {
            lines.push("No trusted global context sources loaded.".to_owned());
        } else {
            for source in &self.sources {
                let suffix = if source.truncated { ", truncated" } else { "" };
                lines.push(format!(
                    "--- {} [trusted] ({} bytes{}) ---",
                    source.relative_path, source.bytes, suffix
                ));
                if source.content.is_empty() {
                    lines.push("(empty source)".to_owned());
                } else {
                    lines.extend(source.content.lines().map(str::to_owned));
                }
            }
        }

        for warning in &self.warnings {
            lines.push(format!("warning: {warning}"));
        }
        lines
    }

    fn load_prompt_files(&mut self, directory: &Path, per_source: usize, total_limit: usize) {
        let prompts_dir = directory.join(GLOBAL_CONTEXT_PROMPTS_DIR);
        let metadata = match fs::symlink_metadata(&prompts_dir) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
            Err(error) => {
                self.push_warning(format!(
                    "failed to inspect {}: {error}",
                    prompts_dir.display()
                ));
                return;
            }
        };
        if !metadata.file_type().is_dir() {
            self.push_warning(format!(
                "skipped {}: not a regular directory",
                prompts_dir.display()
            ));
            return;
        }
        let entries = match fs::read_dir(&prompts_dir) {
            Ok(entries) => entries,
            Err(error) => {
                self.push_warning(format!("failed to read {}: {error}", prompts_dir.display()));
                return;
            }
        };

        let mut names = Vec::new();
        let max_prompt_sources = MAX_GLOBAL_CONTEXT_SOURCES.saturating_sub(self.sources.len());
        let mut prompt_overflow = false;
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    self.push_warning(format!(
                        "failed to enumerate {}: {error}",
                        prompts_dir.display()
                    ));
                    continue;
                }
            };
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                self.push_warning(format!(
                    "skipped non-UTF-8 prompt file name in {}",
                    prompts_dir.display()
                ));
                continue;
            };
            if !name.ends_with(".md") || name.starts_with('.') || !is_safe_source_name(&name) {
                continue;
            }
            if names.len() >= max_prompt_sources {
                prompt_overflow = true;
                break;
            }
            names.push(name);
        }
        if prompt_overflow {
            self.truncated = true;
            self.push_warning(format!(
                "skipped additional prompt files in {}: source count limit reached",
                prompts_dir.display()
            ));
        }
        names.sort();

        for name in names {
            let relative_path = format!("{GLOBAL_CONTEXT_PROMPTS_DIR}/{name}");
            self.load_source(
                directory,
                &relative_path,
                &relative_path,
                per_source,
                total_limit,
            );
        }
    }

    fn load_source(
        &mut self,
        directory: &Path,
        file_path: &str,
        relative_path: &str,
        per_source: usize,
        total_limit: usize,
    ) {
        if self.sources.len() >= MAX_GLOBAL_CONTEXT_SOURCES {
            self.truncated = true;
            self.push_warning(format!(
                "skipped {relative_path}: source count limit reached"
            ));
            return;
        }
        let remaining = total_limit.saturating_sub(self.total_bytes);
        if remaining == 0 {
            self.truncated = true;
            self.push_warning(format!(
                "skipped {relative_path}: global context byte limit reached"
            ));
            return;
        }

        let limit = per_source.min(remaining);
        let path = directory.join(file_path);
        let (content, truncated) = match read_bounded_regular_file(&path, limit) {
            Ok(Some(value)) => value,
            Ok(None) => return,
            Err(warning) => {
                self.push_warning(format!("skipped {relative_path}: {warning}"));
                return;
            }
        };

        self.total_bytes = self.total_bytes.saturating_add(content.len());
        self.truncated |= truncated;
        self.sources.push(GlobalContextSource {
            relative_path: relative_path.to_owned(),
            bytes: content.len(),
            content,
            truncated,
        });
    }

    fn push_warning(&mut self, warning: String) {
        const MAX_WARNING_BYTES: usize = 512;
        let mut warning = warning.replace(['\0', '\r', '\n'], " ");
        if warning.len() > MAX_WARNING_BYTES {
            let mut end = MAX_WARNING_BYTES;
            while end > 0 && !warning.is_char_boundary(end) {
                end -= 1;
            }
            warning.truncate(end);
            warning.push_str("...<truncated>");
        }
        self.warnings.push(warning);
    }
}

fn is_safe_source_name(name: &str) -> bool {
    !name.is_empty() && !name.chars().any(char::is_control)
}

/// Read a regular UTF-8 file without consuming more than `limit + 1` bytes.
///
/// Returns `None` when the file does not exist. Symlinks and other non-regular
/// paths are rejected so a trusted-directory entry cannot redirect the load.
fn read_bounded_regular_file(path: &Path, limit: usize) -> Result<Option<(String, bool)>, String> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("metadata failed: {error}")),
    };
    if !metadata.file_type().is_file() {
        return Err("not a regular file".to_owned());
    }

    let mut file = File::open(path).map_err(|error| format!("open failed: {error}"))?;
    let read_limit = u64::try_from(limit).unwrap_or(u64::MAX).saturating_add(1);
    let mut bytes = Vec::new();
    file.by_ref()
        .take(read_limit)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("read failed: {error}"))?;

    let truncated =
        bytes.len() > limit || metadata.len() > u64::try_from(limit).unwrap_or(u64::MAX);
    if bytes.len() > limit {
        let mut end = limit;
        while end > 0
            && bytes
                .get(end)
                .is_some_and(|byte| is_utf8_continuation(*byte))
        {
            end -= 1;
        }
        bytes.truncate(end);
    }

    let content = String::from_utf8(bytes).map_err(|_| "content is not valid UTF-8".to_owned())?;
    Ok(Some((content, truncated)))
}

fn is_utf8_continuation(byte: u8) -> bool {
    byte & 0b1100_0000 == 0b1000_0000
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_context_displays_provenance_header() {
        let context = GlobalContext::empty("/tmp/harness-config");
        let lines = context.display_lines();
        assert!(lines[0].contains("operator-owned"));
        assert!(
            lines
                .iter()
                .any(|line| line.contains("/tmp/harness-config"))
        );
        assert!(
            lines
                .iter()
                .any(|line| line.contains("No trusted global context sources loaded."))
        );
    }

    #[test]
    fn utf8_continuation_detection_is_narrow() {
        assert!(is_utf8_continuation(0b1000_0000));
        assert!(is_utf8_continuation(0b1011_1111));
        assert!(!is_utf8_continuation(0b1100_0011));
        assert!(!is_utf8_continuation(b'a'));
    }
}
