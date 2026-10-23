//! Bounded approval metadata and the non-interactive approval gate boundary.

use std::collections::BTreeSet;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::oneshot;

use crate::error::{AgentError, Result};
use crate::mcp::AgentTool;
use crate::wiki::redact::redact;

/// Default maximum time to wait for an interactive approval decision.
pub const DEFAULT_APPROVAL_TIMEOUT: Duration = Duration::from_secs(120);
/// Maximum accepted approval timeout.
pub const MAX_APPROVAL_TIMEOUT: Duration = Duration::from_secs(3_600);
/// Default programs treated as risky when command approval is enabled.
pub const DEFAULT_RISKY_PROGRAMS: &[&str] = &["git", "cargo", "npm", "pip", "python3"];

const MAX_APPROVAL_PATH_BYTES: usize = 256;
const MAX_APPROVAL_PROGRAM_BYTES: usize = 64;
const MAX_APPROVAL_ARGUMENTS_BYTES: usize = 512;
const MAX_APPROVAL_REASON_BYTES: usize = 256;
const MAX_APPROVAL_SUMMARY_BYTES: usize = 1_024;

/// Administrator-configured policy for human approval gates.
///
/// Read-only filesystem and Git actions never require approval. Approval can
/// only run after `Policy` permits the action and cannot widen capabilities.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalPolicy {
    pub require_for_mutations: bool,
    pub require_for_risky_commands: bool,
    pub risky_command_programs: BTreeSet<String>,
    pub timeout: Duration,
}

impl Default for ApprovalPolicy {
    fn default() -> Self {
        Self {
            require_for_mutations: true,
            require_for_risky_commands: false,
            risky_command_programs: DEFAULT_RISKY_PROGRAMS
                .iter()
                .map(|program| (*program).to_owned())
                .collect(),
            timeout: DEFAULT_APPROVAL_TIMEOUT,
        }
    }
}

impl ApprovalPolicy {
    pub fn validate(&self) -> Result<()> {
        if self.timeout.is_zero() || self.timeout > MAX_APPROVAL_TIMEOUT {
            return Err(AgentError::Config(format!(
                "approval timeout must be between 1 and {} seconds",
                MAX_APPROVAL_TIMEOUT.as_secs()
            )));
        }
        for program in &self.risky_command_programs {
            if program.is_empty()
                || program.len() > MAX_APPROVAL_PROGRAM_BYTES
                || program.contains('/')
                || program.contains('\\')
                || program.contains('\0')
            {
                return Err(AgentError::Config(
                    "risky command programs must be bare executable names".into(),
                ));
            }
        }
        Ok(())
    }

    /// Return the human-readable reason this tool needs approval, if any.
    pub fn requirement_for(&self, tool: &AgentTool) -> Option<String> {
        match tool {
            AgentTool::WriteFile { .. } | AgentTool::ApplyPatch { .. }
                if self.require_for_mutations =>
            {
                Some("workspace mutation requires approval".into())
            }
            AgentTool::RunCommand { program, .. }
                if self.require_for_risky_commands
                    && self.risky_command_programs.contains(program) =>
            {
                Some("command program is configured as requiring approval".into())
            }
            _ => None,
        }
    }
}

/// Canonical action class shown to the approver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalAction {
    WriteFile,
    ApplyPatch,
    RunCommand,
}

impl ApprovalAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::WriteFile => "write_file",
            Self::ApplyPatch => "apply_patch",
            Self::RunCommand => "run_command",
        }
    }
}

/// Bounded, redacted metadata for exactly one pending action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalRequest {
    pub approval_id: String,
    pub action: ApprovalAction,
    pub target: Option<String>,
    pub program: Option<String>,
    pub arguments: Option<String>,
    pub reason: String,
}

impl ApprovalRequest {
    pub fn for_tool(tool: &AgentTool, reason: impl Into<String>) -> Option<Self> {
        let (action, target, program, arguments) = match tool {
            AgentTool::WriteFile { path, .. } => (
                ApprovalAction::WriteFile,
                Some(bound(path, MAX_APPROVAL_PATH_BYTES)),
                None,
                None,
            ),
            AgentTool::ApplyPatch { path, .. } => (
                ApprovalAction::ApplyPatch,
                Some(bound(path, MAX_APPROVAL_PATH_BYTES)),
                None,
                None,
            ),
            AgentTool::RunCommand { program, args, .. } => (
                ApprovalAction::RunCommand,
                None,
                Some(bound(program, MAX_APPROVAL_PROGRAM_BYTES)),
                Some(bound(&args.join(" "), MAX_APPROVAL_ARGUMENTS_BYTES)),
            ),
            _ => return None,
        };

        Some(Self {
            approval_id: format!("approval-{}", uuid::Uuid::new_v4()),
            action,
            target,
            program,
            arguments,
            reason: bound(&reason.into(), MAX_APPROVAL_REASON_BYTES),
        })
    }

