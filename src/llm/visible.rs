use serde::Deserialize;
use serde_json::Value;
use tokio::sync::mpsc::UnboundedSender;

use crate::error::Result;
use crate::llm::{ChatRequest, ContextAdmission, openai_compat::LlmLimits};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VisibleChatEvent {
    TextDelta(String),
    Finish {
        reason: String,
        empty_visible_response: bool,
    },
    Warning(String),
    Error(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VisibleChatOutcome {
    pub finish_reason: String,
    pub empty_visible_response: bool,
    pub hidden_reasoning_observed: bool,
}

#[async_trait::async_trait]
pub trait VisibleChatBackend: Send + Sync {
    fn context_admission(&self) -> Option<ContextAdmission> {
        Some(ContextAdmission::from_limits(
            &crate::config::Limits::default(),
        ))
    }

    async fn stream_visible_chat(
        &self,
        request: ChatRequest,
        event_tx: Option<&UnboundedSender<VisibleChatEvent>>,
    ) -> Result<VisibleChatOutcome>;
}

#[derive(Debug, Clone)]
pub struct VisibleStreamNormalizer {
    limits: LlmLimits,
    buffer: Vec<u8>,
    total_bytes: usize,
    visible_bytes: usize,
    hidden_reasoning_bytes: usize,
    finish_reason: Option<String>,
    visible_non_whitespace: bool,
    hidden_reasoning_observed: bool,
    future_action_seen: bool,
    done: bool,
    failed: bool,
}

impl Default for VisibleStreamNormalizer {
    fn default() -> Self {
        Self::new()
    }
}

impl VisibleStreamNormalizer {
    pub fn new() -> Self {
        Self::with_limits(LlmLimits::default())
    }

    pub fn with_limits(limits: LlmLimits) -> Self {
        Self {
            limits,
            buffer: Vec::new(),
            total_bytes: 0,
            visible_bytes: 0,
            hidden_reasoning_bytes: 0,
            finish_reason: None,
            visible_non_whitespace: false,
            hidden_reasoning_observed: false,
            future_action_seen: false,
            done: false,
            failed: false,
        }
    }

    pub fn push_chunk(&mut self, chunk: &[u8]) -> Vec<VisibleChatEvent> {
        let mut events = Vec::new();
        if self.done || self.failed {
            return events;
        }

        self.total_bytes = self.total_bytes.saturating_add(chunk.len());
        if self.total_bytes > self.limits.max_stream_bytes {
            self.fail("streamed response exceeded the size limit", &mut events);
            return events;
        }
        self.buffer.extend_from_slice(chunk);

        while let Some((position, consumed)) = next_line(&self.buffer) {
            let line = self.buffer[..position].to_vec();
            self.buffer.drain(..consumed);
            self.handle_line(&line, &mut events);
            if self.done || self.failed {
                return events;
            }
        }

        if self.buffer.len() > self.limits.max_sse_line_bytes {
            self.fail("SSE line exceeded the size limit", &mut events);
        }
        events
    }

    pub fn finish(&mut self) -> Vec<VisibleChatEvent> {
        let mut events = Vec::new();
        if self.done || self.failed {
            return events;
        }

        if !self.buffer.is_empty() {
            let mut line = std::mem::take(&mut self.buffer);
            if let Some(stripped) = line.strip_suffix(b"\r") {
                line.truncate(stripped.len());
            }
            self.handle_line(&line, &mut events);
        }

        if !self.done && !self.failed {
            self.fail("stream ended before [DONE]", &mut events);
        }
        events
    }

    pub fn hidden_reasoning_observed(&self) -> bool {
        self.hidden_reasoning_observed
    }

    pub fn finish_reason(&self) -> Option<&str> {
        self.finish_reason.as_deref()
    }

    pub fn visible_bytes(&self) -> usize {
        self.visible_bytes
    }

    pub fn is_done(&self) -> bool {
        self.done
    }

    pub fn is_failed(&self) -> bool {
        self.failed
    }

    fn handle_line(&mut self, line: &[u8], events: &mut Vec<VisibleChatEvent>) {
        if line.len() > self.limits.max_sse_line_bytes {
            self.fail("SSE line exceeded the size limit", events);
            return;
        }
        let Ok(line) = std::str::from_utf8(line) else {
            self.fail("SSE line was not valid UTF-8", events);
            return;
        };
        if line.is_empty() || line.starts_with(':') {
            return;
        }

        let data = match line.split_once(':') {
            Some(("data", value)) => value.strip_prefix(' ').unwrap_or(value).trim(),
            _ if line == "data" => "",
            _ => return,
        };
        if data.is_empty() {
            return;
        }
        if data == "[DONE]" {
            self.complete(events);
            return;
        }
        self.handle_data(data, events);
    }

    fn handle_data(&mut self, data: &str, events: &mut Vec<VisibleChatEvent>) {
        let frame: VisibleFrame = match serde_json::from_str(data) {
            Ok(frame) => frame,
            Err(_) => {
                self.fail("malformed SSE JSON", events);
                return;
            }
        };
        if frame.error.is_some() {
            self.fail("llama-server returned a stream error", events);
            return;
        }
        let Some(choices) = frame.choices else {
            return;
        };

        for choice in choices {
            if let Some(delta) = choice.delta {
                self.observe_hidden_reasoning(&delta);
                if self.hidden_reasoning_bytes > self.limits.max_reasoning_bytes {
                    self.fail("reasoning content exceeded the size limit", events);
                    return;
                }

                if let Some(content) = delta.content
                    && !content.is_empty()
                {
                    if self.visible_bytes.saturating_add(content.len())
                        > self.limits.max_assistant_bytes
                    {
                        self.fail("assistant text exceeded the size limit", events);
                        return;
                    }
                    self.visible_bytes = self.visible_bytes.saturating_add(content.len());
                    self.visible_non_whitespace |= content.chars().any(|ch| !ch.is_whitespace());
                    events.push(VisibleChatEvent::TextDelta(content));
                }

                self.future_action_seen |= delta
                    .tool_calls
                    .as_ref()
                    .is_some_and(|calls| !calls.is_empty());
                self.future_action_seen |= delta
                    .function_call
                    .as_ref()
                    .is_some_and(|call| !call.is_null());
            }

            if let Some(reason) = choice.finish_reason {
                if reason.len() > self.limits.max_finish_reason_bytes {
                    self.fail("finish_reason exceeded the size limit", events);
                    return;
                }
                self.finish_reason = Some(reason);
            }
        }
    }

    fn observe_hidden_reasoning(&mut self, delta: &VisibleDelta) {
        for field in [
            &delta.reasoning_content,
            &delta.reasoning,
            &delta.thinking,
            &delta.analysis,
        ] {
            let Some(value) = field else {
                continue;
            };
            if value.is_null() {
                continue;
            }
            self.hidden_reasoning_observed = true;
            let bytes = match value {
                Value::String(text) => text.len(),
                other => serde_json::to_string(other)
                    .map(|serialized| serialized.len())
                    .unwrap_or(0),
            };
            self.hidden_reasoning_bytes = self.hidden_reasoning_bytes.saturating_add(bytes);
        }
    }

    fn complete(&mut self, events: &mut Vec<VisibleChatEvent>) {
        self.done = true;
        let empty_visible_response = !self.visible_non_whitespace && !self.future_action_seen;
        let reason = self
            .finish_reason
            .clone()
            .filter(|reason| !reason.is_empty())
            .unwrap_or_else(|| "unknown".to_owned());

        if empty_visible_response {
            events.push(VisibleChatEvent::Warning(
                "empty visible response".to_owned(),
            ));
        }
        events.push(VisibleChatEvent::Finish {
            reason,
            empty_visible_response,
        });
    }

    fn fail(&mut self, message: &str, events: &mut Vec<VisibleChatEvent>) {
        if self.done || self.failed {
            return;
        }
        self.failed = true;
        self.buffer.clear();
        events.push(VisibleChatEvent::Error(message.to_owned()));
    }
}

pub fn normalize_sse_bytes(bytes: &[u8], limits: &LlmLimits) -> Vec<VisibleChatEvent> {
    let mut normalizer = VisibleStreamNormalizer::with_limits(limits.clone());
    let mut events = normalizer.push_chunk(bytes);
    events.extend(normalizer.finish());
    events
}

pub fn normalize_sse(bytes: &[u8]) -> Vec<VisibleChatEvent> {
    normalize_sse_bytes(bytes, &LlmLimits::default())
}

fn next_line(buffer: &[u8]) -> Option<(usize, usize)> {
    for (index, byte) in buffer.iter().enumerate() {
        match byte {
            b'\n' => return Some((index, index + 1)),
            b'\r' => {
                let consumed = if buffer.get(index + 1) == Some(&b'\n') {
                    index + 2
                } else {
                    index + 1
                };
                return Some((index, consumed));
            }
            _ => {}
        }
    }
    None
}

#[derive(Deserialize)]
struct VisibleFrame {
    #[serde(default)]
    choices: Option<Vec<VisibleChoice>>,
    #[serde(default)]
    error: Option<Value>,
}

#[derive(Deserialize)]
struct VisibleChoice {
    #[serde(default)]
    delta: Option<VisibleDelta>,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Deserialize)]
struct VisibleDelta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    reasoning_content: Option<Value>,
    #[serde(default)]
    reasoning: Option<Value>,
    #[serde(default)]
    thinking: Option<Value>,
    #[serde(default)]
    analysis: Option<Value>,
    #[serde(default)]
    tool_calls: Option<Vec<Value>>,
    #[serde(default)]
    function_call: Option<Value>,
}
