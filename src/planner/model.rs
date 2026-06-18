//! A planner backed by a real model. It owns a `Box<dyn Provider>` (runtime
//! dispatch), rebuilds the chat transcript from memory each turn, and maps the
//! model's native `tool_calls` into [`Action`]s. No hand-rolled JSON-from-prose
//! parsing — the provider already returns structured tool calls.

use async_trait::async_trait;

use super::{PlanContext, PlanOutput, Planner};
use crate::action::{Action, ActionOutcome};
use crate::error::PlannerError;
use crate::memory::Record;
use crate::provider::{Message, ModelRequest, ModelResponse, Provider, ToolCall};

const DEFAULT_SYSTEM: &str = "You are an autonomous agent. Use the available tools to accomplish the \
user's goal. Call a tool when you need one; when the goal is complete, reply with a final answer and \
no tool call. If a tool returns an error, read it and correct your next call.";

/// Plans by calling a model provider.
pub struct ModelPlanner {
    provider: Box<dyn Provider>,
    system: String,
}

impl ModelPlanner {
    pub fn new(provider: Box<dyn Provider>) -> Self {
        Self {
            provider,
            system: DEFAULT_SYSTEM.to_string(),
        }
    }

    pub fn with_system(provider: Box<dyn Provider>, system: impl Into<String>) -> Self {
        Self {
            provider,
            system: system.into(),
        }
    }
}

#[async_trait]
impl Planner for ModelPlanner {
    async fn plan_next(&self, ctx: &PlanContext<'_>) -> Result<PlanOutput, PlannerError> {
        let request = build_request(&self.system, ctx);
        let response = self.provider.complete(&request).await?;
        map_response(response)
    }
}

/// Rebuild the full, stateless request from run memory.
fn build_request(system: &str, ctx: &PlanContext<'_>) -> ModelRequest {
    let mut messages = vec![Message::User {
        content: ctx.memory.goal().to_string(),
    }];

    for record in ctx.memory.records() {
        match record {
            Record::Plan {
                thought, actions, ..
            } => {
                let tool_calls: Vec<ToolCall> = actions
                    .iter()
                    .filter_map(|a| match a {
                        Action::CallTool {
                            call_id,
                            name,
                            input,
                        } => Some(ToolCall {
                            id: call_id.clone(),
                            name: name.clone(),
                            arguments: input.clone(),
                        }),
                        Action::Finish { .. } => None,
                    })
                    .collect();
                if thought.is_some() || !tool_calls.is_empty() {
                    messages.push(Message::Assistant {
                        content: thought.clone(),
                        tool_calls,
                    });
                }
            }
            Record::Outcome { outcome, .. } => match outcome {
                ActionOutcome::ToolResult {
                    call_id, output, ..
                } => messages.push(Message::Tool {
                    call_id: call_id.clone(),
                    content: output.to_string(),
                }),
                ActionOutcome::Recoverable { call_id, error } => match call_id {
                    // A failed tool call is replayed as that call's (error) result.
                    Some(id) => messages.push(Message::Tool {
                        call_id: id.clone(),
                        content: error.message(),
                    }),
                    // A failure not tied to a call goes back as a plain nudge.
                    None => messages.push(Message::User {
                        content: error.message(),
                    }),
                },
                ActionOutcome::Finished { .. } => {}
            },
        }
    }

    ModelRequest {
        system: system.to_string(),
        messages,
        tools: ctx.tools.to_vec(),
    }
}

/// Map a model response into a plan. Tool calls become `CallTool`; a text-only
/// response is a `Finish`; an empty response is a recoverable malformed plan.
fn map_response(response: ModelResponse) -> Result<PlanOutput, PlannerError> {
    let usage = response.usage;
    if !response.tool_calls.is_empty() {
        let actions = response
            .tool_calls
            .into_iter()
            .map(|tc| Action::CallTool {
                call_id: tc.id,
                name: tc.name,
                input: tc.arguments,
            })
            .collect();
        Ok(PlanOutput {
            thought: response.text,
            actions,
            usage,
        })
    } else if let Some(text) = response.text {
        Ok(PlanOutput {
            thought: None,
            actions: vec![Action::Finish { message: text }],
            usage,
        })
    } else {
        Err(PlannerError::Malformed(
            "model returned neither text nor tool calls".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action::RecoverableError;
    use crate::memory::Memory;
    use crate::provider::fake::FakeProvider;
    use crate::provider::ModelResponse;
    use serde_json::json;

    fn context<'a>(memory: &'a Memory) -> PlanContext<'a> {
        PlanContext {
            step: 1,
            max_steps: 3,
            memory,
            tools: &[],
        }
    }

    #[test]
    fn build_request_starts_with_goal_and_renders_outcomes() {
        let mut memory = Memory::new("add 2 and 2");
        memory.record_plan(
            1,
            None,
            vec![Action::CallTool {
                call_id: "c1".into(),
                name: "calculator".into(),
                input: json!({"expression": "2+2"}),
            }],
        );
        memory.record_outcome(
            1,
            ActionOutcome::Recoverable {
                call_id: Some("c1".into()),
                error: RecoverableError::ToolFailed {
                    name: "calculator".into(),
                    error: "boom".into(),
                },
            },
        );

        let req = build_request("sys", &context(&memory));
        assert_eq!(
            req.messages[0],
            Message::User {
                content: "add 2 and 2".into()
            }
        );
        assert!(matches!(req.messages[1], Message::Assistant { .. }));
        // The failed call is replayed as a tool result keyed by call_id.
        match &req.messages[2] {
            Message::Tool { call_id, content } => {
                assert_eq!(call_id, "c1");
                assert!(content.contains("boom"));
            }
            other => panic!("expected Tool message, got {other:?}"),
        }
    }

    #[test]
    fn map_response_tool_calls_become_actions() {
        let r = ModelResponse::tool_call("c1", "calculator", json!({"expression": "1+1"}))
            .with_usage(5, 2);
        let out = map_response(r).unwrap();
        assert_eq!(out.usage.total(), 7);
        assert!(matches!(out.actions[0], Action::CallTool { .. }));
    }

    #[test]
    fn map_response_text_becomes_finish() {
        let out = map_response(ModelResponse::text("all done")).unwrap();
        assert_eq!(
            out.actions,
            vec![Action::Finish {
                message: "all done".into()
            }]
        );
    }

    #[test]
    fn map_response_empty_is_malformed() {
        let empty = ModelResponse {
            text: None,
            tool_calls: vec![],
            usage: Default::default(),
        };
        assert!(matches!(
            map_response(empty),
            Err(PlannerError::Malformed(_))
        ));
    }

    #[tokio::test]
    async fn plan_next_drives_the_provider() {
        let provider = FakeProvider::new(vec![Ok(ModelResponse::tool_call(
            "c1",
            "calculator",
            json!({"expression": "2+2"}),
        ))]);
        let planner = ModelPlanner::new(Box::new(provider));
        let memory = Memory::new("compute 2+2");
        let out = planner.plan_next(&context(&memory)).await.unwrap();
        assert!(matches!(out.actions[0], Action::CallTool { .. }));
    }

    #[tokio::test]
    async fn provider_error_is_fatal_planner_error() {
        let provider = FakeProvider::new(vec![Err(crate::error::ProviderError::Transport(
            "offline".into(),
        ))]);
        let planner = ModelPlanner::new(Box::new(provider));
        let memory = Memory::new("g");
        assert!(matches!(
            planner.plan_next(&context(&memory)).await,
            Err(PlannerError::Provider(_))
        ));
    }
}
