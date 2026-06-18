//! agent-rs — a production-shaped LLM agent loop.
//!
//! `perceive -> plan -> act -> observe`, with recoverable errors, native LLM
//! tool-use, and budgets/observability. See `AGENTS.md` and `docs/`.
//!
//! Build order: P1 ships the pure types below; the loop, tools, provider, and
//! planner land in subsequent phases.

pub mod action;
pub mod budget;
pub mod error;
pub mod event;
pub mod memory;
pub mod planner;
pub mod provider;
pub mod tool;

pub use action::{Action, ActionOutcome, RecoverableError};
pub use budget::{Budget, TerminalReason};
pub use error::{AgentError, PlannerError, ProviderError, ToolError};
pub use event::RunEvent;
pub use memory::{Memory, MemorySnapshot, Record};
pub use planner::{model::ModelPlanner, rule::RulePlanner};
pub use planner::{PlanContext, PlanOutput, Planner};
pub use provider::{fake::FakeProvider, openai::OpenAiProvider};
pub use provider::{Message, ModelRequest, ModelResponse, Provider, ToolCall, Usage};
pub use tool::{default_registry, Tool, ToolRegistry, ToolSchema};

use std::time::Instant;

use tokio::sync::mpsc;
use tokio::time::timeout;

/// A bounded agent runner. Drives `perceive -> plan -> act -> observe` to a
/// terminal state.
#[derive(Debug, Clone)]
pub struct Agent {
    pub budget: Budget,
}

/// The result of a run: why it ended, how many steps it took, the full event
/// log, and a memory snapshot for replay.
#[derive(Debug, Clone)]
pub struct RunReport {
    pub outcome: TerminalReason,
    pub steps: usize,
    pub events: Vec<RunEvent>,
    pub snapshot: MemorySnapshot,
}

impl Agent {
    pub fn new(budget: Budget) -> Self {
        Self { budget }
    }

    /// Run to a terminal state, collecting events into the report.
    pub async fn run(
        &self,
        goal: &str,
        planner: &dyn Planner,
        registry: &ToolRegistry,
    ) -> RunReport {
        self.run_with_events(goal, planner, registry, None).await
    }

    /// Like [`run`](Self::run) but also streams each event to `events_tx` as it
    /// happens (in addition to collecting them in the report).
    pub async fn run_with_events(
        &self,
        goal: &str,
        planner: &dyn Planner,
        registry: &ToolRegistry,
        events_tx: Option<mpsc::UnboundedSender<RunEvent>>,
    ) -> RunReport {
        let mut memory = Memory::new(goal);
        let mut events: Vec<RunEvent> = Vec::new();
        let mut tokens_used: u64 = 0;
        let started = Instant::now();
        let schemas = registry.schemas();
        let mut step = 0usize;

        loop {
            step += 1;
            if let Some(reason) = self.budget.exceeded(step, tokens_used, started.elapsed()) {
                // This step never ran, so the completed-step count is step - 1.
                return report(reason, step - 1, events, &memory);
            }
            emit(&mut events, &events_tx, RunEvent::StepStarted { step });

            // --- plan (with the wall-clock budget as a timeout) ---
            let plan = {
                let ctx = PlanContext {
                    step,
                    max_steps: self.budget.max_steps,
                    memory: &memory,
                    tools: &schemas,
                };
                let remaining = self.budget.remaining_time(started.elapsed());
                match timeout(remaining, planner.plan_next(&ctx)).await {
                    // Timeout drops (cancels) the in-flight plan future.
                    Err(_elapsed) => {
                        return report(TerminalReason::TimedOut, step, events, &memory)
                    }
                    // A provider/transport failure is fatal.
                    Ok(Err(PlannerError::Provider(e))) => {
                        return report(
                            TerminalReason::Fatal(AgentError::Provider(e)),
                            step,
                            events,
                            &memory,
                        )
                    }
                    // Malformed model output is recoverable: observe it and retry.
                    Ok(Err(PlannerError::Malformed(why))) => {
                        let error = RecoverableError::MalformedPlan(why);
                        observe_recoverable(
                            &mut memory,
                            &mut events,
                            &events_tx,
                            step,
                            None,
                            error,
                        );
                        continue;
                    }
                    Ok(Ok(plan)) => plan,
                }
            };

            // --- observe the plan ---
            tokens_used += plan.usage.total();
            emit(
                &mut events,
                &events_tx,
                RunEvent::Planned {
                    step,
                    thought: plan.thought.clone(),
                    actions: plan.actions.clone(),
                },
            );
            memory.record_plan(step, plan.thought.clone(), plan.actions.clone());

            // --- act on each action ---
            for action in &plan.actions {
                if let Err(error) = action.validate() {
                    let call_id = match action {
                        Action::CallTool { call_id, .. } => Some(call_id.clone()),
                        Action::Finish { .. } => None,
                    };
                    observe_recoverable(&mut memory, &mut events, &events_tx, step, call_id, error);
                    continue;
                }

                match action {
                    Action::Finish { message } => {
                        emit(
                            &mut events,
                            &events_tx,
                            RunEvent::Finished {
                                step,
                                message: message.clone(),
                            },
                        );
                        return report(
                            TerminalReason::Finished(message.clone()),
                            step,
                            events,
                            &memory,
                        );
                    }
                    Action::CallTool {
                        call_id,
                        name,
                        input,
                    } => {
                        emit(
                            &mut events,
                            &events_tx,
                            RunEvent::ToolCalled {
                                step,
                                name: name.clone(),
                                input: input.clone(),
                            },
                        );
                        match registry.get(name) {
                            None => observe_recoverable(
                                &mut memory,
                                &mut events,
                                &events_tx,
                                step,
                                Some(call_id.clone()),
                                RecoverableError::UnknownTool(name.clone()),
                            ),
                            Some(tool) => match tool.execute(input) {
                                Ok(output) => {
                                    memory.record_outcome(
                                        step,
                                        ActionOutcome::ToolResult {
                                            call_id: call_id.clone(),
                                            name: name.clone(),
                                            output: output.clone(),
                                        },
                                    );
                                    emit(
                                        &mut events,
                                        &events_tx,
                                        RunEvent::ToolSucceeded {
                                            step,
                                            name: name.clone(),
                                            output,
                                        },
                                    );
                                }
                                Err(e) => observe_recoverable(
                                    &mut memory,
                                    &mut events,
                                    &events_tx,
                                    step,
                                    Some(call_id.clone()),
                                    RecoverableError::ToolFailed {
                                        name: name.clone(),
                                        error: e.to_string(),
                                    },
                                ),
                            },
                        }
                    }
                }
            }
        }
    }
}

