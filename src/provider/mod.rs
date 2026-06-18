//! The model provider abstraction: an async, runtime-dispatched boundary to an
//! LLM. The request/response types are provider-agnostic; concrete adapters
//! (OpenAI-compatible, fake) live in submodules.

use async_trait::async_trait;
use serde_json::Value;

use crate::error::ProviderError;
use crate::tool::ToolSchema;

pub mod fake;
pub mod openai;

/// An LLM behind native tool-calling. Runtime-dispatched (`Box<dyn Provider>`).
#[async_trait]
pub trait Provider: Send + Sync {
    async fn complete(&self, request: &ModelRequest) -> Result<ModelResponse, ProviderError>;
}

/// A full, stateless request to the model — rebuilt from memory each turn.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelRequest {
    pub system: String,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSchema>,
}

/// One message in the transcript sent to the model.
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    /// A user/goal message.
    User { content: String },
    /// An assistant turn that may carry text and/or tool-call requests.
    Assistant {
        content: Option<String>,
        tool_calls: Vec<ToolCall>,
    },
    /// A tool result, keyed back to the call that produced it.
    Tool { call_id: String, content: String },
}

/// A request from the model to invoke a tool. `arguments` is already parsed JSON.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

/// What the model returned: free text, tool-call requests, and token usage.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelResponse {
    pub text: Option<String>,
    pub tool_calls: Vec<ToolCall>,
    pub usage: Usage,
}

/// Token usage for one completion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

impl Usage {
    pub fn total(&self) -> u64 {
        self.input_tokens + self.output_tokens
    }
}

impl ModelResponse {
    /// A plain text completion (no tool calls).
    pub fn text(message: impl Into<String>) -> Self {
        Self {
            text: Some(message.into()),
            tool_calls: Vec::new(),
            usage: Usage::default(),
        }
    }

    /// A single tool-call completion.
    pub fn tool_call(id: impl Into<String>, name: impl Into<String>, arguments: Value) -> Self {
        Self {
            text: None,
            tool_calls: vec![ToolCall {
                id: id.into(),
                name: name.into(),
                arguments,
            }],
            usage: Usage::default(),
        }
    }

    /// Attach token usage (builder style).
    pub fn with_usage(mut self, input_tokens: u64, output_tokens: u64) -> Self {
        self.usage = Usage {
            input_tokens,
            output_tokens,
        };
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn usage_total_sums_tokens() {
        let r = ModelResponse::text("hi").with_usage(10, 4);
        assert_eq!(r.usage.total(), 14);
    }

    #[test]
    fn tool_call_builder_sets_one_call() {
        let r = ModelResponse::tool_call("c1", "calculator", json!({"expression": "1+1"}));
        assert!(r.text.is_none());
        assert_eq!(r.tool_calls.len(), 1);
        assert_eq!(r.tool_calls[0].name, "calculator");
    }
}
