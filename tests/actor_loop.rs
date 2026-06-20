//! Integration tests for Spec 002 — the actor agent (mind + brainstem, v0.2).
//!
//! Each test maps to a numbered success criterion (SC) from
//! `docs/specs/002-resilient-loop-renewable-budget.md`. They are written
//! **before** the implementation exists, so the crate will not compile until the
//! `mind` / `brainstem` / renewable-`budget` modules land — that is intentional
//! (spec non-negotiable: tests are the falsifiable contract).
//!
//! ## Public-API surface these tests assume (for the implementer)
//!
//! The plan's "Core contracts" section fixes the types; the constructors below
//! are the minimal harness surface the tests drive. Match these signatures (or
//! adjust the tests if the plan is refined):
//!
//! * `ModelMind::new(Box<dyn Provider>, Budget, mpsc::UnboundedSender<RunEvent>) -> ModelMind`
//!   — a model-backed `Mind`; classification / retry / backoff / malformed cap /
//!   throttle live here (goals 2-5). Emits `RetryScheduled` + `WindowReset`
//!   through the injected event sender. `with_jitter_seed(u64)` fixes the PRNG so
//!   backoff delays are exactly `base * mult^n` (zero jitter) for SC-1 timing.
//! * `FakeMind` — a scripted `Mind` for brainstem-mechanics tests.
//!   `FakeMind::new(Vec<Decision>)` replays decisions in order;
//!   `FakeMind::pending()` yields a `decide` future that never resolves (for the
//!   mid-decide cancellation test, SC 12). It reports a `BudgetSummary` so
//!   `Status` works.
//! * `Brainstem::spawn(mind, registry, max_steps, inbox_rx, status_rx, cancel, events_tx)`
//!   -> `JoinHandle<Termination>`. The brainstem owns the receiving ends; the
//!   test keeps the sending ends. The same `events_tx` is shared with the mind so
//!   cognitive and brainstem events interleave on one stream.
//!
//! All tests run under `tokio::test(start_paused = true)` so every time-dependent
//! `MUST` (backoff, per-call timeout, throttle sleep, window clock) is
//! deterministic (goal 18).

use std::sync::Arc;
use std::time::Duration;

use agent::{
    Budget, Command, Decision, FakeMind, FakeProvider, Lifecycle, Mind, ModelMind, ModelResponse,
    Observation, Outcome, Perception, Period, ProviderError, Reason, RecoverableError, RunEvent,
    Snapshot, Task, TaskFault, TaskOutcome, Termination, ToolError, ToolRegistry,
};
use serde_json::json;
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

/// Configuration handed to the brainstem at spawn time.
struct BrainstemConfig {
    mind: Box<dyn Mind>,
    registry: Arc<ToolRegistry>,
    max_steps: usize,
}

/// Everything a test needs to talk to a running brainstem.
struct BrainstemHandle {
    inbox: mpsc::Sender<Task>,
    status: mpsc::Sender<oneshot::Sender<Snapshot>>,
    cancel: CancellationToken,
    events: mpsc::UnboundedReceiver<RunEvent>,
    join: tokio::task::JoinHandle<Termination>,
}

impl BrainstemHandle {
    /// Send a `Status` query and await the `Snapshot` reply.
    async fn snapshot(&self) -> Snapshot {
        let (tx, rx) = oneshot::channel();
        self.status.send(tx).await.expect("brainstem alive");
        rx.await.expect("brainstem replied")
    }

    /// Drain whatever events are currently buffered (non-blocking).
    fn drain_events(&mut self) -> Vec<RunEvent> {
        let mut out = Vec::new();
        while let Ok(ev) = self.events.try_recv() {
            out.push(ev);
        }
        out
    }
}

/// Spawn a brainstem on the current runtime and return a handle + its join.
fn spawn(cfg: BrainstemConfig) -> BrainstemHandle {
    let (inbox_tx, inbox_rx) = mpsc::channel::<Task>(16);
    let (status_tx, status_rx) = mpsc::channel::<oneshot::Sender<Snapshot>>(16);
    let (events_tx, events_rx) = mpsc::unbounded_channel::<RunEvent>();
    let cancel = CancellationToken::new();

    let join = agent::Brainstem::spawn(
        cfg.mind,
        cfg.registry,
        cfg.max_steps,
        inbox_rx,
        status_rx,
        cancel.clone(),
        events_tx,
    );

    BrainstemHandle {
        inbox: inbox_tx,
        status: status_tx,
        cancel,
        events: events_rx,
        join,
    }
}

