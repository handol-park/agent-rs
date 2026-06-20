//! OpenAI-compatible `/chat/completions` adapter with native tool-calling.
//! Works against OpenAI, GLM, and most compatible gateways. Request building and
//! response parsing are pure functions so they are unit-tested with canned JSON,
//! no network.

use std::env;

use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{Message, ModelRequest, ModelResponse, Provider, ToolCall, Usage};
use crate::error::ProviderError;

/// A client for an OpenAI-compatible chat-completions endpoint.
pub struct OpenAiProvider {
    client: Client,
    base_url: String,
    api_key: String,
    model: String,
}

impl OpenAiProvider {
    /// `base_url` is the API root, e.g. `https://api.openai.com/v1` (a trailing
    /// slash is trimmed).
    pub fn new(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        model: impl Into<String>,
    ) -> Self {
        Self {
            client: Client::new(),
            base_url: base_url.into().trim_end_matches('/').to_string(),
            api_key: api_key.into(),
            model: model.into(),
        }
    }

    /// Build from `LLM_BASE_URL`, `LLM_API_KEY`, `LLM_MODEL`. Returns `None` if
    /// any is unset.
    pub fn from_env() -> Option<Self> {
        Self::from_vars(
            env::var("LLM_BASE_URL").ok(),
            env::var("LLM_API_KEY").ok(),
            env::var("LLM_MODEL").ok(),
        )
    }

    fn from_vars(
        base_url: Option<String>,
        api_key: Option<String>,
        model: Option<String>,
    ) -> Option<Self> {
        Some(Self::new(base_url?, api_key?, model?))
    }
}

#[async_trait]
impl Provider for OpenAiProvider {
    async fn complete(&self, request: &ModelRequest) -> Result<ModelResponse, ProviderError> {
        let body = build_body(request, &self.model);
        let response = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await
            .map_err(|e| ProviderError::Transport(e.to_string()))?;

        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|e| ProviderError::Transport(e.to_string()))?;
        if !status.is_success() {
            return Err(ProviderError::Api {
                status: status.as_u16(),
                body: text,
            });
        }
        parse_response(&text)
    }
}

// --- wire types (serde) ---

#[derive(Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<ChatTool>,
}

#[derive(Serialize)]
struct ChatMessage {
    role: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Vec<ChatToolCallOut>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
}

#[derive(Serialize)]
struct ChatToolCallOut {
    id: String,
    #[serde(rename = "type")]
    kind: &'static str,
    function: FunctionOut,
}

#[derive(Serialize)]
struct FunctionOut {
    name: String,
    /// OpenAI expects the arguments as a JSON-encoded *string*.
    arguments: String,
}

#[derive(Serialize)]
struct ChatTool {
    #[serde(rename = "type")]
    kind: &'static str,
    function: ToolFunction,
}

#[derive(Serialize)]
struct ToolFunction {
    name: String,
    description: String,
    parameters: Value,
}

#[derive(Deserialize)]
struct ChatResponse {
    #[serde(default)]
    choices: Vec<Choice>,
    #[serde(default)]
    usage: Option<UsageIn>,
}

#[derive(Deserialize)]
struct Choice {
    message: ChoiceMessage,
}

#[derive(Deserialize)]
struct ChoiceMessage {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<ToolCallIn>,
}

#[derive(Deserialize)]
struct ToolCallIn {
    id: String,
    function: FunctionIn,
}

#[derive(Deserialize)]
struct FunctionIn {
    name: String,
    arguments: String,
}

#[derive(Deserialize, Default)]
struct UsageIn {
    #[serde(default)]
    prompt_tokens: u64,
    #[serde(default)]
    completion_tokens: u64,
}

/// Build the chat-completions request body from a provider-agnostic request.
fn build_body(request: &ModelRequest, model: &str) -> ChatRequest {
    let mut messages = Vec::new();
    if !request.system.is_empty() {
        messages.push(ChatMessage {
            role: "system",
            content: Some(request.system.clone()),
            tool_calls: None,
            tool_call_id: None,
        });
    }
    for message in &request.messages {
        messages.push(match message {
            Message::User { content } => ChatMessage {
                role: "user",
                content: Some(content.clone()),
                tool_calls: None,
                tool_call_id: None,
            },
            Message::Assistant {
                content,
                tool_calls,
            } => ChatMessage {
                role: "assistant",
                content: content.clone(),
                tool_calls: (!tool_calls.is_empty()).then(|| {
                    tool_calls
                        .iter()
                        .map(|tc| ChatToolCallOut {
                            id: tc.id.clone(),
                            kind: "function",
                            function: FunctionOut {
                                name: tc.name.clone(),
                                arguments: tc.arguments.to_string(),
                            },
                        })
                        .collect()
                }),
                tool_call_id: None,
            },
            Message::Tool { call_id, content } => ChatMessage {
                role: "tool",
                content: Some(content.clone()),
                tool_calls: None,
                tool_call_id: Some(call_id.clone()),
            },
        });
    }

    let tools = request
        .tools
        .iter()
        .map(|schema| ChatTool {
            kind: "function",
            function: ToolFunction {
                name: schema.name.clone(),
                description: schema.description.clone(),
                parameters: schema.parameters.clone(),
            },
        })
        .collect();

    ChatRequest {
        model: model.to_string(),
        messages,
        tools,
    }
}

