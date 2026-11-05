use std::fmt;

use serde_json::Value;

use crate::config::{CONTEXT_SAFETY_MARGIN_TOKENS, Limits};
use crate::llm::ChatMessage;

const TOKEN_ESTIMATE_BYTES_PER_TOKEN: usize = 3;
const MESSAGE_OVERHEAD_TOKENS: u32 = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextAdmission {
    pub effective_context_cap_tokens: u32,
    pub reserved_output_tokens: u32,
    pub safety_margin_tokens: u32,
}

impl ContextAdmission {
    pub fn new(
        effective_context_cap_tokens: u32,
        reserved_output_tokens: u32,
        safety_margin_tokens: u32,
    ) -> Self {
        Self {
            effective_context_cap_tokens,
            reserved_output_tokens,
            safety_margin_tokens,
        }
    }

    pub fn from_limits(limits: &Limits) -> Self {
        Self::new(
            limits.effective_context_cap_tokens(),
            limits.reserved_output_tokens,
            CONTEXT_SAFETY_MARGIN_TOKENS,
        )
    }

    pub fn prompt_budget_tokens(&self) -> u32 {
        self.effective_context_cap_tokens
            .saturating_sub(self.reserved_output_tokens)
            .saturating_sub(self.safety_margin_tokens)
    }

    pub fn plan(&self, messages: &[ChatMessage], tools: &Value) -> ContextPlan {
        ContextPlan {
            estimated_prompt_tokens: estimate_prompt_tokens(messages, tools),
            reserved_output_tokens: self.reserved_output_tokens,
            safety_margin_tokens: self.safety_margin_tokens,
            effective_context_cap_tokens: self.effective_context_cap_tokens,
        }
    }

