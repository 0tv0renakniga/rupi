//! Shared project-root and sensitive-path policy.
//!
//! The orchestrator and fixed helper scripts use the same repository-relative
//! path contract: absolute paths, empty paths, NUL bytes, root traversal,
//! harness-private locations, and sensitive credential patterns are denied.
//! Symlink-escape hardening for existing targets and new-file parents is
//! tracked by TASK-021; this module keeps the pre-existing nearest-ancestor
//! containment check while providing stable codes for both path intents.

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

use thiserror::Error;

/// Maximum bytes of a requested path echoed in a policy error.
pub const MAX_PATH_ERROR_BYTES: usize = 256;

/// Credential file names denied at any depth.
pub const DEFAULT_SENSITIVE_NAMES: &[&str] = &[
    ".env",
    ".envrc",
    ".netrc",
    ".npmrc",
    ".pypirc",
    ".git-credentials",
    "credentials",
    "credentials.json",
    "secrets",
    "secrets.json",
    "id_rsa",
    "id_dsa",
    "id_ecdsa",
    "id_ed25519",
    "authorized_keys",
    "known_hosts",
];

/// Harness-private and credential-directory names denied at any depth.
pub const DEFAULT_SENSITIVE_DIRECTORIES: &[&str] = &[
    ".ssh",
    ".aws",
    ".gcp",
    ".gnupg",
    ".docker",
    ".kube",
    ".azure",
    ".password-store",
    ".config",
    ".harness-audit",
    ".ai-wiki",
];

/// Sensitive file extensions denied at any depth.
pub const DEFAULT_SENSITIVE_EXTENSIONS: &[&str] = &["pem", "key", "p12", "pfx", "jks", "kdbx"];

const TRUNCATION_MARKER: &str = "...[truncated]";

/// Stable path-policy denial codes.
///
/// These codes are part of the model/UI-safe error contract and must not be
/// replaced with free-form messages.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PathPolicyError {
    #[error("[PATH_ROOT_INACCESSIBLE] project root is not accessible: {0}")]
    RootInaccessible(String),

    #[error("[PATH_ROOT_NOT_DIRECTORY] project root must be a directory: {0}")]
    RootNotDirectory(String),

    #[error("[PATH_EMPTY] path must not be empty")]
    EmptyPath,

    #[error("[PATH_NUL] path must not contain NUL bytes")]
    NulByte,

    #[error("[PATH_ABSOLUTE] absolute paths are not allowed: {0}")]
    AbsolutePath(String),

    #[error("[PATH_TRAVERSAL] path escapes the project root: {0}")]
    Traversal(String),

    #[error("[PATH_SENSITIVE] sensitive path is denied: {0}")]
    SensitivePath(String),
}

impl PathPolicyError {
    /// Stable machine-readable denial code.
    pub const fn code(&self) -> &'static str {
        match self {
            Self::RootInaccessible(_) => "PATH_ROOT_INACCESSIBLE",
            Self::RootNotDirectory(_) => "PATH_ROOT_NOT_DIRECTORY",
            Self::EmptyPath => "PATH_EMPTY",
            Self::NulByte => "PATH_NUL",
            Self::AbsolutePath(_) => "PATH_ABSOLUTE",
            Self::Traversal(_) => "PATH_TRAVERSAL",
            Self::SensitivePath(_) => "PATH_SENSITIVE",
        }
    }
}

/// Canonical project root plus the deny-by-default path patterns.
///
/// Construction canonicalizes the configured root exactly once. Model output
/// never supplies the root, the sensitive patterns, or a resolved absolute path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathPolicy {
    root: PathBuf,
    sensitive_names: BTreeSet<String>,
    sensitive_directories: BTreeSet<String>,
    sensitive_extensions: BTreeSet<String>,
}

impl PathPolicy {
    /// Build the default policy for one canonical root.
    pub fn new(root: impl AsRef<Path>) -> Result<Self, PathPolicyError> {
        Self::with_sensitive_paths(
            root,
            DEFAULT_SENSITIVE_NAMES
                .iter()
                .map(|name| (*name).to_owned()),
            DEFAULT_SENSITIVE_DIRECTORIES
                .iter()
                .map(|name| (*name).to_owned()),
            DEFAULT_SENSITIVE_EXTENSIONS
                .iter()
                .map(|name| (*name).to_owned()),
        )
    }

    /// Build a policy with explicit sensitive patterns.
    ///
    /// Patterns are matched case-insensitively against normalized path
    /// components, file names, and the final extension. They are never applied
    /// as substring matches over the whole path.
    pub fn with_sensitive_paths(
        root: impl AsRef<Path>,
        sensitive_names: impl IntoIterator<Item = String>,
        sensitive_directories: impl IntoIterator<Item = String>,
        sensitive_extensions: impl IntoIterator<Item = String>,
    ) -> Result<Self, PathPolicyError> {
        let requested_root = root.as_ref();
        let raw_root = safe_path(&requested_root.to_string_lossy());
        let canonical_root = requested_root
            .canonicalize()
            .map_err(|_| PathPolicyError::RootInaccessible(raw_root.clone()))?;
        if !canonical_root.is_dir() {
            return Err(PathPolicyError::RootNotDirectory(raw_root));
        }

        Ok(Self {
            root: canonical_root,
            sensitive_names: lowercase_set(sensitive_names),
            sensitive_directories: lowercase_set(sensitive_directories),
            sensitive_extensions: lowercase_set(sensitive_extensions),
        })
    }