/// Push an event into the report log and, if streaming, to the channel.
fn emit(events: &mut Vec<RunEvent>, tx: &Option<mpsc::UnboundedSender<RunEvent>>, event: RunEvent) {
    if let Some(tx) = tx {
        let _ = tx.send(event.clone());
    }
    events.push(event);
}

/// Record a recoverable error into memory and emit the matching event. This is
/// the heart of the design: the run continues afterward.
fn observe_recoverable(
    memory: &mut Memory,
    events: &mut Vec<RunEvent>,
    tx: &Option<mpsc::UnboundedSender<RunEvent>>,
    step: usize,
    call_id: Option<String>,
    error: RecoverableError,
) {
    memory.record_outcome(
        step,
        ActionOutcome::Recoverable {
            call_id,
            error: error.clone(),
        },
    );
    emit(events, tx, RunEvent::Recovered { step, error });
}

fn report(
    outcome: TerminalReason,
    steps: usize,
    events: Vec<RunEvent>,
    memory: &Memory,
) -> RunReport {
    RunReport {
        outcome,
        steps,
        snapshot: memory.snapshot(),
        events,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::planner::{PlanContext, PlanOutput, Planner};
    use async_trait::async_trait;
    use serde_json::json;
    use std::time::Duration;

    /// A planner that returns the same plan every step.
    struct Always(PlanOutput);

    #[async_trait]
    impl Planner for Always {
        async fn plan_next(&self, _ctx: &PlanContext<'_>) -> Result<PlanOutput, PlannerError> {
            Ok(self.0.clone())
        }
    }

    fn plan(actions: Vec<Action>, usage: Usage) -> PlanOutput {
        PlanOutput {
            thought: None,
            actions,
            usage,
        }
    }

    fn budget(max_steps: usize, max_tokens: u64) -> Budget {
        Budget {
            max_steps,
            max_tokens,
            wall_clock: Duration::from_secs(60),
        }
    }

    #[tokio::test]
    async fn finishes_on_finish_action() {
        let planner = Always(plan(
            vec![Action::Finish {
                message: "done".into(),
            }],
            Usage::default(),
        ));
        let report = Agent::new(budget(8, 1000))
            .run("goal", &planner, &default_registry())
            .await;
        assert_eq!(report.outcome, TerminalReason::Finished("done".into()));
        assert_eq!(report.steps, 1);
    }

    #[tokio::test]
    async fn stops_at_max_steps_when_never_finishing() {
        let planner = Always(plan(
            vec![Action::CallTool {
                call_id: "c".into(),
                name: "calculator".into(),
                input: json!({"expression": "1+1"}),
            }],
            Usage::default(),
        ));
        let report = Agent::new(budget(3, 1_000_000))
            .run("goal", &planner, &default_registry())
            .await;
        assert_eq!(report.outcome, TerminalReason::MaxSteps);
        assert_eq!(report.steps, 3);
    }

    #[tokio::test]
    async fn stops_when_token_budget_exceeded() {
        let planner = Always(plan(
            vec![Action::CallTool {
                call_id: "c".into(),
                name: "calculator".into(),
                input: json!({"expression": "1+1"}),
            }],
            Usage {
                input_tokens: 100,
                output_tokens: 0,
            },
        ));
        let report = Agent::new(budget(100, 10))
            .run("goal", &planner, &default_registry())
            .await;
        assert_eq!(report.outcome, TerminalReason::TokenBudget);
        assert_eq!(report.steps, 1);
    }

    #[tokio::test]
    async fn unknown_tool_is_recovered_not_fatal() {
        let planner = Always(plan(
            vec![Action::CallTool {
                call_id: "c".into(),
                name: "frobnicate".into(),
                input: json!({}),
            }],
            Usage::default(),
        ));
        let report = Agent::new(budget(1, 1_000_000))
            .run("goal", &planner, &default_registry())
            .await;
        // Ran out of steps rather than dying on the unknown tool.
        assert_eq!(report.outcome, TerminalReason::MaxSteps);
        assert!(report.events.iter().any(|e| matches!(
            e,
            RunEvent::Recovered {
                error: RecoverableError::UnknownTool(_),
                ..
            }
        )));
    }
}
