//! End-to-end tests against a REAL OpenAI-compatible LLM backend (e.g. a local
//! Ollama serving a tool-calling model). Unlike the rest of the suite — which
//! drives `FakeMind` with scripted decisions — these exercise the live
//! `OpenAiProvider` through the v0.2 `ModelMind` + `Brainstem` actor stack:
//! request construction, tool-schema serialization, and `tool_calls` parsing all
//! run for real.
//!
//! ## They never run in `make check`
//!
//! Every test here is `#[ignore]`, so `cargo test` (and the `make check` gate)
//! skips them. They still *compile* under the gate, which keeps them honest
//! against API drift. To actually run them you must opt in AND point the crate
//! at a backend:
//!
//! ```bash
//! # 1. Serve a tool-calling model locally (verified with qwen3.5:4b-nvfp4):
//! ollama serve &
//! ollama pull qwen3.5:4b-nvfp4
//!
//! # 2. Point the provider at it (the /v1 suffix is required):
//! export LLM_BASE_URL=http://localhost:11434/v1
//! export LLM_API_KEY=ollama          # any non-empty string; Ollama ignores it
//! export LLM_MODEL=qwen3.5:4b-nvfp4
//!
//! # 3. Run only the ignored e2e suite:
//! nix develop -c cargo test --test e2e_ollama -- --ignored --nocapture
//! ```
//!
//! If `LLM_*` is not set, each test prints a SKIP line and returns early — so
//! running with `--ignored` but no backend is a no-op, not a failure. A backend
//! that is configured-but-unreachable WILL fail the test (that is the point of
//! opting in).

use std::sync::Arc;
use std::time::Duration;

use agent::{
    default_registry, Brainstem, Lifecycle, Mind, ModelMind, OpenAiProvider, Period,
    RenewableBudget, RunEvent, Snapshot, Task, TaskOutcome, Termination, ToolRegistry,
};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

/// 1234 * 5678 — large enough that a small model leans on the calculator tool
/// rather than answering from its head, so the tool-calling tests actually
/// exercise the round-trip instead of a plain text reply.
const FACTOR_A: i64 = 1234;
const FACTOR_B: i64 = 5678;
/// The product's digits (1234 * 5678), used to confirm the tool result reached
/// the final answer regardless of separators the model might insert (e.g. commas).
const PRODUCT_DIGITS: &str = "7006652";

/// Upper bound on how long a single task may take before the harness gives up.
/// `ModelMind` retries transient provider errors with *unbounded* exponential
/// backoff, so a wedged or unreachable backend would otherwise hang the test
/// forever. Sized to clear two full retry cycles: each is the mind's 120s
/// per-call timeout + up to the 60s backoff cap (`src/mind/model.rs`), i.e.
/// 2 * (120 + 120) ≈ 480s including a cold-start model load. The ceiling is
/// harmless — these tests only run when the user has opted in — so only a
/// genuinely stuck run trips it.
const RUN_TASK_TIMEOUT: Duration = Duration::from_secs(480);

/// Build a provider from `LLM_*`, or skip the test (returns from the caller) when
/// no backend is configured. Mirrors `OpenAiProvider::from_env`'s contract: all
/// three of `LLM_BASE_URL` / `LLM_API_KEY` / `LLM_MODEL` must be set.
macro_rules! provider_or_skip {
    () => {
        match OpenAiProvider::from_env() {
            Some(p) => p,
            None => {
                eprintln!(
                    "SKIP e2e: set LLM_BASE_URL / LLM_API_KEY / LLM_MODEL (e.g. local Ollama) to run"
                );
                return;
            }
        }
    };
}

// ---------------------------------------------------------------------------
// Harness — a running brainstem driven by a real ModelMind, with helpers to
// feed tasks, query status, and drain the event stream.
// ---------------------------------------------------------------------------

struct E2eHandle {
    inbox: mpsc::Sender<Task>,
    status: mpsc::Sender<oneshot::Sender<Snapshot>>,
    cancel: CancellationToken,
    events: mpsc::UnboundedReceiver<RunEvent>,
    join: tokio::task::JoinHandle<Termination>,
    // The mind's own cognitive-event sink. The brainstem injects its own sink at
    // spawn, so this receiver is just kept alive (same as examples/service.rs).
    _cog_events: mpsc::UnboundedReceiver<RunEvent>,
}

impl E2eHandle {
    /// Feed one task through the inbox and await its `TaskOutcome`.
    async fn run_task(&self, goal: impl Into<String>) -> TaskOutcome {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.inbox
            .send(Task {
                goal: goal.into(),
                reply: Some(reply_tx),
            })
            .await
            .expect("brainstem inbox open");
        tokio::time::timeout(RUN_TASK_TIMEOUT, reply_rx)
            .await
            .expect("task timed out — backend unreachable or wedged in retry backoff?")
            .expect("brainstem replied")
    }

    /// Query `Status` and await the `Snapshot` reply. An idle brainstem answers
    /// immediately; the short timeout makes a wedged loop fail loudly rather than
    /// hang the test.
    async fn snapshot(&self) -> Snapshot {
        let (tx, rx) = oneshot::channel();
        self.status.send(tx).await.expect("status channel open");
        tokio::time::timeout(Duration::from_secs(10), rx)
            .await
            .expect("status query timed out — brainstem wedged?")
            .expect("brainstem replied to status")
    }

    /// Drain whatever brainstem events are currently buffered (non-blocking).
    fn drain_events(&mut self) -> Vec<RunEvent> {
        let mut out = Vec::new();
        while let Ok(e) = self.events.try_recv() {
            out.push(e);
        }
        out
    }

    /// Cancel the loop and await its terminal `Termination`.
    async fn shutdown(self) -> Termination {
        self.cancel.cancel();
        self.join.await.expect("brainstem joins")
    }
}

