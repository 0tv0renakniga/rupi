//! Isolated adapter for the assumed Jev System 1 HTTP contract.
//!
//! The wire format is deliberately private to this module. A deployment whose
//! Jev service uses a different contract only needs to change this adapter.
//! Jev is advisory: its outputs cannot authorize tools, bypass policy, execute
//! MCP calls, claim verification passed, or complete a task.
//!
//! Assumed contract:
//!
//! - `POST /v1/choice` with `{"prompt", "options"}` -> `{"index", "confidence"}`
//! - `POST /v1/noul` with `{"claim", "evidence"}` -> `{"pass", "confidence"}`
//! - `POST /v1/score` with `{"query", "docs"}` -> `{"scores"}`
//!
//! The base URL comes from validated configuration. Request callers cannot
//! supply a URL, path, host, scheme, redirect target, or proxy configuration.

use std::fmt;
use std::time::Duration;

use reqwest::header::{ACCEPT, CONTENT_TYPE};
use reqwest::redirect::Policy;
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::time::timeout;

use crate::config::log_safe_endpoint;

const CHOICE_PATH: &str = "/v1/choice";
const NOUL_PATH: &str = "/v1/noul";
const SCORE_PATH: &str = "/v1/score";

const TRUNCATION_MARKER: &str = "...[truncated]";
const MIN_TIMEOUT: Duration = Duration::from_millis(1);
const MAX_CONNECT_TIMEOUT: Duration = Duration::from_millis(500);
const MAX_REQUEST_TIMEOUT: Duration = Duration::from_millis(2_000);

/// Validated System 1 routing decision. Raw wire values never escape the
/// adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    Pick(usize),
    Escalate,
}

/// Validated System 1 review verdict.
///
/// A `Pass` is advisory evidence only. It never substitutes for a real
/// verifier exit code and can never complete a task.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Verdict {
    Pass(f32),
    Fail(f32),
    Uncertain,
}

/// Local request bounds for Jev. Fields larger than their byte limit are
/// truncated at a UTF-8 boundary before the request is built. If the complete
/// JSON body still exceeds `max_request_bytes`, no HTTP request is sent and the
/// operation returns its safe fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct JevLimits {
    pub connect_timeout: Duration,
    pub request_timeout: Duration,
    pub max_prompt_bytes: usize,
    pub max_claim_bytes: usize,
    pub max_evidence_bytes: usize,
    pub max_query_bytes: usize,
    pub max_doc_bytes: usize,
    pub max_docs: usize,
    pub max_options: usize,
    pub max_option_bytes: usize,
    pub max_request_bytes: usize,
    pub max_response_bytes: usize,
    /// When true (the default), every score must be finite and in `[0.0, 1.0]`.
    /// Set this to false only when the deployed adapter contract intentionally
    /// returns arbitrary finite ranking values.
    pub require_unit_scores: bool,
}

impl Default for JevLimits {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_millis(250),
            request_timeout: Duration::from_millis(800),
            max_prompt_bytes: 4 * 1024,
            max_claim_bytes: 2 * 1024,
            max_evidence_bytes: 8 * 1024,
            max_query_bytes: 2 * 1024,
            max_doc_bytes: 4 * 1024,
            max_docs: 32,
            max_options: 16,
            max_option_bytes: 512,
            max_request_bytes: 64 * 1024,
            max_response_bytes: 64 * 1024,
            require_unit_scores: true,
        }
    }
}

impl JevLimits {
    fn bounded(mut self) -> Self {
        self.request_timeout = self.request_timeout.clamp(MIN_TIMEOUT, MAX_REQUEST_TIMEOUT);
        self.connect_timeout = self
            .connect_timeout
            .clamp(MIN_TIMEOUT, MAX_CONNECT_TIMEOUT)
            .min(self.request_timeout);
        self
    }
}

/// HTTP adapter for the assumed Jev contract.
#[derive(Clone)]
pub struct JevClient {
    base: Option<String>,
    http: Option<reqwest::Client>,
    limits: JevLimits,
}

impl fmt::Debug for JevClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let endpoint = self
            .base
            .as_deref()
            .map(log_safe_endpoint)
            .unwrap_or_else(|| "<invalid-endpoint>".to_owned());
        formatter
            .debug_struct("JevClient")
            .field("endpoint", &endpoint)
            .field("available", &self.http.is_some())
            .field("limits", &self.limits)
            .finish()
    }
}