    /// Bounded one-line description safe for events and rendering.
    pub fn summary(&self) -> String {
        let action = match self.action {
            ApprovalAction::WriteFile => format!(
                "write file {}",
                self.target.as_deref().unwrap_or("<unknown>")
            ),
            ApprovalAction::ApplyPatch => format!(
                "apply patch to {}",
                self.target.as_deref().unwrap_or("<unknown>")
            ),
            ApprovalAction::RunCommand => {
                let program = self.program.as_deref().unwrap_or("<unknown>");
                match self.arguments.as_deref() {
                    Some(arguments) if !arguments.is_empty() => {
                        format!("run {program} {arguments}")
                    }
                    _ => format!("run {program}"),
                }
            }
        };
        bound(
            &format!("{action} — {}", self.reason),
            MAX_APPROVAL_SUMMARY_BYTES,
        )
    }
}

/// Result of waiting for an approval decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalDecision {
    Approved,
    Denied,
    Expired,
    Unavailable,
}

/// A future that resolves when one approval request has a decision.
pub type ApprovalFuture = Pin<Box<dyn Future<Output = ApprovalDecision> + Send + 'static>>;

/// Headless-safe approval boundary.
///
/// The default implementation fails closed. Implementations must not grant
/// approval based on model output, and the orchestrator applies the configured
/// timeout and cancellation around this future.
pub trait ApprovalGate: Send + Sync {
    fn request(&self, request: &ApprovalRequest) -> ApprovalFuture;

    /// Remove a pending request after timeout or cancellation.
    fn cancel(&self, _approval_id: &str) {}
}

/// Approval gate used when no interactive client exists.
#[derive(Debug, Clone, Copy, Default)]
pub struct DenyApprovalGate;