/// Spawn a brainstem backed by a real `ModelMind` over the given provider.
fn spawn_brainstem(provider: OpenAiProvider) -> E2eHandle {
    let (cog_sink, cog_events) = mpsc::unbounded_channel::<RunEvent>();
    let mind: Box<dyn Mind> = Box::new(ModelMind::new(
        Box::new(provider),
        RenewableBudget {
            period: Period::Daily,
            max_tokens: 1_000_000,
        },
        cog_sink,
        Duration::from_secs(120),
    ));

    let (inbox_tx, inbox_rx) = mpsc::channel::<Task>(16);
    let (status_tx, status_rx) = mpsc::channel::<oneshot::Sender<Snapshot>>(16);
    let (events_tx, events_rx) = mpsc::unbounded_channel::<RunEvent>();
    let cancel = CancellationToken::new();
    let registry: Arc<ToolRegistry> = Arc::new(default_registry());

    let join = Brainstem::spawn(
        mind,
        registry,
        /* max_steps */ 8,
        inbox_rx,
        status_rx,
        cancel.clone(),
        events_tx,
    );

    E2eHandle {
        inbox: inbox_tx,
        status: status_tx,
        cancel,
        events: events_rx,
        join,
        _cog_events: cog_events,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// Tool calling, end to end: the real mind issues a `calculator` command, the
/// brainstem actuates it, and — the strong assertion — the tool's result reaches
/// the final answer. We strip non-digits before matching so the check is robust
/// to thousands separators the model might add.
#[tokio::test]
#[ignore = "requires a live LLM backend (LLM_* env, e.g. local Ollama); run with --ignored"]
async fn brainstem_tool_call_result_reaches_the_answer() {
    let provider = provider_or_skip!();
    let mut h = spawn_brainstem(provider);

    let goal = format!(
        "Use the calculator tool to compute {FACTOR_A} * {FACTOR_B}, then state the result."
    );
    let message = match h.run_task(goal).await {
        TaskOutcome::Completed(outcome) => outcome.message,
        other => panic!("expected the task to complete, got {other:?}"),
    };

    let events = h.drain_events();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, RunEvent::CommandResult { ok: true, .. })),
        "the calculator command must succeed; events: {events:?}"
    );

    let digits: String = message.chars().filter(|c| c.is_ascii_digit()).collect();
    assert!(
        digits.contains(PRODUCT_DIGITS),
        "final answer should carry the computed product ({PRODUCT_DIGITS}); got: {message:?}"
    );

    assert_eq!(h.shutdown().await, Termination::Cancelled);
}

/// The conversational path: a plain greeting task runs end to end and completes.
///
/// We deliberately do NOT assert tool-*absence* here: the calculator schema is
/// still advertised in every request, and small models occasionally invoke it
/// even when told not to, so "no command was actuated" is non-deterministic
/// against a real backend. The structural no-tool guarantee is covered offline
/// with `FakeMind`; here we only claim the conversational task reaches a
/// `Completed` outcome and emits its `TaskCompleted` event.
#[tokio::test]
#[ignore = "requires a live LLM backend (LLM_* env, e.g. local Ollama); run with --ignored"]
async fn brainstem_completes_a_conversational_task() {
    let provider = provider_or_skip!();
    let mut h = spawn_brainstem(provider);

    let outcome = h.run_task("Reply with a one-sentence greeting.").await;
    assert!(
        matches!(outcome, TaskOutcome::Completed(_)),
        "expected completion, got {outcome:?}"
    );

    let events = h.drain_events();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, RunEvent::TaskCompleted { .. })),
        "expected a TaskCompleted event; events: {events:?}"
    );

    assert_eq!(h.shutdown().await, Termination::Cancelled);
}

/// The perpetual loop serves multiple tasks: a tool task followed by a no-tool
/// task, each producing its own completion. Exercises the actor service's core
/// value — staying alive across tasks.
#[tokio::test]
#[ignore = "requires a live LLM backend (LLM_* env, e.g. local Ollama); run with --ignored"]
async fn brainstem_serves_two_tasks_in_sequence() {
    let provider = provider_or_skip!();
    let mut h = spawn_brainstem(provider);

    let first = h
        .run_task(format!(
            "Use the calculator tool to compute {FACTOR_A} + {FACTOR_B}."
        ))
        .await;
    let second = h.run_task("Reply with a one-word greeting.").await;

    assert!(
        matches!(first, TaskOutcome::Completed(_)),
        "first task should complete, got {first:?}"
    );
    assert!(
        matches!(second, TaskOutcome::Completed(_)),
        "second task should complete, got {second:?}"
    );

    let events = h.drain_events();
    let completed = events
        .iter()
        .filter(|e| matches!(e, RunEvent::TaskCompleted { .. }))
        .count();
    assert_eq!(
        completed, 2,
        "one TaskCompleted per task; events: {events:?}"
    );

    assert_eq!(h.shutdown().await, Termination::Cancelled);
}

/// `Status` is answerable against a real-model brainstem: a freshly spawned loop
/// (no task yet) reports an idle snapshot with the full token quota.
#[tokio::test]
#[ignore = "requires a live LLM backend (LLM_* env, e.g. local Ollama); run with --ignored"]
async fn brainstem_reports_idle_snapshot_before_any_task() {
    let provider = provider_or_skip!();
    let h = spawn_brainstem(provider);

    let snap = h.snapshot().await;
    assert_eq!(snap.lifecycle, Lifecycle::Idle, "no task yet -> Idle");
    assert_eq!(snap.current_task, None);
    assert_eq!(
        snap.tokens_remaining, 1_000_000,
        "the full window quota is intact before any model call"
    );

    assert_eq!(h.shutdown().await, Termination::Cancelled);
}