/// Parse a chat-completions response body into a provider-agnostic response.
fn parse_response(body: &str) -> Result<ModelResponse, ProviderError> {
    let parsed: ChatResponse =
        serde_json::from_str(body).map_err(|e| ProviderError::Decode(e.to_string()))?;
    let choice = parsed
        .choices
        .into_iter()
        .next()
        .ok_or_else(|| ProviderError::Decode("response had no choices".into()))?;

    let tool_calls = choice
        .message
        .tool_calls
        .into_iter()
        .map(|tc| ToolCall {
            id: tc.id,
            name: tc.function.name,
            // OpenAI encodes arguments as a JSON string; on garbage, fall back to
            // Null so the planner surfaces it as a recoverable malformed plan.
            arguments: serde_json::from_str(&tc.function.arguments).unwrap_or(Value::Null),
        })
        .collect();

    let usage = parsed.usage.unwrap_or_default();
    Ok(ModelResponse {
        text: choice.message.content,
        tool_calls,
        usage: Usage {
            input_tokens: usage.prompt_tokens,
            output_tokens: usage.completion_tokens,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tool::ToolSchema;
    use serde_json::json;

    #[test]
    fn from_vars_requires_all_three() {
        assert!(
            OpenAiProvider::from_vars(Some("u".into()), Some("k".into()), Some("m".into()))
                .is_some()
        );
        assert!(OpenAiProvider::from_vars(None, Some("k".into()), Some("m".into())).is_none());
        assert!(OpenAiProvider::from_vars(Some("u".into()), None, Some("m".into())).is_none());
    }

    #[test]
    fn build_body_maps_roles_and_tool_calls() {
        let request = ModelRequest {
            system: "be precise".into(),
            messages: vec![
                Message::User {
                    content: "add 1 and 2".into(),
                },
                Message::Assistant {
                    content: None,
                    tool_calls: vec![ToolCall {
                        id: "c1".into(),
                        name: "calculator".into(),
                        arguments: json!({"expression": "1+2"}),
                    }],
                },
                Message::Tool {
                    call_id: "c1".into(),
                    content: "{\"result\":3.0}".into(),
                },
            ],
            tools: vec![ToolSchema {
                name: "calculator".into(),
                description: "math".into(),
                parameters: json!({"type": "object"}),
            }],
        };
        let body = serde_json::to_value(build_body(&request, "gpt-test")).unwrap();

        assert_eq!(body["model"], "gpt-test");
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][1]["role"], "user");
        assert_eq!(body["messages"][2]["role"], "assistant");
        // arguments serialized as a JSON-encoded string
        assert_eq!(
            body["messages"][2]["tool_calls"][0]["function"]["arguments"],
            "{\"expression\":\"1+2\"}"
        );
        assert_eq!(body["messages"][3]["role"], "tool");
        assert_eq!(body["messages"][3]["tool_call_id"], "c1");
        assert_eq!(body["tools"][0]["function"]["name"], "calculator");
    }

    #[test]
    fn parse_response_reads_tool_call_and_usage() {
        let body = r#"{
            "choices": [{
                "message": {
                    "content": null,
                    "tool_calls": [{
                        "id": "call_abc",
                        "type": "function",
                        "function": { "name": "calculator", "arguments": "{\"expression\":\"2+2\"}" }
                    }]
                }
            }],
            "usage": { "prompt_tokens": 31, "completion_tokens": 9 }
        }"#;
        let r = parse_response(body).unwrap();
        assert_eq!(r.text, None);
        assert_eq!(r.tool_calls.len(), 1);
        assert_eq!(r.tool_calls[0].id, "call_abc");
        assert_eq!(r.tool_calls[0].arguments, json!({"expression": "2+2"}));
        assert_eq!(
            r.usage,
            Usage {
                input_tokens: 31,
                output_tokens: 9
            }
        );
    }

    #[test]
    fn parse_response_reads_plain_text() {
        let body = r#"{"choices":[{"message":{"content":"the answer is 4"}}]}"#;
        let r = parse_response(body).unwrap();
        assert_eq!(r.text.as_deref(), Some("the answer is 4"));
        assert!(r.tool_calls.is_empty());
        assert_eq!(r.usage, Usage::default());
    }

    #[test]
    fn parse_response_errors_on_no_choices() {
        let err = parse_response(r#"{"choices":[]}"#).unwrap_err();
        assert!(matches!(err, ProviderError::Decode(_)));
    }

    #[test]
    fn parse_response_errors_on_garbage() {
        assert!(matches!(
            parse_response("not json"),
            Err(ProviderError::Decode(_))
        ));
    }
}
