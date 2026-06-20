//! Integration test for Spec 003 — tool schemas in ModelMind.
//!
//! Proves end-to-end SC-4: the Brainstem injects `registry.schemas()` into
//! `ModelMind`, the Mind advertises them to the Provider, the Provider includes
//! them in the ModelRequest, and a tool-call loop actually works.

use std::sync::Arc;
use std::time::Duration;

use agent::{
    default_registry, FakeProvider, ModelMind, ModelResponse, Observation, Outcome,
    RenewableBudget, RunEvent, Task, TaskOutcome, ToolRegistry,
};
use serde_json::json;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------
// Harness (minimal copy from actor_loop.rs)
// ---------------------------------------------------------------------------

/// Configuration handed to the brainstem at spawn time.
struct BrainstemConfig {
    mind: Box<dyn agent::Mind>,
    registry: Arc<ToolRegistry>,
    max_steps: usize,
}

/// Everything a test needs to talk to a running brainstem.
struct BrainstemHandle {
    inbox: mpsc::Sender<Task>,
    #[allow(dead_code)]
    status: mpsc::Sender<oneshot::Sender<agent::Snapshot>>,
    cancel: CancellationToken,
    events: mpsc::UnboundedReceiver<RunEvent>,
    join: tokio::task::JoinHandle<agent::Termination>,
}

impl BrainstemHandle {
    /// Drain whatever events are currently buffered (non-blocking).
    #[allow(dead_code)]
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
    let (status_tx, status_rx) = mpsc::channel::<oneshot::Sender<agent::Snapshot>>(16);
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

fn ample_budget() -> RenewableBudget {
    RenewableBudget {
        period: agent::Period::Every(Duration::from_secs(3600)),
        max_tokens: 1_000_000,
    }
}

// ---------------------------------------------------------------------------
// SC-4 — end-to-end tool-call loop with schema advertisement
// ---------------------------------------------------------------------------

#[tokio::test(start_paused = true)]
async fn sc4_tool_schemas_advertised_and_tool_call_loop_works() {
    // Script: first call returns a tool call, second call returns done.
    let provider = FakeProvider::new(vec![
        Ok(ModelResponse::tool_call(
            "c1",
            "calculator",
            json!({"expression": "1+1"}),
        )),
        Ok(ModelResponse::text("the answer is 2")),
    ]);

    // Grab the requests handle BEFORE moving the provider into the mind.
    let requests_handle = provider.requests_handle();

    // Build the mind with the scripted provider.
    let (cog_tx, _cog_rx) = mpsc::unbounded_channel::<RunEvent>();
    let mind = ModelMind::new(
        Box::new(provider),
        ample_budget(),
        cog_tx,
        Duration::from_secs(30),
    );

    // Build the brainstem with the default registry (has the calculator).
    let registry = Arc::new(default_registry());
    let h = spawn(BrainstemConfig {
        mind: Box::new(mind),
        registry,
        max_steps: 8,
    });

    // Send a task, await the outcome.
    let (t, reply) = task("calculate 1+1");
    h.inbox.send(t).await.unwrap();
    let outcome = reply.await.expect("task replied");

    // Assert: the task completed (proves the tool call was executed and the loop
    // converged with "the answer is 2").
    assert!(
        matches!(outcome, TaskOutcome::Completed(Outcome { ref message }) if message == "the answer is 2"),
        "tool-call loop must complete successfully, got {outcome:?}"
    );

    // Assert: the FIRST request advertised the calculator schema (proves the
    // brainstem injected registry.schemas() and it reached the wire).
    {
        let requests = requests_handle.lock().expect("not poisoned");
        assert_eq!(
            requests.len(),
            2,
            "expected 2 requests (tool call + done), got {}",
            requests.len()
        );

        let first_req = &requests[0];
        assert_eq!(
            first_req.tools.len(),
            1,
            "first request must advertise exactly 1 tool (calculator)"
        );
        assert_eq!(
            first_req.tools[0].name, "calculator",
            "advertised tool must be the calculator"
        );
        assert_eq!(
            first_req.tools[0].description,
            "Evaluate a basic arithmetic expression (supports + - * /, parentheses).",
            "schema description must match CalculatorTool::description"
        );
        // Belt-and-suspenders: assert the schema has the 'expression' parameter.
        assert!(
            first_req.tools[0]
                .parameters
                .get("properties")
                .and_then(|p| p.get("expression"))
                .is_some(),
            "calculator schema must define the 'expression' parameter"
        );

        // Optional (belt-and-suspenders): assert the second request carries the tool
        // result message, proving the loop folded the result back.
        let second_req = &requests[1];
        let has_tool_result = second_req.messages.iter().any(|m| match m {
            agent::Message::Tool { call_id, content } => {
                call_id == "c1" && content.contains("\"result\":2")
            }
            _ => false,
        });
        assert!(
            has_tool_result,
            "second request must include the tool result observation"
        );
    } // Drop the lock before await

    h.cancel.cancel();
    assert!(matches!(
        h.join.await.unwrap(),
        agent::Termination::Cancelled
    ));
}

// Type anchors so Observation is referenced (keeps the import list honest for
// the implementer).
#[allow(dead_code)]
fn _type_anchor(_: Observation) {}
