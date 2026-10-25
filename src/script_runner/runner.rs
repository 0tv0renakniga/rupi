use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::time::{Duration, Instant};

use serde_json::Map;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout, Command};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::audit::AuditSink;
use crate::error::{AgentError, Result};

use super::actions::ActionId;
use super::protocol::{
    MAX_REQUEST_BYTES, MAX_RESPONSE_BYTES, MAX_STDERR_BYTES, ProtocolErrorCode, ScriptOutcome,
    ScriptRequest, ScriptResponse, sanitize_diagnostic,
};

/// Default hard timeout for one helper-script execution.
pub const DEFAULT_SCRIPT_TIMEOUT: Duration = Duration::from_secs(60);

/// Maximum accepted helper-script timeout.
pub const MAX_SCRIPT_TIMEOUT: Duration = Duration::from_secs(3_600);

/// Fixed PATH provided to helper scripts. The interpreter itself is launched by
/// absolute path, so this only supports reviewed script dependencies.
#[cfg(windows)]
pub const SCRIPT_ENV_PATH: &str = "C:\\Windows\\System32;C:\\Windows";
#[cfg(not(windows))]
pub const SCRIPT_ENV_PATH: &str = "/usr/local/bin:/usr/bin:/bin";

const SHELL_INTERPRETER_NAMES: &[&str] = &[
    "sh",
    "bash",
    "zsh",
    "dash",
    "ksh",
    "ksh93",
    "csh",
    "tcsh",
    "fish",
    "cmd",
    "powershell",
    "pwsh",
];

enum Completion {
    Finished(Result<ExitStatus>),
    Stopped(StopReason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StopReason {
    Timeout,
    Cancelled,
}

/// Fully resolved, harness-owned launch description for one action.
///
/// Every field is derived from fixed action mapping or validated constructor
/// input. None of these values can be supplied by model output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptInvocation {
    action: ActionId,
    interpreter: PathBuf,
    script: PathBuf,
    working_directory: PathBuf,
}

impl ScriptInvocation {
    pub fn action(&self) -> ActionId {
        self.action
    }

    pub fn interpreter(&self) -> &Path {
        &self.interpreter
    }

    pub fn script(&self) -> &Path {
        &self.script
    }

    pub fn working_directory(&self) -> &Path {
        &self.working_directory
    }
}

/// Rust-owned dispatcher for the fixed helper-script inventory.
///
/// The runner requires a canonical project root, a canonical harness-owned
/// script directory outside that root, and an absolute interpreter outside that
/// root. Model output never participates in any of those choices.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptRunner {
    project_root: PathBuf,
    script_root: PathBuf,
    interpreter: PathBuf,
    timeout: Duration,
}

impl ScriptRunner {
    pub fn new(
        project_root: impl AsRef<Path>,
        script_root: impl AsRef<Path>,
        interpreter: impl AsRef<Path>,
    ) -> Result<Self> {
        Self::new_with_timeout(
            project_root,
            script_root,
            interpreter,
            DEFAULT_SCRIPT_TIMEOUT,
        )
    }

    pub fn new_with_timeout(
        project_root: impl AsRef<Path>,
        script_root: impl AsRef<Path>,
        interpreter: impl AsRef<Path>,
        timeout: Duration,
    ) -> Result<Self> {
        validate_timeout(timeout)?;
        let project_root = canonical_directory("project root", project_root.as_ref())?;
        let script_root = canonical_directory("script root", script_root.as_ref())?;

        if script_root.starts_with(&project_root) {
            return Err(AgentError::Script(
                "harness script root must be outside the configured project root".to_owned(),
            ));
        }

        let interpreter = canonical_interpreter(interpreter.as_ref(), &project_root)?;

        Ok(Self {
            project_root,
            script_root,
            interpreter,
            timeout,
        })
    }

    /// Replace the timeout after construction while preserving validation.
    pub fn with_timeout(mut self, timeout: Duration) -> Result<Self> {
        validate_timeout(timeout)?;
        self.timeout = timeout;
        Ok(self)
    }

    pub fn project_root(&self) -> &Path {
        &self.project_root
    }

    pub fn script_root(&self) -> &Path {
        &self.script_root
    }

    pub fn interpreter(&self) -> &Path {
        &self.interpreter
    }

    pub fn timeout(&self) -> Duration {
        self.timeout
    }

