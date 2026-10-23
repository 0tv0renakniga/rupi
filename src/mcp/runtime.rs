use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use crate::config::{
    McpProfile, McpRuntimeConfig, McpSandboxConfig, McpServerConfig, McpServerKind,
};
use crate::error::{AgentError, Result};
use crate::mcp::capability::{CapabilityManifest, McpPool, ToolDispatcher};
use crate::mcp::client::{McpClient, McpClientOptions};
use crate::mcp::process::ProcessSpec;
use crate::policy::Policy;

const STARTUP_CLEANUP_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_DIAGNOSTIC_BYTES: usize = 2_048;
static HOME_SEQUENCE: AtomicU64 = AtomicU64::new(1);

/// Connected, fixed-slot MCP runtime.
///
/// Startup is all-or-nothing: if any enabled server fails configuration,
/// spawning, initialization, or capability validation, every process started
/// by this runtime is terminated before an error is returned.
pub struct McpRuntime {
    dispatcher: Arc<ToolDispatcher>,
    profile: McpProfile,
    started_servers: Vec<McpServerKind>,
    warnings: Vec<String>,
}

impl McpRuntime {
    pub async fn start(
        config: &McpRuntimeConfig,
        repository_root: &Path,
        policy: Policy,
        max_tool_output_bytes: usize,
    ) -> Result<Self> {
        config.validate()?;

        let capabilities = CapabilityManifest::default();
        let configured = config
            .enabled_servers()
            .into_iter()
            .map(|(kind, _)| kind)
            .collect::<Vec<_>>();
        let configured_allowed = allowed_capabilities(config.profile, &configured);
        let mut pool = McpPool::new();
        let mut started_servers = Vec::new();

        for (kind, server) in config.enabled_servers() {
            let spec = process_spec(config, kind, server)?;
            tracing::info!(
                server = kind.name(),
                executable = %safe_basename(&spec.program),
                sandbox = %sandbox_label(&server.sandbox),
                "starting MCP server"
            );

            let options = McpClientOptions {
                protocol_version: server.protocol_version.clone(),
                startup_timeout: server.startup_timeout,
                request_timeout: server.request_timeout,
                stderr_capacity_bytes: server.stderr_capacity_bytes,
            };

            let mut client = match McpClient::connect(kind.name(), &spec, options).await {
                Ok(client) => client,
                Err(error) => {
                    let _ = pool.shutdown(STARTUP_CLEANUP_TIMEOUT).await;
                    return Err(AgentError::Mcp(format!(
                        "MCP server `{}` failed to start: {}",
                        kind.name(),
                        sanitize_diagnostic(&error.to_string())
                    )));
                }
            };

            if let Err(error) = capabilities.validate_client_for(&client, &configured_allowed) {
                let stderr = client.stderr_snapshot();
                let _ = client.shutdown(STARTUP_CLEANUP_TIMEOUT).await;
                let _ = pool.shutdown(STARTUP_CLEANUP_TIMEOUT).await;
                let mut message = format!(
                    "MCP server `{}` failed fixed-capability validation: {error}",
                    kind.name()
                );
                if !stderr.trim().is_empty() {
                    message.push_str("\nserver stderr (bounded): ");
                    message.push_str(&sanitize_diagnostic(&stderr));
                }
                return Err(AgentError::Mcp(message));
            }

            pool.insert(kind.name(), client);
            started_servers.push(kind);
            tracing::info!(server = kind.name(), "MCP server ready");
        }

        let allowed = allowed_capabilities(config.profile, &started_servers);
        let dispatcher = Arc::new(
            ToolDispatcher::new(policy, capabilities, pool)
                .with_tool_output_limit(max_tool_output_bytes)
                .with_allowed_capabilities(allowed),
        );
        let warnings = startup_warnings(config, repository_root, &started_servers);

        Ok(Self {
            dispatcher,
            profile: config.profile,
            started_servers,
            warnings,
        })
    }

    pub fn dispatcher(&self) -> Arc<ToolDispatcher> {
        Arc::clone(&self.dispatcher)
    }

