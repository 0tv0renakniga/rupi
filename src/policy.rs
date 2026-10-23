use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

use thiserror::Error;

use crate::mcp::tools::AgentTool;

/// Upper bound for model-requested command timeouts.
pub const MAX_COMMAND_TIMEOUT_SECS: u64 = 900;

/// Programs allowed by the default shell command profile. The filesystem and
/// Git adapters remain separate capabilities; this list is only for
/// program-plus-argv command requests.
pub const DEFAULT_ALLOWED_PROGRAMS: &[&str] = &[
    "cargo",
    "rustc",
    "rustfmt",
    "clippy-driver",
    "git",
    "npm",
    "node",
    "python3",
    "pytest",
    "pip",
];

/// Characters that have shell meaning. The harness never invokes a shell, but
/// rejecting these prevents model output from being blindly copied into one
/// later and makes the argv contract unambiguous.
const SHELL_METACHARACTERS: &[char] = &[
    ';', '&', '|', '<', '>', '(', ')', '$', '`', '\\', '"', '\'', '\n', '\r', '\0', '*', '?', '[',
    ']', '{', '}', '!', '#', '~',
];

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PolicyViolation {
    #[error("path must not be empty")]
    EmptyPath,

    #[error("absolute paths are not allowed: {0}")]
    AbsolutePath(String),

    #[error("path escapes the repository root: {0}")]
    PathTraversal(String),

    #[error("sensitive path is denied: {0}")]
    SensitivePath(String),

    #[error("program must be a bare executable name: {0}")]
    InvalidProgram(String),

    #[error("program is not allowlisted: {0}")]
    ProgramNotAllowed(String),

    #[error("sudo is forbidden")]
    SudoForbidden,

    #[error("argument contains a shell metacharacter: {0}")]
    ShellMetacharacter(String),

    #[error("command timeout must be between 1 and {max} seconds, got {requested}")]
    TimeoutOutOfRange { requested: u64, max: u64 },

    #[error("direct MCP tool calls are not permitted by this policy")]
    DirectMcpToolDenied,
}

/// Structured policy for one repository task.
///
/// Path decisions are made from components and file extensions, not substring
/// matches over an entire path string.
#[derive(Debug, Clone)]
pub struct Policy {
    pub repo_root: PathBuf,
    pub allowed_programs: BTreeSet<String>,
    pub sensitive_names: BTreeSet<String>,
    pub sensitive_directories: BTreeSet<String>,
    pub sensitive_extensions: BTreeSet<String>,
    pub allow_git_commit: bool,
    pub allow_network: bool,
}