fn model_mind(
    script: Vec<Result<ModelResponse, ProviderError>>,
    budget: Budget,
) -> (Box<dyn Mind>, mpsc::UnboundedReceiver<RunEvent>) {
    let provider = FakeProvider::new(script);
    let (cog_tx, cog_rx) = mpsc::unbounded_channel::<RunEvent>();
    let mind = ModelMind::new(Box::new(provider), budget, cog_tx);
    (Box::new(mind), cog_rx)
}

fn registry() -> Arc<ToolRegistry> {
    Arc::new(agent::default_registry())
}

/// A budget that funds many tokens over a long window — out of the way for tests
/// that don't exercise throttling.
fn ample_budget() -> Budget {
    Budget {
        period: Period::Every(Duration::from_secs(3600)),
        max_tokens: 1_000_000,
    }
}

fn task(goal: &str) -> (Task, oneshot::Receiver<TaskOutcome>) {
    let (reply_tx, reply_rx) = oneshot::channel();
    (
        Task {
            goal: goal.to_string(),
            reply: Some(reply_tx),
        },
        reply_rx,
    )
}

fn act_tool(call_id: &str, name: &str, input: serde_json::Value) -> Decision {
    Decision::Act(Command::CallTool {
        call_id: call_id.to_string(),
        name: name.to_string(),
        input,
    })
}

fn done(message: &str) -> Decision {
    Decision::Done(Outcome {
        message: message.to_string(),
    })
}

// ---------------------------------------------------------------------------
// SC 1 — transient errors retry with backoff; episode proceeds; no termination.
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn sc1_transient_503_retries_with_backoff_then_proceeds() {
    let (mind, _cog_rx) = model_mind(
        vec![
            Err(ProviderError::Api {
                status: 503,
                body: "unavailable".into(),
            }),
            Err(ProviderError::Api {
                status: 503,
                body: "unavailable".into(),
            }),
            Ok(ModelResponse::text("recovered")),
        ],
        ample_budget(),
    );

    let mut h = spawn(BrainstemConfig {
        mind,
        registry: registry(),
        max_steps: 8,
    });

    let (t, reply) = task("retry me");
    h.inbox.send(t).await.unwrap();

    // Auto-advance under start_paused walks the backoff timers forward.
    let outcome = reply.await.expect("task replied");
    assert!(matches!(outcome, TaskOutcome::Completed(_)));

    let events = h.drain_events();
    let retries = events
        .iter()
        .filter(|e| matches!(e, RunEvent::RetryScheduled { .. }))
        .count();
    assert_eq!(retries, 2, "one RetryScheduled per transient attempt: {events:?}");

    assert!(
        !events.iter().any(|e| matches!(e, RunEvent::Terminated { .. })),
        "service must not terminate on a transient error"
    );

    h.cancel.cancel();
    assert!(matches!(h.join.await.unwrap(), Termination::Cancelled));
}

#[tokio::test(start_paused = true)]
async fn sc1_backoff_is_exponential() {
    // Drive ModelMind directly so we control time around each retry precisely.
    let provider = FakeProvider::new(vec![
        Err(ProviderError::Api {
            status: 503,
            body: "x".into(),
        }),
        Err(ProviderError::Api {
            status: 503,
            body: "x".into(),
        }),
        Ok(ModelResponse::text("ok")),
    ]);
    let (cog_tx, mut cog_rx) = mpsc::unbounded_channel::<RunEvent>();
    // Zero jitter so the delays are exactly base * mult^n.
    let mut mind =
        ModelMind::new(Box::new(provider), ample_budget(), cog_tx).with_jitter_seed(0);

    let decide =
        tokio::spawn(async move { mind.decide(Perception::NewTask { goal: "g".into() }).await });

    // First failure schedules retry 1 with delay ~1s.
    let ev1 = recv_retry(&mut cog_rx).await;
    let RunEvent::RetryScheduled { attempt, delay, .. } = ev1 else {
        unreachable!()
    };
    assert_eq!(attempt, 1);
    assert_eq!(delay, Duration::from_secs(1));

    // Advancing by < 1s yields no second retry yet.
    tokio::time::advance(Duration::from_millis(900)).await;
    assert!(cog_rx.try_recv().is_err(), "retry 2 must not fire before its delay");

    // Advancing past 1s lets retry 2 schedule, with delay ~2s (exponential).
    tokio::time::advance(Duration::from_millis(200)).await;
    let ev2 = recv_retry(&mut cog_rx).await;
    let RunEvent::RetryScheduled { attempt, delay, .. } = ev2 else {
        unreachable!()
    };
    assert_eq!(attempt, 2);
    assert_eq!(delay, Duration::from_secs(2), "delay must double (exponential)");

    tokio::time::advance(Duration::from_secs(2)).await;
    let decision = decide.await.unwrap();
    assert!(matches!(decision, Decision::Done(_)));
}

