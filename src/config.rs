use std::net::IpAddr;
use std::path::PathBuf;
use std::time::Duration;

use reqwest::Url;

use crate::approval::{ApprovalPolicy, DEFAULT_APPROVAL_TIMEOUT};
use crate::error::{AgentError, Result};
use crate::mcp::client::DEFAULT_MCP_PROTOCOL_VERSION;
use crate::mcp::process::DEFAULT_STDERR_CAPACITY_BYTES;
use crate::state_machine::TaskBudget;

/// Runtime configuration.
///
/// This milestone uses a deliberately small environment-variable source:
///
/// - `HARNESS_ROOT` (default: current directory)
/// - `HARNESS_LLM_BASE` (default: `http://127.0.0.1:8080`)
/// - `HARNESS_MODEL` (default: `local-model`)
/// - `HARNESS_JEV_BASE` (default: `http://127.0.0.1:9090`)
/// - `HARNESS_MIN_ROUTE_CONF` (default: `0.85`)
/// - `HARNESS_MAX_ACT_TURNS` (default: `40`)
/// - `HARNESS_MAX_TOOL_CALLS` (default: `120`)
/// - `HARNESS_MAX_VERIFY_RETRIES` (default: `3`)
/// - `HARNESS_MAX_SYSTEM2_CALLS` (default: `80`)
/// - `HARNESS_MAX_WALL_SECS` (default: `3600`)
/// - `HARNESS_MAX_TOOL_OUTPUT_BYTES` (default: `4000`)
/// - `HARNESS_APPROVAL_MUTATIONS` (default: `true`)
/// - `HARNESS_APPROVAL_RISKY_COMMANDS` (default: `false`)
/// - `HARNESS_APPROVAL_RISKY_PROGRAMS` (default: `git,cargo,npm,pip,python3`)
/// - `HARNESS_APPROVAL_TIMEOUT_SECS` (default: `120`)
///
/// In the default air-gapped build, both endpoint URLs must use plaintext HTTP
/// on a loopback host. The `remote` Cargo feature permits HTTPS and
/// non-loopback hosts. Enabling that feature does not configure, contact, or
/// upload data to any endpoint; callers must still supply an explicit URL.
///
/// Static MCP server configuration is read from the fixed `HARNESS_MCP_*`
/// variables documented in `README.md`. No server name, executable, argument,
/// working directory, or sandbox comes from model output.
#[derive(Debug, Clone)]
pub struct Config {
    pub root: PathBuf,
    pub llm_base: String,
    pub model: String,
    pub jev_base: String,
    pub min_route_conf: f32,
    pub budget: TaskBudget,
    pub approval: ApprovalPolicy,
    pub mcp: McpRuntimeConfig,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let root = std::env::var_os("HARNESS_ROOT")
            .map(PathBuf::from)
            .unwrap_or(std::env::current_dir()?);

        let budget = TaskBudget {
            max_act_turns: parse_env("HARNESS_MAX_ACT_TURNS", 40usize)?,
            max_tool_calls: parse_env("HARNESS_MAX_TOOL_CALLS", 120usize)?,
            max_verify_retries: parse_env("HARNESS_MAX_VERIFY_RETRIES", 3u8)?,
            max_system2_calls: parse_env("HARNESS_MAX_SYSTEM2_CALLS", 80usize)?,
            max_wall_time: Duration::from_secs(parse_env("HARNESS_MAX_WALL_SECS", 3_600u64)?),
            max_tool_output_bytes: parse_env("HARNESS_MAX_TOOL_OUTPUT_BYTES", 4_000usize)?,
        };

