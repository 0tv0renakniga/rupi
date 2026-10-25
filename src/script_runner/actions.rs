use serde::{Deserialize, Serialize};

use crate::error::{AgentError, Result};

/// Fixed internal action identifiers available to the script substrate.
///
/// The model may request one of these identifiers, but it cannot provide a
/// script path, executable path, interpreter, working directory, or shell
/// command. New actions require a reviewed code change and a corresponding
/// harness-owned helper script.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionId {
    ReadFile,
    GitStatus,
    GitDiff,
    WriteFile,
    RunVerifier,
}

impl ActionId {
    /// Complete initial action catalog in stable order.
    pub const ALL: [Self; 5] = [
        Self::ReadFile,
        Self::GitStatus,
        Self::GitDiff,
        Self::WriteFile,
        Self::RunVerifier,
    ];

    /// Stable protocol and audit identifier.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ReadFile => "read_file",
            Self::GitStatus => "git_status",
            Self::GitDiff => "git_diff",
            Self::WriteFile => "write_file",
            Self::RunVerifier => "run_verifier",
        }
    }

    /// Fixed helper-script file name owned by the harness.
    ///
    /// This is a bare file name by construction. The runner joins it to the
    /// canonical harness script directory and canonicalizes the result before
    /// launch.
    pub const fn script_file_name(self) -> &'static str {
        match self {
            Self::ReadFile => "read_file.py",
            Self::GitStatus => "git_status.py",
            Self::GitDiff => "git_diff.py",
            Self::WriteFile => "write_file.py",
            Self::RunVerifier => "run_verifier.py",
        }
    }

    /// Parse only the exact fixed identifiers.
    ///
    /// Paths, aliases, shell fragments, and all other model-controlled strings
    /// are rejected rather than interpreted.
    pub fn from_identifier(raw: &str) -> Result<Self> {
        match raw {
            "read_file" => Ok(Self::ReadFile),
            "git_status" => Ok(Self::GitStatus),
            "git_diff" => Ok(Self::GitDiff),
            "write_file" => Ok(Self::WriteFile),
            "run_verifier" => Ok(Self::RunVerifier),
            _ => Err(AgentError::Script(
                "unknown script action identifier".to_owned(),
            )),
        }
    }
}

impl std::fmt::Display for ActionId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn action_catalog_is_exact_and_ordered() {
        let identifiers = ActionId::ALL.map(ActionId::as_str);
        assert_eq!(
            identifiers,
            [
                "read_file",
                "git_status",
                "git_diff",
                "write_file",
                "run_verifier"
            ]
        );

        for action in ActionId::ALL {
            assert_eq!(
                ActionId::from_identifier(action.as_str()).expect("known action"),
                action
            );
        }
    }

    #[test]
    fn rejects_unknown_and_model_controlled_identifiers() {
        for raw in [
            "",
            "ReadFile",
            "read_file.py",
            "scripts/read_file.py",
            "/bin/sh",
            "../read_file",
            "run_command",
            "run_shell",
            "apply_patch",
            "read_file\0",
            "read_file ",
        ] {
            assert!(
                ActionId::from_identifier(raw).is_err(),
                "`{raw}` must not resolve to an action"
            );
        }
    }
}