    pub fn profile(&self) -> McpProfile {
        self.profile
    }

    pub fn started_servers(&self) -> &[McpServerKind] {
        &self.started_servers
    }

    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// Terminate and reap every owned MCP server within `timeout` per server.
    pub async fn shutdown(self, timeout: Duration) -> Result<()> {
        let dispatcher = Arc::try_unwrap(self.dispatcher).map_err(|_| {
            AgentError::Mcp(
                "cannot shut down MCP runtime while its tool dispatcher is still in use".into(),
            )
        })?;
        let mut dispatcher = dispatcher;
        dispatcher.shutdown(timeout).await
    }
}

fn process_spec(
    config: &McpRuntimeConfig,
    kind: McpServerKind,
    server: &McpServerConfig,
) -> Result<ProcessSpec> {
    let env = child_environment(config, kind)?;
    let (program, args) = match &server.sandbox {
        McpSandboxConfig::None => (server.executable.clone(), server.args.clone()),
        McpSandboxConfig::ExternalWrapper {
            executable,
            args_prefix,
        } => {
            let server_program = server.executable.to_str().ok_or_else(|| {
                AgentError::Config(
                    "MCP server executable must be valid UTF-8 for external-wrapper mode".into(),
                )
            })?;
            let mut args = args_prefix.clone();
            args.push(server_program.to_owned());
            args.extend(server.args.iter().cloned());
            (executable.clone(), args)
        }
        McpSandboxConfig::Bubblewrap { executable } => {
            let mut args = bubblewrap_args(config, server, &env)?;
            args.push("--".into());
            args.push(path_utf8(&server.executable, "MCP server executable")?);
            args.extend(server.args.iter().cloned());
            (executable.clone(), args)
        }
    };

    Ok(ProcessSpec {
        program,
        args,
        cwd: Some(server.working_directory.clone()),
        env,
    })
}

fn bubblewrap_args(
    config: &McpRuntimeConfig,
    server: &McpServerConfig,
    env: &BTreeMap<String, String>,
) -> Result<Vec<String>> {
    if !cfg!(target_os = "linux") {
        return Err(AgentError::Config(
            "bubblewrap sandbox mode is supported only on Linux".into(),
        ));
    }

    let home = env
        .get("HOME")
        .ok_or_else(|| AgentError::Mcp("bubblewrap mode requires a controlled HOME".into()))?;
    let path = env
        .get("PATH")
        .ok_or_else(|| AgentError::Mcp("bubblewrap mode requires a controlled PATH".into()))?;
    let cwd = std::fs::canonicalize(&server.working_directory).map_err(|error| {
        AgentError::Config(format!(
            "MCP working directory cannot be canonicalized for bubblewrap: {}: {error}",
            server.working_directory.display()
        ))
    })?;
    let home = std::fs::canonicalize(home)
        .map_err(|error| AgentError::Config(format!("controlled MCP HOME is invalid: {error}")))?;
    let cwd_text = path_utf8(&cwd, "MCP working directory")?;
    let home_text = path_utf8(&home, "controlled MCP HOME")?;

    let mut args = vec![
        "--die-with-parent".into(),
        "--new-session".into(),
        "--unshare-net".into(),
        "--unshare-pid".into(),
        "--unshare-ipc".into(),
        "--unshare-uts".into(),
        "--clearenv".into(),
        "--proc".into(),
        "/proc".into(),
        "--dev".into(),
        "/dev".into(),
        "--tmpfs".into(),
        "/tmp".into(),
    ];

    for system_path in ["/usr", "/bin", "/sbin", "/lib", "/lib64"] {
        let source = Path::new(system_path);
        if source.exists() {
            args.push("--ro-bind".into());
            args.push(system_path.into());
            args.push(system_path.into());
        }
    }

    let server_program = std::fs::canonicalize(&server.executable).map_err(|error| {
        AgentError::Config(format!(
            "MCP server executable cannot be canonicalized for bubblewrap: {}: {error}",
            server.executable.display()
        ))
    })?;
    let under_system_path = ["/usr", "/bin", "/sbin", "/lib", "/lib64"]
        .iter()
        .any(|prefix| server_program.starts_with(prefix));
    if !under_system_path {
        let program = path_utf8(&server_program, "MCP server executable")?;
        args.push("--ro-bind".into());
        args.push(program.clone());
        args.push(program);
    }

    args.push("--dir".into());
    args.push(home_text.clone());
    args.push("--bind".into());
    args.push(home_text.clone());
    args.push(home_text.clone());

    if !["/usr", "/bin", "/sbin", "/lib", "/lib64"]
        .iter()
        .any(|prefix| cwd.starts_with(prefix))
    {
        args.push("--dir".into());
        args.push(cwd_text.clone());
    }
    args.push(if config.profile == McpProfile::WorkspaceMutation {
        "--bind".into()
    } else {
        "--ro-bind".into()
    });
    args.push(cwd_text.clone());
    args.push(cwd_text.clone());

    args.extend([
        "--chdir".into(),
        cwd_text,
        "--setenv".into(),
        "HOME".into(),
        home_text,
        "--setenv".into(),
        "PATH".into(),
        path.clone(),
    ]);

    Ok(args)
}

