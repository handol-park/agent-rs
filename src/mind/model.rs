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
use crate::tool::ToolSchema;

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
    tools: Vec<ToolSchema>,
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
            tools: Vec::new(),
        }
    }

    /// Override the backoff jitter seed. A seed of `0` disables jitter (exact
    /// exponential delays) — used by tests to assert the backoff math.
    pub fn with_jitter_seed(mut self, seed: u64) -> Self {
        self.backoff_seed = seed;
        self
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

        // The request is invariant across retries — `working_memory` and `tools`
        // are not mutated inside this loop (the malformed re-prompt path lives in
        // `decide`, not here). Build it once so a long conversation history and the
        // tool schemas aren't re-cloned on every transient retry.
        let request = ModelRequest {
            system: "You are a helpful assistant.".to_string(),
            messages: self.working_memory.clone(),
            tools: self.tools.clone(),
        };

        loop {
            // A timed-out call is itself a transient error (spec goal 3): flatten the
            // `Result<Result<_, _>, Elapsed>` into the provider-error channel so the
            // timeout falls into the `Err(e)` arm below and is retried — never
            // propagated out of the retry loop.
            let result =
                tokio::time::timeout(self.per_call_timeout, self.provider.complete(&request))
                    .await
                    .unwrap_or_else(|_| Err(ProviderError::Transport("call timeout".into())));

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
                    window: self.budget_state.current_window(),
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
                            // call_with_retry loops on transient errors, so this is
                            // unreachable in correct operation. Fail the task rather
                            // than panic — a perpetual service must never crash on a
                            // logic slip (spec: errors are recoverable, not terminal).
                            Decision::Failed(Reason::Task(TaskFault::BadRequest(format!(
                                "transient error escaped retry loop: {e}"
                            ))))
                        }
                    };
                }
            };

            // Charge tokens
            let now = Instant::now();
            self.budget_state
                .charge(now, &self.budget, response.usage.total());

            // Map response to decision. Tool calls take priority over text: a model
            // that wants to act often emits explanatory text alongside the tool call,
            // and dropping the call (returning Done on the text) would strand the
            // intended action. So check tool_calls first, then a final text answer.
            if !response.tool_calls.is_empty() {
                self.malformed_count = 0;
                self.resuming = false;
                // LIMITATION: only the first tool call is actuated. Providers can
                // emit several tool calls in one turn, but `Decision::Act` carries a
                // single `Command`, so the rest are dropped. Parallel/multiple tool
                // calls per turn are explicitly out of scope for spec-002/003.
                let tc = &response.tool_calls[0];
                return Decision::Act(Command::CallTool {
                    call_id: tc.id.clone(),
                    name: tc.name.clone(),
                    input: tc.arguments.clone(),
                });
            }

            if let Some(text) = response.text {
                if !text.trim().is_empty() {
                    self.resuming = false;
                    return Decision::Done(Outcome { message: text });
                }
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

    fn set_tools(&mut self, tools: Vec<ToolSchema>) {
        self.tools = tools;
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

    /// A provider whose first call hangs past the per-call timeout, then succeeds.
    /// `FakeProvider` returns instantly, so it cannot exercise the timeout path.
    struct SlowThenFastProvider {
        calls: std::sync::atomic::AtomicUsize,
    }

    #[async_trait]
    impl crate::provider::Provider for SlowThenFastProvider {
        async fn complete(
            &self,
            _request: &ModelRequest,
        ) -> Result<crate::provider::ModelResponse, ProviderError> {
            let n = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if n == 0 {
                // Outlives the 10s per-call timeout; the timeout cancels this future.
                sleep_until(Instant::now() + Duration::from_secs(30)).await;
            }
            Ok(ModelResponse::text("recovered"))
        }
    }

    /// Spec goal 3: a timed-out provider call is transient and MUST be retried,
    /// not propagated out of the retry loop (which previously hit `unreachable!`).
    #[tokio::test(start_paused = true)]
    async fn per_call_timeout_is_retried_as_transient() {
        let (tx, mut rx) = mpsc::unbounded_channel();

        let provider = SlowThenFastProvider {
            calls: std::sync::atomic::AtomicUsize::new(0),
        };

        let mut mind = ModelMind::new(
            Box::new(provider),
            RenewableBudget {
                period: crate::budget::Period::Daily,
                max_tokens: 100_000,
            },
            tx,
            Duration::from_secs(10), // per-call timeout < the 30s first call
        );

        let decide_handle = tokio::spawn(async move {
            mind.decide(Perception::NewTask {
                goal: "test".into(),
            })
            .await
        });

        tokio::time::advance(Duration::from_secs(120)).await;

        let decision = decide_handle.await.unwrap();
        assert!(
            matches!(decision, Decision::Done(_)),
            "a timed-out call must be retried to success, not panic"
        );

        let mut retries = 0;
        while let Ok(event) = rx.try_recv() {
            if matches!(event, RunEvent::RetryScheduled { .. }) {
                retries += 1;
            }
        }
        assert!(retries >= 1, "the timed-out call must schedule a retry");
    }

    /// Spec goal 2: a response carrying BOTH text and a tool call must `Act` on the
    /// tool, not return `Done` on the text (which would strand the action).
    #[tokio::test]
    async fn tool_call_takes_priority_over_text() {
        let (tx, _rx) = mpsc::unbounded_channel();

        let response = ModelResponse {
            text: Some("Let me calculate that.".to_string()),
            tool_calls: vec![crate::provider::ToolCall {
                id: "c1".to_string(),
                name: "calculator".to_string(),
                arguments: serde_json::json!({"expression": "1+1"}),
            }],
            usage: crate::provider::Usage::default(),
        };

        let provider = FakeProvider::new(vec![Ok(response)]);
        let mut mind = ModelMind::new(
            Box::new(provider),
            RenewableBudget::default(),
            tx,
            Duration::from_secs(10),
        );

        let decision = mind
            .decide(Perception::NewTask {
                goal: "compute".into(),
            })
            .await;

        match decision {
            Decision::Act(Command::CallTool { name, .. }) => assert_eq!(name, "calculator"),
            _ => panic!("a response with text and a tool call must return Act(CallTool)"),
        }
    }

    /// Spec goal 3 / SC 1: a body-`Decode` failure is transient and retried as a
    /// **blind re-issue** — the retry must send the byte-identical request.
    #[tokio::test(start_paused = true)]
    async fn decode_retry_reissues_identical_request() {
        let (tx, _rx) = mpsc::unbounded_channel();

        let provider = FakeProvider::new(vec![
            Err(ProviderError::Decode("bad json".into())),
            Ok(ModelResponse::text("ok")),
        ]);
        let seen = provider.requests_handle();

        let mut mind = ModelMind::new(
            Box::new(provider),
            RenewableBudget::default(),
            tx,
            Duration::from_secs(10),
        );

        let decide_handle =
            tokio::spawn(
                async move { mind.decide(Perception::NewTask { goal: "g".into() }).await },
            );
        tokio::time::advance(Duration::from_secs(5)).await;
        let decision = decide_handle.await.unwrap();
        assert!(matches!(decision, Decision::Done(_)));

        let reqs = seen.lock().expect("not poisoned").clone();
        assert_eq!(reqs.len(), 2, "Decode must trigger exactly one retry");
        assert_eq!(
            reqs[0], reqs[1],
            "Decode retry must be a blind re-issue of the identical request"
        );
    }

    /// Spec 003 / SC 2 positive: `set_tools` schemas reach the provider request.
    #[tokio::test]
    async fn set_tools_schemas_reach_the_provider_request() {
        let (tx, _rx) = mpsc::unbounded_channel();

        let provider = FakeProvider::new(vec![Ok(ModelResponse::text("computed"))]);
        let seen = provider.requests_handle();

        let mut mind = ModelMind::new(
            Box::new(provider),
            RenewableBudget::default(),
            tx,
            Duration::from_secs(10),
        );

        // Set tools from the default registry
        let registry = crate::tool::default_registry();
        mind.set_tools(registry.schemas());

        let decision = mind
            .decide(Perception::NewTask {
                goal: "test".into(),
            })
            .await;
        assert!(matches!(decision, Decision::Done(_)));

        let reqs = seen.lock().expect("not poisoned").clone();
        assert_eq!(reqs.len(), 1);
        assert!(
            !reqs[0].tools.is_empty(),
            "request.tools must be non-empty after set_tools"
        );
        assert!(
            reqs[0].tools.iter().any(|s| s.name == "calculator"),
            "request.tools must contain the calculator schema"
        );
    }

    /// Spec 003 / SC 2 negative: without `set_tools`, request advertises no tools.
    #[tokio::test]
    async fn without_set_tools_request_advertises_no_tools() {
        let (tx, _rx) = mpsc::unbounded_channel();

        let provider = FakeProvider::new(vec![Ok(ModelResponse::text("done"))]);
        let seen = provider.requests_handle();

        let mut mind = ModelMind::new(
            Box::new(provider),
            RenewableBudget::default(),
            tx,
            Duration::from_secs(10),
        );

        // Do NOT call set_tools

        let decision = mind
            .decide(Perception::NewTask {
                goal: "test".into(),
            })
            .await;
        assert!(matches!(decision, Decision::Done(_)));

        let reqs = seen.lock().expect("not poisoned").clone();
        assert_eq!(reqs.len(), 1);
        assert!(
            reqs[0].tools.is_empty(),
            "request.tools must be empty when set_tools was never called"
        );
    }
}
