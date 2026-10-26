//! Integration coverage for the shared script-substrate composition.
//!
//! These tests construct the same runtime used by the TUI and headless paths.
//! They never instantiate an MCP runtime or dispatcher.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use local_agent_harness::agent::verify::{VerificationCommand, VerifyReport};
use local_agent_harness::approval::ApprovalPolicy;
use local_agent_harness::config::{Config, EndpointConfig, ScriptConfig};
use local_agent_harness::error::{AgentError, Result};
use local_agent_harness::events::{AgentEvent, Command};
use local_agent_harness::jev::{Route, Verdict};
use local_agent_harness::llm::{
    AssistantTurn, ChatRequest, FnCall, LlmBackend, LlmStreamEvent, ToolCall,
};
use local_agent_harness::orchestrator::{
    JevAdvisor, LocalWikiStore, Orchestrator, OrchestratorLimits, VerificationEngine,
};
use local_agent_harness::runtime;
use local_agent_harness::script_runner::{ActionId, DEFAULT_SCRIPT_TIMEOUT};
use local_agent_harness::state_machine::{TaskBudget, TaskMode};
use local_agent_harness::tui::worker;
use serde_json::Value;
use tokio::sync::mpsc::UnboundedSender;
use uuid::Uuid;

const OBJECTIVE: &str = "Investigate the fixture without changing it";

struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "rupi-runtime-composition-{label}-{}",
            Uuid::new_v4()
        ));
        std::fs::create_dir_all(&path).expect("create temp directory");
        Self { path }
    }

    fn project(&self) -> PathBuf {
        self.path.join("project")
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

struct FakeLlm {
    turns: Mutex<VecDeque<AssistantTurn>>,
    requests: Mutex<Vec<ChatRequest>>,
    calls: AtomicUsize,
}

impl FakeLlm {
    fn new(turns: Vec<AssistantTurn>) -> Self {
        Self {
            turns: Mutex::new(turns.into()),
            requests: Mutex::new(Vec::new()),
            calls: AtomicUsize::new(0),
        }
    }

    fn requests(&self) -> Vec<ChatRequest> {
        self.requests.lock().expect("requests lock").clone()
    }
}

#[async_trait]
impl LlmBackend for FakeLlm {
    async fn stream_chat(
        &self,
        request: ChatRequest,
        _event_tx: Option<&UnboundedSender<LlmStreamEvent>>,
    ) -> Result<AssistantTurn> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.requests.lock().expect("requests lock").push(request);
        self.turns
            .lock()
            .expect("turns lock")
            .pop_front()
            .ok_or_else(|| AgentError::Llm("script exhausted".into()))
    }

    async fn complete_text(&self, _system: &str, _user: &str) -> Result<String> {
        Ok("unused".into())
    }
}

struct FakeJev;

#[async_trait]
impl JevAdvisor for FakeJev {
    async fn choice(&self, _prompt: &str, _options: &[&str], _minimum_confidence: f32) -> Route {
        Route::Pick(0)
    }

    async fn noul(&self, _claim: &str, _evidence: &str, _threshold: f32) -> Verdict {
        Verdict::Pass(0.99)
    }

    async fn score(&self, _query: &str, _docs: &[String]) -> Option<Vec<f32>> {
        None
    }
}

struct UnusedVerifier;

#[async_trait]
impl VerificationEngine for UnusedVerifier {
    async fn detect(&self, _root: &Path) -> Result<Vec<VerificationCommand>> {
        Ok(Vec::new())
    }

    async fn run(
        &self,
        _root: &Path,
        _commands: &[VerificationCommand],
        _output_cap: usize,
    ) -> Result<VerifyReport> {
        Err(AgentError::Blocked(
            "verification must not run in Investigate mode".into(),
        ))
    }
}

fn turn(text: impl Into<String>) -> AssistantTurn {
    AssistantTurn {
        text: text.into(),
        ..AssistantTurn::default()
    }
}

fn tool_turn(calls: Vec<(&str, String)>) -> AssistantTurn {
    AssistantTurn {
        text: "inspecting".into(),
        calls: calls
            .into_iter()
            .enumerate()
            .map(|(index, (name, arguments))| ToolCall {
                id: format!("call_{index}"),
                kind: "function".into(),
                function: FnCall {
                    name: name.into(),
                    arguments,
                },
            })
            .collect(),
        ..AssistantTurn::default()
    }
}

fn findings() -> AssistantTurn {
    turn(
        "CONFIRMED FACTS:\n- The fixture contains a tracked file.\n\
         HYPOTHESES:\n- The fixture exercises script-backed investigation.\n\
         MISSING EVIDENCE:\n- No tests were inspected.\n\
         RECOMMENDED NEXT ACTION:\n- Review the brief and decide whether a Change task is needed.",
    )
}

