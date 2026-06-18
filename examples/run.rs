//! Run one agent turn. With `LLM_BASE_URL`/`LLM_API_KEY`/`LLM_MODEL` set, it
//! drives a real model; otherwise it falls back to the offline `RulePlanner`.
//!
//!   nix develop -c cargo run --example run -- "12 * (3 + 4)"

use agent::{
    default_registry, Agent, Budget, ModelPlanner, OpenAiProvider, Planner, RulePlanner,
    TerminalReason,
};

#[tokio::main]
async fn main() {
    let goal = std::env::args().skip(1).collect::<Vec<_>>().join(" ");
    let goal = if goal.trim().is_empty() {
        "12 * (3 + 4)".to_string()
    } else {
        goal
    };

    let planner: Box<dyn Planner> = match OpenAiProvider::from_env() {
        Some(provider) => {
            eprintln!("[provider] using LLM from LLM_* env");
            Box::new(ModelPlanner::new(Box::new(provider)))
        }
        None => {
            eprintln!("[provider] no LLM_* env set — using offline RulePlanner");
            Box::new(RulePlanner)
        }
    };

    let report = Agent::new(Budget::default())
        .run(&goal, planner.as_ref(), &default_registry())
        .await;

    for event in &report.events {
        println!("{event:?}");
    }
    match report.outcome {
        TerminalReason::Finished(message) => println!("\n✓ {message}"),
        other => println!("\n✗ ended: {other:?}"),
    }
}
