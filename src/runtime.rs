//! Shared composition for the active fixed-script execution substrate.
//!
//! The TUI and headless preflight use this module so there is one runtime
//! dependency path: policy plus audit plus the canonical helper-script runner
//! injected into the orchestrator as [`ToolExecutor`]. The frozen MCP adapter
//! is not constructed here and is never an automatic fallback.

use std::path::PathBuf;
use std::sync::Arc;

use crate::audit::AuditLog;
use crate::config::Config;
use crate::error::{AgentError, Result};
use crate::orchestrator::ToolExecutor;
use crate::policy::Policy;
use crate::script_runner::{ActionId, ScriptRunner};
use crate::script_tools::ScriptToolExecutor;

/// Actions resolved from the installed script inventory at startup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptCapabilities {
    pub available: Vec<ActionId>,
    pub missing: Vec<ActionId>,
}

impl ScriptCapabilities {
    pub fn read_only_ready(&self) -> bool {
        [ActionId::ReadFile, ActionId::GitStatus, ActionId::GitDiff]
            .into_iter()
            .all(|action| self.available.contains(&action))
    }

    pub fn change_ready(&self) -> bool {
        self.read_only_ready()
            && [ActionId::WriteFile, ActionId::RunVerifier]
                .into_iter()
                .all(|action| self.available.contains(&action))
    }

    pub fn missing_file_names(&self) -> Vec<&'static str> {
        self.missing
            .iter()
            .map(|action| action.script_file_name())
            .collect()
    }
}

/// Active script-substrate dependencies for one process.
#[derive(Clone)]
pub struct ScriptRuntime {
    pub policy: Policy,
    pub runner: ScriptRunner,
    pub audit: Arc<AuditLog>,
    pub executor: Arc<dyn ToolExecutor>,
    pub capabilities: ScriptCapabilities,
}

/// Read-only composition summary used by `--headless`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptPreflight {
    pub project_root: PathBuf,
    pub script_root: PathBuf,
    pub interpreter: PathBuf,
    pub audit_path: PathBuf,
    pub mappings: Vec<(ActionId, PathBuf)>,
    pub capabilities: ScriptCapabilities,
}

/// Build the default active execution profile.
///
/// The read-only action set is mandatory because Investigate is the first
/// supported workflow. A missing verifier script leaves Change gated rather
/// than selecting another backend.
pub fn build(config: &Config) -> Result<ScriptRuntime> {
    let policy = Policy::new(config.root.clone()).map_err(|error| {
        AgentError::Config(format!("repository path policy is invalid: {error}"))
    })?;
    let audit = Arc::new(AuditLog::new(config.audit_path.clone()));
    audit.prepare()?;
    let runner = ScriptRunner::new_with_timeout(
        &config.root,
        &config.scripts.root,
        &config.scripts.interpreter,
        config.scripts.timeout,
    )?;
    let capabilities = detect_capabilities(&runner)?;
    if !capabilities.read_only_ready() {
        return Err(AgentError::Config(format!(
            "read-only script profile is incomplete; missing {}",
            capabilities.missing_file_names().join(", ")
        )));
    }
    let executor = Arc::new(ScriptToolExecutor::with_audit(
        runner.clone(),
        policy.clone(),
        Some(audit.clone()),
    )?);
    Ok(ScriptRuntime {
        policy,
        runner,
        audit,
        executor,
        capabilities,
    })
}

/// Build the active profile and return the canonical headless summary.
pub fn preflight(config: &Config) -> Result<ScriptPreflight> {
    let runtime = build(config)?;
    let mut mappings = Vec::new();
    for action in &runtime.capabilities.available {
        let invocation = runtime.runner.invocation(*action)?;
        mappings.push((*action, invocation.script().to_path_buf()));
    }
    Ok(ScriptPreflight {
        project_root: runtime.runner.project_root().to_path_buf(),
        script_root: runtime.runner.script_root().to_path_buf(),
        interpreter: runtime.runner.interpreter().to_path_buf(),
        audit_path: runtime.audit.path().to_path_buf(),
        mappings,
        capabilities: runtime.capabilities,
    })
}

fn detect_capabilities(runner: &ScriptRunner) -> Result<ScriptCapabilities> {
    let mut available = Vec::new();
    let mut missing = Vec::new();
    for action in ActionId::ALL {
        match runner.invocation(action) {
            Ok(_) => available.push(action),
            Err(_) => missing.push(action),
        }
    }
    Ok(ScriptCapabilities { available, missing })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::script_runner::DEFAULT_SCRIPT_TIMEOUT;
    use std::time::Duration;
    use uuid::Uuid;

    fn fixture() -> (PathBuf, PathBuf, PathBuf) {
        let base = std::env::temp_dir().join(format!("rupi-runtime-{}", Uuid::new_v4()));
        let project = base.join("project");
        let scripts = base.join("scripts");
        std::fs::create_dir_all(&project).expect("create project");
        std::fs::create_dir_all(&scripts).expect("create scripts");
        for file in ["read_file.py", "git_status.py", "git_diff.py"] {
            std::fs::write(scripts.join(file), "#!/usr/bin/env python3\n").expect("write script");
        }
        (base, project, scripts)
    }

    fn python() -> Option<PathBuf> {
        ["/usr/bin/python3", "/usr/local/bin/python3", "/bin/python3"]
            .into_iter()
            .map(PathBuf::from)
            .find(|path| path.is_file())
    }

    #[test]
    fn detects_read_only_and_gates_change_until_verifier_exists() {
        let (base, project, scripts) = fixture();
        let runner = ScriptRunner::new_with_timeout(
            &project,
            &scripts,
            python().expect("python"),
            DEFAULT_SCRIPT_TIMEOUT,
        )
        .expect("runner");
        let capabilities = detect_capabilities(&runner).expect("capabilities");
        assert!(capabilities.read_only_ready());
        assert!(!capabilities.change_ready());
        assert_eq!(
            capabilities.missing,
            vec![ActionId::WriteFile, ActionId::RunVerifier]
        );

        std::fs::write(scripts.join("write_file.py"), "#!/usr/bin/env python3\n")
            .expect("write script");
        std::fs::write(scripts.join("run_verifier.py"), "#!/usr/bin/env python3\n")
            .expect("verifier script");
        let capabilities = detect_capabilities(&runner).expect("capabilities");
        assert!(capabilities.change_ready());
        assert_eq!(capabilities.missing, Vec::new());

        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn script_config_defaults_are_bounded() {
        let config = crate::config::ScriptConfig::default();
        assert_eq!(config.timeout, Duration::from_secs(60));
    }
}