impl JevClient {
    /// Build a client with bounded defaults. An invalid endpoint or client
    /// construction failure leaves the adapter unavailable, so every operation
    /// fails safely without sending a request.
    pub fn new(base: impl Into<String>) -> Self {
        Self::with_limits(base, JevLimits::default())
    }

    /// Build a client with explicit local limits. Timeouts are clamped to a
    /// bounded System 1 range. The base may include a path prefix; fixed
    /// endpoint paths are appended to it.
    pub fn with_limits(base: impl Into<String>, limits: JevLimits) -> Self {
        let base = base.into();
        let limits = limits.bounded();
        let endpoint = normalize_base(&base);

        let http = endpoint.as_ref().and_then(|_| {
            reqwest::Client::builder()
                .redirect(Policy::none())
                .no_proxy()
                .connect_timeout(limits.connect_timeout)
                .timeout(limits.request_timeout)
                .build()
                .ok()
        });

        if endpoint.is_none() || http.is_none() {
            return Self {
                base: None,
                http: None,
                limits,
            };
        }

        Self {
            base: endpoint,
            http,
            limits,
        }
    }

    /// Rank a bounded set of options. Any validation or transport failure
    /// escalates to System 2. Empty or oversized option arrays and invalid
    /// confidence thresholds never cause a request.
    pub async fn choice(&self, prompt: &str, options: &[&str], minimum_confidence: f32) -> Route {
        if options.is_empty()
            || options.len() > self.limits.max_options
            || !is_unit_interval(minimum_confidence)
        {
            return Route::Escalate;
        }

        let request = ChoiceRequest {
            prompt: truncate_utf8(prompt, self.limits.max_prompt_bytes),
            options: options
                .iter()
                .map(|option| truncate_utf8(option, self.limits.max_option_bytes))
                .collect(),
        };
        let Some(raw) = self.post::<RawChoice>(CHOICE_PATH, &request).await else {
            return Route::Escalate;
        };

        let Ok(index) = usize::try_from(raw.index) else {
            return Route::Escalate;
        };
        if index >= options.len()
            || !is_unit_interval(raw.confidence)
            || raw.confidence < minimum_confidence
        {
            return Route::Escalate;
        }

        Route::Pick(index)
    }

    /// Evaluate a claimed completion against evidence. A valid high-confidence
    /// pass or fail is returned; every other outcome, including a low-confidence
    /// result, is `Uncertain`. `Pass` remains advisory only.
    pub async fn noul(&self, claim: &str, evidence: &str, threshold: f32) -> Verdict {
        if !is_unit_interval(threshold) {
            return Verdict::Uncertain;
        }

        let request = NoulRequest {
            claim: truncate_utf8(claim, self.limits.max_claim_bytes),
            evidence: truncate_utf8(evidence, self.limits.max_evidence_bytes),
        };
        let Some(raw) = self.post::<RawNoul>(NOUL_PATH, &request).await else {
            return Verdict::Uncertain;
        };

        if !is_unit_interval(raw.confidence) || raw.confidence < threshold {
            return Verdict::Uncertain;
        }

        if raw.pass {
            Verdict::Pass(raw.confidence)
        } else {
            Verdict::Fail(raw.confidence)
        }
    }

    /// Score bounded documents without reordering them. `None` means Jev was
    /// unavailable, returned unusable data, or received more than `max_docs`
    /// documents (which is rejected locally without an HTTP request). Empty
    /// input returns an empty vector locally without an HTTP request.
    pub async fn score(&self, query: &str, docs: &[String]) -> Option<Vec<f32>> {
        if docs.is_empty() {
            return Some(Vec::new());
        }
        if docs.len() > self.limits.max_docs {
            return None;
        }

        let request = ScoreRequest {
            query: truncate_utf8(query, self.limits.max_query_bytes),
            docs: docs
                .iter()
                .map(|doc| truncate_utf8(doc, self.limits.max_doc_bytes))
                .collect(),
        };
        let raw = self.post::<RawScore>(SCORE_PATH, &request).await?;

        if raw.scores.len() != docs.len()
            || !raw.scores.iter().all(|score| score.is_finite())
            || (self.limits.require_unit_scores
                && !raw.scores.iter().all(|score| is_unit_interval(*score)))
        {
            return None;
        }

        Some(raw.scores)
    }