#[tokio::test(start_paused = true)]
async fn sc1_decode_error_is_transient_blind_reissue() {
    let (mind, _cog_rx) = model_mind(
        vec![
            Err(ProviderError::Decode("truncated body".into())),
            Ok(ModelResponse::text("decoded fine now")),
        ],
        ample_budget(),
    );
    let mut h = spawn(BrainstemConfig {
        mind,
        registry: registry(),
        max_steps: 8,
    });

    let (t, reply) = task("decode then ok");
    h.inbox.send(t).await.unwrap();
    let outcome = reply.await.unwrap();

    assert!(
        matches!(outcome, TaskOutcome::Completed(_)),
        "Decode must be retried (transient), not fail the task"
    );
    let events = h.drain_events();
    assert!(
        events.iter().any(|e| matches!(e, RunEvent::RetryScheduled { .. })),
        "Decode must emit a retry (blind re-issue): {events:?}"
    );
    assert!(!events.iter().any(|e| matches!(e, RunEvent::TaskFailed { .. })));

    h.cancel.cancel();
    let _ = h.join.await;
}

// ---------------------------------------------------------------------------
// SC 2 — service-fatal (401) terminates Fatal with no retry.
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn sc2_service_fatal_401_terminates_fatal() {
    let (mind, _cog_rx) = model_mind(
        vec![Err(ProviderError::Api {
            status: 401,
            body: "unauthorized".into(),
        })],
        ample_budget(),
    );
    let mut h = spawn(BrainstemConfig {
        mind,
        registry: registry(),
        max_steps: 8,
    });

    let (t, _reply) = task("auth fails");
    h.inbox.send(t).await.unwrap();

    let term = h.join.await.unwrap();
    assert!(matches!(term, Termination::Fatal(_)), "401 must be service-fatal");

    let events = h.drain_events();
    assert!(
        !events.iter().any(|e| matches!(e, RunEvent::RetryScheduled { .. })),
        "service-fatal must not retry"
    );
    assert!(events.iter().any(|e| matches!(
        e,
        RunEvent::Terminated {
            termination: Termination::Fatal(_)
        }
    )));
}

// ---------------------------------------------------------------------------
// SC 3 — task-fatal (400, and a step-liveness trip) fails the task; service runs on.
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn sc3_task_fatal_400_fails_task_service_continues() {
    let (mind, _cog_rx) = model_mind(
        vec![
            Err(ProviderError::Api {
                status: 400,
                body: "bad request".into(),
            }),
            Ok(ModelResponse::text("second task ok")),
        ],
        ample_budget(),
    );
    let mut h = spawn(BrainstemConfig {
        mind,
        registry: registry(),
        max_steps: 8,
    });

    let (t1, reply1) = task("bad");
    h.inbox.send(t1).await.unwrap();
    assert!(matches!(reply1.await.unwrap(), TaskOutcome::Failed(_)));

    let (t2, reply2) = task("good");
    h.inbox.send(t2).await.unwrap();
    assert!(
        matches!(reply2.await.unwrap(), TaskOutcome::Completed(_)),
        "service must keep serving after a task-fatal error"
    );

    h.cancel.cancel();
    assert!(matches!(h.join.await.unwrap(), Termination::Cancelled));
}