fn path_utf8(path: &Path, label: &str) -> Result<String> {
    path.to_str().map(str::to_owned).ok_or_else(|| {
        AgentError::Config(format!("{label} must be valid UTF-8 for bubblewrap mode"))
    })
}

fn child_environment(
    config: &McpRuntimeConfig,
    kind: McpServerKind,
) -> Result<BTreeMap<String, String>> {
    let home = std::env::temp_dir().join(format!(
        "local-agent-harness-mcp-{}-{}-{}",
        std::process::id(),
        kind.name(),
        HOME_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&home).map_err(|error| {
        AgentError::Mcp(format!(
            "failed to create MCP-controlled HOME directory for `{}`: {error}",
            kind.name()
        ))
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700));
    }

    let mut env = BTreeMap::new();
    env.insert("HOME".into(), home.to_string_lossy().into_owned());
    #[cfg(windows)]
    env.insert("USERPROFILE".into(), home.to_string_lossy().into_owned());
    env.insert(
        "PATH".into(),
        config.path.clone().unwrap_or_else(default_child_path),
    );
    Ok(env)
}

fn default_child_path() -> String {
    #[cfg(windows)]
    {
        "C:\\Windows\\System32;C:\\Windows".into()
    }
    #[cfg(not(windows))]
    {
        "/usr/local/bin:/usr/bin:/bin".into()
    }
}

fn allowed_capabilities(profile: McpProfile, started: &[McpServerKind]) -> Vec<&'static str> {
    let has = |kind: McpServerKind| started.contains(&kind);
    match profile {
        McpProfile::None => Vec::new(),
        McpProfile::ReadOnly => {
            let mut allowed = Vec::new();
            if has(McpServerKind::Filesystem) {
                allowed.push("read_file");
            }
            if has(McpServerKind::Git) {
                allowed.extend(["git_status", "git_diff"]);
            }
            if has(McpServerKind::Shell) {
                allowed.push("run_command");
            }
            allowed
        }
        McpProfile::WorkspaceMutation => {
            let mut allowed = vec![
                "read_file",
                "write_file",
                "apply_patch",
                "git_status",
                "git_diff",
            ];
            if has(McpServerKind::Shell) {
                allowed.push("run_command");
            }
            allowed
        }
    }
}