    /// Resolve one fixed action to its canonical interpreter, script, and
    /// working directory without spawning anything.
    pub fn invocation(&self, action: ActionId) -> Result<ScriptInvocation> {
        let script = self.resolve_script(action)?;
        Ok(ScriptInvocation {
            action,
            interpreter: self.interpreter.clone(),
            script,
            working_directory: self.project_root.clone(),
        })
    }

    /// Send one bounded request to the fixed helper script for `action` and
    /// return a bounded structured outcome.
    ///
    /// Request serialization, stdout, and stderr are bounded. Stdout is parsed
    /// as exactly one protocol response; stderr is diagnostic only and is
    /// redacted before it can reach a UI or model context. The runner applies a
    /// hard timeout and terminates the child if the timeout expires.
    pub async fn execute(&self, action: ActionId) -> Result<ScriptOutcome> {
        self.execute_with_params(action, Map::new()).await
    }

    /// Execute a fixed action with action-specific parameters.
    ///
    /// Parameters are harness-owned data. They are nested under `params` so
    /// they cannot overwrite the protocol version, action, or project root.
    pub async fn execute_with_params(
        &self,
        action: ActionId,
        params: Map<String, serde_json::Value>,
    ) -> Result<ScriptOutcome> {
        let cancel = CancellationToken::new();
        self.execute_with_cancel(action, params, &cancel).await
    }

    /// Execute a fixed action with an external cancellation token.
    ///
    /// Cancellation and timeout produce a structured failure outcome and
    /// explicitly terminate and reap the child process. A cancelled or
    /// timed-out action can never report success.
    pub async fn execute_with_cancel(
        &self,
        action: ActionId,
        params: Map<String, serde_json::Value>,
        cancel: &CancellationToken,
    ) -> Result<ScriptOutcome> {
        let started = Instant::now();
        let mut outcome = self.execute_inner(action, params, cancel).await?;
        outcome.set_duration(started.elapsed());
        Ok(outcome)
    }

    /// Execute a fixed action and append one bounded audit record when a sink
    /// is supplied.
    pub async fn execute_for_task(
        &self,
        task_id: Uuid,
        action: ActionId,
        params: Map<String, serde_json::Value>,
        cancel: &CancellationToken,
        audit: Option<&dyn AuditSink>,
    ) -> Result<ScriptOutcome> {
        let outcome = self.execute_with_cancel(action, params, cancel).await?;
        if let Some(sink) = audit {
            sink.record(&outcome.audit_record(task_id)).await?;
        }
        Ok(outcome)
    }