#[tokio::test(start_paused = true)]
async fn sc3_step_liveness_trip_is_task_fatal() {
    // FakeMind always asks to call the calculator -> never finishes -> trips
    // max_steps. The trip must be a task-fatal NoProgress, not service-fatal.
    let loops = vec![act_tool("c", "calculator", json!({"expression": "1+1"})); 10];
    let mut h = spawn(BrainstemConfig {
        mind: Box::new(FakeMind::new(loops)),
        registry: registry(),
        max_steps: 3,
    });

    let (t, reply) = task("spin forever");
    h.inbox.send(t).await.unwrap();
    let outcome = reply.await.unwrap();
    assert_eq!(outcome, TaskOutcome::Failed(TaskFault::NoProgress));

    let events = h.drain_events();
    assert!(events.iter().any(|e| matches!(
        e,
        RunEvent::TaskFailed {
            reason: TaskFault::NoProgress
        }
    )));
    assert!(!events.iter().any(|e| matches!(e, RunEvent::Terminated { .. })));

    h.cancel.cancel();
    assert!(matches!(h.join.await.unwrap(), Termination::Cancelled));
}

// ---------------------------------------------------------------------------
// SC 4 — 2 malformed responses recovered in cognition; the 3rd is task-fatal.
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn sc4_two_malformed_recovered_third_is_task_fatal() {
    // A malformed response = decoded body with no usable command (no text, no
    // tool calls). Two are re-prompted within cognition; a third -> task-fatal.
    let malformed = || {
        Ok(ModelResponse {
            text: None,
            tool_calls: Vec::new(),
            usage: agent::Usage::default(),
        })
    };

    // Two malformed then a recovery -> the episode proceeds (task done).
    let (mind, _cog_rx) = model_mind(
        vec![malformed(), malformed(), Ok(ModelResponse::text("recovered after 2"))],
        ample_budget(),
    );
    let mut h = spawn(BrainstemConfig {
        mind,
        registry: registry(),
        max_steps: 8,
    });
    let (t, reply) = task("two malformed");
    h.inbox.send(t).await.unwrap();
    assert!(
        matches!(reply.await.unwrap(), TaskOutcome::Completed(_)),
        "two consecutive malformed responses must be recovered"
    );
    h.cancel.cancel();
    let _ = h.join.await;

    // Three malformed -> task-fatal Malformed, service continues.
    let (mind3, _cog_rx3) =
        model_mind(vec![malformed(), malformed(), malformed()], ample_budget());
    let mut h3 = spawn(BrainstemConfig {
        mind: mind3,
        registry: registry(),
        max_steps: 8,
    });
    let (t3, reply3) = task("three malformed");
    h3.inbox.send(t3).await.unwrap();
    match reply3.await.unwrap() {
        TaskOutcome::Failed(TaskFault::Malformed(_)) => {}
        other => panic!("third malformed must be task-fatal Malformed, got {other:?}"),
    }
    let events = h3.drain_events();
    assert!(!events.iter().any(|e| matches!(e, RunEvent::Terminated { .. })));
    h3.cancel.cancel();
    assert!(matches!(h3.join.await.unwrap(), Termination::Cancelled));
}

// ---------------------------------------------------------------------------
// SC 5 — token-window exhaustion -> Throttle; brainstem sleeps; resumes after
// reset with consumption zeroed.
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn sc5_token_exhaustion_throttles_then_resumes_after_reset() {
    // A small window funds the first (overspending) call, but not a second within
    // the same window. The second decide finds the window exhausted -> Throttle;
    // after the window resets the resume succeeds with `used` zeroed.
    let window = Duration::from_secs(60);
    let budget = Budget {
        period: Period::Every(window),
        max_tokens: 10,
    };
    let (mind, _cog_rx) = model_mind(
        vec![
            Ok(
                ModelResponse::tool_call("c1", "calculator", json!({"expression": "1+1"}))
                    .with_usage(50, 0),
            ),
            Ok(ModelResponse::text("done after reset").with_usage(1, 0)),
        ],
        budget,
    );
    let mut h = spawn(BrainstemConfig {
        mind,
        registry: registry(),
        max_steps: 8,
    });

    let (t, reply) = task("exhaust then resume");
    h.inbox.send(t).await.unwrap();

    let outcome = reply.await.unwrap();
    assert!(matches!(outcome, TaskOutcome::Completed(_)));

    let events = h.drain_events();
    assert!(
        events.iter().any(|e| matches!(e, RunEvent::ThrottleSleep { .. })),
        "exhaustion must throttle-sleep: {events:?}"
    );
    assert!(
        events.iter().any(|e| matches!(e, RunEvent::WindowReset { .. })),
        "crossing the window boundary must emit WindowReset: {events:?}"
    );

    let snap = h.snapshot().await;
    assert!(snap.tokens_remaining > 0, "window reset must zero consumption");

    h.cancel.cancel();
    assert!(matches!(h.join.await.unwrap(), Termination::Cancelled));
}

