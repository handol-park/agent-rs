//! Run the v0.2 perpetual actor service. Spawns a `Brainstem`, feeds it a couple
//! of tasks via the inbox, prints the `RunEvent` stream + each `TaskOutcome`, then
//! cancels cleanly.
//!
//! With `LLM_BASE_URL`/`LLM_API_KEY`/`LLM_MODEL` set it drives a real model
//! (`ModelMind`); otherwise it falls back to a scripted `FakeMind` so the demo
//! runs fully offline — same pattern as `examples/run.rs`.
//!
//!   nix develop -c cargo run --example service

use std::sync::Arc;
use std::time::Duration;

use agent::{
    default_registry, Brainstem, Command, Decision, FakeMind, Mind, ModelMind, OpenAiProvider,
    Outcome, Period, RenewableBudget, RunEvent, Snapshot, Task, TaskOutcome, ToolRegistry,
};
use serde_json::json;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

#[tokio::main]
async fn main() {
    // --- choose a Mind: real provider if LLM_* is set, else a scripted fake ---
    let mind: Box<dyn Mind> = match OpenAiProvider::from_env() {
        Some(provider) => {
            eprintln!("[provider] using LLM from LLM_* env");
            // The brainstem injects its own event sink at spawn, so this one is a
            // placeholder that is replaced before the loop starts.
            let (sink, _drop) = mpsc::unbounded_channel::<RunEvent>();
            let budget = RenewableBudget {
                period: Period::Daily,
                max_tokens: 1_000_000,
            };
            Box::new(ModelMind::new(
                Box::new(provider),
                budget,
                sink,
                Duration::from_secs(30),
            ))
        }
        None => {
            eprintln!("[provider] no LLM_* env set — using offline FakeMind");
            // Scripted cognition: call the calculator, then report the answer.
            Box::new(FakeMind::with_script_only(vec![
                Decision::Act(Command::CallTool {
                    call_id: "c1".into(),
                    name: "calculator".into(),
                    input: json!({ "expression": "12 * (3 + 4)" }),
                }),
                Decision::Done(Outcome {
                    message: "12 * (3 + 4) = 84".into(),
                }),
                // Second task: a plain answer, no tool call.
                Decision::Done(Outcome {
                    message: "the second task is done".into(),
                }),
            ]))
        }
    };

    // --- wire the channels and spawn the brainstem ---
    let (inbox_tx, inbox_rx) = mpsc::channel::<Task>(16);
    let (status_tx, status_rx) = mpsc::channel::<oneshot::Sender<Snapshot>>(16);
    let (events_tx, mut events_rx) = mpsc::unbounded_channel::<RunEvent>();
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

    // --- print RunEvents as they stream in, on a background task ---
    let printer = tokio::spawn(async move {
        while let Some(event) = events_rx.recv().await {
            println!("[event] {event:?}");
        }
    });

    // --- feed two tasks through the inbox and await their outcomes ---
    for goal in ["12 * (3 + 4)", "say hello"] {
        let (reply_tx, reply_rx) = oneshot::channel();
        inbox_tx
            .send(Task {
                goal: goal.to_string(),
                reply: Some(reply_tx),
            })
            .await
            .expect("brainstem inbox open");

        // A Status query works concurrently with the running episode.
        let (snap_tx, snap_rx) = oneshot::channel();
        let _ = status_tx.send(snap_tx).await;
        if let Ok(snap) = snap_rx.await {
            println!(
                "[status] lifecycle={:?} task={:?}",
                snap.lifecycle, snap.current_task
            );
        }

        match reply_rx.await {
            Ok(TaskOutcome::Completed(Outcome { message })) => {
                println!("[outcome] ✓ {message}");
            }
            Ok(TaskOutcome::Failed(fault)) => {
                println!("[outcome] ✗ task failed: {fault:?}");
            }
            Err(_) => {
                println!("[outcome] brainstem dropped the reply (service stopped)");
                break;
            }
        }
    }

    // --- shut down cleanly: cancel the loop and await its termination ---
    cancel.cancel();
    match join.await {
        Ok(termination) => println!("\n[terminated] {termination:?}"),
        Err(e) => eprintln!("[terminated] brainstem task panicked: {e}"),
    }
    let _ = printer.await;
}
