//! Manual local-model compatibility smoke suite.
//!
//! Live execution is opt-in and requires a running local llama-server:
//!
//! ```sh
//! ./smoke/run.sh
//! ```
//!
//! The ordinary test suite only exercises bounded report construction and does
//! not contact a model endpoint.

use std::collections::BTreeMap;
use std::fmt::Display;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::{Duration, Instant};

use chrono::{SecondsFormat, Utc};
use local_agent_harness::config::{Limits, log_safe_endpoint};
use local_agent_harness::error::AgentError;
use local_agent_harness::llm::openai_compat::OpenAiCompatClient;
use local_agent_harness::llm::{
    ChatMessage, ChatRequest, ContextAdmission, VisibleChatEvent, VisibleChatOutcome,
    estimate_text_tokens,
};
use local_agent_harness::wiki::redact::redact;
use serde::Serialize;
use serde_json::Value;
use tokio::sync::mpsc;
use tokio::task::JoinError;
use tokio::time::{sleep, timeout};

const SCHEMA_VERSION: u32 = 1;
const DEFAULT_ATTEMPTS: usize = 5;
const MAX_ATTEMPTS: usize = 20;
const MAX_STRING_BYTES: usize = 512;
const MAX_REPORT_BYTES: usize = 64 * 1024;
const DEFAULT_REQUEST_TIMEOUT_SECS: u64 = 120;
const DEFAULT_CANCEL_GRACE_SECS: u64 = 15;

#[derive(Debug, Clone)]
struct SmokeConfig {
    base: String,
    explicit_model: Option<String>,
    attempts: usize,
    context_cap_tokens: u32,
    reserved_output_tokens: u32,
    output_dir: PathBuf,
    request_timeout: Duration,
    cancel_grace: Duration,
}

impl SmokeConfig {
    fn from_env() -> Result<Self, String> {
        let defaults = Limits::default();
        let base = std::env::var("LLM_BASE")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .or_else(|| {
                std::env::var("HARNESS_LLM_BASE")
                    .ok()
                    .filter(|value| !value.trim().is_empty())
            })
            .unwrap_or_else(|| "http://127.0.0.1:8080".to_owned());
        let explicit_model = std::env::var("LLM_MODEL")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .or_else(|| {
                std::env::var("HARNESS_MODEL")
                    .ok()
                    .filter(|value| !value.trim().is_empty())
            });
        let attempts = parse_env("SMOKE_ATTEMPTS", DEFAULT_ATTEMPTS)?.clamp(1, MAX_ATTEMPTS);
        let context_cap_tokens =
            parse_env("HARNESS_CONTEXT_CAP_TOKENS", defaults.context_cap_tokens)?;
        let reserved_output_tokens = parse_env(
            "HARNESS_RESERVED_OUTPUT_TOKENS",
            defaults.reserved_output_tokens,
        )?;
        let output_dir = std::env::var_os("SMOKE_OUTPUT_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("target/local-model-smoke"));
        let timeout_secs =
            parse_env("SMOKE_REQUEST_TIMEOUT_SECS", DEFAULT_REQUEST_TIMEOUT_SECS)?.clamp(1, 600);
        let cancel_grace_secs =
            parse_env("SMOKE_CANCEL_GRACE_SECS", DEFAULT_CANCEL_GRACE_SECS)?.clamp(1, 600);

        Ok(Self {
            base: base.trim_end_matches('/').to_owned(),
            explicit_model,
            attempts,
            context_cap_tokens,
            reserved_output_tokens,
            output_dir,
            request_timeout: Duration::from_secs(timeout_secs),
            cancel_grace: Duration::from_secs(cancel_grace_secs),
        })
    }
}

#[derive(Debug, Clone)]
struct ServerMetadata {
    model_id: String,
    model_source: String,
    llama_cpp_server_version: String,
    version_source: String,
    server_context_tokens: Option<u32>,
    warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Default)]
struct SmokeObservation {
    scenario: String,
    outcome: String,
    finish_reason: Option<String>,
    time_to_first_token_ms: Option<u64>,
    elapsed_ms: u64,
    visible_chars: usize,
    visible_tokens: u32,
    empty_visible_response: bool,
    error: Option<String>,
}

