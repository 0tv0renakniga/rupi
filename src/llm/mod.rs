pub mod openai_compat;
pub mod prompts;

pub use openai_compat::{LlmLimits, OpenAiCompatClient};

use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::UnboundedSender;

use crate::error::Result;

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct FnCall {
    pub name: String,
    pub arguments: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub function: FnCall,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ChatMessage {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl ChatMessage {
    pub fn new(role: &str, text: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            content: Some(text.into()),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    pub fn system(text: impl Into<String>) -> Self {
        Self::new("system", text)
    }

    pub fn user(text: impl Into<String>) -> Self {
        Self::new("user", text)
    }

    pub fn assistant(text: impl Into<String>) -> Self {
        Self::new("assistant", text)
    }

    pub fn assistant_with_tool_calls(text: impl Into<String>, tool_calls: Vec<ToolCall>) -> Self {
        Self {
            role: "assistant".into(),
            content: Some(text.into()),
            tool_calls: Some(tool_calls),
            tool_call_id: None,
        }
    }

    pub fn tool(id: String, text: String) -> Self {
        Self {
            role: "tool".into(),
            content: Some(text),
            tool_calls: None,
            tool_call_id: Some(id),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ChatRequest {
    pub messages: Vec<ChatMessage>,
    pub tools: serde_json::Value,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
}

impl Default for ChatRequest {
    fn default() -> Self {
        Self {
            messages: Vec::new(),
            tools: serde_json::Value::Array(Vec::new()),
            temperature: None,
            max_tokens: None,
        }
    }
}

impl ChatRequest {
    pub fn new(messages: Vec<ChatMessage>) -> Self {
        Self {
            messages,
            ..Self::default()
        }
    }

    pub fn with_tools(mut self, tools: serde_json::Value) -> Self {
        self.tools = tools;
        self
    }

    pub fn with_temperature(mut self, temperature: f32) -> Self {
        self.temperature = Some(temperature);
        self
    }

    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = Some(max_tokens);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LlmStreamEvent {
    TextDelta(String),
    ToolCallDelta {
        index: usize,
        id: Option<String>,
        name: Option<String>,
        arguments: Option<String>,
    },
    Finished {
        finish_reason: Option<String>,
    },
}

#[derive(Default, Clone, PartialEq, Eq)]
pub struct AssistantTurn {
    pub text: String,
    pub calls: Vec<ToolCall>,
    pub finish: String,
    pub reasoning_content: Option<String>,
}

impl std::fmt::Debug for AssistantTurn {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AssistantTurn")
            .field("text", &self.text)
            .field("calls", &self.calls)
            .field("finish", &self.finish)
            .finish()
    }
}

impl AssistantTurn {
    pub fn as_message(&self) -> ChatMessage {
        ChatMessage {
            role: "assistant".into(),
            content: (!self.text.is_empty()).then(|| self.text.clone()),
            tool_calls: (!self.calls.is_empty()).then(|| self.calls.clone()),
            tool_call_id: None,
        }
    }

    /// Some local servers omit tool-call ids; synthesize stable local ids.
    pub fn finish_ids(mut self) -> Self {
        for (index, call) in self.calls.iter_mut().enumerate() {
            if call.id.is_empty() {
                call.id = format!("call_{index}");
            }
        }
        self
    }
}

#[async_trait::async_trait]
pub trait LlmBackend: Send + Sync {
    async fn stream_chat(
        &self,
        request: ChatRequest,
        event_tx: Option<&UnboundedSender<LlmStreamEvent>>,
    ) -> Result<AssistantTurn>;

    async fn complete_text(&self, system: &str, user: &str) -> Result<String>;
}
