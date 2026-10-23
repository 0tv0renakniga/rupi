use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::mpsc::UnboundedSender;

use crate::config::{log_safe_endpoint, validate_endpoint};
use crate::error::{AgentError, Result};
use crate::llm::{
    AssistantTurn, ChatMessage, ChatRequest, FnCall, LlmBackend, LlmStreamEvent, ToolCall,
};

const DEFAULT_TEMPERATURE: f32 = 0.2;
const MAX_TEMPERATURE: f32 = 2.0;
const MAX_TOKENS: u32 = 32_768;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LlmLimits {
    pub max_messages: usize,
    pub max_message_content_bytes: usize,
    pub max_tool_definitions: usize,
    pub max_tool_definition_bytes: usize,
    pub max_tool_calls: usize,
    pub max_tool_call_id_bytes: usize,
    pub max_tool_name_bytes: usize,
    pub max_tool_arguments_bytes: usize,
    pub max_reasoning_bytes: usize,
    pub max_finish_reason_bytes: usize,
    pub max_request_bytes: usize,
    pub max_sse_line_bytes: usize,
    pub max_stream_bytes: usize,
    pub max_assistant_bytes: usize,
    pub connect_timeout: Duration,
    pub request_timeout: Duration,
}

impl Default for LlmLimits {
    fn default() -> Self {
        Self {
            max_messages: 128,
            max_message_content_bytes: 64 * 1024,
            max_tool_definitions: 64,
            max_tool_definition_bytes: 16 * 1024,
            max_tool_calls: 32,
            max_tool_call_id_bytes: 256,
            max_tool_name_bytes: 128,
            max_tool_arguments_bytes: 64 * 1024,
            max_reasoning_bytes: 256 * 1024,
            max_finish_reason_bytes: 128,
            max_request_bytes: 1024 * 1024,
            max_sse_line_bytes: 64 * 1024,
            max_stream_bytes: 4 * 1024 * 1024,
            max_assistant_bytes: 1024 * 1024,
            connect_timeout: Duration::from_secs(2),
            request_timeout: Duration::from_secs(120),
        }
    }
}

#[derive(Clone)]
pub struct OpenAiCompatClient {
    pub base: String,
    pub model: String,
    pub max_tokens: u32,
    pub temperature: f32,
    pub limits: LlmLimits,
}

impl std::fmt::Debug for OpenAiCompatClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OpenAiCompatClient")
            .field("base", &log_safe_endpoint(&self.base))
            .field("model", &self.model)
            .field("max_tokens", &self.max_tokens)
            .field("temperature", &self.temperature)
            .field("limits", &self.limits)
            .finish()
    }
}