        let mcp = McpRuntimeConfig::from_env(&root)?;
        let approval = ApprovalPolicy {
            require_for_mutations: parse_env("HARNESS_APPROVAL_MUTATIONS", true)?,
            require_for_risky_commands: parse_env("HARNESS_APPROVAL_RISKY_COMMANDS", false)?,
            risky_command_programs: parse_program_list(
                "HARNESS_APPROVAL_RISKY_PROGRAMS",
                &ApprovalPolicy::default().risky_command_programs,
            )?,
            timeout: Duration::from_secs(parse_env(
                "HARNESS_APPROVAL_TIMEOUT_SECS",
                DEFAULT_APPROVAL_TIMEOUT.as_secs(),
            )?),
        };

        Ok(Self {
            root,
            llm_base: env_or("HARNESS_LLM_BASE", "http://127.0.0.1:8080"),
            model: env_or("HARNESS_MODEL", "local-model"),
            jev_base: env_or("HARNESS_JEV_BASE", "http://127.0.0.1:9090"),
            min_route_conf: parse_env("HARNESS_MIN_ROUTE_CONF", 0.85f32)?,
            budget,
            approval,
            mcp,
        })
    }

    /// Reject unsafe endpoint configuration and nonsensical budgets.
    pub fn validate(&self) -> Result<()> {
        if self.root.as_os_str().is_empty() {
            return Err(AgentError::Config(
                "repository root must not be empty".into(),
            ));
        }
        if self.model.trim().is_empty() {
            return Err(AgentError::Config("model name must not be empty".into()));
        }
        if !self.min_route_conf.is_finite() || !(0.0..=1.0).contains(&self.min_route_conf) {
            return Err(AgentError::Config(
                "min_route_conf must be a finite value in [0.0, 1.0]".into(),
            ));
        }

        validate_budget(&self.budget)?;
        validate_endpoint("llm_base", &self.llm_base)?;
        validate_endpoint("jev_base", &self.jev_base)?;
        self.approval.validate()?;
        self.mcp.validate()?;
        Ok(())
    }
}

/// Maximum accepted MCP process arguments.
pub const MAX_MCP_ARGS: usize = 64;
/// Maximum bytes in one MCP process argument or configuration string.
pub const MAX_MCP_CONFIG_STRING_BYTES: usize = 4_096;
/// Default bound for MCP server startup handshakes.
pub const DEFAULT_MCP_STARTUP_TIMEOUT: Duration = Duration::from_secs(30);
/// Maximum accepted MCP startup timeout.
pub const MAX_MCP_STARTUP_TIMEOUT: Duration = Duration::from_secs(300);
/// Default per-request timeout for an initialized MCP client.
pub const DEFAULT_MCP_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
/// Maximum accepted MCP request timeout.
pub const MAX_MCP_REQUEST_TIMEOUT: Duration = Duration::from_secs(3_600);
/// Maximum accepted MCP stderr capture capacity.
pub const MAX_MCP_STDERR_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum McpProfile {
    #[default]
    None,
    ReadOnly,
    WorkspaceMutation,
}