#[derive(Debug, Serialize)]
struct SmokeReport {
    schema_version: u32,
    generated_at: String,
    endpoint: String,
    model_id: String,
    model_source: String,
    llama_cpp_server_version: String,
    version_source: String,
    context_cap_tokens: u32,
    reserved_output_tokens: u32,
    prompt_budget_tokens: u32,
    server_context_tokens: Option<u32>,
    attempts_requested: usize,
    attempts_recorded: usize,
    normal_attempts_completed: usize,
    failed_attempts: usize,
    empty_responses: usize,
    empty_response_rate: f64,
    time_to_first_token_ms: Option<u64>,
    time_to_first_token_samples_ms: Vec<u64>,
    finish_reasons: BTreeMap<String, usize>,
    cancellation_result: String,
    cancellation: SmokeObservation,
    observations: Vec<SmokeObservation>,
    discovery_warnings: Vec<String>,
}

#[derive(Debug, Default)]
struct StreamStats {
    first_token_ms: Option<u64>,
    visible_chars: usize,
    visible_tokens: u32,
}

#[tokio::test]
#[ignore = "requires a manually started local llama-server; run smoke/run.sh"]
async fn local_model_compatibility_smoke_suite() {
    let config = SmokeConfig::from_env().expect("validate local smoke configuration");
    let metadata = discover_server(&config)
        .await
        .expect("discover local llama-server metadata");
    let client = OpenAiCompatClient::new(config.base.clone(), metadata.model_id.clone())
        .with_context_admission(ContextAdmission::from_limits(&Limits {
            context_cap_tokens: config.context_cap_tokens,
            reserved_output_tokens: config.reserved_output_tokens,
            ..Limits::default()
        }));

    let mut observations = Vec::with_capacity(config.attempts);
    for attempt in 0..config.attempts {
        println!("smoke: normal attempt {}/{}", attempt + 1, config.attempts);
        observations.push(run_normal_attempt(&config, &client).await);
    }

    println!("smoke: cancellation attempt");
    let cancellation = run_cancellation_attempt(&config, &client).await;
    let report = build_report(&config, &metadata, observations, cancellation);
    let path = write_report(&report, &config.output_dir).expect("write smoke report");

    println!(
        "smoke: model={} version={} empty_rate={:.3} cancellation={}",
        report.model_id,
        report.llama_cpp_server_version,
        report.empty_response_rate,
        report.cancellation_result
    );
    println!("smoke: report={}", path.display());
}

async fn discover_server(config: &SmokeConfig) -> Result<ServerMetadata, String> {
    let http = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|error| format!("build discovery client: {error}"))?;
    let base = config.base.trim_end_matches('/');
    let mut warnings = Vec::new();

    let mut model_id = config.explicit_model.clone();
    let mut model_source = if model_id.is_some() {
        "environment".to_owned()
    } else {
        String::new()
    };
    match http.get(format!("{base}/v1/models")).send().await {
        Ok(response) if response.status().is_success() => {
            let value: Value = response
                .json()
                .await
                .map_err(|error| format!("parse GET /v1/models: {error}"))?;
            let discovered = value
                .get("data")
                .and_then(Value::as_array)
                .and_then(|models| {
                    models
                        .iter()
                        .find_map(|model| model.get("id").and_then(Value::as_str))
                })
                .filter(|id| !id.trim().is_empty())
                .map(str::to_owned);
            if model_id.is_none() {
                model_id = discovered;
                if model_id.is_some() {
                    model_source = "GET /v1/models".to_owned();
                }
            }
        }
        Ok(response) => warnings.push(format!(
            "GET /v1/models returned HTTP {}; using configured model if present",
            response.status()
        )),
        Err(error) => warnings.push(format!("GET /v1/models failed: {error}")),
    }
    let Some(model_id) = model_id else {
        return Err(
            "could not determine model ID; set LLM_MODEL or make GET /v1/models succeed".into(),
        );
    };

    let props = fetch_props(&http, base, &mut warnings).await;
    let env_version = std::env::var("LLAMA_SERVER_VERSION")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(|value| bounded_redacted(&value, MAX_STRING_BYTES));
    let (llama_cpp_server_version, version_source) = if let Some(version) = env_version {
        (version, "LLAMA_SERVER_VERSION".to_owned())
    } else if let Some((version, source)) = props.as_ref().and_then(version_from_props) {
        (version, source)
    } else {
        warnings.push(
            "llama-server version unavailable; set LLAMA_SERVER_VERSION or expose GET /props"
                .into(),
        );
        ("unavailable".to_owned(), "unavailable".to_owned())
    };
    let server_context_tokens = props.as_ref().and_then(context_from_props);

    Ok(ServerMetadata {
        model_id: bounded_redacted(&model_id, MAX_STRING_BYTES),
        model_source,
        llama_cpp_server_version,
        version_source,
        server_context_tokens,
        warnings,
    })
}

