use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt};

use crate::error::{AgentError, Result};

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(900);
const MAX_MANIFEST_BYTES: u64 = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerificationCommand {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub timeout: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyStep {
    pub command: String,
    pub exit_code: i32,
    pub duration: Duration,
    pub timed_out: bool,
}

impl VerifyStep {
    pub fn succeeded(&self) -> bool {
        !self.timed_out && self.exit_code == 0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyReport {
    pub passed: bool,
    pub steps: Vec<VerifyStep>,
    pub tail: String,
}

impl VerifyReport {
    pub fn from_steps(steps: Vec<VerifyStep>, tail: impl Into<String>) -> Self {
        let passed = !steps.is_empty() && steps.iter().all(VerifyStep::succeeded);
        Self {
            passed,
            steps,
            tail: tail.into(),
        }
    }

    pub fn blocked(reason: impl Into<String>) -> Self {
        Self {
            passed: false,
            steps: Vec::new(),
            tail: reason.into(),
        }
    }

    pub fn verified(&self) -> bool {
        self.passed && !self.steps.is_empty() && self.steps.iter().all(VerifyStep::succeeded)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutput {
    pub exit_code: i32,
    pub timed_out: bool,
    pub duration: Duration,
    pub stdout: String,
    pub stderr: String,
}

#[async_trait::async_trait]
pub trait VerificationExecutor: Send + Sync {
    async fn execute(
        &self,
        command: &VerificationCommand,
        output_cap: usize,
    ) -> Result<CommandOutput>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct LocalCommandExecutor;

#[async_trait::async_trait]
impl VerificationExecutor for LocalCommandExecutor {
    async fn execute(
        &self,
        command: &VerificationCommand,
        output_cap: usize,
    ) -> Result<CommandOutput> {
        let started = Instant::now();
        let mut child = tokio::process::Command::new(&command.program)
            .args(&command.args)
            .current_dir(&command.cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| {
                AgentError::Blocked(format!(
                    "failed to start verification command `{}`: {error}",
                    render_command(command)
                ))
            })?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| AgentError::Blocked("failed to capture verifier stdout".into()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| AgentError::Blocked("failed to capture verifier stderr".into()))?;
        let stdout_task = tokio::spawn(read_bounded(stdout, output_cap));
        let stderr_task = tokio::spawn(read_bounded(stderr, output_cap));

        let wait = tokio::time::timeout(command.timeout, child.wait()).await;
        let (exit_code, timed_out) = match wait {
            Ok(Ok(status)) => (status.code().unwrap_or(-1), false),
            Ok(Err(error)) => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                return Err(AgentError::Blocked(format!(
                    "failed to wait for verification command: {error}"
                )));
            }
            Err(_) => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                (-2, true)
            }
        };
        let duration = started.elapsed();

        let stdout = stdout_task.await.map_err(|error| {
            AgentError::Blocked(format!("failed to collect verifier stdout: {error}"))
        })??;
        let stderr = stderr_task.await.map_err(|error| {
            AgentError::Blocked(format!("failed to collect verifier stderr: {error}"))
        })??;

        Ok(CommandOutput {
            exit_code,
            timed_out,
            duration,
            stdout,
            stderr,
        })
    }
}

pub fn detect(root: &Path) -> Result<Vec<VerificationCommand>> {
    let root = validate_root(root)?;
    let mut commands = Vec::new();

    if root.join("Cargo.toml").is_file() {
        commands.push(command(&root, "cargo", &["fmt", "--check"]));
        commands.push(command(&root, "cargo", &["check", "--all-targets"]));
        commands.push(command(&root, "cargo", &["test"]));
    }

    if root.join("package.json").is_file() {
        detect_node(&root, &mut commands)?;
    }

    if root.join("pyproject.toml").is_file()
        || root.join("requirements.txt").is_file()
        || root.join("pytest.ini").is_file()
    {
        let program = if root.join(".venv/bin/python").is_file() {
            ".venv/bin/python"
        } else {
            "python3"
        };
        commands.push(command(&root, program, &["-m", "pytest", "-q"]));
    }

    Ok(commands)
}

pub async fn run(
    root: &Path,
    commands: &[VerificationCommand],
    output_cap: usize,
) -> Result<VerifyReport> {
    run_with_executor(root, commands, output_cap, &LocalCommandExecutor).await
}

pub async fn run_with_executor(
    root: &Path,
    commands: &[VerificationCommand],
    output_cap: usize,
    executor: &dyn VerificationExecutor,
) -> Result<VerifyReport> {
    let root = validate_root(root)?;
    let commands = prepare_commands(&root, commands)?;
    if commands.is_empty() {
        return Ok(VerifyReport::blocked("no verification commands detected"));
    }

    let mut steps = Vec::new();
    let mut failure_tail = String::new();

    for command in commands {
        let rendered = render_command(&command);
        let output = executor
            .execute(&command, output_cap)
            .await
            .unwrap_or_else(|error| CommandOutput {
                exit_code: -1,
                timed_out: false,
                duration: Duration::ZERO,
                stdout: String::new(),
                stderr: error.to_string(),
            });
        let step = VerifyStep {
            command: rendered,
            exit_code: output.exit_code,
            duration: output.duration,
            timed_out: output.timed_out,
        };
        let succeeded = step.succeeded();
        steps.push(step);

        if !succeeded {
            let combined = combine_output(&output.stdout, &output.stderr);
            failure_tail = if output.timed_out {
                format!(
                    "verification timed out after {:?}\n{}",
                    command.timeout, combined
                )
            } else if combined.trim().is_empty() {
                format!(
                    "verification command exited with status {}",
                    output.exit_code
                )
            } else {
                combined
            };
            failure_tail = bound_output(&failure_tail, output_cap);
            break;
        }
    }

    Ok(VerifyReport::from_steps(steps, failure_tail))
}

fn command(root: &Path, program: &str, args: &[&str]) -> VerificationCommand {
    VerificationCommand {
        program: program.to_owned(),
        args: args.iter().map(|arg| (*arg).to_owned()).collect(),
        cwd: root.to_path_buf(),
        timeout: DEFAULT_TIMEOUT,
    }
}

fn detect_node(root: &Path, commands: &mut Vec<VerificationCommand>) -> Result<()> {
    let raw = read_manifest(&root.join("package.json"), "package.json")?;
    let manifest: Value = serde_json::from_str(&raw)
        .map_err(|_| AgentError::Blocked("package.json is not valid JSON".into()))?;
    let scripts = match manifest.get("scripts") {
        None => return Ok(()),
        Some(Value::Object(scripts)) => scripts,
        Some(_) => {
            return Err(AgentError::Blocked(
                "package.json scripts must be an object".into(),
            ));
        }
    };

    for name in ["test", "build", "typecheck"] {
        if scripts.get(name).is_some_and(Value::is_string) {
            commands.push(command(root, "npm", &["run", name]));
        }
    }

    Ok(())
}

fn read_manifest(path: &Path, label: &str) -> Result<String> {
    let metadata = std::fs::metadata(path)
        .map_err(|error| AgentError::Blocked(format!("cannot inspect {label}: {error}")))?;
    if metadata.len() > MAX_MANIFEST_BYTES {
        return Err(AgentError::Blocked(format!(
            "{label} exceeds the manifest size limit"
        )));
    }
    std::fs::read_to_string(path)
        .map_err(|error| AgentError::Blocked(format!("cannot read {label}: {error}")))
}

fn validate_root(root: &Path) -> Result<PathBuf> {
    let canonical = root.canonicalize().map_err(|error| {
        AgentError::Blocked(format!("repository root is not accessible: {error}"))
    })?;
    if !canonical.is_dir() {
        return Err(AgentError::Blocked(
            "repository root must be a directory".into(),
        ));
    }
    Ok(canonical)
}

fn prepare_commands(
    root: &Path,
    commands: &[VerificationCommand],
) -> Result<Vec<VerificationCommand>> {
    commands
        .iter()
        .map(|command| {
            validate_command(command)?;
            let cwd = resolve_cwd(root, &command.cwd)?;
            Ok(VerificationCommand {
                cwd,
                ..command.clone()
            })
        })
        .collect()
}

fn validate_command(command: &VerificationCommand) -> Result<()> {
    if command.program.trim().is_empty() {
        return Err(AgentError::Blocked(
            "verification program must not be empty".into(),
        ));
    }
    if command.timeout.is_zero() {
        return Err(AgentError::Blocked(
            "verification timeout must be greater than zero".into(),
        ));
    }
    if is_forbidden_program(&command.program) {
        return Err(AgentError::Blocked(format!(
            "verification program `{}` is not allowed",
            command.program
        )));
    }
    Ok(())
}

fn resolve_cwd(root: &Path, cwd: &Path) -> Result<PathBuf> {
    let candidate = if cwd.is_absolute() {
        cwd.to_path_buf()
    } else {
        root.join(cwd)
    };
    let canonical = candidate.canonicalize().map_err(|error| {
        AgentError::Blocked(format!("verification cwd is not accessible: {error}"))
    })?;
    if !canonical.starts_with(root) {
        return Err(AgentError::Blocked(
            "verification cwd escapes the repository root".into(),
        ));
    }
    Ok(canonical)
}

fn is_forbidden_program(program: &str) -> bool {
    let Some(name) = Path::new(program)
        .file_name()
        .and_then(|name| name.to_str())
    else {
        return true;
    };
    let lower = name.to_ascii_lowercase();
    let name = lower.strip_suffix(".exe").unwrap_or(lower.as_str());
    matches!(
        name,
        "sh" | "bash"
            | "dash"
            | "zsh"
            | "ksh"
            | "csh"
            | "tcsh"
            | "fish"
            | "cmd"
            | "powershell"
            | "pwsh"
            | "sudo"
            | "su"
            | "doas"
            | "env"
    )
}

pub(crate) fn render_command(command: &VerificationCommand) -> String {
    let mut rendered = command.program.clone();
    for arg in &command.args {
        rendered.push(' ');
        if arg.is_empty()
            || arg
                .chars()
                .any(|character| character.is_whitespace() || "'\"\\$`".contains(character))
        {
            rendered.push('\'');
            rendered.push_str(&arg.replace('\'', "'\\''"));
            rendered.push('\'');
        } else {
            rendered.push_str(arg);
        }
    }
    rendered
}

fn combine_output(stdout: &str, stderr: &str) -> String {
    match (stdout.is_empty(), stderr.is_empty()) {
        (true, true) => String::new(),
        (false, true) => stdout.to_owned(),
        (true, false) => stderr.to_owned(),
        (false, false) => format!("stdout:\n{stdout}\nstderr:\n{stderr}"),
    }
}

fn bound_output(output: &str, cap: usize) -> String {
    if output.len() <= cap {
        return output.to_owned();
    }

    let half = cap / 2;
    let mut head = half.min(output.len());
    while head > 0 && !output.is_char_boundary(head) {
        head -= 1;
    }

    let mut tail_start = output.len().saturating_sub(half);
    while tail_start < output.len() && !output.is_char_boundary(tail_start) {
        tail_start += 1;
    }

    format!(
        "{}\n...[{} bytes omitted]...\n{}",
        &output[..head],
        tail_start.saturating_sub(head),
        &output[tail_start..]
    )
}

async fn read_bounded<R>(mut reader: R, cap: usize) -> std::io::Result<String>
where
    R: AsyncRead + Unpin,
{
    let mut capture = BoundedCapture::new(cap);
    let mut buffer = [0_u8; 8192];
    loop {
        let read = reader.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        capture.push(&buffer[..read]);
    }
    Ok(capture.finish())
}

struct BoundedCapture {
    cap: usize,
    head: Vec<u8>,
    tail: VecDeque<u8>,
    total: usize,
}

impl BoundedCapture {
    fn new(cap: usize) -> Self {
        Self {
            cap,
            head: Vec::new(),
            tail: VecDeque::new(),
            total: 0,
        }
    }

    fn push(&mut self, bytes: &[u8]) {
        self.total = self.total.saturating_add(bytes.len());
        if self.cap == 0 {
            return;
        }

        let head_cap = self.cap / 2;
        let mut remaining = bytes;
        if self.head.len() < head_cap {
            let take = (head_cap - self.head.len()).min(remaining.len());
            self.head.extend_from_slice(&remaining[..take]);
            remaining = &remaining[take..];
        }

        let tail_cap = self.cap - head_cap;
        if !remaining.is_empty() && tail_cap > 0 {
            self.tail.extend(remaining);
            while self.tail.len() > tail_cap {
                self.tail.pop_front();
            }
        }
    }

    fn finish(self) -> String {
        if self.cap == 0 {
            return String::new();
        }
        let omitted = self
            .total
            .saturating_sub(self.head.len().saturating_add(self.tail.len()));
        let head = String::from_utf8_lossy(&self.head);
        let mut tail_bytes = self.tail;
        let tail = String::from_utf8_lossy(tail_bytes.make_contiguous());
        if omitted == 0 {
            format!("{head}{tail}")
        } else {
            format!("{head}\n...[{} bytes omitted]...\n{tail}", omitted)
        }
    }
}
