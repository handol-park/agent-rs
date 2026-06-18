//! A deterministic, offline planner. It runs without a model so the example
//! binary works with no API key. It is a stand-in, not intelligent: it treats
//! the goal as an arithmetic expression to evaluate, then finishes with the
//! result.

use async_trait::async_trait;
use serde_json::json;

use super::{PlanContext, PlanOutput, Planner};
use crate::action::{Action, ActionOutcome};
use crate::error::PlannerError;
use crate::memory::Record;
use crate::provider::Usage;

/// Evaluates the goal once via the `calculator` tool, then finishes.
pub struct RulePlanner;

impl RulePlanner {
    fn finish(message: String) -> PlanOutput {
        PlanOutput {
            thought: None,
            actions: vec![Action::Finish { message }],
            usage: Usage::default(),
        }
    }
}

#[async_trait]
impl Planner for RulePlanner {
    async fn plan_next(&self, ctx: &PlanContext<'_>) -> Result<PlanOutput, PlannerError> {
        let output = match ctx.memory.records().last() {
            // First step: try to evaluate the goal as an expression.
            None => {
                if ctx.tools.iter().any(|t| t.name == "calculator") {
                    PlanOutput {
                        thought: Some("treat the goal as an expression to evaluate".into()),
                        actions: vec![Action::CallTool {
                            call_id: "rule-1".into(),
                            name: "calculator".into(),
                            input: json!({ "expression": ctx.memory.goal() }),
                        }],
                        usage: Usage::default(),
                    }
                } else {
                    Self::finish(format!(
                        "No tools available; goal was: {}",
                        ctx.memory.goal()
                    ))
                }
            }
            // The tool returned — report and finish.
            Some(Record::Outcome {
                outcome: ActionOutcome::ToolResult { output, .. },
                ..
            }) => Self::finish(format!("Result: {output}")),
            // The tool (or action) failed — report and finish.
            Some(Record::Outcome {
                outcome: ActionOutcome::Recoverable { error, .. },
                ..
            }) => Self::finish(format!("Could not complete the goal: {}", error.message())),
            // Anything else: nothing left to do.
            _ => Self::finish("Nothing to do.".into()),
        };
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::Memory;
    use crate::tool::ToolSchema;

    fn calc_schema() -> ToolSchema {
        ToolSchema {
            name: "calculator".into(),
            description: "math".into(),
            parameters: json!({"type": "object"}),
        }
    }

    fn ctx<'a>(memory: &'a Memory, tools: &'a [ToolSchema]) -> PlanContext<'a> {
        PlanContext {
            step: 1,
            max_steps: 3,
            memory,
            tools,
        }
    }

    #[tokio::test]
    async fn first_step_calls_calculator_with_goal() {
        let memory = Memory::new("2 + 3");
        let tools = [calc_schema()];
        let out = RulePlanner.plan_next(&ctx(&memory, &tools)).await.unwrap();
        match &out.actions[0] {
            Action::CallTool { name, input, .. } => {
                assert_eq!(name, "calculator");
                assert_eq!(input["expression"], "2 + 3");
            }
            other => panic!("expected CallTool, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn finishes_with_result_after_tool() {
        let mut memory = Memory::new("2 + 3");
        memory.record_outcome(
            1,
            ActionOutcome::ToolResult {
                call_id: "rule-1".into(),
                name: "calculator".into(),
                output: json!({"result": 5.0}),
            },
        );
        let tools = [calc_schema()];
        let out = RulePlanner.plan_next(&ctx(&memory, &tools)).await.unwrap();
        match &out.actions[0] {
            Action::Finish { message } => assert!(message.contains("result")),
            other => panic!("expected Finish, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn finishes_when_no_tools() {
        let memory = Memory::new("hello");
        let out = RulePlanner.plan_next(&ctx(&memory, &[])).await.unwrap();
        assert!(matches!(out.actions[0], Action::Finish { .. }));
    }
}