async fn fetch_props(
    http: &reqwest::Client,
    base: &str,
    warnings: &mut Vec<String>,
) -> Option<Value> {
    for path in ["/props", "/v1/props"] {
        match http.get(format!("{base}{path}")).send().await {
            Ok(response) if response.status().is_success() => {
                match response.json::<Value>().await {
                    Ok(value) => return Some(value),
                    Err(error) => {
                        warnings.push(format!("parse GET {path}: {error}"));
                    }
                }
            }
            Ok(response) => {
                warnings.push(format!("GET {path} returned HTTP {}", response.status()));
            }
            Err(error) => {
                warnings.push(format!("GET {path} failed: {error}"));
            }
        }
    }
    None
}

fn version_from_props(props: &Value) -> Option<(String, String)> {
    for field in ["build_info", "version", "system_info"] {
        let Some(value) = props.get(field).and_then(Value::as_str) else {
            continue;
        };
        if value.trim().is_empty() {
            continue;
        }
        return Some((
            bounded_redacted(value, MAX_STRING_BYTES),
            format!("GET /props.{field}"),
        ));
    }
    None
}

fn context_from_props(props: &Value) -> Option<u32> {
    props
        .get("default_generation_settings")
        .and_then(|settings| settings.get("n_ctx"))
        .or_else(|| props.get("n_ctx"))
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .filter(|value| *value > 0)
}

fn normal_request() -> ChatRequest {
    ChatRequest::new(vec![
        ChatMessage::system("You are a local-model compatibility smoke test."),
        ChatMessage::user("Reply with exactly: ok"),
    ])
    .with_max_tokens(64)
    .with_temperature(0.0)
}

fn cancellation_request() -> ChatRequest {
    ChatRequest::new(vec![
        ChatMessage::system("You are a local-model compatibility smoke test."),
        ChatMessage::user(
            "Write a long, detailed numbered list of 200 distinct facts. Continue until the token limit.",
        ),
    ])
    .with_max_tokens(512)
    .with_temperature(0.0)
}