fn startup_warnings(
    config: &McpRuntimeConfig,
    repository_root: &Path,
    started: &[McpServerKind],
) -> Vec<String> {
    let mut warnings = Vec::new();
    if started.is_empty() {
        warnings
            .push("MCP profile `none`: tool capabilities are unavailable for this session.".into());
        return warnings;
    }

    let enabled = config.enabled_servers();
    let unsandboxed = enabled
        .iter()
        .filter(|(_, server)| matches!(server.sandbox, McpSandboxConfig::None))
        .count();
    let bubblewrap = enabled
        .iter()
        .filter(|(_, server)| matches!(server.sandbox, McpSandboxConfig::Bubblewrap { .. }))
        .count();
    let external = enabled
        .iter()
        .filter(|(_, server)| matches!(server.sandbox, McpSandboxConfig::ExternalWrapper { .. }))
        .count();
    if unsandboxed > 0 {
        warnings.push(format!(
            "MCP profile `{}` starts {} of {} server(s) without an OS network sandbox; network isolation is not provided for those servers.",
            config.profile.as_str(),
            unsandboxed,
            started.len()
        ));
    }
    if bubblewrap > 0 {
        warnings.push(format!(
            "MCP profile `{}` runs {} server(s) inside harness-managed bubblewrap network namespaces.",
            config.profile.as_str(),
            bubblewrap
        ));
    }
    if external > 0 {
        warnings.push(format!(
            "MCP profile `{}` delegates isolation for {} server(s) to an external sandbox wrapper; the harness cannot verify that wrapper's network policy.",
            config.profile.as_str(),
            external
        ));
    }

    warnings.push(format!(
        "MCP servers: {}. Repository root: {}.",
        started
            .iter()
            .map(|kind| kind.name())
            .collect::<Vec<_>>()
            .join(", "),
        repository_root.display()
    ));
    warnings
}

fn safe_basename(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "<unknown>".into())
}

fn sandbox_label(sandbox: &McpSandboxConfig) -> String {
    match sandbox {
        McpSandboxConfig::None => "none (no network isolation)".into(),
        McpSandboxConfig::ExternalWrapper { executable, .. } => {
            format!("external-wrapper:{}", safe_basename(executable))
        }
        McpSandboxConfig::Bubblewrap { executable } => {
            format!("bubblewrap:{}", safe_basename(executable))
        }
    }
}

fn sanitize_diagnostic(text: &str) -> String {
    let redacted = crate::wiki::redact::redact(text).unwrap_or_else(|_| text.to_owned());
    if redacted.len() <= MAX_DIAGNOSTIC_BYTES {
        return redacted.trim().to_owned();
    }
    let mut end = MAX_DIAGNOSTIC_BYTES;
    while !redacted.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...[truncated]", &redacted[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_only_allows_only_read_routes() {
        let allowed = allowed_capabilities(
            McpProfile::ReadOnly,
            &[
                McpServerKind::Filesystem,
                McpServerKind::Git,
                McpServerKind::Shell,
            ],
        );
        assert!(allowed.contains(&"read_file"));
        assert!(!allowed.contains(&"write_file"));
        assert!(!allowed.contains(&"apply_patch"));
        assert!(allowed.contains(&"git_status"));
        assert!(allowed.contains(&"git_diff"));
        assert!(allowed.contains(&"run_command"));
    }

    #[test]
    fn diagnostics_are_bounded_and_redacted() {
        let input = format!(
            "token=SUPER_SECRET {}",
            "x".repeat(MAX_DIAGNOSTIC_BYTES * 2)
        );
        let output = sanitize_diagnostic(&input);
        assert!(!output.contains("SUPER_SECRET"));
        assert!(output.contains("[REDACTED]"));
        assert!(output.len() < MAX_DIAGNOSTIC_BYTES + 32);
    }

    #[test]
    fn none_profile_has_no_allowed_capabilities() {
        assert!(allowed_capabilities(McpProfile::None, &[]).is_empty());
    }

    #[test]
    fn child_environment_is_allowlisted() {
        let env =
            child_environment(&McpRuntimeConfig::default(), McpServerKind::Filesystem).unwrap();
        let keys = env.keys().map(String::as_str).collect::<Vec<_>>();
        if cfg!(windows) {
            assert_eq!(keys, ["HOME", "PATH", "USERPROFILE"]);
        } else {
            assert_eq!(keys, ["HOME", "PATH"]);
        }
        assert!(Path::new(env.get("HOME").unwrap()).is_dir());
        assert_eq!(env.get("PATH").unwrap(), &default_child_path());
    }
}