    /// Canonical configured root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Validate a repository-relative path with no implied existence.
    ///
    /// The returned path is lexically normalized and relative to [`Self::root`].
    pub fn authorize_path(&self, raw: &str) -> Result<PathBuf, PathPolicyError> {
        if raw.trim().is_empty() {
            return Err(PathPolicyError::EmptyPath);
        }
        if raw.contains('\0') {
            return Err(PathPolicyError::NulByte);
        }

        let requested = safe_path(raw);
        let path = Path::new(raw);
        if path.is_absolute() {
            return Err(PathPolicyError::AbsolutePath(requested));
        }

        let mut normalized = PathBuf::new();
        for component in path.components() {
            match component {
                Component::Normal(part) => normalized.push(part),
                Component::CurDir => {}
                Component::ParentDir => {
                    if !normalized.pop() {
                        return Err(PathPolicyError::Traversal(requested));
                    }
                }
                Component::RootDir | Component::Prefix(_) => {
                    return Err(PathPolicyError::AbsolutePath(requested));
                }
            }
        }

        if normalized.as_os_str().is_empty() {
            return Err(PathPolicyError::EmptyPath);
        }
        if self.is_sensitive(&normalized) {
            return Err(PathPolicyError::SensitivePath(requested));
        }

        self.reject_symlink_escape(&normalized, &requested)?;
        Ok(normalized)
    }

    /// Validate a path that should identify an existing target.
    ///
    /// Existence and target-type checks remain with the caller; this method
    /// applies the same root, traversal, sensitive-path, and containment
    /// semantics as pre-dispatch authorization. TASK-021 hardens existing-target
    /// symlink resolution.
    pub fn authorize_existing_path(&self, raw: &str) -> Result<PathBuf, PathPolicyError> {
        self.authorize_path(raw)
    }

    /// Validate a proposed new-file path.
    ///
    /// The final component may not exist. The nearest existing ancestor must
    /// remain inside the canonical root. TASK-021 hardens new-parent symlink
    /// resolution.
    pub fn authorize_new_file_path(&self, raw: &str) -> Result<PathBuf, PathPolicyError> {
        self.authorize_path(raw)
    }

    /// Return whether a normalized relative path matches a denied pattern.
    pub fn is_sensitive(&self, path: &Path) -> bool {
        for component in path.components() {
            let Component::Normal(part) = component else {
                continue;
            };
            let part = part.to_string_lossy().to_ascii_lowercase();
            if self.sensitive_directories.contains(&part)
                || self.sensitive_names.contains(&part)
                || part.starts_with(".env.")
                || part.starts_with("credentials.")
                || part.starts_with("secrets.")
            {
                return true;
            }
        }

        path.extension()
            .map(|extension| extension.to_string_lossy().to_ascii_lowercase())
            .map(|extension| self.sensitive_extensions.contains(&extension))
            .unwrap_or(false)
    }

    fn reject_symlink_escape(
        &self,
        relative: &Path,
        requested: &str,
    ) -> Result<(), PathPolicyError> {
        let candidate = self.root.join(relative);
        let Some(existing) = deepest_existing_ancestor(&candidate) else {
            return Ok(());
        };

        match existing.canonicalize() {
            Ok(resolved) if resolved.starts_with(&self.root) => Ok(()),
            Ok(_) => Err(PathPolicyError::Traversal(requested.to_owned())),
            Err(_) => Ok(()),
        }
    }
}

fn deepest_existing_ancestor(path: &Path) -> Option<&Path> {
    let mut current = Some(path);
    while let Some(candidate) = current {
        if candidate.exists() {
            return Some(candidate);
        }
        current = candidate.parent();
    }
    None
}

fn lowercase_set(values: impl IntoIterator<Item = String>) -> BTreeSet<String> {
    values
        .into_iter()
        .map(|value| value.to_ascii_lowercase())
        .collect()
}

fn safe_path(raw: &str) -> String {
    let sanitized = raw
        .chars()
        .map(|character| {
            if character.is_control() {
                '\u{FFFD}'
            } else {
                character
            }
        })
        .collect::<String>();

    if sanitized.len() <= MAX_PATH_ERROR_BYTES {
        return sanitized;
    }
    if MAX_PATH_ERROR_BYTES <= TRUNCATION_MARKER.len() {
        let mut end = MAX_PATH_ERROR_BYTES.min(sanitized.len());
        while end > 0 && !sanitized.is_char_boundary(end) {
            end -= 1;
        }
        return sanitized[..end].to_owned();
    }

    let limit = MAX_PATH_ERROR_BYTES - TRUNCATION_MARKER.len();
    let mut end = limit.min(sanitized.len());
    while end > 0 && !sanitized.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{}", &sanitized[..end], TRUNCATION_MARKER)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(label: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("rupi-path-policy-{label}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).expect("create temp root");
        root
    }

    #[test]
    fn bounds_and_sanitizes_paths_in_errors() {
        let root = temp_root("bounded");
        let policy = PathPolicy::new(&root).expect("policy");
        let raw = format!("/{}\n{}", "x".repeat(1_000), "y".repeat(1_000));
        let error = policy.authorize_path(&raw).expect_err("absolute path");
        let message = error.to_string();
        assert_eq!(error.code(), "PATH_ABSOLUTE");
        assert!(message.len() < MAX_PATH_ERROR_BYTES + 128);
        assert!(!message.contains('\n'));

        let _ = std::fs::remove_dir_all(root);
    }
}
