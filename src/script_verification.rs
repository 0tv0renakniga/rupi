//! Script-backed deterministic verification.
//!
//! Detection and report construction remain in Rust, so the model cannot choose
//! verifier profiles or commands. Each detected argv vector is executed by the
//! fixed `run_verifier.py` helper through the bounded script runner.

use std::path::Path;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Map, Value};

use crate::agent::verify::{
    self, CommandOutput, VerificationCommand, VerificationExecutor, VerifyReport,
};
use crate::error::{AgentError, Result};
use crate::orchestrator::VerificationEngine;
use crate::script_runner::{ActionId, MAX_SCRIPT_TIMEOUT, ScriptOutcome, ScriptRunner};

const VERIFIER_TIMEOUT_GRACE: Duration = Duration::from_secs(30);
const MAX_VERIFIER_TIMEOUT_MS: u64 = 900_000;
const MAX_VERIFIER_OUTPUT_BYTES: usize = 64 * 1024;

/// Verification engine that keeps only the fixed helper script as the command
/// execution boundary.
#[derive(Debug, Clone)]
pub struct ScriptVerificationEngine {
    runner: ScriptRunner,
}

impl ScriptVerificationEngine {
    pub fn new(runner: ScriptRunner) -> Self {
        Self { runner }
    }

    fn ensure_root(&self, root: &Path) -> Result<()> {
        let canonical = root.canonicalize().map_err(|error| {
            AgentError::Blocked(format!("repository root is not accessible: {error}"))
        })?;
        if canonical != self.runner.project_root() {
            return Err(AgentError::Blocked(
                "verification root does not match the fixed script runtime root".into(),
            ));
        }
        Ok(())
    }
}

#[async_trait]
impl VerificationEngine for ScriptVerificationEngine {
    async fn detect(&self, root: &Path) -> Result<Vec<VerificationCommand>> {
        self.ensure_root(root)?;
        verify::detect(root)
    }

    async fn run(
        &self,
        root: &Path,
        commands: &[VerificationCommand],
        output_cap: usize,
    ) -> Result<VerifyReport> {
        self.ensure_root(root)?;
        let executor = ScriptVerificationExecutor {
            runner: self.runner.clone(),
        };
        verify::run_with_executor(root, commands, output_cap, &executor).await
    }
}

struct ScriptVerificationExecutor {
    runner: ScriptRunner,
}

#[async_trait]
impl VerificationExecutor for ScriptVerificationExecutor {
    async fn execute(
        &self,
        command: &VerificationCommand,
        output_cap: usize,
    ) -> Result<CommandOutput> {
        let cwd = command
            .cwd
            .strip_prefix(self.runner.project_root())
            .map_err(|_| {
                AgentError::Blocked("verification cwd is outside the script runtime root".into())
            })?;
        let cwd = if cwd.as_os_str().is_empty() {
            ".".to_owned()
        } else {
            cwd.to_str()
                .ok_or_else(|| AgentError::Blocked("verification cwd is not valid UTF-8".into()))?
                .to_owned()
        };

        let timeout_ms = u64::try_from(command.timeout.as_millis())
            .unwrap_or(u64::MAX)
            .clamp(1, MAX_VERIFIER_TIMEOUT_MS);
        let outer_timeout = command
            .timeout
            .saturating_add(VERIFIER_TIMEOUT_GRACE)
            .min(MAX_SCRIPT_TIMEOUT);
        let runner = self.runner.clone().with_timeout(outer_timeout)?;
        let params = Map::from_iter([
            ("program".into(), Value::String(command.program.clone())),
            (
                "args".into(),
                Value::Array(
                    command
                        .args
                        .iter()
                        .map(|arg| Value::String(arg.clone()))
                        .collect(),
                ),
            ),
            ("cwd".into(), Value::String(cwd)),
            ("timeout_ms".into(), Value::from(timeout_ms)),
            (
                "output_cap".into(),
                Value::from(
                    u64::try_from(output_cap.min(MAX_VERIFIER_OUTPUT_BYTES))
                        .unwrap_or(MAX_VERIFIER_OUTPUT_BYTES as u64),
                ),
            ),
        ]);

        let outcome = runner
            .execute_with_params(ActionId::RunVerifier, params)
            .await?;
        if !outcome.is_ok() {
            return Err(AgentError::Blocked(outcome_failure(&outcome)));
        }
        let data = outcome.data().ok_or_else(|| {
            AgentError::Blocked("verifier helper succeeded without result data".into())
        })?;

        let exit_code = data
            .get("exit_code")
            .and_then(Value::as_i64)
            .and_then(|value| i32::try_from(value).ok())
            .ok_or_else(|| malformed("exit_code"))?;
        let timed_out = data
            .get("timed_out")
            .and_then(Value::as_bool)
            .ok_or_else(|| malformed("timed_out"))?;
        let duration_ms = data
            .get("duration_ms")
            .and_then(Value::as_u64)
            .ok_or_else(|| malformed("duration_ms"))?;
        let stdout = string_field(data, "stdout")?;
        let stderr = string_field(data, "stderr")?;

        Ok(CommandOutput {
            exit_code,
            timed_out,
            duration: Duration::from_millis(duration_ms),
            stdout: stdout.to_owned(),
            stderr: stderr.to_owned(),
        })
    }
}

fn string_field<'a>(data: &'a Value, key: &str) -> Result<&'a str> {
    data.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| malformed(key))
}

fn malformed(key: &str) -> AgentError {
    AgentError::Blocked(format!("verifier helper returned malformed field `{key}`"))
}

fn outcome_failure(outcome: &ScriptOutcome) -> String {
    match outcome.error() {
        Some(error) => format!(
            "verifier helper failed [{}]: {}",
            error.code(),
            error.message()
        ),
        None => format!(
            "verifier helper failed [{}] without structured detail",
            outcome.result_code()
        ),
    }
}