// ---------------------------------------------------------------------------
// SC 6 / 12 — cancellation mid-sleep and mid-decide -> Cancelled.
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn sc6_cancel_mid_sleep_wins_over_timer() {
    // Throttle for a long window, then cancel BEFORE advancing the clock past the
    // wake instant, so the cancel branch deterministically beats the timer.
    let reset_at = Instant::now() + Duration::from_secs(3600);
    let mut h = spawn(BrainstemConfig {
        mind: Box::new(FakeMind::new(vec![Decision::Throttle(reset_at)])),
        registry: registry(),
        max_steps: 8,
    });

    let (t, _reply) = task("will throttle");
    h.inbox.send(t).await.unwrap();

    tokio::task::yield_now().await;
    let events = h.drain_events();
    assert!(events.iter().any(|e| matches!(e, RunEvent::ThrottleSleep { .. })));

    h.cancel.cancel();
    assert!(matches!(h.join.await.unwrap(), Termination::Cancelled));
}

#[tokio::test(start_paused = true)]
async fn sc12_cancel_mid_decide_terminates_cancelled() {
    // FakeMind::pending() yields a decide future that never resolves; cancelling
    // while parked on it must terminate Cancelled (goal 11/12).
    let mut h = spawn(BrainstemConfig {
        mind: Box::new(FakeMind::pending()),
        registry: registry(),
        max_steps: 8,
    });

    let (t, _reply) = task("hangs in decide");
    h.inbox.send(t).await.unwrap();

    tokio::task::yield_now().await;
    // Status MUST still be answered while the decide is in flight (goal 11).
    let snap = h.snapshot().await;
    assert_eq!(snap.lifecycle, Lifecycle::Working);

    h.cancel.cancel();
    assert!(matches!(h.join.await.unwrap(), Termination::Cancelled));
}

// ---------------------------------------------------------------------------
// SC 7 — two tasks processed in sequence; each TaskOutcome emitted.
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn sc7_two_tasks_in_sequence_each_emits_outcome() {
    let mut h = spawn(BrainstemConfig {
        mind: Box::new(FakeMind::new(vec![done("first"), done("second")])),
        registry: registry(),
        max_steps: 8,
    });

    let (t1, reply1) = task("one");
    let (t2, reply2) = task("two");
    h.inbox.send(t1).await.unwrap();
    h.inbox.send(t2).await.unwrap();

    let o1 = reply1.await.unwrap();
    let o2 = reply2.await.unwrap();
    assert!(matches!(o1, TaskOutcome::Completed(Outcome { ref message }) if message == "first"));
    assert!(matches!(o2, TaskOutcome::Completed(Outcome { ref message }) if message == "second"));

    let events = h.drain_events();
    let completed = events
        .iter()
        .filter(|e| matches!(e, RunEvent::TaskCompleted { .. }))
        .count();
    assert_eq!(completed, 2, "one TaskCompleted per task: {events:?}");

    h.cancel.cancel();
    assert!(matches!(h.join.await.unwrap(), Termination::Cancelled));
}

// ---------------------------------------------------------------------------
// SC 8 — inbox closed (all senders dropped) -> Stopped.
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn sc8_inbox_closed_terminates_stopped() {
    let h = spawn(BrainstemConfig {
        mind: Box::new(FakeMind::new(vec![])),
        registry: registry(),
        max_steps: 8,
    });

    let BrainstemHandle {
        inbox,
        status: _status,
        cancel: _cancel,
        events: _events,
        join,
    } = h;
    drop(inbox);

    assert!(matches!(join.await.unwrap(), Termination::Stopped));
}