impl OpenAiCompatClient {
    pub fn new(base: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            base: base.into().trim_end_matches('/').to_owned(),
            model: model.into(),
            max_tokens: 4_096,
            temperature: DEFAULT_TEMPERATURE,
            limits: LlmLimits::default(),
        }
    }

    pub fn with_limits(mut self, limits: LlmLimits) -> Self {
        self.limits = limits;
        self
    }

    fn endpoint(&self) -> Result<String> {
        if self.model.trim().is_empty() {
            return Err(AgentError::Llm("model name must not be empty".into()));
        }

        let base = self.base.trim_end_matches('/');
        if base.is_empty() {
            return Err(AgentError::Llm("LLM base URL must not be empty".into()));
        }

        let url = reqwest::Url::parse(base)
            .map_err(|error| AgentError::Llm(format!("invalid LLM base URL: {error}")))?;
        if url.query().is_some() || url.fragment().is_some() {
            return Err(AgentError::Llm(
                "LLM base URL must not contain a query or fragment".into(),
            ));
        }

        validate_endpoint("llm_base", base).map_err(|error| AgentError::Llm(error.to_string()))?;

        Ok(format!("{base}/v1/chat/completions"))
    }

    fn http_client(&self) -> Result<reqwest::Client> {
        let connect_timeout = self
            .limits
            .connect_timeout
            .clamp(Duration::from_millis(1), Duration::from_secs(10));
        let request_timeout = self
            .limits
            .request_timeout
            .clamp(Duration::from_millis(1), Duration::from_secs(600));

        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .connect_timeout(connect_timeout)
            .timeout(request_timeout)
            .build()
            .map_err(|error| AgentError::Llm(format!("failed to build HTTP client: {error}")))
    }

    fn temperature(&self, request: &ChatRequest) -> f32 {
        let fallback = if self.temperature.is_finite() {
            self.temperature
        } else {
            DEFAULT_TEMPERATURE
        };
        let value = request.temperature.unwrap_or(fallback);
        let value = if value.is_finite() { value } else { fallback };
        value.clamp(0.0, MAX_TEMPERATURE)
    }

    fn max_tokens(&self, request: &ChatRequest) -> u32 {
        request
            .max_tokens
            .unwrap_or(self.max_tokens)
            .clamp(1, MAX_TOKENS)
    }

    fn build_body(&self, request: &ChatRequest, stream: bool) -> Result<Vec<u8>> {
        self.validate_messages(&request.messages)?;

        let tools: &[Value] = match &request.tools {
            Value::Null => &[],
            Value::Array(tools) => tools,
            _ => return Err(AgentError::Llm("tools must be a JSON array".into())),
        };
        if tools.len() > self.limits.max_tool_definitions {
            return Err(AgentError::Llm(
                "tool definitions exceeded the size limit".into(),
            ));
        }
        for tool in tools {
            let size = serde_json::to_vec(tool)?.len();
            if size > self.limits.max_tool_definition_bytes {
                return Err(AgentError::Llm(
                    "a tool definition exceeded the size limit".into(),
                ));
            }
        }

        let wire = WireRequest {
            model: self.model.trim(),
            messages: &request.messages,
            tools: (!tools.is_empty()).then_some(tools),
            stream,
            temperature: self.temperature(request),
            max_tokens: self.max_tokens(request),
        };
        let body = serde_json::to_vec(&wire)?;
        if body.len() > self.limits.max_request_bytes {
            return Err(AgentError::Llm(
                "request body exceeded the size limit".into(),
            ));
        }
        Ok(body)
    }

    fn validate_messages(&self, messages: &[ChatMessage]) -> Result<()> {
        if messages.is_empty() {
            return Err(AgentError::Llm("messages must not be empty".into()));
        }
        if messages.len() > self.limits.max_messages {
            return Err(AgentError::Llm("message count exceeded the limit".into()));
        }

        for message in messages {
            let role = message.role.as_str();
            if !matches!(role, "system" | "user" | "assistant" | "tool") {
                return Err(AgentError::Llm("unsupported message role".into()));
            }
            if role != "assistant" && message.content.is_none() {
                return Err(AgentError::Llm("message content must not be null".into()));
            }
            if message
                .content
                .as_ref()
                .is_some_and(|content| content.len() > self.limits.max_message_content_bytes)
            {
                return Err(AgentError::Llm(
                    "message content exceeded the size limit".into(),
                ));
            }
            if role == "tool"
                && message
                    .tool_call_id
                    .as_deref()
                    .is_none_or(|id| id.is_empty())
            {
                return Err(AgentError::Llm(
                    "tool messages require a non-empty tool_call_id".into(),
                ));
            }
            if message
                .tool_call_id
                .as_ref()
                .is_some_and(|id| id.len() > self.limits.max_tool_call_id_bytes)
            {
                return Err(AgentError::Llm(
                    "tool_call_id exceeded the size limit".into(),
                ));
            }
            if let Some(calls) = &message.tool_calls {
                if role != "assistant" {
                    return Err(AgentError::Llm(
                        "tool_calls are only valid on assistant messages".into(),
                    ));
                }
                if calls.len() > self.limits.max_tool_calls {
                    return Err(AgentError::Llm("tool-call count exceeded the limit".into()));
                }
                for call in calls {
                    if call.id.len() > self.limits.max_tool_call_id_bytes {
                        return Err(AgentError::Llm(
                            "tool-call id exceeded the size limit".into(),
                        ));
                    }
                    if !call.kind.is_empty() && call.kind != "function" {
                        return Err(AgentError::Llm("unsupported tool-call type".into()));
                    }
                    if call.function.name.len() > self.limits.max_tool_name_bytes {
                        return Err(AgentError::Llm("tool name exceeded the size limit".into()));
                    }
                    if call.function.arguments.len() > self.limits.max_tool_arguments_bytes {
                        return Err(AgentError::Llm(
                            "tool arguments exceeded the size limit".into(),
                        ));
                    }
                }
            }
        }
        Ok(())
    }

    async fn read_body(&self, response: reqwest::Response) -> Result<Vec<u8>> {
        let mut stream = response.bytes_stream();
        let mut body = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(transport_error)?;
            if body.len().saturating_add(chunk.len()) > self.limits.max_stream_bytes {
                return Err(AgentError::Llm(
                    "response body exceeded the size limit".into(),
                ));
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }

    async fn consume_sse(
        &self,
        response: reqwest::Response,
        event_tx: Option<&UnboundedSender<LlmStreamEvent>>,
    ) -> Result<AssistantTurn> {
        let mut stream = response.bytes_stream();
        let mut buffer = Vec::new();
        let mut turn = AssistantTurn::default();
        let mut calls = BTreeMap::new();
        let mut total_bytes = 0usize;
        let mut done = false;
        let mut finished_sent = false;

        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(transport_error)?;
            total_bytes = total_bytes.saturating_add(chunk.len());
            if total_bytes > self.limits.max_stream_bytes {
                return Err(AgentError::Llm(
                    "streamed response exceeded the size limit".into(),
                ));
            }
            buffer.extend_from_slice(&chunk);

            while let Some(position) = buffer.iter().position(|byte| *byte == b'\n') {
                let line_bytes: Vec<u8> = buffer.drain(..=position).collect();
                let line = &line_bytes[..line_bytes.len() - 1];
                let line = line.strip_suffix(b"\r").unwrap_or(line);
                if line.len() > self.limits.max_sse_line_bytes {
                    return Err(AgentError::Llm("SSE line exceeded the size limit".into()));
                }
                let line = std::str::from_utf8(line)
                    .map_err(|_| AgentError::Llm("SSE line was not valid UTF-8".into()))?;
                if line.is_empty() || line.starts_with(':') {
                    continue;
                }
                let Some(data) = line.strip_prefix("data:") else {
                    continue;
                };
                let data = data.strip_prefix(' ').unwrap_or(data).trim();
                if data.is_empty() {
                    continue;
                }
                if data == "[DONE]" {
                    done = true;
                    break;
                }
                self.handle_data(data, &mut turn, &mut calls, event_tx, &mut finished_sent)?;
            }

            if done {
                break;
            }
            if buffer.len() > self.limits.max_sse_line_bytes {
                return Err(AgentError::Llm("SSE line exceeded the size limit".into()));
            }
        }

        if !done && !buffer.is_empty() {
            let line = buffer.strip_suffix(b"\r").unwrap_or(&buffer);
            if line.len() <= self.limits.max_sse_line_bytes
                && let Ok(line) = std::str::from_utf8(line)
            {
                let data = line
                    .strip_prefix("data:")
                    .map(|data| data.strip_prefix(' ').unwrap_or(data).trim());
                if data == Some("[DONE]") {
                    done = true;
                }
            }
        }

        if !done {
            return Err(AgentError::Llm("stream ended before [DONE]".into()));
        }

        let assembled_bytes = turn
            .text
            .len()
            .saturating_add(calls.values().map(partial_call_size).sum::<usize>());
        if assembled_bytes > self.limits.max_assistant_bytes {
            return Err(AgentError::Llm(
                "assembled response exceeded the size limit".into(),
            ));
        }

        let mut seen_ids = BTreeSet::new();
        for (position, (_, partial)) in calls.into_iter().enumerate() {
            let id = if partial.id.is_empty() {
                format!("call_{position}")
            } else {
                partial.id
            };
            if !seen_ids.insert(id.clone()) {
                return Err(AgentError::Llm("duplicate tool-call id".into()));
            }
            let kind = if partial.kind.is_empty() {
                "function".into()
            } else {
                partial.kind
            };
            turn.calls.push(ToolCall {
                id,
                kind,
                function: FnCall {
                    name: partial.name,
                    arguments: partial.arguments,
                },
            });
        }

        if !finished_sent {
            send_event(
                event_tx,
                LlmStreamEvent::Finished {
                    finish_reason: (!turn.finish.is_empty()).then(|| turn.finish.clone()),
                },
            );
        }

        Ok(turn)
    }

    fn handle_data(
        &self,
        data: &str,
        turn: &mut AssistantTurn,
        calls: &mut BTreeMap<usize, PartialCall>,
        event_tx: Option<&UnboundedSender<LlmStreamEvent>>,
        finished_sent: &mut bool,
    ) -> Result<()> {
        let frame: StreamFrame =
            serde_json::from_str(data).map_err(|_| AgentError::Llm("malformed SSE JSON".into()))?;
        if frame.error.is_some() {
            return Err(AgentError::Llm(
                "llama-server returned a stream error".into(),
            ));
        }
        let Some(choices) = frame.choices else {
            return Ok(());
        };

        for choice in choices {
            if let Some(delta) = choice.delta {
                if let Some(content) = delta.content
                    && !content.is_empty()
                {
                    if turn.text.len().saturating_add(content.len())
                        > self.limits.max_assistant_bytes
                    {
                        return Err(AgentError::Llm(
                            "assistant text exceeded the size limit".into(),
                        ));
                    }
                    turn.text.push_str(&content);
                    send_event(event_tx, LlmStreamEvent::TextDelta(content));
                }
                if let Some(reasoning) = delta.reasoning_content
                    && !reasoning.is_empty()
                {
                    let reasoning_content = turn.reasoning_content.get_or_insert_with(String::new);
                    if reasoning_content.len().saturating_add(reasoning.len())
                        > self.limits.max_reasoning_bytes
                    {
                        return Err(AgentError::Llm(
                            "reasoning content exceeded the size limit".into(),
                        ));
                    }
                    reasoning_content.push_str(&reasoning);
                }
                if let Some(tool_calls) = delta.tool_calls {
                    for tool_call in tool_calls {
                        self.apply_tool_delta(tool_call, calls, event_tx)?;
                    }
                }
            }

            if let Some(reason) = choice.finish_reason {
                if reason.len() > self.limits.max_finish_reason_bytes {
                    return Err(AgentError::Llm(
                        "finish_reason exceeded the size limit".into(),
                    ));
                }
                turn.finish = reason.clone();
                if !*finished_sent {
                    send_event(
                        event_tx,
                        LlmStreamEvent::Finished {
                            finish_reason: Some(reason),
                        },
                    );
                    *finished_sent = true;
                }
            }
        }

        Ok(())
    }

    fn apply_tool_delta(
        &self,
        delta: StreamToolCall,
        calls: &mut BTreeMap<usize, PartialCall>,
        event_tx: Option<&UnboundedSender<LlmStreamEvent>>,
    ) -> Result<()> {
        let index = delta.index;
        if index >= self.limits.max_tool_calls {
            return Err(AgentError::Llm("tool-call index exceeded the limit".into()));
        }
        if !calls.contains_key(&index) && calls.len() >= self.limits.max_tool_calls {
            return Err(AgentError::Llm("tool-call count exceeded the limit".into()));
        }

        let event_id = delta.id.clone().filter(|id| !id.is_empty());
        let partial = calls.entry(index).or_default();

        if let Some(id) = delta.id
            && !id.is_empty()
        {
            if id.len() > self.limits.max_tool_call_id_bytes {
                return Err(AgentError::Llm(
                    "tool-call id exceeded the size limit".into(),
                ));
            }
            if partial.id.is_empty() {
                partial.id = id;
            } else if partial.id != id {
                return Err(AgentError::Llm("conflicting tool-call id".into()));
            }
        }

        if let Some(kind) = delta.kind
            && !kind.is_empty()
        {
            if kind != "function" {
                return Err(AgentError::Llm("unsupported tool-call type".into()));
            }
            partial.kind = kind;
        }

        let mut name_fragment = None;
        let mut arguments_fragment = None;
        if let Some(function) = delta.function {
            if let Some(name) = function.name
                && !name.is_empty()
            {
                if partial.name.len().saturating_add(name.len()) > self.limits.max_tool_name_bytes {
                    return Err(AgentError::Llm("tool name exceeded the size limit".into()));
                }
                partial.name.push_str(&name);
                name_fragment = Some(name);
            }
            if let Some(arguments) = function.arguments
                && !arguments.is_empty()
            {
                if partial.arguments.len().saturating_add(arguments.len())
                    > self.limits.max_tool_arguments_bytes
                {
                    return Err(AgentError::Llm(
                        "tool arguments exceeded the size limit".into(),
                    ));
                }
                partial.arguments.push_str(&arguments);
                arguments_fragment = Some(arguments);
            }
        }

        send_event(
            event_tx,
            LlmStreamEvent::ToolCallDelta {
                index,
                id: event_id,
                name: name_fragment,
                arguments: arguments_fragment,
            },
        );

        Ok(())
    }
}

