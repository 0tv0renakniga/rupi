#![forbid(unsafe_code)]

use std::time::Duration;

use local_agent_harness::config::{Config, log_safe_endpoint};
use local_agent_harness::error::{AgentError, Result};
use local_agent_harness::mcp::McpRuntime;
use local_agent_harness::policy::Policy;
use local_agent_harness::tui;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();

    let config = Config::from_env()?;
    config.validate()?;

    tracing::info!(
        root = %config.root.display(),
        llm_base = %log_safe_endpoint(&config.llm_base),
        model = %config.model,
        jev_base = %log_safe_endpoint(&config.jev_base),
        "configuration validated"
    );

    if std::env::args().skip(1).any(|arg| arg == "--headless") {
        return run_headless(config).await;
    }

    tui::run(config).await
}

async fn run_headless(config: Config) -> Result<()> {
    let policy = Policy::new(config.root.clone()).map_err(|error| {
        AgentError::Config(format!("repository path policy is invalid: {error}"))
    })?;
    let runtime = McpRuntime::start(
        &config.mcp,
        &config.root,
        policy,
        config.budget.max_tool_output_bytes,
    )
    .await?;

    for warning in runtime.warnings() {
        println!("{warning}");
    }
    println!(
        "mcp startup ok: profile={} servers={}",
        runtime.profile().as_str(),
        runtime.started_servers().len()
    );

    runtime.shutdown(Duration::from_secs(2)).await
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();
}