    async fn execute_inner(
        &self,
        action: ActionId,
        params: Map<String, serde_json::Value>,
        cancel: &CancellationToken,
    ) -> Result<ScriptOutcome> {
        if cancel.is_cancelled() {
            return Ok(ScriptOutcome::protocol_failure(
                action,
                None,
                ProtocolErrorCode::ScriptCancelled,
                None,
                None,
            ));
        }

        let request = ScriptRequest::new(action, &self.project_root, params)?;
        let request_bytes = request.to_json_bytes()?;
        if request_bytes.len() > MAX_REQUEST_BYTES {
            return Ok(ScriptOutcome::protocol_failure(
                action,
                None,
                ProtocolErrorCode::RequestTooLarge,
                None,
                None,
            ));
        }

        let mut running = self.launch(action).await?;
        let script = Some(running.invocation().script().to_path_buf());
        let mut stdin = running.take_stdin()?;
        let stdout = running.take_stdout()?;
        let stderr = running.take_stderr()?;

        let write_task = tokio::spawn(async move {
            stdin.write_all(&request_bytes).await?;
            stdin.shutdown().await
        });
        let stdout_task = tokio::spawn(read_bounded(stdout, MAX_RESPONSE_BYTES));
        let stderr_task = tokio::spawn(read_bounded(stderr, MAX_STDERR_BYTES));

        let completion = tokio::select! {
            status = running.wait() => Completion::Finished(status),
            _ = cancel.cancelled() => Completion::Stopped(StopReason::Cancelled),
            _ = tokio::time::sleep(self.timeout) => Completion::Stopped(StopReason::Timeout),
        };

        let status = match completion {
            Completion::Finished(Ok(status)) => status,
            Completion::Finished(Err(_)) => {
                write_task.abort();
                stdout_task.abort();
                stderr_task.abort();
                let termination = running.kill().await;
                let code = if termination.is_ok() {
                    ProtocolErrorCode::ChildWaitFailed
                } else {
                    ProtocolErrorCode::ChildTerminationFailed
                };
                return Ok(ScriptOutcome::protocol_failure(
                    action, script, code, None, None,
                ));
            }
            Completion::Stopped(reason) => {
                write_task.abort();
                stdout_task.abort();
                stderr_task.abort();
                let termination = running.kill().await;
                let code = if termination.is_err() {
                    ProtocolErrorCode::ChildTerminationFailed
                } else {
                    match reason {
                        StopReason::Timeout => ProtocolErrorCode::ScriptTimeout,
                        StopReason::Cancelled => ProtocolErrorCode::ScriptCancelled,
                    }
                };
                return Ok(ScriptOutcome::protocol_failure(
                    action, script, code, None, None,
                ));
            }
        };

        let write_result = match write_task.await {
            Ok(result) => result,
            Err(_) => {
                return Ok(ScriptOutcome::protocol_failure(
                    action,
                    script,
                    ProtocolErrorCode::RequestWriteFailed,
                    None,
                    None,
                ));
            }
        };
        let stdout_result = match stdout_task.await {
            Ok(result) => result,
            Err(_) => {
                return Ok(ScriptOutcome::protocol_failure(
                    action,
                    script,
                    ProtocolErrorCode::StreamReadFailed,
                    None,
                    None,
                ));
            }
        };
        let stderr_result = match stderr_task.await {
            Ok(result) => result,
            Err(_) => {
                return Ok(ScriptOutcome::protocol_failure(
                    action,
                    script,
                    ProtocolErrorCode::StreamReadFailed,
                    None,
                    None,
                ));
            }
        };

        let diagnostic = stderr_result
            .as_ref()
            .ok()
            .and_then(|captured| sanitize_diagnostic(&captured.bytes, captured.truncated));

        if write_result.is_err() {
            return Ok(ScriptOutcome::protocol_failure(
                action,
                script,
                ProtocolErrorCode::RequestWriteFailed,
                diagnostic,
                None,
            ));
        }
        if stderr_result.is_err() {
            return Ok(ScriptOutcome::protocol_failure(
                action,
                script,
                ProtocolErrorCode::StreamReadFailed,
                diagnostic,
                None,
            ));
        }
        let stdout = match stdout_result {
            Ok(captured) => captured,
            Err(_) => {
                return Ok(ScriptOutcome::protocol_failure(
                    action,
                    script,
                    ProtocolErrorCode::StreamReadFailed,
                    diagnostic,
                    None,
                ));
            }
        };
        let exit_code = status.code();

        if stdout.truncated {
            return Ok(ScriptOutcome::protocol_failure(
                action,
                script,
                ProtocolErrorCode::ResponseTooLarge,
                diagnostic,
                exit_code,
            ));
        }
        if !status.success() {
            return Ok(ScriptOutcome::protocol_failure(
                action,
                script,
                ProtocolErrorCode::ScriptExitNonzero,
                diagnostic,
                exit_code,
            ));
        }

        let response = match ScriptResponse::parse_stdout(&stdout.bytes) {
            Ok(response) => response,
            Err(code) => {
                return Ok(ScriptOutcome::protocol_failure(
                    action, script, code, diagnostic, exit_code,
                ));
            }
        };

        if response.ok {
            let Some(data) = response.data else {
                return Ok(ScriptOutcome::protocol_failure(
                    action,
                    script,
                    ProtocolErrorCode::InvalidProtocolResponse,
                    diagnostic,
                    exit_code,
                ));
            };
            Ok(ScriptOutcome::success(
                action, script, data, diagnostic, exit_code,
            ))
        } else {
            let Some(error) = response.error else {
                return Ok(ScriptOutcome::protocol_failure(
                    action,
                    script,
                    ProtocolErrorCode::InvalidProtocolResponse,
                    diagnostic,
                    exit_code,
                ));
            };
            Ok(ScriptOutcome::helper_failure(
                action, script, error, diagnostic, exit_code,
            ))
        }
    }

