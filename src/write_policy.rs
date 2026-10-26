//! Strict schema and policy preflight for the narrow `write_file` action.
//!
//! This module validates model-supplied write requests before policy, approval,
//! or dispatch. It does not write files. Path containment, sensitive-path
//! denial, and symlink escapes remain owned by the shared [`crate::policy`]
//! layer, which runs immediately after this schema gate.

use std::path::{Component, Path};

use thiserror::Error;

use crate::mcp::AgentTool;
use crate::state_machine::TaskMode;

/// Maximum UTF-8 content accepted by one `write_file` request.
///
/// The cap leaves room for JSON metadata inside the fixed 64 KiB script
/// protocol request envelope.
pub const MAX_WRITE_CONTENT_BYTES: usize = 48 * 1024;

/// Maximum repository-relative path accepted by one write request.
///
/// This matches the approval prompt's target bound so the displayed target is
/// never truncated.
pub const MAX_WRITE_PATH_BYTES: usize = 256;

/// Required length of a lowercase hexadecimal SHA-256 digest.
pub const SHA256_HEX_BYTES: usize = 64;

/// Validated write request. No filesystem mutation occurs here.
///
/// The digest is optional at the schema layer because new-file creation does not
/// need one. The helper requires it before replacing an existing target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriteFileRequest {
    pub path: String,
    pub content: String,
    pub expected_sha256: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum WritePreflightError {
    #[error("[WRITE_MODE_DENIED] write_file is available only in Change mode")]
    ModeDenied,

    #[error("[WRITE_PATH_EMPTY] write_file path must not be empty")]
    EmptyPath,

    #[error("[WRITE_PATH_TOO_LONG] write_file path is {actual} bytes; maximum is {max}")]
    PathTooLong { actual: usize, max: usize },

    #[error("[WRITE_PATH_NUL] write_file path must not contain NUL bytes")]
    PathNul,

    #[error("[WRITE_PATH_CONTROL] write_file path must not contain control characters")]
    PathControl,

    #[error("[WRITE_PATH_NOT_RELATIVE] write_file path must be relative to the project root")]
    PathNotRelative,

    #[error("[WRITE_PATH_TRAVERSAL] write_file path escapes the project root")]
    PathTraversal,

    #[error("[WRITE_CONTENT_TOO_LARGE] write_file content is {actual} bytes; maximum is {max}")]
    ContentTooLarge { actual: usize, max: usize },

    #[error("[WRITE_CONTENT_NUL] write_file content must be UTF-8 text without NUL bytes")]
    ContentNul,

    #[error(
        "[WRITE_HASH_FORMAT] expected_sha256 must be exactly 64 lowercase hexadecimal characters"
    )]
    InvalidExpectedSha256,
}

impl WritePreflightError {
    /// Whether this failure is a policy denial rather than malformed input.
    pub fn is_policy_denial(&self) -> bool {
        matches!(
            self,
            Self::ModeDenied
                | Self::PathNul
                | Self::PathControl
                | Self::PathNotRelative
                | Self::PathTraversal
        )
    }
}

/// Validate one tool call without touching the filesystem.
///
/// `Ok(None)` means the action is not `write_file`. A write request is accepted
/// only in Change mode and only with a bounded relative path, bounded UTF-8
/// content, and an optional lowercase SHA-256 digest.
pub fn preflight(
    tool: &AgentTool,
    mode: TaskMode,
) -> std::result::Result<Option<WriteFileRequest>, WritePreflightError> {
    let AgentTool::WriteFile {
        path,
        content,
        expected_sha256,
    } = tool
    else {
        return Ok(None);
    };

    if mode != TaskMode::Change {
        return Err(WritePreflightError::ModeDenied);
    }

    validate_path(path)?;
    validate_content(content)?;
    if let Some(expected_sha256) = expected_sha256
        && !is_lowercase_sha256(expected_sha256)
    {
        return Err(WritePreflightError::InvalidExpectedSha256);
    }

    Ok(Some(WriteFileRequest {
        path: path.clone(),
        content: content.clone(),
        expected_sha256: expected_sha256.clone(),
    }))
}

fn validate_path(path: &str) -> std::result::Result<(), WritePreflightError> {
    if path.trim().is_empty() {
        return Err(WritePreflightError::EmptyPath);
    }
    if path.len() > MAX_WRITE_PATH_BYTES {
        return Err(WritePreflightError::PathTooLong {
            actual: path.len(),
            max: MAX_WRITE_PATH_BYTES,
        });
    }
    if path.contains('\0') {
        return Err(WritePreflightError::PathNul);
    }
    if path.chars().any(char::is_control) {
        return Err(WritePreflightError::PathControl);
    }

    let mut depth = 0usize;
    for component in Path::new(path).components() {
        match component {
            Component::Normal(_) => depth += 1,
            Component::CurDir => {}
            Component::ParentDir => {
                if depth == 0 {
                    return Err(WritePreflightError::PathTraversal);
                }
                depth -= 1;
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(WritePreflightError::PathNotRelative);
            }
        }
    }
    if depth == 0 {
        return Err(WritePreflightError::EmptyPath);
    }
    Ok(())
}

