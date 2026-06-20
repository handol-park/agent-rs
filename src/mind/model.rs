//! ModelMind: a model-backed Mind implementation (spec 002 goals 2-5).

use async_trait::async_trait;
use std::time::Duration;
use tokio::sync::mpsc::UnboundedSender;
use tokio::time::{sleep_until, Instant};

use crate::budget::{BudgetState, BudgetSummary, RenewableBudget};
use crate::error::{AgentError, ErrorClass, ProviderError};
use crate::event::RunEvent;
use crate::mind::{Command, Decision, Mind, Perception, Reason, TaskFault};
use crate::observation::{Observation, Outcome};
use crate::provider::{Message, ModelRequest, Provider};

/// A model-backed Mind that owns the provider, budget, working memory, and retry logic.
pub struct ModelMind {
    provider: Box<dyn Provider>,
    budget: RenewableBudget,
    budget_state: BudgetState,
    event_tx: UnboundedSender<RunEvent>,
    per_call_timeout: Duration,
    working_memory: Vec<Message>,
    resuming: bool,
    malformed_count: usize,
    backoff_seed: u64,
}

impl ModelMind {
    pub fn new(
        provider: Box<dyn Provider>,
        budget: RenewableBudget,
        event_tx: UnboundedSender<RunEvent>,
        per_call_timeout: Duration,
    ) -> Self {
        Self {
            provider,
            budget_state: BudgetState::new(Instant::now()),
            budget,
            event_tx,
            per_call_timeout,
            working_memory: Vec::new(),
            resuming: false,
            malformed_count: 0,
            backoff_seed: 12345, // Fixed seed for deterministic tests
        }
    }

    /// Fold a perception into working memory.
    fn fold(&mut self, perception: &Perception) {
        match perception {
            Perception::NewTask { goal } => {
                self.working_memory.clear();
                self.working_memory.push(Message::User {
                    content: goal.clone(),
                });
                self.resuming = false;
                self.malformed_count = 0;
            }
            Perception::Observation(obs) => {
                let msg = match obs {
                    Observation::ToolResult { call_id, output } => Message::Tool {
                        call_id: call_id.clone(),
                        content: output.to_string(),
                    },
                    Observation::Recoverable { call_id, error } => {
                        let error_msg = format!("{:?}", error); // Use Debug since Display is not implemented
                        match call_id {
                            Some(id) => Message::Tool {
                                call_id: id.clone(),
                                content: error_msg,
                            },
                            None => Message::User { content: error_msg },
                        }
                    }
                };
                self.working_memory.push(msg);
                self.malformed_count = 0; // Reset on new stimulus
            }
            Perception::Resume => {
                // Do not fold; working memory unchanged
            }
        }
    }

    /// Call the provider with retry logic (exponential backoff, unbounded retries).
    async fn call_with_retry(&mut self) -> Result<crate::provider::ModelResponse, ProviderError> {
        let mut attempt = 0;
        let base_delay = Duration::from_secs(1);
        let max_delay = Duration::from_secs(60);

        loop {
            let request = ModelRequest {
                system: "You are a helpful assistant.".to_string(),
                messages: self.working_memory.clone(),
                tools: Vec::new(), // TODO: pass tool schemas
            };

            let result =
                tokio::time::timeout(self.per_call_timeout, self.provider.complete(&request))
                    .await
                    .map_err(|_| ProviderError::Transport("call timeout".into()))?;

            match result {
                Ok(response) => return Ok(response),
                Err(e) => {
                    match e.class() {
                        ErrorClass::Transient => {
                            attempt += 1;
                            let delay = exponential_backoff(
                                attempt,
                                base_delay,
                                max_delay,
                                self.backoff_seed,
                            );
                            let _ = self.event_tx.send(RunEvent::RetryScheduled {
                                attempt,
                                delay,
                                error: e.clone(),
                            });
                            sleep_until(Instant::now() + delay).await;
                            // Continue retry loop
                        }
                        ErrorClass::ServiceFatal => return Err(e),
                        ErrorClass::TaskFatal => return Err(e),
                    }
                }
            }
        }
    }
}

#[async_trait]
impl Mind for ModelMind {
    async fn decide(&mut self, perception: Perception) -> Decision {
        // 1. Fold the perception
        self.fold(&perception);

        // 2. Attempt loop
        loop {
            let now = Instant::now();

            // Check budget window refresh
            if self.budget_state.refresh(now, &self.budget) {
                let _ = self.event_tx.send(RunEvent::WindowReset {
                    window: 0, // We'd track actual window number in a full impl
                });
            }

            // Check if exhausted
            if self.budget_state.exhausted(now, &self.budget) {
                if self.resuming {
                    // Freshly reset window still can't fund this decision
                    self.resuming = false;
                    return Decision::Failed(Reason::Task(TaskFault::BudgetTooSmall));
                } else {
                    self.resuming = true;
                    return Decision::Throttle(self.budget_state.next_reset(now, &self.budget));
                }
            }

            // Call provider
            let response = match self.call_with_retry().await {
                Ok(r) => r,
                Err(e) => {
                    return match e.class() {
                        ErrorClass::ServiceFatal => {
                            Decision::Failed(Reason::Service(AgentError::Provider(e)))
                        }
                        ErrorClass::TaskFatal => {
                            Decision::Failed(Reason::Task(TaskFault::BadRequest(e.to_string())))
                        }
                        ErrorClass::Transient => {
                            // Should never reach here (call_with_retry loops on transient)
                            unreachable!("call_with_retry should loop on transient errors")
                        }
                    };
                }
            };

            // Charge tokens
            let now = Instant::now();
            self.budget_state
                .charge(now, &self.budget, response.usage.total());

            // Map response to decision
            if let Some(text) = response.text {
                if !text.trim().is_empty() {
                    self.resuming = false;
                    return Decision::Done(Outcome { message: text });
                }
            }

            if !response.tool_calls.is_empty() {
                self.malformed_count = 0;
                self.resuming = false;
                // Return the first tool call as a command
                let tc = &response.tool_calls[0];
                return Decision::Act(Command::CallTool {
                    call_id: tc.id.clone(),
                    name: tc.name.clone(),
                    input: tc.arguments.clone(),
                });
            }

            // No usable command: malformed output
            self.malformed_count += 1;
            if self.malformed_count > 2 {
                self.resuming = false;
                return Decision::Failed(Reason::Task(TaskFault::Malformed(
                    "model produced no usable output after 3 attempts".into(),
                )));
            }

            // Re-prompt
            let error_msg = format!(
                "Your response contained no text and no tool calls. Please provide either a final answer or call a tool. (Attempt {}/2)",
                self.malformed_count
            );
            self.working_memory
                .push(Message::User { content: error_msg });
            // Loop to try again
        }
    }