// ---------------------------------------------------------------------------
// SC 9 — Status query returns a correct Snapshot, including mid-throttle.
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn sc9_status_while_working_reports_lifecycle_and_steps() {
    let mut h = spawn(BrainstemConfig {
        mind: Box::new(FakeMind::new(vec![
            act_tool("c", "calculator", json!({"expression": "2+2"})),
            done("answered"),
        ])),
        registry: registry(),
        max_steps: 8,
    });

    let (t, reply) = task("status me");
    h.inbox.send(t).await.unwrap();

    let snap = h.snapshot().await;
    assert!(matches!(snap.lifecycle, Lifecycle::Working | Lifecycle::Idle));
    if snap.lifecycle == Lifecycle::Working {
        assert_eq!(snap.current_task.as_deref(), Some("status me"));
    }

    let _ = reply.await.unwrap();
    h.cancel.cancel();
    let _ = h.join.await;
}

#[tokio::test(start_paused = true)]
async fn sc9_status_during_throttle_reports_throttling() {
    // Mid-throttle Status MUST be answered (not blocked) and report Throttling
    // with the reset instant (goal 11/12). The only test that catches a
    // throttle-sleep select! omitting the status arm.
    let reset_at = Instant::now() + Duration::from_secs(3600);
    let mut h = spawn(BrainstemConfig {
        mind: Box::new(FakeMind::new(vec![Decision::Throttle(reset_at)])),
        registry: registry(),
        max_steps: 8,
    });

    let (t, _reply) = task("throttle me");
    h.inbox.send(t).await.unwrap();
    tokio::task::yield_now().await;

    // Query WITHOUT advancing the clock past `reset_at`: the brainstem is parked
    // in the throttle sleep and must still answer.
    let snap = h.snapshot().await;
    assert_eq!(snap.lifecycle, Lifecycle::Throttling);
    assert_eq!(snap.next_reset, reset_at);

    h.cancel.cancel();
    assert!(matches!(h.join.await.unwrap(), Termination::Cancelled));
}

// ---------------------------------------------------------------------------
// SC 10 — unknown command -> recoverable Observation; episode continues.
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn sc10_unknown_tool_is_recoverable_observation() {
    let mut h = spawn(BrainstemConfig {
        mind: Box::new(FakeMind::new(vec![
            act_tool("c", "frobnicate", json!({})),
            done("continued after unknown tool"),
        ])),
        registry: registry(),
        max_steps: 8,
    });

    let (t, reply) = task("call unknown");
    h.inbox.send(t).await.unwrap();
    assert!(matches!(reply.await.unwrap(), TaskOutcome::Completed(_)));

    let events = h.drain_events();
    assert!(
        events.iter().any(|e| matches!(
            e,
            RunEvent::Recovered {
                error: RecoverableError::UnknownTool(_)
            }
        )),
        "unknown tool must surface a Recovered(UnknownTool): {events:?}"
    );

    h.cancel.cancel();
    let _ = h.join.await;
}

// ---------------------------------------------------------------------------
// SC 11 — the documented RunEvent set is emitted across the scenarios.
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn sc11_event_set_is_emitted_on_its_paths() {
    // A single episode that exercises: task-received, command, command-result,
    // recovered (unknown tool), task-completed, and a terminal event.
    let mut h = spawn(BrainstemConfig {
        mind: Box::new(FakeMind::new(vec![
            act_tool("c1", "calculator", json!({"expression": "1+1"})),
            act_tool("c2", "frobnicate", json!({})),
            done("all paths"),
        ])),
        registry: registry(),
        max_steps: 8,
    });

    let (t, reply) = task("event coverage");
    h.inbox.send(t).await.unwrap();
    let _ = reply.await.unwrap();
    h.cancel.cancel();
    let term = h.join.await.unwrap();
    assert!(matches!(term, Termination::Cancelled));

    let events = h.drain_events();
    let has = |pred: fn(&RunEvent) -> bool| events.iter().any(pred);

    assert!(has(|e| matches!(e, RunEvent::TaskReceived { .. })), "TaskReceived");
    assert!(has(|e| matches!(e, RunEvent::Command { .. })), "Command");
    assert!(has(|e| matches!(e, RunEvent::CommandResult { .. })), "CommandResult");
    assert!(
        has(|e| matches!(e, RunEvent::Recovered { .. })),
        "Recovered (unknown tool)"
    );
    assert!(has(|e| matches!(e, RunEvent::TaskCompleted { .. })), "TaskCompleted");
    assert!(has(|e| matches!(e, RunEvent::Terminated { .. })), "Terminated");
}