    /// Launch the fixed helper script for `action` with argv only.
    ///
    /// The child receives exactly one argument: the canonical script path. It
    /// is not run through a shell, and no model-controlled executable, script,
    /// working directory, or inline code can be supplied. The child receives a
    /// minimal allowlisted environment and a harness-owned temporary HOME,
    /// never the ambient harness environment.
    pub async fn launch(&self, action: ActionId) -> Result<RunningScript> {
        let invocation = self.invocation(action)?;
        let (environment, home) = script_environment(&self.project_root)?;

        let mut command = Command::new(invocation.interpreter());
        command
            .arg(invocation.script())
            .current_dir(invocation.working_directory())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .env_clear();
        for (key, value) in &environment {
            command.env(key, value);
        }

        let child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                return Err(AgentError::Script(format!(
                    "failed to launch helper script for action `{}`: {error}",
                    action.as_str()
                )));
            }
        };

        Ok(RunningScript {
            child,
            invocation,
            _home: Some(home),
        })
    }

    fn resolve_script(&self, action: ActionId) -> Result<PathBuf> {
        let candidate = self.script_root.join(action.script_file_name());
        let script = candidate.canonicalize().map_err(|error| {
            AgentError::Script(format!(
                "helper script for action `{}` is unavailable: {error}",
                action.as_str()
            ))
        })?;

        if !script.starts_with(&self.script_root) {
            return Err(AgentError::Script(format!(
                "helper script for action `{}` resolves outside the harness script root",
                action.as_str()
            )));
        }

        let metadata = std::fs::metadata(&script).map_err(|error| {
            AgentError::Script(format!(
                "helper script for action `{}` is not accessible: {error}",
                action.as_str()
            ))
        })?;
        if !metadata.is_file() {
            return Err(AgentError::Script(format!(
                "helper script for action `{}` is not a regular file",
                action.as_str()
            )));
        }

        Ok(script)
    }
}

/// A launched helper script.
///
/// The child is configured with kill-on-drop so abandonment, panic, timeout,
/// and cancellation cannot leave an orphaned helper process. Dropping this
/// value therefore terminates the child and removes its temporary HOME.
#[derive(Debug)]
pub struct RunningScript {
    child: Child,
    invocation: ScriptInvocation,
    _home: Option<ScriptHome>,
}

impl RunningScript {
    pub fn invocation(&self) -> &ScriptInvocation {
        &self.invocation
    }

    pub fn action(&self) -> ActionId {
        self.invocation.action()
    }

    pub fn take_stdin(&mut self) -> Result<ChildStdin> {
        self.child
            .stdin
            .take()
            .ok_or_else(|| AgentError::Script("helper script did not expose stdin".to_owned()))
    }

    pub fn take_stdout(&mut self) -> Result<ChildStdout> {
        self.child
            .stdout
            .take()
            .ok_or_else(|| AgentError::Script("helper script did not expose stdout".to_owned()))
    }

    pub fn take_stderr(&mut self) -> Result<ChildStderr> {
        self.child
            .stderr
            .take()
            .ok_or_else(|| AgentError::Script("helper script did not expose stderr".to_owned()))
    }

    pub async fn wait(&mut self) -> Result<ExitStatus> {
        self.child.wait().await.map_err(|error| {
            AgentError::Script(format!(
                "failed to wait for helper script action `{}`: {error}",
                self.action().as_str()
            ))
        })
    }

    pub async fn kill(&mut self) -> Result<()> {
        if let Err(error) = self.child.start_kill()
            && self.child.try_wait().ok().flatten().is_none()
        {
            return Err(AgentError::Script(format!(
                "failed to terminate helper script action `{}`: {error}",
                self.action().as_str()
            )));
        }
        let _ = self.child.wait().await;
        Ok(())
    }
}

#[derive(Debug)]
struct ScriptHome {
    path: PathBuf,
}