    pub fn admit(
        &self,
        messages: &[ChatMessage],
        tools: &Value,
    ) -> std::result::Result<ContextPlan, ContextRejection> {
        let plan = self.plan(messages, tools);
        if plan.estimated_prompt_tokens <= self.prompt_budget_tokens() {
            Ok(plan)
        } else {
            Err(ContextRejection { plan })
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextPlan {
    pub estimated_prompt_tokens: u32,
    pub reserved_output_tokens: u32,
    pub safety_margin_tokens: u32,
    pub effective_context_cap_tokens: u32,
}

impl ContextPlan {
    pub fn total_tokens(self) -> u32 {
        self.estimated_prompt_tokens
            .saturating_add(self.reserved_output_tokens)
            .saturating_add(self.safety_margin_tokens)
    }

    pub fn utilization_percent(self) -> u32 {
        if self.effective_context_cap_tokens == 0 {
            return 0;
        }
        let percent = u64::from(self.estimated_prompt_tokens).saturating_mul(100)
            / u64::from(self.effective_context_cap_tokens);
        u32::try_from(percent).unwrap_or(u32::MAX).min(100)
    }

    pub fn available_prompt_tokens(self) -> u32 {
        self.effective_context_cap_tokens
            .saturating_sub(self.reserved_output_tokens)
            .saturating_sub(self.safety_margin_tokens)
    }

    pub fn rejection_message(self) -> String {
        format!(
            "context admission rejected: estimated prompt {} + reserved {} + safety {} exceeds effective cap {} tokens (prompt budget {})",
            self.estimated_prompt_tokens,
            self.reserved_output_tokens,
            self.safety_margin_tokens,
            self.effective_context_cap_tokens,
            self.available_prompt_tokens()
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextRejection {
    pub plan: ContextPlan,
}

impl ContextRejection {
    pub fn message(self) -> String {
        self.plan.rejection_message()
    }
}

impl fmt::Display for ContextRejection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.plan.rejection_message())
    }
}

pub fn estimate_prompt_tokens(messages: &[ChatMessage], tools: &Value) -> u32 {
    let mut bytes = 0usize;
    for message in messages {
        bytes = bytes.saturating_add(message.role.len());
        if let Some(content) = &message.content {
            bytes = bytes.saturating_add(content.len());
        }
        if let Some(tool_call_id) = &message.tool_call_id {
            bytes = bytes.saturating_add(tool_call_id.len());
        }
        if let Some(tool_calls) = &message.tool_calls {
            let serialized = serde_json::to_vec(tool_calls).unwrap_or_default();
            bytes = bytes.saturating_add(serialized.len());
        }
    }
    let has_tools = match tools {
        Value::Null => false,
        Value::Array(tools) => !tools.is_empty(),
        _ => true,
    };
    if has_tools {
        let serialized = serde_json::to_vec(tools).unwrap_or_default();
        bytes = bytes.saturating_add(serialized.len());
    }

    let message_tokens = u32::try_from(messages.len())
        .unwrap_or(u32::MAX)
        .saturating_mul(MESSAGE_OVERHEAD_TOKENS);
    estimate_bytes_tokens(bytes).saturating_add(message_tokens)
}

pub fn estimate_text_tokens(text: &str) -> u32 {
    estimate_bytes_tokens(text.len())
}

fn estimate_bytes_tokens(bytes: usize) -> u32 {
    let tokens =
        bytes.saturating_add(TOKEN_ESTIMATE_BYTES_PER_TOKEN - 1) / TOKEN_ESTIMATE_BYTES_PER_TOKEN;
    u32::try_from(tokens).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn admission() -> ContextAdmission {
        ContextAdmission::new(1_000, 200, 100)
    }

    #[test]
    fn admission_formula_reserves_output_and_safety_margin() {
        let admission = admission();
        assert_eq!(admission.prompt_budget_tokens(), 700);

        let admitted = admission
            .admit(&[ChatMessage::user("x".repeat(600))], &Value::Null)
            .expect("small prompt should fit");
        assert_eq!(admitted.estimated_prompt_tokens, 210);
        assert_eq!(admitted.total_tokens(), 510);
        assert_eq!(admitted.utilization_percent(), 21);
    }

    #[test]
    fn oversized_prompt_is_rejected_before_delivery() {
        let rejection = admission()
            .admit(&[ChatMessage::user("x".repeat(3_000))], &Value::Null)
            .expect_err("oversized prompt must be rejected");
        assert_eq!(rejection.plan.estimated_prompt_tokens, 1_010);
        assert!(rejection.message().contains("context admission rejected"));
        assert!(rejection.message().contains("prompt budget 700"));
    }

    #[test]
    fn estimates_include_message_overhead_and_tool_definitions() {
        let plain = estimate_prompt_tokens(&[ChatMessage::user("hello")], &Value::Null);
        assert_eq!(plain, 11);
        let with_tools = estimate_prompt_tokens(
            &[ChatMessage::user("hello")],
            &serde_json::json!([{"type":"function","function":{"name":"read_file"}}]),
        );
        assert!(with_tools > plain);
    }

    #[test]
    fn text_estimation_is_utf8_safe_and_bounded() {
        assert_eq!(estimate_text_tokens(""), 0);
        assert_eq!(estimate_text_tokens("abc"), 1);
        assert_eq!(estimate_text_tokens("東京"), 2);
        assert!(estimate_text_tokens(&"x".repeat(10_000)) > 3_000);
    }

    #[test]
    fn limits_map_to_configured_cap_reserve_and_margin() {
        let limits = Limits::default();
        let admission = ContextAdmission::from_limits(&limits);
        assert_eq!(
            admission.effective_context_cap_tokens,
            limits.context_cap_tokens
        );
        assert_eq!(
            admission.reserved_output_tokens,
            limits.reserved_output_tokens
        );
        assert_eq!(admission.safety_margin_tokens, CONTEXT_SAFETY_MARGIN_TOKENS);
        assert_eq!(
            admission.prompt_budget_tokens(),
            limits.prompt_token_budget_tokens()
        );
    }
}