#[async_trait::async_trait]
impl LlmBackend for OpenAiCompatClient {
    async fn stream_chat(
        &self,
        request: ChatRequest,
        event_tx: Option<&UnboundedSender<LlmStreamEvent>>,
    ) -> Result<AssistantTurn> {
        let body = self.build_body(&request, true)?;
        let endpoint = self.endpoint()?;
        let client = self.http_client()?;
        let response = client
            .post(endpoint)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header(reqwest::header::ACCEPT, "text/event-stream")
            .body(body)
            .send()
            .await
            .map_err(transport_error)?;
        let status = response.status();
        if !status.is_success() {
            return Err(AgentError::Llm(format!(
                "llama-server returned HTTP status {}",
                status.as_u16()
            )));
        }
        self.consume_sse(response, event_tx).await
    }

    async fn complete_text(&self, system: &str, user: &str) -> Result<String> {
        let request = ChatRequest::new(vec![
            ChatMessage::system(system.to_owned()),
            ChatMessage::user(user.to_owned()),
        ]);
        let body = self.build_body(&request, false)?;
        let endpoint = self.endpoint()?;
        let client = self.http_client()?;
        let response = client
            .post(endpoint)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header(reqwest::header::ACCEPT, "application/json")
            .body(body)
            .send()
            .await
            .map_err(transport_error)?;
        let status = response.status();
        if !status.is_success() {
            return Err(AgentError::Llm(format!(
                "llama-server returned HTTP status {}",
                status.as_u16()
            )));
        }

        let body = self.read_body(response).await?;
        let completion: CompletionResponse = serde_json::from_slice(&body).map_err(|_| {
            AgentError::Llm("llama-server returned malformed completion JSON".into())
        })?;
        let choice =
            completion.choices.into_iter().next().ok_or_else(|| {
                AgentError::Llm("llama-server returned no completion choices".into())
            })?;
        if choice
            .message
            .tool_calls
            .as_ref()
            .is_some_and(|calls| !calls.is_empty())
        {
            return Err(AgentError::Llm(
                "llama-server returned tool calls for a text-only completion".into(),
            ));
        }
        let content = choice.message.content.unwrap_or_default();
        if content.trim().is_empty() {
            return Err(AgentError::Llm(
                "llama-server returned an empty completion".into(),
            ));
        }
        Ok(content)
    }
}

