#![forbid(unsafe_code)]

use local_agent_harness::config::{Config, log_safe_endpoint};
use local_agent_harness::error::Result;
use local_agent_harness::runtime;
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
    let preflight = runtime::preflight(&config)?;
    println!(
        "script runtime ok: project_root={} script_root={} interpreter={} audit={}",
        preflight.project_root.display(),
        preflight.script_root.display(),
        preflight.interpreter.display(),
        preflight.audit_path.display()
    );
    for (action, script) in &preflight.mappings {
        println!("script action: {} -> {}", action.as_str(), script.display());
    }
    if preflight.capabilities.change_ready() {
        println!("change mode: available");
    } else {
        println!(
            "change mode: gated; missing {}",
            preflight.capabilities.missing_file_names().join(", ")
        );
    }
    Ok(())
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .init();
}