    fn budget_summary(&self) -> BudgetSummary {
        let now = Instant::now();
        BudgetSummary::from_state(&self.budget_state, now, &self.budget)
    }

    fn set_event_sink(&mut self, events: UnboundedSender<RunEvent>) {
        self.event_tx = events;
    }
}

/// Exponential backoff (goal 3): the uncapped target delay for retry `attempt`
/// (1-based) is `base * multiplier^(attempt-1)`, capped at `cap`. With full
/// jitter the actual delay is `random(0, capped)`. A `seed` of `0` disables
/// jitter so the delay is exactly the capped target (used by tests to assert the
/// exponential math deterministically).
fn exponential_backoff(attempt: usize, base: Duration, cap: Duration, seed: u64) -> Duration {
    let exponent = (attempt as u32).saturating_sub(1);
    let multiplier = 2u64.saturating_pow(exponent);
    let delay_secs = base.as_secs().saturating_mul(multiplier);
    let capped = delay_secs.min(cap.as_secs());

    if seed == 0 {
        // No jitter: exact base * multiplier^(attempt-1), capped.
        return Duration::from_secs(capped);
    }

    // Full jitter: random in [0, capped].
    let jittered = xorshift_range(seed.wrapping_add(attempt as u64), capped);
    Duration::from_secs(jittered)
}

/// Simple xorshift PRNG for deterministic jitter in tests.
fn xorshift_range(mut seed: u64, max: u64) -> u64 {
    if max == 0 {
        return 0;
    }
    seed ^= seed << 13;
    seed ^= seed >> 7;
    seed ^= seed << 17;
    seed % (max + 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::fake::FakeProvider;
    use crate::provider::ModelResponse;
    use tokio::sync::mpsc;

    #[tokio::test(start_paused = true)]
    async fn transient_error_retries_with_backoff() {
        let (tx, mut rx) = mpsc::unbounded_channel();

        let provider = FakeProvider::new(vec![
            Err(ProviderError::Api {
                status: 503,
                body: "down".into(),
            }),
            Err(ProviderError::Api {
                status: 503,
                body: "down".into(),
            }),
            Ok(ModelResponse::text("ok")),
        ]);

        let mut mind = ModelMind::new(
            Box::new(provider),
            RenewableBudget {
                period: crate::budget::Period::Daily,
                max_tokens: 100_000,
            },
            tx,
            Duration::from_secs(10),
        );

        let goal = "test".to_string();
        let perception = Perception::NewTask { goal };

        // Spawn the decide future
        let decide_handle = tokio::spawn(async move { mind.decide(perception).await });

        // Advance time to let retries happen
        tokio::time::advance(Duration::from_secs(120)).await;

        let decision = decide_handle.await.unwrap();
        assert!(matches!(decision, Decision::Done(_)));

        // Check retry events
        let mut retry_count = 0;
        while let Ok(event) = rx.try_recv() {
            if matches!(event, RunEvent::RetryScheduled { .. }) {
                retry_count += 1;
            }
        }
        assert_eq!(retry_count, 2);
    }

    #[tokio::test]
    async fn service_fatal_error_returns_failed() {
        let (tx, _rx) = mpsc::unbounded_channel();

        let provider = FakeProvider::new(vec![Err(ProviderError::Api {
            status: 401,
            body: "unauthorized".into(),
        })]);

        let mut mind = ModelMind::new(
            Box::new(provider),
            RenewableBudget::default(),
            tx,
            Duration::from_secs(10),
        );

        let decision = mind
            .decide(Perception::NewTask {
                goal: "test".into(),
            })
            .await;
        assert!(matches!(decision, Decision::Failed(Reason::Service(_))));
    }

    #[tokio::test(start_paused = true)]
    async fn budget_exhaustion_throttles() {
        let (tx, _rx) = mpsc::unbounded_channel();

        let provider = FakeProvider::new(vec![Ok(ModelResponse::text("response"))]);

        let mut mind = ModelMind::new(
            Box::new(provider),
            RenewableBudget {
                period: crate::budget::Period::Every(Duration::from_secs(10)),
                max_tokens: 10,
            },
            tx,
            Duration::from_secs(10),
        );

        // Exhaust budget
        mind.budget_state.set_used_for_test(10);

        let decision = mind
            .decide(Perception::NewTask {
                goal: "test".into(),
            })
            .await;
        assert!(matches!(decision, Decision::Throttle(_)));
    }
}

#[allow(clippy::items_after_test_module)]
impl ModelMind {
    /// Set the backoff jitter seed for deterministic tests.
    pub fn with_jitter_seed(mut self, seed: u64) -> Self {
        self.backoff_seed = seed;
        self
    }
}