impl Drop for ScriptHome {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

struct BoundedStream {
    bytes: Vec<u8>,
    truncated: bool,
}

async fn read_bounded<R>(mut reader: R, cap: usize) -> std::io::Result<BoundedStream>
where
    R: AsyncRead + Unpin,
{
    let mut bytes = Vec::with_capacity(cap.min(8 * 1024));
    let mut truncated = false;
    let mut buffer = [0_u8; 8 * 1024];

    loop {
        let read = reader.read(&mut buffer).await?;
        if read == 0 {
            break;
        }

        let remaining = cap.saturating_sub(bytes.len());
        if remaining == 0 {
            truncated = true;
            continue;
        }

        let take = remaining.min(read);
        bytes.extend_from_slice(&buffer[..take]);
        if take < read {
            truncated = true;
        }
    }

    Ok(BoundedStream { bytes, truncated })
}

fn validate_timeout(timeout: Duration) -> Result<()> {
    if timeout.is_zero() || timeout > MAX_SCRIPT_TIMEOUT {
        return Err(AgentError::Script(format!(
            "helper script timeout must be between 1 and {} seconds",
            MAX_SCRIPT_TIMEOUT.as_secs()
        )));
    }
    Ok(())
}

/// Build the complete child environment. The ambient environment is never
/// inherited. Proxies, cloud credentials, model credentials, and arbitrary
/// user variables are therefore absent by construction.
fn script_environment(project_root: &Path) -> Result<(BTreeMap<String, String>, ScriptHome)> {
    let candidate = std::env::temp_dir().join(format!(
        "local-agent-harness-script-{}-{}",
        std::process::id(),
        Uuid::new_v4()
    ));
    std::fs::create_dir_all(&candidate).map_err(|error| {
        AgentError::Script(format!(
            "failed to create helper script HOME directory: {error}"
        ))
    })?;
    let mut home = ScriptHome { path: candidate };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&home.path, std::fs::Permissions::from_mode(0o700)).map_err(
            |error| {
                AgentError::Script(format!(
                    "failed to secure helper script HOME directory: {error}"
                ))
            },
        )?;
    }

    home.path = home.path.canonicalize().map_err(|error| {
        AgentError::Script(format!(
            "failed to resolve helper script HOME directory: {error}"
        ))
    })?;
    if home.path.starts_with(project_root) {
        return Err(AgentError::Script(
            "helper script HOME must be outside the configured project root".to_owned(),
        ));
    }
    let home_text = home.path.to_str().ok_or_else(|| {
        AgentError::Script(
            "helper script HOME must be valid UTF-8 for the child environment".to_owned(),
        )
    })?;

    let mut environment = BTreeMap::new();
    environment.insert("HOME".to_owned(), home_text.to_owned());
    environment.insert("PATH".to_owned(), SCRIPT_ENV_PATH.to_owned());
    #[cfg(unix)]
    {
        environment.insert("LANG".to_owned(), "C.UTF-8".to_owned());
        environment.insert("LC_ALL".to_owned(), "C.UTF-8".to_owned());
        environment.insert("TMPDIR".to_owned(), home_text.to_owned());
    }
    #[cfg(windows)]
    {
        environment.insert("USERPROFILE".to_owned(), home_text.to_owned());
        environment.insert("TEMP".to_owned(), home_text.to_owned());
        environment.insert("TMP".to_owned(), home_text.to_owned());
    }

    Ok((environment, home))
}

fn canonical_directory(label: &str, path: &Path) -> Result<PathBuf> {
    if path.as_os_str().is_empty() {
        return Err(AgentError::Script(format!("{label} must not be empty")));
    }

    let canonical = path.canonicalize().map_err(|error| {
        AgentError::Script(format!("{label} is not accessible as a directory: {error}"))
    })?;
    let metadata = std::fs::metadata(&canonical)
        .map_err(|error| AgentError::Script(format!("{label} is not accessible: {error}")))?;
    if !metadata.is_dir() {
        return Err(AgentError::Script(format!("{label} must be a directory")));
    }

    Ok(canonical)
}

fn canonical_interpreter(path: &Path, project_root: &Path) -> Result<PathBuf> {
    if path.as_os_str().is_empty() {
        return Err(AgentError::Script(
            "interpreter path must not be empty".to_owned(),
        ));
    }
    if !path.is_absolute() {
        return Err(AgentError::Script(
            "interpreter path must be absolute; PATH lookup is not used".to_owned(),
        ));
    }

    let canonical = path
        .canonicalize()
        .map_err(|error| AgentError::Script(format!("interpreter is not accessible: {error}")))?;

    if canonical.starts_with(project_root) {
        return Err(AgentError::Script(
            "interpreter must be outside the configured project root".to_owned(),
        ));
    }
    if is_shell_interpreter(&canonical) {
        return Err(AgentError::Script(
            "shell interpreters are not permitted for helper scripts".to_owned(),
        ));
    }

    let metadata = std::fs::metadata(&canonical)
        .map_err(|error| AgentError::Script(format!("interpreter is not accessible: {error}")))?;
    if !metadata.is_file() {
        return Err(AgentError::Script(
            "interpreter must be a regular file".to_owned(),
        ));
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            return Err(AgentError::Script(
                "interpreter is not executable".to_owned(),
            ));
        }
    }

    Ok(canonical)
}

fn is_shell_interpreter(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let name = name
        .strip_suffix(".exe")
        .unwrap_or(name)
        .to_ascii_lowercase();
    SHELL_INTERPRETER_NAMES.contains(&name.as_str())
}