async fn run_normal_attempt(config: &SmokeConfig, client: &OpenAiCompatClient) -> SmokeObservation {
    let started = Instant::now();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let stats_task = tokio::spawn(async move {
        let mut stats = StreamStats::default();
        while let Some(event) = rx.recv().await {
            collect_event(started, &mut stats, event);
        }
        stats
    });
    let result = timeout(
        config.request_timeout,
        client.stream_visible_chat(normal_request(), Some(&tx)),
    )
    .await;
    drop(tx);
    let stats = stats_task.await.unwrap_or_default();
    let elapsed_ms = elapsed_ms(started);

    match result {
        Ok(Ok(outcome)) => observation_from_outcome("normal", outcome, stats, elapsed_ms),
        Ok(Err(error)) => failed_observation("normal", stats, elapsed_ms, error),
        Err(_) => failed_observation(
            "normal",
            stats,
            elapsed_ms,
            AgentError::Llm(format!(
                "request timed out after {} ms",
                config.request_timeout.as_millis()
            )),
        ),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CancelAbort {
    FirstVisibleToken,
    TokenGraceExpired,
    RequestTimeout,
}

async fn run_cancellation_attempt(
    config: &SmokeConfig,
    client: &OpenAiCompatClient,
) -> SmokeObservation {
    let started = Instant::now();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let task = tokio::spawn({
        let client = client.clone();
        async move {
            client
                .stream_visible_chat(cancellation_request(), Some(&tx))
                .await
        }
    });
    let deadline = sleep(config.request_timeout);
    tokio::pin!(deadline);
    let grace = sleep(config.cancel_grace);
    tokio::pin!(grace);
    let mut stats = StreamStats::default();

    loop {
        tokio::select! {
            _ = &mut deadline => {
                task.abort();
                let join = task.await;
                return cancellation_observation(
                    stats,
                    elapsed_ms(started),
                    join,
                    CancelAbort::RequestTimeout,
                );
            }
            _ = &mut grace => {
                task.abort();
                let join = task.await;
                return cancellation_observation(
                    stats,
                    elapsed_ms(started),
                    join,
                    CancelAbort::TokenGraceExpired,
                );
            }
            event = rx.recv() => {
                match event {
                    Some(VisibleChatEvent::TextDelta(text)) => {
                        collect_text(started, &mut stats, &text);
                        task.abort();
                        let join = task.await;
                        return cancellation_observation(
                            stats,
                            elapsed_ms(started),
                            join,
                            CancelAbort::FirstVisibleToken,
                        );
                    }
                    Some(VisibleChatEvent::Finish { .. }) => {
                        let join = task.await;
                        return cancellation_observation(
                            stats,
                            elapsed_ms(started),
                            join,
                            CancelAbort::FirstVisibleToken,
                        );
                    }
                    Some(_) => {}
                    None => {
                        let join = task.await;
                        return cancellation_observation(
                            stats,
                            elapsed_ms(started),
                            join,
                            CancelAbort::FirstVisibleToken,
                        );
                    }
                }
            }
        }
    }
}

fn collect_event(started: Instant, stats: &mut StreamStats, event: VisibleChatEvent) {
    if let VisibleChatEvent::TextDelta(text) = event {
        collect_text(started, stats, &text);
    }
}

fn collect_text(started: Instant, stats: &mut StreamStats, text: &str) {
    if stats.first_token_ms.is_none() && !text.is_empty() {
        stats.first_token_ms = Some(elapsed_ms(started));
    }
    stats.visible_chars = stats.visible_chars.saturating_add(text.chars().count());
    stats.visible_tokens = stats
        .visible_tokens
        .saturating_add(estimate_text_tokens(text));
}

fn observation_from_outcome(
    scenario: &str,
    outcome: VisibleChatOutcome,
    stats: StreamStats,
    elapsed_ms: u64,
) -> SmokeObservation {
    SmokeObservation {
        scenario: scenario.to_owned(),
        outcome: if outcome.empty_visible_response {
            "empty".to_owned()
        } else {
            "completed".to_owned()
        },
        finish_reason: Some(bounded_redacted(&outcome.finish_reason, MAX_STRING_BYTES)),
        time_to_first_token_ms: stats.first_token_ms,
        elapsed_ms,
        visible_chars: stats.visible_chars,
        visible_tokens: stats.visible_tokens,
        empty_visible_response: outcome.empty_visible_response,
        error: None,
    }
}

fn failed_observation(
    scenario: &str,
    stats: StreamStats,
    elapsed_ms: u64,
    error: AgentError,
) -> SmokeObservation {
    SmokeObservation {
        scenario: scenario.to_owned(),
        outcome: "failed".to_owned(),
        finish_reason: None,
        time_to_first_token_ms: stats.first_token_ms,
        elapsed_ms,
        visible_chars: stats.visible_chars,
        visible_tokens: stats.visible_tokens,
        empty_visible_response: false,
        error: Some(bounded_redacted(&error.to_string(), MAX_STRING_BYTES)),
    }
}

fn cancellation_observation(
    stats: StreamStats,
    elapsed_ms: u64,
    join: Result<Result<VisibleChatOutcome, AgentError>, JoinError>,
    abort: CancelAbort,
) -> SmokeObservation {
    match join {
        Ok(Ok(outcome)) => SmokeObservation {
            scenario: "cancellation".to_owned(),
            outcome: "completed_before_cancel".to_owned(),
            finish_reason: Some(bounded_redacted(&outcome.finish_reason, MAX_STRING_BYTES)),
            time_to_first_token_ms: stats.first_token_ms,
            elapsed_ms,
            visible_chars: stats.visible_chars,
            visible_tokens: stats.visible_tokens,
            empty_visible_response: outcome.empty_visible_response,
            error: None,
        },
        Ok(Err(error)) => failed_observation("cancellation", stats, elapsed_ms, error),
        Err(error) if error.is_cancelled() => SmokeObservation {
            scenario: "cancellation".to_owned(),
            outcome: if stats.first_token_ms.is_some() {
                "cancelled_after_first_visible_token".to_owned()
            } else if abort == CancelAbort::RequestTimeout {
                "no_visible_token_before_timeout".to_owned()
            } else {
                "cancelled_before_first_visible_token".to_owned()
            },
            finish_reason: None,
            time_to_first_token_ms: stats.first_token_ms,
            elapsed_ms,
            visible_chars: stats.visible_chars,
            visible_tokens: stats.visible_tokens,
            empty_visible_response: false,
            error: None,
        },
        Err(error) => SmokeObservation {
            scenario: "cancellation".to_owned(),
            outcome: "failed".to_owned(),
            finish_reason: None,
            time_to_first_token_ms: stats.first_token_ms,
            elapsed_ms,
            visible_chars: stats.visible_chars,
            visible_tokens: stats.visible_tokens,
            empty_visible_response: false,
            error: Some(bounded_redacted(
                &format!("cancellation task failed: {error}"),
                MAX_STRING_BYTES,
            )),
        },
    }
}

fn build_report(
    config: &SmokeConfig,
    metadata: &ServerMetadata,
    observations: Vec<SmokeObservation>,
    cancellation: SmokeObservation,
) -> SmokeReport {
    let normal_attempts_completed = observations
        .iter()
        .filter(|observation| observation.outcome != "failed")
        .count();
    let failed_attempts = observations
        .iter()
        .filter(|observation| observation.outcome == "failed")
        .count();
    let empty_responses = observations
        .iter()
        .filter(|observation| observation.outcome == "empty")
        .count();
    let mut finish_reasons = BTreeMap::new();
    for observation in &observations {
        let reason = observation
            .finish_reason
            .clone()
            .unwrap_or_else(|| "transport_error".to_owned());
        *finish_reasons.entry(reason).or_insert(0) += 1;
    }
    let mut time_to_first_token_samples_ms: Vec<u64> = observations
        .iter()
        .filter_map(|observation| observation.time_to_first_token_ms)
        .collect();
    time_to_first_token_samples_ms.sort_unstable();
    let time_to_first_token_ms = median(&time_to_first_token_samples_ms);
    let empty_response_rate = if normal_attempts_completed == 0 {
        0.0
    } else {
        empty_responses as f64 / normal_attempts_completed as f64
    };
    let prompt_budget_tokens = config
        .context_cap_tokens
        .saturating_sub(config.reserved_output_tokens);
    let cancellation_result = cancellation.outcome.clone();

    SmokeReport {
        schema_version: SCHEMA_VERSION,
        generated_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        endpoint: log_safe_endpoint(&config.base),
        model_id: metadata.model_id.clone(),
        model_source: metadata.model_source.clone(),
        llama_cpp_server_version: metadata.llama_cpp_server_version.clone(),
        version_source: metadata.version_source.clone(),
        context_cap_tokens: config.context_cap_tokens,
        reserved_output_tokens: config.reserved_output_tokens,
        prompt_budget_tokens,
        server_context_tokens: metadata.server_context_tokens,
        attempts_requested: config.attempts,
        attempts_recorded: observations.len(),
        normal_attempts_completed,
        failed_attempts,
        empty_responses,
        empty_response_rate,
        time_to_first_token_ms,
        time_to_first_token_samples_ms,
        finish_reasons,
        cancellation_result,
        cancellation,
        observations,
        discovery_warnings: metadata.warnings.clone(),
    }
}

fn write_report(report: &SmokeReport, output_dir: &Path) -> Result<PathBuf, String> {
    std::fs::create_dir_all(output_dir)
        .map_err(|error| format!("create {}: {error}", output_dir.display()))?;
    let bytes = serde_json::to_vec_pretty(report)
        .map_err(|error| format!("serialize smoke report: {error}"))?;
    if bytes.len() > MAX_REPORT_BYTES {
        return Err(format!(
            "smoke report is {} bytes, exceeding the {} byte limit",
            bytes.len(),
            MAX_REPORT_BYTES
        ));
    }
    let timestamp = Utc::now().format("%Y%m%dT%H%M%SZ");
    let path = output_dir.join(format!("run-{timestamp}-{}.json", std::process::id()));
    std::fs::write(&path, &bytes).map_err(|error| format!("write {}: {error}", path.display()))?;
    let latest = output_dir.join("latest.json");
    std::fs::write(&latest, &bytes)
        .map_err(|error| format!("write {}: {error}", latest.display()))?;
    Ok(path)
}

fn median(sorted: &[u64]) -> Option<u64> {
    match sorted.len() {
        0 => None,
        len if len % 2 == 1 => Some(sorted[len / 2]),
        len => {
            let left = sorted[len / 2 - 1];
            let right = sorted[len / 2];
            Some(left.saturating_add(right) / 2)
        }
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn bounded_redacted(input: &str, max_bytes: usize) -> String {
    let redacted = redact(input).unwrap_or_else(|_| "<redaction-failed>".to_owned());
    truncate_utf8(&redacted, max_bytes)
}

fn truncate_utf8(input: &str, max_bytes: usize) -> String {
    if input.len() <= max_bytes {
        return input.to_owned();
    }
    let suffix = "...<truncated>";
    let budget = max_bytes.saturating_sub(suffix.len());
    let mut end = budget.min(input.len());
    while end > 0 && !input.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{suffix}", &input[..end])
}

fn parse_env<T>(name: &str, default: T) -> Result<T, String>
where
    T: FromStr,
    T::Err: Display,
{
    match std::env::var(name) {
        Ok(value) if !value.trim().is_empty() => value
            .parse()
            .map_err(|error| format!("{name} is invalid: {error}")),
        _ => Ok(default),
    }
}

#[test]
fn report_contains_required_compatibility_fields_and_rates() {
    let config = SmokeConfig {
        base: "http://127.0.0.1:8080".into(),
        explicit_model: None,
        attempts: 3,
        context_cap_tokens: 16_384,
        reserved_output_tokens: 4_096,
        output_dir: PathBuf::from("target/local-model-smoke"),
        request_timeout: Duration::from_secs(1),
        cancel_grace: Duration::from_secs(1),
    };
    let metadata = ServerMetadata {
        model_id: "local-test".into(),
        model_source: "GET /v1/models".into(),
        llama_cpp_server_version: "b1234-test".into(),
        version_source: "GET /props.build_info".into(),
        server_context_tokens: Some(8_192),
        warnings: vec!["warning".into()],
    };
    let observations = vec![
        smoke_observation("completed", Some("stop"), Some(120), false, None),
        smoke_observation("empty", Some("stop"), None, true, None),
        smoke_observation("failed", None, None, false, Some("boom")),
    ];
    let cancellation = smoke_observation(
        "cancelled_after_first_visible_token",
        None,
        Some(50),
        false,
        None,
    );

    let report = build_report(&config, &metadata, observations, cancellation);
    let value = serde_json::to_value(&report).expect("report serializes");

    for field in [
        "model_id",
        "llama_cpp_server_version",
        "context_cap_tokens",
        "reserved_output_tokens",
        "time_to_first_token_ms",
        "empty_response_rate",
        "finish_reasons",
        "cancellation_result",
        "observations",
    ] {
        assert!(value.get(field).is_some(), "missing report field {field}");
    }
    assert_eq!(report.empty_responses, 1);
    assert_eq!(report.normal_attempts_completed, 2);
    assert_eq!(report.empty_response_rate, 0.5);
    assert_eq!(report.time_to_first_token_ms, Some(120));
    assert_eq!(report.finish_reasons.get("stop"), Some(&2));
    assert_eq!(report.finish_reasons.get("transport_error"), Some(&1));
    assert_eq!(
        report.cancellation_result,
        "cancelled_after_first_visible_token"
    );
    assert!(
        serde_json::to_vec_pretty(&report)
            .expect("serialize report")
            .len()
            <= MAX_REPORT_BYTES
    );
}

#[test]
fn report_bounds_and_redacts_recorded_strings() {
    let secret = "Bearer sk-test-secret-token".repeat(100);
    let bounded = bounded_redacted(&secret, 80);
    assert!(bounded.len() <= 80);
    assert!(!bounded.contains("sk-test-secret-token"));
    assert!(bounded.ends_with("...<truncated>"));
}

fn smoke_observation(
    outcome: &str,
    finish_reason: Option<&str>,
    first_token_ms: Option<u64>,
    empty: bool,
    error: Option<&str>,
) -> SmokeObservation {
    SmokeObservation {
        scenario: "normal".into(),
        outcome: outcome.into(),
        finish_reason: finish_reason.map(str::to_owned),
        time_to_first_token_ms: first_token_ms,
        elapsed_ms: first_token_ms.unwrap_or_default(),
        visible_chars: 2,
        visible_tokens: 1,
        empty_visible_response: empty,
        error: error.map(str::to_owned),
    }
}