    async fn post<T: DeserializeOwned>(&self, path: &str, payload: &impl Serialize) -> Option<T> {
        let http = self.http.as_ref()?;
        let base = self.base.as_deref()?;
        let body = serde_json::to_vec(payload).ok()?;
        if body.len() > self.limits.max_request_bytes {
            return None;
        }

        let request = http
            .post(format!("{base}{path}"))
            .header(CONTENT_TYPE, "application/json")
            .header(ACCEPT, "application/json")
            .body(body);

        let response = match timeout(self.limits.request_timeout, request.send()).await {
            Ok(Ok(response)) => response,
            _ => return None,
        };
        if !response.status().is_success() {
            return None;
        }

        let body = read_bounded_body(response, self.limits.max_response_bytes).await?;
        serde_json::from_slice(&body).ok()
    }
}

#[derive(Debug, Serialize)]
struct ChoiceRequest {
    prompt: String,
    options: Vec<String>,
}

#[derive(Debug, Serialize)]
struct NoulRequest {
    claim: String,
    evidence: String,
}

#[derive(Debug, Serialize)]
struct ScoreRequest {
    query: String,
    docs: Vec<String>,
}

#[derive(Debug, serde::Deserialize)]
struct RawChoice {
    index: i64,
    confidence: f32,
}

#[derive(Debug, serde::Deserialize)]
struct RawNoul {
    pass: bool,
    confidence: f32,
}

#[derive(Debug, serde::Deserialize)]
struct RawScore {
    scores: Vec<f32>,
}

fn normalize_base(raw: &str) -> Option<String> {
    let url = reqwest::Url::parse(raw).ok()?;
    if url.query().is_some() || url.fragment().is_some() {
        return None;
    }
    crate::config::validate_endpoint("jev_base", raw).ok()?;
    Some(raw.trim_end_matches('/').to_owned())
}

fn is_unit_interval(value: f32) -> bool {
    value.is_finite() && (0.0..=1.0).contains(&value)
}

fn truncate_utf8(input: &str, max_bytes: usize) -> String {
    if input.len() <= max_bytes {
        return input.to_owned();
    }
    if max_bytes == 0 {
        return String::new();
    }
    if max_bytes <= TRUNCATION_MARKER.len() {
        let mut end = max_bytes;
        while end > 0 && !TRUNCATION_MARKER.is_char_boundary(end) {
            end -= 1;
        }
        return TRUNCATION_MARKER[..end].to_owned();
    }

    let mut end = max_bytes - TRUNCATION_MARKER.len();
    while end > 0 && !input.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{TRUNCATION_MARKER}", &input[..end])
}

async fn read_bounded_body(mut response: reqwest::Response, max_bytes: usize) -> Option<Vec<u8>> {
    if response
        .content_length()
        .is_some_and(|length| length > max_bytes as u64)
    {
        return None;
    }

    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.ok()? {
        if body.len().saturating_add(chunk.len()) > max_bytes {
            return None;
        }
        body.extend_from_slice(&chunk);
    }
    Some(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confidence_validation_rejects_non_finite_and_out_of_range_values() {
        for value in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -0.1, 1.1] {
            assert!(!is_unit_interval(value), "{value} must be invalid");
        }
        for value in [0.0, 0.5, 1.0] {
            assert!(is_unit_interval(value), "{value} must be valid");
        }
    }

    #[test]
    fn truncation_is_utf8_safe_and_bounded() {
        let truncated = truncate_utf8(&"é".repeat(100), 21);
        assert!(truncated.len() <= 21);
        assert!(truncated.ends_with(TRUNCATION_MARKER));
        assert!(std::str::from_utf8(truncated.as_bytes()).is_ok());
    }

    #[test]
    fn invalid_or_non_loopback_bases_remain_unavailable_in_default_build() {
        #[cfg(not(feature = "remote"))]
        {
            let client = JevClient::new("https://example.com");
            assert!(client.http.is_none());
            assert!(client.base.is_none());
        }
    }

    #[test]
    fn debug_output_uses_safe_endpoint_only() {
        let client = JevClient::new("http://127.0.0.1:9090/private?token=secret");
        let output = format!("{client:?}");
        assert!(!output.contains("secret"));
        assert!(!output.contains("/private"));
    }
}