// ---------------------------------------------------------------------------
// SC 13 — zero-quota window -> Throttle first, then BudgetTooSmall (goal 4 order).
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn sc13_zero_quota_mind_throttles_then_budget_too_small() {
    // max_tokens == 0: a window that funds zero calls. The FIRST decide returns
    // Throttle with NO provider call made. After the clock advances past the
    // reset, the resume decide hits the same fresh-but-still-zero window with
    // resuming == true -> task-fatal BudgetTooSmall. Asserting the intermediate
    // Throttle rejects an impl that fails on first exhaustion, skipping the wait
    // (goal 4 wait-then-decide order).
    let window = Duration::from_secs(60);
    let provider = FakeProvider::new(vec![Ok(ModelResponse::text("never reached"))]);
    let (cog_tx, _cog_rx) = mpsc::unbounded_channel::<RunEvent>();
    let mut mind = ModelMind::new(
        Box::new(provider),
        Budget {
            period: Period::Every(window),
            max_tokens: 0,
        },
        cog_tx,
    );

    let first = mind
        .decide(Perception::NewTask {
            goal: "zero quota".into(),
        })
        .await;
    let reset = match first {
        Decision::Throttle(t) => t,
        other => panic!("first decide must Throttle (goal 4 wait-then-decide), got {other:?}"),
    };

    let now = Instant::now();
    let wait = reset.saturating_duration_since(now) + Duration::from_secs(1);
    tokio::time::advance(wait).await;

    let second = mind.decide(Perception::Resume).await;
    match second {
        Decision::Failed(Reason::Task(TaskFault::BudgetTooSmall)) => {}
        other => panic!(
            "resume against zero-quota window must be task-fatal BudgetTooSmall, got {other:?}"
        ),
    }
}

#[tokio::test(start_paused = true)]
async fn sc13_zero_quota_through_brainstem_throttles_then_fails_and_continues() {
    // End-to-end: zero-quota -> ThrottleSleep emitted with no provider call ->
    // advance past reset -> TaskFailed(BudgetTooSmall); service continues.
    let (mind, _cog_rx) = model_mind(
        vec![Ok(ModelResponse::text("never reached"))],
        Budget {
            period: Period::Every(Duration::from_secs(60)),
            max_tokens: 0,
        },
    );
    let mut h = spawn(BrainstemConfig {
        mind,
        registry: registry(),
        max_steps: 8,
    });

    let (t, reply) = task("zero quota e2e");
    h.inbox.send(t).await.unwrap();

    let outcome = reply.await.unwrap();
    assert_eq!(outcome, TaskOutcome::Failed(TaskFault::BudgetTooSmall));

    let events = h.drain_events();
    assert!(
        events.iter().any(|e| matches!(e, RunEvent::ThrottleSleep { .. })),
        "zero-quota must throttle first (goal 4 order): {events:?}"
    );
    assert!(!events.iter().any(|e| matches!(e, RunEvent::RetryScheduled { .. })));
    assert!(!events.iter().any(|e| matches!(e, RunEvent::Terminated { .. })));

    h.cancel.cancel();
    assert!(matches!(h.join.await.unwrap(), Termination::Cancelled));
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Await the next `RetryScheduled` cognitive event, yielding the runtime so the
/// `decide` task can make progress under paused time.
async fn recv_retry(rx: &mut mpsc::UnboundedReceiver<RunEvent>) -> RunEvent {
    loop {
        if let Ok(ev) = rx.try_recv() {
            if matches!(ev, RunEvent::RetryScheduled { .. }) {
                return ev;
            }
            continue;
        }
        tokio::task::yield_now().await;
    }
}

// Type anchors so `Observation` / `ToolError` are referenced even if a path that
// uses them is cfg-gated out; keeps the import list honest for the implementer.
#[allow(dead_code)]
fn _type_anchor(_: Observation, _: ToolError) {}
