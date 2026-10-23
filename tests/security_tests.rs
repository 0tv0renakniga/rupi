use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::PathBuf;

use local_agent_harness::approval::ApprovalPolicy;
use local_agent_harness::config::{Config, EndpointConfig, McpRuntimeConfig};
use local_agent_harness::error::AgentError;
use local_agent_harness::jev::{JevClient, Route};
use local_agent_harness::llm::LlmBackend;
use local_agent_harness::llm::openai_compat::OpenAiCompatClient;
use local_agent_harness::mcp::client::{McpClient, McpClientOptions};
use local_agent_harness::mcp::process::ProcessSpec;
use local_agent_harness::state_machine::TaskBudget;
use serde_json::{Value, json};

const MOCK_SERVER: &str = env!("CARGO_BIN_EXE_mock_mcp_server");

const PARENT_ONLY_VARIABLES: &[&str] = &[
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "NO_PROXY",
    "http_proxy",
    "https_proxy",
    "all_proxy",
    "no_proxy",
    "HARNESS_PARENT_ONLY",
    "PATH",
];

struct EnvRestore {
    values: Vec<(&'static str, Option<OsString>)>,
}

impl EnvRestore {
    fn poison_parent(variables: &[&'static str]) -> Self {
        let values = variables
            .iter()
            .map(|name| (*name, std::env::var_os(name)))
            .collect::<Vec<_>>();

        for name in variables {
            // SAFETY: this integration test owns the only environment mutation
            // in this test binary and does not read unrelated variables while
            // the poison values are installed.
            unsafe { std::env::set_var(name, format!("http://{name}.invalid:9")) };
        }

        Self { values }
    }
}

impl Drop for EnvRestore {
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

fn mock_spec() -> ProcessSpec {
    let home = std::env::temp_dir().join("local-agent-harness-security-test-home");
    std::fs::create_dir_all(&home).expect("create controlled HOME");

    let mut env = BTreeMap::new();
    env.insert("HOME".to_owned(), home.to_string_lossy().into_owned());
    env.insert("HARNESS_CHILD_MARKER".to_owned(), "kept".to_owned());

    ProcessSpec {
        program: PathBuf::from(MOCK_SERVER),
        args: Vec::new(),
        cwd: Some(std::env::current_dir().expect("current directory")),
        env,
    }
}

fn local_endpoint() -> EndpointConfig {
    EndpointConfig::default()
}

fn config_with(llm_base: &str, jev_base: &str) -> Config {
    Config {
        root: PathBuf::from("."),
        audit_path: PathBuf::from(".harness-audit/audit.jsonl"),
        llm_base: llm_base.to_owned(),
        model: "local-model".into(),
        jev_base: jev_base.to_owned(),
        endpoint: local_endpoint(),
        min_route_conf: 0.85,
        budget: TaskBudget::default(),
        approval: ApprovalPolicy::default(),
        mcp: McpRuntimeConfig::default(),
    }
}

#[cfg(not(feature = "remote"))]
#[test]
fn default_build_rejects_non_loopback_and_tls_endpoints() {
    for endpoint in ["http://example.com", "https://127.0.0.1:8080"] {
        assert!(
            config_with(endpoint, "http://127.0.0.1:9090")
                .validate()
                .is_err(),
            "{endpoint} must require the remote feature"
        );
    }
}

#[cfg(feature = "remote")]
#[test]
fn remote_build_allows_explicitly_approved_origin_without_contacting_it() {
    let mut config = config_with("https://api.example.com", "http://127.0.0.1:9090");
    config.endpoint = EndpointConfig {
        profile: local_agent_harness::config::EndpointProfile::ApprovedRemote,
        allowed_origins: ["https://api.example.com".to_owned()].into(),
        credential_env: Some("HARNESS_TEST_CREDENTIAL".into()),
        model_explicit: true,
    };
    assert!(config.validate().is_ok());
    assert_eq!(
        local_agent_harness::config::log_safe_endpoint(&config.llm_base),
        "https://api.example.com"
    );
}

#[tokio::test]
async fn unavailable_llm_and_jev_have_no_remote_fallback() {
    let llm = OpenAiCompatClient::new("http://127.0.0.1:1", "local-model");
    let error = llm
        .complete_text("system", "user")
        .await
        .expect_err("unavailable LLM endpoint must fail explicitly");
    assert!(matches!(error, AgentError::Llm(_)));

    let jev = JevClient::new("http://127.0.0.1:1");
    assert_eq!(
        jev.choice("prompt", &["a", "b"], 0.9).await,
        Route::Escalate,
        "unavailable Jev must escalate without a remote fallback"
    );
}

#[tokio::test]
async fn mcp_child_does_not_inherit_parent_environment_or_proxy_variables() {
    let _restore = EnvRestore::poison_parent(PARENT_ONLY_VARIABLES);

    let client = McpClient::connect("env-probe", &mock_spec(), McpClientOptions::default())
        .await
        .expect("mock MCP lifecycle should succeed");

    let requested = PARENT_ONLY_VARIABLES
        .iter()
        .copied()
        .chain(["HOME", "HARNESS_CHILD_MARKER"])
        .collect::<Vec<_>>();
    let outcome = client.call_tool("env", json!({ "names": requested })).await;
    assert!(!outcome.is_error, "{}", outcome.text());

    let observed: Value = serde_json::from_str(&outcome.text()).expect("env tool JSON");
    assert_eq!(observed["HARNESS_CHILD_MARKER"], "kept");
    assert_eq!(
        observed["HOME"],
        std::env::temp_dir()
            .join("local-agent-harness-security-test-home")
            .to_string_lossy()
            .as_ref()
    );

    for name in PARENT_ONLY_VARIABLES {
        assert!(
            observed[name].is_null(),
            "MCP child inherited `{name}`: {observed}"
        );
    }
}