fn python_interpreter() -> PathBuf {
    for candidate in [
        "/usr/bin/python3",
        "/usr/local/bin/python3",
        "/bin/python3",
        "/usr/bin/python",
    ] {
        let path = Path::new(candidate);
        if path.is_file() {
            return path.to_path_buf();
        }
    }
    panic!("python3 is required for runtime composition tests");
}

fn scripts_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts")
}

fn config(project: &Path) -> Config {
    Config {
        root: project.to_path_buf(),
        audit_path: project.join(".harness-audit/audit.jsonl"),
        llm_base: "http://127.0.0.1:8080".into(),
        model: "local-model".into(),
        jev_base: "http://127.0.0.1:9090".into(),
        endpoint: EndpointConfig::default(),
        min_route_conf: 0.85,
        budget: TaskBudget {
            max_tool_output_bytes: 512,
            ..TaskBudget::default()
        },
        approval: ApprovalPolicy::default(),
        scripts: ScriptConfig {
            root: scripts_dir(),
            interpreter: python_interpreter(),
            timeout: DEFAULT_SCRIPT_TIMEOUT,
        },
    }
}

fn tool_names(request: &ChatRequest) -> Vec<String> {
    request
        .tools
        .as_array()
        .expect("tools array")
        .iter()
        .filter_map(|tool| {
            tool.get("function")
                .and_then(|function| function.get("name"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .collect()
}

fn git(root: &Path, args: &[&str]) {
    let status = ProcessCommand::new("git")
        .args(args)
        .current_dir(root)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_AUTHOR_NAME", "Rupi Test")
        .env("GIT_AUTHOR_EMAIL", "rupi-test@example.invalid")
        .env("GIT_COMMITTER_NAME", "Rupi Test")
        .env("GIT_COMMITTER_EMAIL", "rupi-test@example.invalid")
        .status()
        .expect("run git");
    assert!(status.success(), "git command failed: {args:?}");
}

#[test]
fn headless_preflight_uses_scripts_and_does_not_read_mcp_configuration() {
    let temp = TempDir::new("headless-env");
    let project = temp.project();
    std::fs::create_dir_all(&project).expect("create project");

    let _guard = EnvGuard::new(&[
        ("HARNESS_ROOT", Some(project.as_os_str().to_owned())),
        (
            "HARNESS_SCRIPT_ROOT",
            Some(scripts_dir().as_os_str().to_owned()),
        ),
        (
            "HARNESS_SCRIPT_INTERPRETER",
            Some(python_interpreter().into_os_string()),
        ),
        ("HARNESS_ENDPOINT_PROFILE", Some("local".into())),
        ("HARNESS_LLM_BASE", Some("http://127.0.0.1:8080".into())),
        ("HARNESS_JEV_BASE", Some("http://127.0.0.1:9090".into())),
        ("HARNESS_LLM_ALLOWED_ORIGINS", None),
        ("HARNESS_LLM_CREDENTIAL_ENV", None),
        ("HARNESS_AUDIT_PATH", None),
        ("HARNESS_MCP_PROFILE", Some("not-a-real-profile".into())),
        (
            "HARNESS_MCP_FS_EXECUTABLE",
            Some("/definitely/missing/mcp".into()),
        ),
        ("HARNESS_MCP_FS_SANDBOX_MODE", Some("bubblewrap".into())),
        ("HARNESS_MCP_APPROVED_DIGESTS", Some("{invalid".into())),
    ]);

    let config = Config::from_env().expect("configuration without MCP");
    config
        .validate()
        .expect("script-only configuration validates");
    assert_eq!(config.root, project);
    assert_eq!(config.scripts.root, scripts_dir());

    let preflight = runtime::preflight(&config).expect("script runtime preflight");
    assert_eq!(
        preflight
            .mappings
            .iter()
            .map(|(action, _)| *action)
            .collect::<Vec<_>>(),
        vec![
            ActionId::ReadFile,
            ActionId::GitStatus,
            ActionId::GitDiff,
            ActionId::WriteFile,
            ActionId::RunVerifier,
        ]
    );
    assert!(preflight.capabilities.read_only_ready());
    assert!(preflight.capabilities.change_ready());
    assert!(preflight.capabilities.missing_file_names().is_empty());
    assert!(preflight.audit_path.starts_with(&project));
}

#[tokio::test]
async fn tui_worker_dispatches_investigate_through_the_script_runtime() {
    let temp = TempDir::new("worker");
    let project = temp.project();
    std::fs::create_dir_all(project.join("src")).expect("create project");
    std::fs::write(project.join("src/lib.rs"), "pub fn value() -> u8 { 1 }\n")
        .expect("write source");
    std::fs::write(project.join("tracked.txt"), "changed\n").expect("write tracked");
    git(&project, &["init", "--quiet"]);
    git(&project, &["add", "tracked.txt"]);
    git(&project, &["commit", "--quiet", "-m", "initial"]);

    let config = config(&project);
    let script_runtime = runtime::build(&config).expect("script runtime");
    assert!(script_runtime.capabilities.change_ready());

    let llm = Arc::new(FakeLlm::new(vec![
        tool_turn(vec![
            ("read_file", r#"{"path":"src/lib.rs"}"#.into()),
            ("git_status", "{}".into()),
            ("git_diff", r#"{"staged":false}"#.into()),
        ]),
        findings(),
    ]));
    let orchestrator = Orchestrator::new(
        llm.clone(),
        Arc::new(FakeJev),
        script_runtime.executor.clone(),
        Arc::new(UnusedVerifier),
        Arc::new(LocalWikiStore::default()),
    )
    .with_limits(OrchestratorLimits::default())
    .with_approval_gate(Arc::new(
        local_agent_harness::approval::InteractiveApprovalGate::new(),
    ))
    .with_audit(script_runtime.audit.clone());

    let mut worker = worker::spawn(
        orchestrator,
        project.clone(),
        config.budget.clone(),
        local_agent_harness::approval::InteractiveApprovalGate::new(),
        false,
    );
    worker
        .commands
        .send(Command::Submit {
            objective: OBJECTIVE.into(),
            mode: TaskMode::Investigate,
        })
        .await
        .expect("submit task");

    let mut events = Vec::new();
    loop {
        let next = tokio::time::timeout(Duration::from_secs(20), worker.events.recv())
            .await
            .expect("worker event timeout");
        let Some(event) = next else {
            panic!("worker closed before terminal event");
        };
        let terminal = matches!(
            event,
            AgentEvent::Completed { .. }
                | AgentEvent::Failed { .. }
                | AgentEvent::Blocked { .. }
                | AgentEvent::Cancelled { .. }
        );
        events.push(event);
        if terminal {
            break;
        }
    }
    worker.shutdown().await;

    let completed = events.iter().any(|event| {
        matches!(event, AgentEvent::Completed { summary } if summary.contains("evidence brief"))
    });
    assert!(completed, "events: {events:?}");
    let evidence = events
        .iter()
        .find_map(|event| match event {
            AgentEvent::EvidenceCompiled { path } => Some(PathBuf::from(path)),
            _ => None,
        })
        .expect("evidence brief event");
    let content = std::fs::read_to_string(&evidence).expect("read evidence brief");
    assert!(content.contains("`src/lib.rs`"), "{content}");
    assert!(content.contains("git_status clean=false"), "{content}");
    assert!(content.contains("## Recommended next action"), "{content}");
    assert!(!content.contains("mcp_servers"), "{content}");

    let requests = llm.requests();
    let tool_request = requests
        .iter()
        .find(|request| !tool_names(request).is_empty())
        .expect("investigation request");
    assert_eq!(
        tool_names(tool_request),
        ["read_file", "git_status", "git_diff"]
    );

    let audit = std::fs::read_to_string(&config.audit_path).expect("read audit");
    assert!(audit.contains("\"kind\":\"script_finished\""), "{audit}");
    assert!(audit.contains("\"action\":\"read_file\""), "{audit}");
    assert!(audit.contains("\"action\":\"git_status\""), "{audit}");
    assert!(audit.contains("\"action\":\"git_diff\""), "{audit}");
    assert!(!audit.contains("mcp_servers"), "{audit}");
}

struct EnvGuard {
    values: Vec<(&'static str, Option<OsString>)>,
}

impl EnvGuard {
    fn new(values: &[(&'static str, Option<OsString>)]) -> Self {
        let saved = values
            .iter()
            .map(|(name, _)| (*name, std::env::var_os(name)))
            .collect::<Vec<_>>();
        for (name, value) in values {
            // SAFETY: this integration-test binary has one environment-mutating
            // test and does not read unrelated variables during the mutation.
            match value {
                Some(value) => unsafe { std::env::set_var(name, value) },
                None => unsafe { std::env::remove_var(name) },
            }
        }
        Self { values: saved }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (name, value) in &self.values {
            // SAFETY: restore exactly the values captured before the test.
            match value {
                Some(value) => unsafe { std::env::set_var(name, value) },
                None => unsafe { std::env::remove_var(name) },
            }
        }
    }
}