impl McpProfile {
    pub fn parse(raw: &str) -> Result<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "none" => Ok(Self::None),
            "read_only" | "read-only" => Ok(Self::ReadOnly),
            "workspace_mutation" | "workspace-mutation" => Ok(Self::WorkspaceMutation),
            other => Err(AgentError::Config(format!(
                "HARNESS_MCP_PROFILE must be one of none, read_only, workspace_mutation; got `{other}`"
            ))),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::ReadOnly => "read_only",
            Self::WorkspaceMutation => "workspace_mutation",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpServerKind {
    Filesystem,
    Git,
    Shell,
}

impl McpServerKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Filesystem => "filesystem",
            Self::Git => "git",
            Self::Shell => "shell",
        }
    }

    pub fn env_prefix(self) -> &'static str {
        match self {
            Self::Filesystem => "HARNESS_MCP_FS",
            Self::Git => "HARNESS_MCP_GIT",
            Self::Shell => "HARNESS_MCP_SHELL",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum McpSandboxConfig {
    /// Harness process hygiene only. This does not provide network isolation.
    #[default]
    None,
    /// Administrator-supplied executable wrapper and fixed argument prefix.
    ExternalWrapper {
        executable: PathBuf,
        args_prefix: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpServerConfig {
    pub enabled: bool,
    pub executable: PathBuf,
    pub args: Vec<String>,
    pub working_directory: PathBuf,
    pub protocol_version: String,
    pub startup_timeout: Duration,
    pub request_timeout: Duration,
    pub stderr_capacity_bytes: usize,
    pub sandbox: McpSandboxConfig,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct McpRuntimeConfig {
    pub profile: McpProfile,
    pub filesystem: Option<McpServerConfig>,
    pub git: Option<McpServerConfig>,
    pub shell: Option<McpServerConfig>,
    /// Explicit child PATH. When absent, a minimal platform default is used.
    pub path: Option<String>,
}

impl McpRuntimeConfig {
    pub fn from_env(root: &std::path::Path) -> Result<Self> {
        let profile = McpProfile::parse(&env_or("HARNESS_MCP_PROFILE", "none"))?;
        let path = match std::env::var("HARNESS_MCP_PATH") {
            Ok(value) => {
                validate_config_string("HARNESS_MCP_PATH", &value)?;
                Some(value)
            }
            Err(_) => None,
        };

        Ok(Self {
            profile,
            filesystem: parse_mcp_server(McpServerKind::Filesystem, root)?,
            git: parse_mcp_server(McpServerKind::Git, root)?,
            shell: parse_mcp_server(McpServerKind::Shell, root)?,
            path,
        })
    }

    pub fn server(&self, kind: McpServerKind) -> Option<&McpServerConfig> {
        match kind {
            McpServerKind::Filesystem => self.filesystem.as_ref(),
            McpServerKind::Git => self.git.as_ref(),
            McpServerKind::Shell => self.shell.as_ref(),
        }
    }

    pub fn enabled_servers(&self) -> Vec<(McpServerKind, &McpServerConfig)> {
        [
            McpServerKind::Filesystem,
            McpServerKind::Git,
            McpServerKind::Shell,
        ]
        .into_iter()
        .filter_map(|kind| {
            self.server(kind)
                .filter(|server| server.enabled)
                .map(|server| (kind, server))
        })
        .collect()
    }

    pub fn validate(&self) -> Result<()> {
        if let Some(path) = &self.path {
            validate_config_string("HARNESS_MCP_PATH", path)?;
        }

        let enabled = self.enabled_servers();
        if self.profile == McpProfile::None && !enabled.is_empty() {
            return Err(AgentError::Config(
                "profile `none` does not permit enabled MCP server configuration".into(),
            ));
        }

        for required in required_servers(self.profile) {
            let present = self.server(*required).is_some_and(|server| server.enabled);
            if !present {
                return Err(AgentError::Config(format!(
                    "MCP profile `{}` requires an enabled `{}` server",
                    self.profile.as_str(),
                    required.name()
                )));
            }
        }

        for (kind, server) in enabled {
            server.validate(kind)?;
        }
        Ok(())
    }
}

fn required_servers(profile: McpProfile) -> &'static [McpServerKind] {
    match profile {
        McpProfile::None => &[],
        McpProfile::ReadOnly => &[McpServerKind::Filesystem, McpServerKind::Git],
        McpProfile::WorkspaceMutation => &[
            McpServerKind::Filesystem,
            McpServerKind::Git,
            McpServerKind::Shell,
        ],
    }
}

impl McpServerConfig {
    fn validate(&self, kind: McpServerKind) -> Result<()> {
        let prefix = kind.env_prefix();
        if !self.enabled {
            return Ok(());
        }
        validate_executable(&format!("{prefix}_EXECUTABLE"), &self.executable)?;
        validate_args(&format!("{prefix}_ARGS"), &self.args)?;

        if self.working_directory.as_os_str().is_empty() {
            return Err(AgentError::Config(format!(
                "{} working_directory must not be empty",
                kind.name()
            )));
        }
        if !self.working_directory.is_dir() {
            return Err(AgentError::Config(format!(
                "{} working_directory is not an accessible directory: {}",
                kind.name(),
                self.working_directory.display()
            )));
        }

        validate_config_string(
            &format!("{prefix}_PROTOCOL_VERSION"),
            &self.protocol_version,
        )?;
        if self.protocol_version.trim().is_empty() {
            return Err(AgentError::Config(format!(
                "{} protocol_version must not be empty",
                kind.name()
            )));
        }
        if self.startup_timeout.is_zero() || self.startup_timeout > MAX_MCP_STARTUP_TIMEOUT {
            return Err(AgentError::Config(format!(
                "{} startup_timeout must be between 1 and {} seconds",
                kind.name(),
                MAX_MCP_STARTUP_TIMEOUT.as_secs()
            )));
        }
        if self.request_timeout.is_zero() || self.request_timeout > MAX_MCP_REQUEST_TIMEOUT {
            return Err(AgentError::Config(format!(
                "{} request_timeout must be between 1 and {} seconds",
                kind.name(),
                MAX_MCP_REQUEST_TIMEOUT.as_secs()
            )));
        }
        if self.stderr_capacity_bytes == 0 || self.stderr_capacity_bytes > MAX_MCP_STDERR_BYTES {
            return Err(AgentError::Config(format!(
                "{} stderr_capacity_bytes must be between 1 and {}",
                kind.name(),
                MAX_MCP_STDERR_BYTES
            )));
        }

        if let McpSandboxConfig::ExternalWrapper {
            executable,
            args_prefix,
        } = &self.sandbox
        {
            validate_executable(&format!("{prefix}_SANDBOX_EXECUTABLE"), executable)?;
            validate_args(&format!("{prefix}_SANDBOX_ARGS"), args_prefix)?;
        }
        Ok(())
    }
}

fn parse_mcp_server(
    kind: McpServerKind,
    root: &std::path::Path,
) -> Result<Option<McpServerConfig>> {
    let prefix = kind.env_prefix();
    let executable = match std::env::var(format!("{prefix}_EXECUTABLE")) {
        Ok(value) => {
            if value.is_empty() {
                return Err(AgentError::Config(format!(
                    "{prefix}_EXECUTABLE must not be empty"
                )));
            }
            validate_config_string(&format!("{prefix}_EXECUTABLE"), &value)?;
            Some(PathBuf::from(value))
        }
        Err(_) => None,
    };

    let enabled = match std::env::var(format!("{prefix}_ENABLED")) {
        Ok(raw) => parse_bool(&format!("{prefix}_ENABLED"), &raw)?,
        Err(_) => executable.is_some(),
    };

    let Some(executable) = executable else {
        if enabled {
            return Err(AgentError::Config(format!(
                "{prefix}_ENABLED is true but {prefix}_EXECUTABLE is not set"
            )));
        }
        return Ok(None);
    };

    let args = parse_string_array(
        &format!("{prefix}_ARGS"),
        std::env::var(format!("{prefix}_ARGS")).ok(),
    )?;
    let working_directory = std::env::var_os(format!("{prefix}_WORKDIR"))
        .map(PathBuf::from)
        .unwrap_or_else(|| root.to_path_buf());
    let protocol_version = std::env::var(format!("{prefix}_PROTOCOL_VERSION"))
        .unwrap_or_else(|_| DEFAULT_MCP_PROTOCOL_VERSION.to_owned());
    let startup_timeout = Duration::from_secs(parse_env(
        &format!("{prefix}_STARTUP_TIMEOUT_SECS"),
        DEFAULT_MCP_STARTUP_TIMEOUT.as_secs(),
    )?);
    let request_timeout = Duration::from_secs(parse_env(
        &format!("{prefix}_REQUEST_TIMEOUT_SECS"),
        DEFAULT_MCP_REQUEST_TIMEOUT.as_secs(),
    )?);
    let stderr_capacity_bytes = parse_env(
        &format!("{prefix}_STDERR_BYTES"),
        DEFAULT_STDERR_CAPACITY_BYTES,
    )?;

    let sandbox_mode =
        std::env::var(format!("{prefix}_SANDBOX_MODE")).unwrap_or_else(|_| "none".to_owned());
    let sandbox = match sandbox_mode.trim().to_ascii_lowercase().as_str() {
        "none" => McpSandboxConfig::None,
        "external_wrapper" | "external-wrapper" => {
            let executable = std::env::var(format!("{prefix}_SANDBOX_EXECUTABLE"))
                .map(PathBuf::from)
                .map_err(|_| {
                    AgentError::Config(format!(
                        "{prefix}_SANDBOX_EXECUTABLE is required for external-wrapper sandbox mode"
                    ))
                })?;
            let args_prefix = parse_string_array(
                &format!("{prefix}_SANDBOX_ARGS"),
                std::env::var(format!("{prefix}_SANDBOX_ARGS")).ok(),
            )?;
            McpSandboxConfig::ExternalWrapper {
                executable,
                args_prefix,
            }
        }
        other => {
            return Err(AgentError::Config(format!(
                "{prefix}_SANDBOX_MODE must be `none` or `external-wrapper`; got `{other}`"
            )));
        }
    };

    Ok(Some(McpServerConfig {
        enabled,
        executable,
        args,
        working_directory,
        protocol_version,
        startup_timeout,
        request_timeout,
        stderr_capacity_bytes,
        sandbox,
    }))
}

fn parse_string_array(label: &str, raw: Option<String>) -> Result<Vec<String>> {
    let Some(raw) = raw else {
        return Ok(Vec::new());
    };
    let values = serde_json::from_str::<Vec<String>>(&raw).map_err(|error| {
        AgentError::Config(format!("{label} must be a JSON string array: {error}"))
    })?;
    validate_args(label, &values)?;
    Ok(values)
}

fn parse_bool(label: &str, raw: &str) -> Result<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Ok(true),
        "0" | "false" | "no" | "off" => Ok(false),
        _ => Err(AgentError::Config(format!("{label} must be true or false"))),
    }
}

fn validate_executable(label: &str, path: &std::path::Path) -> Result<()> {
    if path.as_os_str().is_empty() {
        return Err(AgentError::Config(format!("{label} must not be empty")));
    }
    if !path.is_absolute() {
        return Err(AgentError::Config(format!(
            "{label} must be an absolute path: {}",
            path.display()
        )));
    }
    let metadata = std::fs::metadata(path).map_err(|error| {
        AgentError::Config(format!(
            "{label} is not an accessible file ({}): {error}",
            path.display()
        ))
    })?;
    if !metadata.is_file() {
        return Err(AgentError::Config(format!(
            "{label} must refer to a regular file: {}",
            path.display()
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            return Err(AgentError::Config(format!(
                "{label} is not executable: {}",
                path.display()
            )));
        }
    }
    Ok(())
}

fn validate_args(label: &str, args: &[String]) -> Result<()> {
    if args.len() > MAX_MCP_ARGS {
        return Err(AgentError::Config(format!(
            "{label} contains {} arguments; maximum is {MAX_MCP_ARGS}",
            args.len()
        )));
    }
    for (index, arg) in args.iter().enumerate() {
        validate_config_string(&format!("{label}[{index}]"), arg)?;
    }
    Ok(())
}

fn validate_config_string(label: &str, value: &str) -> Result<()> {
    if value.contains('\0') {
        return Err(AgentError::Config(format!("{label} must not contain NUL")));
    }
    if value.len() > MAX_MCP_CONFIG_STRING_BYTES {
        return Err(AgentError::Config(format!(
            "{label} is {} bytes; maximum is {MAX_MCP_CONFIG_STRING_BYTES}",
            value.len()
        )));
    }
    Ok(())
}

fn parse_program_list(
    name: &str,
    default: &std::collections::BTreeSet<String>,
) -> Result<std::collections::BTreeSet<String>> {
    match std::env::var(name) {
        Ok(raw) => {
            let programs = raw
                .split(',')
                .map(str::trim)
                .filter(|program| !program.is_empty())
                .map(str::to_owned)
                .collect::<std::collections::BTreeSet<_>>();
            Ok(programs)
        }
        Err(_) => Ok(default.clone()),
    }
}

fn env_or(name: &str, default: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| default.to_owned())
}

fn parse_env<T>(name: &str, default: T) -> Result<T>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    match std::env::var(name) {
        Ok(raw) => raw
            .parse::<T>()
            .map_err(|error| AgentError::Config(format!("{name} is invalid: {error}"))),
        Err(_) => Ok(default),
    }
}

fn validate_budget(budget: &TaskBudget) -> Result<()> {
    let invalid = [
        (
            budget.max_act_turns == 0,
            "max_act_turns must be greater than zero",
        ),
        (
            budget.max_tool_calls == 0,
            "max_tool_calls must be greater than zero",
        ),
        (
            budget.max_verify_retries == 0,
            "max_verify_retries must be greater than zero",
        ),
        (
            budget.max_system2_calls == 0,
            "max_system2_calls must be greater than zero",
        ),
        (
            budget.max_wall_time.is_zero(),
            "max_wall_time must be greater than zero",
        ),
        (
            budget.max_tool_output_bytes == 0,
            "max_tool_output_bytes must be greater than zero",
        ),
    ];

    for (is_invalid, message) in invalid {
        if is_invalid {
            return Err(AgentError::Config(message.into()));
        }
    }
    Ok(())
}

/// Redact an endpoint for logs.
///
/// Only the scheme, host, and explicit port are retained. User information,
/// paths, query strings, and fragments are discarded so API keys or tokens
/// embedded in a URL cannot leak into logs or traces.
pub fn log_safe_endpoint(raw: &str) -> String {
    let Ok(url) = Url::parse(raw) else {
        return "<invalid-endpoint>".into();
    };
    let Some(host) = url.host_str() else {
        return "<invalid-endpoint>".into();
    };

    let mut safe = format!("{}://{host}", url.scheme());
    if let Some(port) = url.port() {
        safe.push(':');
        safe.push_str(&port.to_string());
    }
    safe
}

pub(crate) fn validate_endpoint(label: &str, raw: &str) -> Result<()> {
    let url = Url::parse(raw)
        .map_err(|error| AgentError::Config(format!("{label} is not a valid URL: {error}")))?;

    if !url.username().is_empty() || url.password().is_some() {
        return Err(AgentError::Config(format!(
            "{label} must not embed credentials"
        )));
    }

    let remote = cfg!(feature = "remote");
    let scheme_ok = url.scheme() == "http" || (remote && url.scheme() == "https");
    if !scheme_ok {
        return Err(AgentError::Config(format!(
            "{label} must use http:// in the air-gapped build"
        )));
    }

    if !remote && !is_loopback_url(&url) {
        return Err(AgentError::Config(format!(
            "{label} must target a loopback host in the air-gapped build (got {})",
            log_safe_endpoint(raw)
        )));
    }

    Ok(())
}

fn is_loopback_url(url: &Url) -> bool {
    let Some(host) = url.host_str() else {
        return false;
    };
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }

    let unbracketed = host
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
        .unwrap_or(host);

    unbracketed
        .parse::<IpAddr>()
        .map(|address| address.is_loopback())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_with(llm_base: &str, jev_base: &str) -> Config {
        Config {
            root: PathBuf::from("."),
            llm_base: llm_base.to_owned(),
            model: "local-model".into(),
            jev_base: jev_base.to_owned(),
            min_route_conf: 0.85,
            budget: TaskBudget::default(),
            approval: ApprovalPolicy::default(),
            mcp: McpRuntimeConfig::default(),
        }
    }

    #[test]
    fn accepts_loopback_endpoint_variants() {
        let loopback = [
            "http://127.0.0.1:8080",
            "http://127.0.0.1",
            "http://127.0.0.2:9090/v1",
            "http://localhost:8080",
            "http://LOCALHOST:8080",
            "http://[::1]:8080",
            "http://[::1]",
        ];

        for endpoint in loopback {
            let config = config_with(endpoint, endpoint);
            assert!(
                config.validate().is_ok(),
                "{endpoint} should be accepted as loopback"
            );
        }
    }

    #[cfg(not(feature = "remote"))]
    #[test]
    fn rejects_non_loopback_endpoints_in_air_gapped_build() {
        let non_loopback = [
            "http://example.com",
            "http://192.168.1.20:8080",
            "http://localhost.example.com:9090",
            "https://api.example.com",
        ];

        for endpoint in non_loopback {
            let config = config_with(endpoint, "http://127.0.0.1:9090");
            assert!(
                config.validate().is_err(),
                "LLM endpoint {endpoint} should be rejected"
            );

            let config = config_with("http://127.0.0.1:8080", endpoint);
            assert!(
                config.validate().is_err(),
                "Jev endpoint {endpoint} should be rejected"
            );
        }
    }

    #[cfg(not(feature = "remote"))]
    #[test]
    fn rejects_https_in_air_gapped_build_even_on_loopback() {
        let config = config_with("https://127.0.0.1:8080", "http://127.0.0.1:9090");
        assert!(config.validate().is_err());
    }

    #[test]
    fn rejects_invalid_urls_and_embedded_credentials() {
        for endpoint in [
            "not a url",
            "ftp://127.0.0.1:8080",
            "http://user:password@127.0.0.1:8080",
        ] {
            let config = config_with(endpoint, "http://127.0.0.1:9090");
            assert!(config.validate().is_err(), "{endpoint} should be rejected");
        }
    }

    #[test]
    fn rejects_invalid_approval_timeout() {
        let mut config = config_with("http://127.0.0.1:8080", "http://127.0.0.1:9090");
        config.approval.timeout = std::time::Duration::ZERO;
        assert!(config.validate().is_err());

        config.approval.timeout = crate::approval::MAX_APPROVAL_TIMEOUT + Duration::from_secs(1);
        assert!(config.validate().is_err());
    }

    #[test]
    fn rejects_invalid_route_confidence_and_empty_budget_limits() {
        let mut config = config_with("http://127.0.0.1:8080", "http://127.0.0.1:9090");
        config.min_route_conf = f32::NAN;
        assert!(config.validate().is_err());

        let mut config = config_with("http://127.0.0.1:8080", "http://127.0.0.1:9090");
        config.min_route_conf = 1.5;
        assert!(config.validate().is_err());

        let mut config = config_with("http://127.0.0.1:8080", "http://127.0.0.1:9090");
        config.budget.max_tool_calls = 0;
        assert!(config.validate().is_err());
    }

    #[test]
    fn endpoint_logging_redacts_credentials_paths_and_queries() {
        assert_eq!(
            log_safe_endpoint("https://user:password@api.example.com:8443/v1?token=secret#frag"),
            "https://api.example.com:8443"
        );
        assert_eq!(
            log_safe_endpoint("http://127.0.0.1:8080/v1/chat?api_key=secret"),
            "http://127.0.0.1:8080"
        );
        assert_eq!(
            log_safe_endpoint("http://[::1]:9090/v1"),
            "http://[::1]:9090"
        );
        assert_eq!(log_safe_endpoint("not a url"), "<invalid-endpoint>");
    }

    #[cfg(feature = "remote")]
    #[test]
    fn remote_feature_allows_non_loopback_endpoints() {
        let config = config_with("https://api.example.com", "http://127.0.0.1:9090");
        assert!(config.validate().is_ok());
    }

    fn server(executable: PathBuf) -> McpServerConfig {
        McpServerConfig {
            enabled: true,
            executable,
            args: Vec::new(),
            working_directory: PathBuf::from("."),
            protocol_version: DEFAULT_MCP_PROTOCOL_VERSION.into(),
            startup_timeout: DEFAULT_MCP_STARTUP_TIMEOUT,
            request_timeout: DEFAULT_MCP_REQUEST_TIMEOUT,
            stderr_capacity_bytes: DEFAULT_STDERR_CAPACITY_BYTES,
            sandbox: McpSandboxConfig::None,
        }
    }

    fn test_executable() -> PathBuf {
        std::env::current_exe().expect("test executable path")
    }

    fn mcp_config(profile: McpProfile) -> McpRuntimeConfig {
        McpRuntimeConfig {
            profile,
            filesystem: Some(server(test_executable())),
            git: Some(server(test_executable())),
            shell: Some(server(test_executable())),
            path: None,
        }
    }

    #[test]
    fn mcp_profiles_require_fixed_servers() {
        assert!(McpRuntimeConfig::default().validate().is_ok());

        let mut config = mcp_config(McpProfile::ReadOnly);
        config.git = None;
        assert!(config.validate().is_err());

        assert!(mcp_config(McpProfile::ReadOnly).validate().is_ok());

        let mut config = mcp_config(McpProfile::WorkspaceMutation);
        config.shell = None;
        assert!(config.validate().is_err());

        assert!(mcp_config(McpProfile::WorkspaceMutation).validate().is_ok());
    }

    #[test]
    fn mcp_none_profile_rejects_enabled_servers() {
        let config = McpRuntimeConfig {
            filesystem: Some(server(test_executable())),
            ..McpRuntimeConfig::default()
        };
        assert!(config.validate().is_err());
    }

    #[test]
    fn mcp_rejects_relative_missing_and_non_file_executables() {
        let kind = McpServerKind::Filesystem;
        let mut relative = server(PathBuf::from("mock-server"));
        assert!(relative.validate(kind).is_err());

        let missing = server(std::env::temp_dir().join("definitely-missing-mcp-server"));
        assert!(missing.validate(kind).is_err());

        let directory = server(std::env::temp_dir());
        assert!(directory.validate(kind).is_err());

        relative.enabled = false;
        assert!(relative.validate(kind).is_ok());
    }

    #[test]
    fn mcp_rejects_nul_and_oversized_arguments() {
        let mut value = server(test_executable());
        value.args = vec!["ok".into(), "bad\0arg".into()];
        assert!(value.validate(McpServerKind::Filesystem).is_err());

        let mut value = server(test_executable());
        value.args = vec!["x".repeat(MAX_MCP_CONFIG_STRING_BYTES + 1)];
        assert!(value.validate(McpServerKind::Filesystem).is_err());

        let mut value = server(test_executable());
        value.args = vec!["ok".into(); MAX_MCP_ARGS + 1];
        assert!(value.validate(McpServerKind::Filesystem).is_err());
    }

    #[test]
    fn mcp_validates_external_wrapper_and_bounds() {
        let mut value = server(test_executable());
        value.sandbox = McpSandboxConfig::ExternalWrapper {
            executable: PathBuf::from("relative-wrapper"),
            args_prefix: Vec::new(),
        };
        assert!(value.validate(McpServerKind::Filesystem).is_err());

        value.sandbox = McpSandboxConfig::ExternalWrapper {
            executable: test_executable(),
            args_prefix: vec!["--net=none".into()],
        };
        assert!(value.validate(McpServerKind::Filesystem).is_ok());

        value.startup_timeout = Duration::ZERO;
        assert!(value.validate(McpServerKind::Filesystem).is_err());
        value.startup_timeout = DEFAULT_MCP_STARTUP_TIMEOUT;
        value.stderr_capacity_bytes = MAX_MCP_STDERR_BYTES + 1;
        assert!(value.validate(McpServerKind::Filesystem).is_err());
    }
}