impl Policy {
    pub fn default_for_root(repo_root: impl Into<PathBuf>) -> Self {
        Self {
            repo_root: repo_root.into(),
            allowed_programs: DEFAULT_ALLOWED_PROGRAMS
                .iter()
                .map(|program| (*program).to_owned())
                .collect(),
            sensitive_names: [
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
            ]
            .into_iter()
            .map(str::to_owned)
            .collect(),
            sensitive_directories: [
                ".ssh",
                ".aws",
                ".gnupg",
                ".docker",
                ".kube",
                ".azure",
                ".password-store",
                ".config",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect(),
            sensitive_extensions: ["pem", "key", "p12", "pfx", "jks", "kdbx"]
                .into_iter()
                .map(str::to_owned)
                .collect(),
            allow_git_commit: false,
            allow_network: false,
        }
    }

    pub fn authorize(&self, tool: &AgentTool) -> Result<(), PolicyViolation> {
        match tool {
            AgentTool::ReadFile { path, .. }
            | AgentTool::WriteFile { path, .. }
            | AgentTool::ApplyPatch { path, .. } => {
                self.authorize_path(path)?;
                Ok(())
            }
            AgentTool::RunCommand {
                program,
                args,
                cwd,
                timeout_secs,
            } => self.authorize_command_with_timeout(program, args, cwd.as_deref(), *timeout_secs),
            AgentTool::GitStatus | AgentTool::GitDiff { .. } => Ok(()),
            AgentTool::McpTool { .. } => Err(PolicyViolation::DirectMcpToolDenied),
        }
    }

    /// Validate and lexically normalize a repository-relative path.
    pub fn authorize_path(&self, raw: &str) -> Result<PathBuf, PolicyViolation> {
        if raw.trim().is_empty() {
            return Err(PolicyViolation::EmptyPath);
        }

        let path = Path::new(raw);
        if path.is_absolute() {
            return Err(PolicyViolation::AbsolutePath(raw.to_owned()));
        }

        let mut normalized = PathBuf::new();
        for component in path.components() {
            match component {
                Component::Normal(part) => normalized.push(part),
                Component::CurDir => {}
                Component::ParentDir => {
                    if !normalized.pop() {
                        return Err(PolicyViolation::PathTraversal(raw.to_owned()));
                    }
                }
                Component::RootDir | Component::Prefix(_) => {
                    return Err(PolicyViolation::AbsolutePath(raw.to_owned()));
                }
            }
        }

        if normalized.as_os_str().is_empty() {
            return Err(PolicyViolation::EmptyPath);
        }
        if self.is_sensitive(&normalized) {
            return Err(PolicyViolation::SensitivePath(raw.to_owned()));
        }

        self.reject_existing_symlink_escape(&normalized, raw)?;
        Ok(normalized)
    }

    pub fn authorize_command(
        &self,
        program: &str,
        args: &[String],
        cwd: Option<&str>,
    ) -> Result<(), PolicyViolation> {
        self.authorize_command_with_timeout(program, args, cwd, None)
    }

    pub fn authorize_command_with_timeout(
        &self,
        program: &str,
        args: &[String],
        cwd: Option<&str>,
        timeout_secs: Option<u64>,
    ) -> Result<(), PolicyViolation> {
        self.authorize_program(program)?;

        if let Some(requested) = timeout_secs
            && (requested == 0 || requested > MAX_COMMAND_TIMEOUT_SECS)
        {
            return Err(PolicyViolation::TimeoutOutOfRange {
                requested,
                max: MAX_COMMAND_TIMEOUT_SECS,
            });
        }

        if let Some(argument) = args.iter().find(|argument| {
            argument
                .chars()
                .any(|ch| SHELL_METACHARACTERS.contains(&ch))
        }) {
            return Err(PolicyViolation::ShellMetacharacter(argument.clone()));
        }

        if let Some(cwd) = cwd {
            self.authorize_path(cwd)?;
        }
        Ok(())
    }

    fn authorize_program(&self, program: &str) -> Result<(), PolicyViolation> {
        if program.is_empty() || program.contains('/') || program.contains('\\') {
            return Err(PolicyViolation::InvalidProgram(program.to_owned()));
        }

        if program.eq_ignore_ascii_case("sudo") {
            return Err(PolicyViolation::SudoForbidden);
        }

        if !self.allowed_programs.contains(program) {
            return Err(PolicyViolation::ProgramNotAllowed(program.to_owned()));
        }
        Ok(())
    }

    pub fn is_sensitive(&self, path: &Path) -> bool {
        for component in path.components() {
            if let Component::Normal(part) = component {
                let part = part.to_string_lossy().to_ascii_lowercase();
                if self.sensitive_directories.contains(&part) {
                    return true;
                }
            }
        }

        let Some(file_name) = path.file_name() else {
            return false;
        };
        let file_name = file_name.to_string_lossy().to_ascii_lowercase();

        if self.sensitive_names.contains(&file_name) || file_name.starts_with(".env.") {
            return true;
        }

        path.extension()
            .map(|extension| extension.to_string_lossy().to_ascii_lowercase())
            .map(|extension| self.sensitive_extensions.contains(&extension))
            .unwrap_or(false)
    }

    fn reject_existing_symlink_escape(
        &self,
        relative: &Path,
        raw: &str,
    ) -> Result<(), PolicyViolation> {
        let Ok(root) = self.repo_root.canonicalize() else {
            // Policy remains useful for a not-yet-created repository root.
            return Ok(());
        };

        let candidate = root.join(relative);
        let mut existing = candidate.as_path();
        while !existing.exists() {
            let Some(parent) = existing.parent() else {
                return Ok(());
            };
            existing = parent;
        }

        match existing.canonicalize() {
            Ok(resolved) if resolved.starts_with(&root) => Ok(()),
            Ok(_) => Err(PolicyViolation::PathTraversal(raw.to_owned())),
            Err(_) => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy() -> Policy {
        Policy::default_for_root("/tmp/local-agent-harness-policy-tests")
    }

    #[test]
    fn rejects_absolute_paths() {
        let policy = policy();
        for path in ["/etc/passwd", "/tmp/file", "//server/share"] {
            assert!(
                matches!(
                    policy.authorize_path(path),
                    Err(PolicyViolation::AbsolutePath(_))
                ),
                "{path} should be rejected"
            );
        }
    }

    #[test]
    fn rejects_repository_traversal() {
        let policy = policy();
        for path in [
            "..",
            "../Cargo.toml",
            "src/../../outside.txt",
            "./../../etc/passwd",
        ] {
            assert!(
                matches!(
                    policy.authorize_path(path),
                    Err(PolicyViolation::PathTraversal(_))
                ),
                "{path} should be rejected"
            );
        }
    }

    #[test]
    fn accepts_normal_paths_and_normalizes_internal_parent_components() {
        let policy = policy();
        assert_eq!(
            policy.authorize_path("src/main.rs").unwrap(),
            PathBuf::from("src/main.rs")
        );
        assert_eq!(
            policy.authorize_path("src/../README.md").unwrap(),
            PathBuf::from("README.md")
        );
        assert_eq!(
            policy.authorize_path("./tests/policy_tests.rs").unwrap(),
            PathBuf::from("tests/policy_tests.rs")
        );
    }

    #[test]
    fn rejects_sensitive_names_directories_and_extensions() {
        let policy = policy();
        for path in [
            ".env",
            ".env.local",
            "config/.env.production",
            ".ssh/id_rsa",
            "home/user/.ssh/authorized_keys",
            "credentials.json",
            "config/credentials",
            ".aws/credentials",
            "certs/server.pem",
            "keys/signing.key",
            "certs/bundle.p12",
        ] {
            assert!(
                matches!(
                    policy.authorize_path(path),
                    Err(PolicyViolation::SensitivePath(_))
                ),
                "{path} should be denied"
            );
        }
    }

    #[test]
    fn does_not_use_substring_matching_for_secret_names() {
        let policy = policy();
        for path in [
            "src/environment.rs",
            "docs/secret-management.md",
            "tests/keyboard.rs",
            "src/credentials_policy.rs",
        ] {
            assert!(
                policy.authorize_path(path).is_ok(),
                "{path} should not be denied by substring matching"
            );
        }
    }

    #[test]
    fn rejects_non_allowlisted_programs_and_sudo() {
        let policy = policy();
        for program in ["rm", "sh", "bash", "curl", "wget", "sudo"] {
            assert!(
                policy.authorize_command(program, &[], None).is_err(),
                "{program} should be denied"
            );
        }

        assert_eq!(
            policy.authorize_command("sudo", &["cargo".to_owned(), "test".to_owned()], None),
            Err(PolicyViolation::SudoForbidden)
        );

        for program in ["/bin/sh", "bin/cargo", "..\\cargo"] {
            assert!(
                matches!(
                    policy.authorize_command(program, &[], None),
                    Err(PolicyViolation::InvalidProgram(_))
                ),
                "{program} should be rejected as a non-bare program"
            );
        }
    }

    #[test]
    fn rejects_shell_metacharacters_in_arguments() {
        let policy = policy();
        let denied = [
            vec!["test; rm -rf /".to_owned()],
            vec!["test".to_owned(), "&&".to_owned(), "rm".to_owned()],
            vec!["$(touch /tmp/pwned)".to_owned()],
            vec!["a|b".to_owned()],
            vec!["--message=\"hello\"".to_owned()],
            vec!["*.rs".to_owned()],
            vec!["line\nbreak".to_owned()],
        ];

        for args in denied {
            assert!(
                matches!(
                    policy.authorize_command("cargo", &args, None),
                    Err(PolicyViolation::ShellMetacharacter(_))
                ),
                "{args:?} should be denied"
            );
        }

        assert!(
            policy
                .authorize_command("cargo", &["test".into(), "--all-targets".into()], None)
                .is_ok()
        );
    }

    #[test]
    fn authorizes_canonical_tools_and_checks_cwd() {
        let policy = policy();
        assert!(
            policy
                .authorize(&AgentTool::ReadFile {
                    path: "src/main.rs".into(),
                    line_start: None,
                    line_end: None,
                })
                .is_ok()
        );
        assert!(
            policy
                .authorize(&AgentTool::RunCommand {
                    program: "cargo".into(),
                    args: vec!["check".into()],
                    cwd: Some("src".into()),
                    timeout_secs: None,
                })
                .is_ok()
        );
        assert!(
            policy
                .authorize(&AgentTool::RunCommand {
                    program: "cargo".into(),
                    args: vec!["check".into()],
                    cwd: Some("../outside".into()),
                    timeout_secs: None,
                })
                .is_err()
        );
        assert_eq!(
            policy.authorize(&AgentTool::McpTool {
                server: "shell".into(),
                name: "run".into(),
                arguments: serde_json::json!({}),
            }),
            Err(PolicyViolation::DirectMcpToolDenied)
        );
    }
}