fn validate_content(content: &str) -> std::result::Result<(), WritePreflightError> {
    if content.len() > MAX_WRITE_CONTENT_BYTES {
        return Err(WritePreflightError::ContentTooLarge {
            actual: content.len(),
            max: MAX_WRITE_CONTENT_BYTES,
        });
    }
    if content.contains('\0') {
        return Err(WritePreflightError::ContentNul);
    }
    Ok(())
}

/// Validate the optional expected digest without normalizing or accepting a
/// prefixed/uppercase variant.
pub fn is_lowercase_sha256(value: &str) -> bool {
    value.len() == SHA256_HEX_BYTES
        && value
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &str, content: &str, expected_sha256: Option<&str>) -> AgentTool {
        AgentTool::WriteFile {
            path: path.into(),
            content: content.into(),
            expected_sha256: expected_sha256.map(str::to_owned),
        }
    }

    #[test]
    fn accepts_a_bounded_change_mode_write() {
        let request = preflight(
            &write("src/main.rs", "fn main() {}\n", Some(&"ab".repeat(32))),
            TaskMode::Change,
        )
        .expect("valid write")
        .expect("write action");

        assert_eq!(request.path, "src/main.rs");
        assert_eq!(request.content, "fn main() {}\n");
        assert_eq!(request.expected_sha256, Some("ab".repeat(32)));
    }

    #[test]
    fn rejects_read_only_modes() {
        for mode in [TaskMode::Investigate, TaskMode::Plan] {
            assert_eq!(
                preflight(&write("src/main.rs", "ok", None), mode),
                Err(WritePreflightError::ModeDenied)
            );
        }
    }

    #[test]
    fn ignores_non_write_actions() {
        let read = AgentTool::ReadFile {
            path: "src/main.rs".into(),
            line_start: None,
            line_end: None,
        };
        assert_eq!(preflight(&read, TaskMode::Change), Ok(None));
    }

    #[test]
    fn rejects_invalid_hash_formats() {
        for hash in [
            "abc123",
            &"AB".repeat(32),
            &"gg".repeat(32),
            &format!("sha256:{}", "ab".repeat(32)),
            &"a".repeat(63),
            &"a".repeat(65),
        ] {
            assert_eq!(
                preflight(&write("src/main.rs", "ok", Some(hash)), TaskMode::Change),
                Err(WritePreflightError::InvalidExpectedSha256),
                "{hash}"
            );
        }
    }

    #[test]
    fn enforces_the_content_byte_cap() {
        let at_limit = "x".repeat(MAX_WRITE_CONTENT_BYTES);
        assert!(preflight(&write("src/main.rs", &at_limit, None), TaskMode::Change).is_ok());

        let over_limit = "x".repeat(MAX_WRITE_CONTENT_BYTES + 1);
        assert_eq!(
            preflight(&write("src/main.rs", &over_limit, None), TaskMode::Change),
            Err(WritePreflightError::ContentTooLarge {
                actual: MAX_WRITE_CONTENT_BYTES + 1,
                max: MAX_WRITE_CONTENT_BYTES,
            })
        );

        let multibyte = "é".repeat(MAX_WRITE_CONTENT_BYTES / 2 + 1);
        assert!(multibyte.len() > MAX_WRITE_CONTENT_BYTES);
        assert!(preflight(&write("src/main.rs", &multibyte, None), TaskMode::Change).is_err());
    }

    #[test]
    fn rejects_nul_and_unsafe_paths() {
        assert_eq!(
            preflight(&write("src/main.rs", "a\0b", None), TaskMode::Change),
            Err(WritePreflightError::ContentNul)
        );
        assert_eq!(
            preflight(&write("", "ok", None), TaskMode::Change),
            Err(WritePreflightError::EmptyPath)
        );
        assert_eq!(
            preflight(&write(".", "ok", None), TaskMode::Change),
            Err(WritePreflightError::EmptyPath)
        );
        assert_eq!(
            preflight(&write("/etc/passwd", "ok", None), TaskMode::Change),
            Err(WritePreflightError::PathNotRelative)
        );
        assert_eq!(
            preflight(&write("../outside.txt", "ok", None), TaskMode::Change),
            Err(WritePreflightError::PathTraversal)
        );
        assert_eq!(
            preflight(&write("src/\nmain.rs", "ok", None), TaskMode::Change),
            Err(WritePreflightError::PathControl)
        );
        assert_eq!(
            preflight(
                &write(&"a".repeat(MAX_WRITE_PATH_BYTES + 1), "ok", None),
                TaskMode::Change
            ),
            Err(WritePreflightError::PathTooLong {
                actual: MAX_WRITE_PATH_BYTES + 1,
                max: MAX_WRITE_PATH_BYTES,
            })
        );
    }
}