impl ApprovalGate for DenyApprovalGate {
    fn request(&self, _request: &ApprovalRequest) -> ApprovalFuture {
        Box::pin(async { ApprovalDecision::Unavailable })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalResolveError {
    NoPending,
    Mismatched,
}

#[derive(Debug)]
struct PendingApproval {
    approval_id: String,
    decision: oneshot::Sender<bool>,
}

/// TUI/worker approval broker. Only the exact displayed request ID can resolve
/// the one pending action.
#[derive(Debug, Clone, Default)]
pub struct InteractiveApprovalGate {
    pending: Arc<Mutex<Option<PendingApproval>>>,
}

impl InteractiveApprovalGate {
    pub fn new() -> Self {
        Self::default()
    }

    /// Resolve the pending request only when the IDs match exactly.
    pub fn resolve(
        &self,
        approval_id: &str,
        approved: bool,
    ) -> std::result::Result<(), ApprovalResolveError> {
        let mut slot = self
            .pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(pending) = slot.as_ref() else {
            return Err(ApprovalResolveError::NoPending);
        };
        if pending.approval_id != approval_id {
            return Err(ApprovalResolveError::Mismatched);
        }
        let pending = slot.take().expect("checked pending request");
        let _ = pending.decision.send(approved);
        Ok(())
    }
}

impl ApprovalGate for InteractiveApprovalGate {
    fn request(&self, request: &ApprovalRequest) -> ApprovalFuture {
        let (tx, rx) = oneshot::channel();
        {
            let mut slot = self
                .pending
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if slot.is_some() {
                return Box::pin(async { ApprovalDecision::Unavailable });
            }
            *slot = Some(PendingApproval {
                approval_id: request.approval_id.clone(),
                decision: tx,
            });
        }

        Box::pin(async move {
            match rx.await {
                Ok(true) => ApprovalDecision::Approved,
                Ok(false) => ApprovalDecision::Denied,
                Err(_) => ApprovalDecision::Unavailable,
            }
        })
    }

    fn cancel(&self, approval_id: &str) {
        let mut slot = self
            .pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if slot
            .as_ref()
            .is_some_and(|pending| pending.approval_id == approval_id)
        {
            slot.take();
        }
    }
}

fn bound(text: &str, cap: usize) -> String {
    let redacted = redact(text).unwrap_or_else(|_| "[redaction unavailable]".to_owned());
    let single_line = redacted.replace(['\r', '\n', '\t'], " ");
    truncate_utf8(single_line.trim(), cap)
}

fn truncate_utf8(text: &str, cap: usize) -> String {
    if text.len() <= cap {
        return text.to_owned();
    }
    let mut end = cap;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...[truncated]", &text[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(id: &str) -> ApprovalRequest {
        ApprovalRequest {
            approval_id: id.into(),
            action: ApprovalAction::WriteFile,
            target: Some("src/main.rs".into()),
            program: None,
            arguments: None,
            reason: "workspace mutation requires approval".into(),
        }
    }

    #[test]
    fn mutation_and_configured_command_policy_are_independent() {
        let policy = ApprovalPolicy {
            require_for_mutations: true,
            require_for_risky_commands: false,
            ..ApprovalPolicy::default()
        };
        let write = AgentTool::WriteFile {
            path: "src/main.rs".into(),
            content: "secret".into(),
            expected_sha256: None,
        };
        let command = AgentTool::RunCommand {
            program: "cargo".into(),
            args: vec!["test".into()],
            cwd: None,
            timeout_secs: None,
        };
        assert!(policy.requirement_for(&write).is_some());
        assert!(policy.requirement_for(&command).is_none());

        let command_policy = ApprovalPolicy {
            require_for_mutations: false,
            require_for_risky_commands: true,
            ..ApprovalPolicy::default()
        };
        assert!(command_policy.requirement_for(&write).is_none());
        assert!(command_policy.requirement_for(&command).is_some());
    }

    #[test]
    fn request_metadata_excludes_payload_content_and_redacts_arguments() {
        let write = AgentTool::ApplyPatch {
            path: "src/main.rs".into(),
            unified_diff: "RAW_PATCH_SECRET".into(),
            expected_sha256: None,
        };
        let request =
            ApprovalRequest::for_tool(&write, "workspace mutation requires approval").unwrap();
        assert_eq!(request.action, ApprovalAction::ApplyPatch);
        assert_eq!(request.target.as_deref(), Some("src/main.rs"));
        assert!(!request.summary().contains("RAW_PATCH_SECRET"));
        assert!(!format!("{request:?}").contains("RAW_PATCH_SECRET"));

        let command = AgentTool::RunCommand {
            program: "cargo".into(),
            args: vec!["token=supersecret".into()],
            cwd: None,
            timeout_secs: None,
        };
        let request = ApprovalRequest::for_tool(&command, "risky command").unwrap();
        assert!(
            !request
                .arguments
                .as_deref()
                .unwrap()
                .contains("supersecret")
        );
        assert!(request.arguments.as_deref().unwrap().contains("[REDACTED]"));
    }

    #[tokio::test]
    async fn only_the_exact_pending_id_can_resolve() {
        let gate = InteractiveApprovalGate::new();
        let request = request("approval-1");
        let wait = gate.request(&request);

        assert_eq!(
            gate.resolve("approval-2", true),
            Err(ApprovalResolveError::Mismatched)
        );
        assert_eq!(gate.resolve("approval-1", false), Ok(()));
        assert_eq!(wait.await, ApprovalDecision::Denied);
        assert_eq!(
            gate.resolve("approval-1", true),
            Err(ApprovalResolveError::NoPending)
        );
    }

    #[tokio::test]
    async fn cancellation_removes_pending_request() {
        let gate = InteractiveApprovalGate::new();
        let request = request("approval-cancel");
        let wait = gate.request(&request);
        gate.cancel("approval-cancel");
        assert_eq!(wait.await, ApprovalDecision::Unavailable);
        assert_eq!(
            gate.resolve("approval-cancel", true),
            Err(ApprovalResolveError::NoPending)
        );
    }

    #[test]
    fn risky_program_configuration_rejects_paths() {
        let policy = ApprovalPolicy {
            risky_command_programs: ["./cargo".to_owned()].into_iter().collect(),
            ..ApprovalPolicy::default()
        };
        assert!(policy.validate().is_err());
    }
}