fn transport_error(error: reqwest::Error) -> AgentError {
    let kind = if error.is_timeout() {
        "request timed out"
    } else if error.is_connect() {
        "could not connect"
    } else {
        "request failed"
    };
    AgentError::Llm(format!("llama-server {kind}"))
}

fn send_event(event_tx: Option<&UnboundedSender<LlmStreamEvent>>, event: LlmStreamEvent) {
    if let Some(event_tx) = event_tx {
        let _ = event_tx.send(event);
    }
}

fn partial_call_size(call: &PartialCall) -> usize {
    call.name
        .len()
        .saturating_add(call.arguments.len())
        .saturating_add(call.id.len())
        .saturating_add(call.kind.len())
}

#[derive(Default)]
struct PartialCall {
    id: String,
    kind: String,
    name: String,
    arguments: String,
}

#[derive(Serialize)]
struct WireRequest<'a> {
    model: &'a str,
    messages: &'a [ChatMessage],
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<&'a [Value]>,
    stream: bool,
    temperature: f32,
    max_tokens: u32,
}

#[derive(Deserialize)]
struct CompletionResponse {
    #[serde(default)]
    choices: Vec<CompletionChoice>,
}

#[derive(Deserialize)]
struct CompletionChoice {
    message: CompletionMessage,
}

#[derive(Deserialize)]
struct CompletionMessage {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<Value>>,
}

#[derive(Deserialize)]
struct StreamFrame {
    #[serde(default)]
    choices: Option<Vec<StreamChoice>>,
    #[serde(default)]
    error: Option<Value>,
}

#[derive(Deserialize)]
struct StreamChoice {
    #[serde(default)]
    delta: Option<StreamDelta>,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct StreamDelta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    reasoning_content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<StreamToolCall>>,
}

#[derive(Deserialize)]
struct StreamToolCall {
    #[serde(default)]
    index: usize,
    #[serde(default)]
    id: Option<String>,
    #[serde(rename = "type", default)]
    kind: Option<String>,
    #[serde(default)]
    function: Option<StreamFunction>,
}

#[derive(Deserialize)]
struct StreamFunction {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}
