//! The headline integration test (Spec 001 success criterion): the model makes
//! a failing tool call, the loop records the error and feeds it back, the model
//! corrects, and the run finishes successfully — all offline via `FakeProvider`.

use std::time::Duration;

use agent::{
    default_registry, Agent, Budget, FakeProvider, ModelPlanner, ModelResponse, RecoverableError,
    RunEvent, TerminalReason,
};
use serde_json::json;

#[tokio::test]
async fn recovers_from_a_failed_tool_call_then_finishes() {
    // Scripted model turns:
    //   1. call calculator with a bad field   -> tool fails (recovered)
    //   2. call calculator with valid input   -> tool succeeds
    //   3. reply with final text              -> finish
    let provider = FakeProvider::new(vec![
        Ok(ModelResponse::tool_call(
            "c1",
            "calculator",
            json!({ "wrong_field": "2 + 2" }),
        )),
        Ok(ModelResponse::tool_call(
            "c2",
            "calculator",
            json!({ "expression": "2 + 2" }),
        )),
        Ok(ModelResponse::text("The answer is 4.")),
    ]);

    let planner = ModelPlanner::new(Box::new(provider));
    let budget = Budget {
        max_steps: 8,
        max_tokens: 1_000_000,
        wall_clock: Duration::from_secs(30),
    };

    let report = Agent::new(budget)
        .run("what is 2 + 2?", &planner, &default_registry())
        .await;

    // Finished successfully...
    assert_eq!(
        report.outcome,
        TerminalReason::Finished("The answer is 4.".into())
    );

    // ...after recovering from the bad first call (not terminating on it)...
    assert!(
        report.events.iter().any(|e| matches!(
            e,
            RunEvent::Recovered {
                error: RecoverableError::ToolFailed { .. },
                ..
            }
        )),
        "expected a recovered tool failure in {:?}",
        report.events
    );

    // ...and actually ran the corrected call...
    assert!(report
        .events
        .iter()
        .any(|e| matches!(e, RunEvent::ToolSucceeded { .. })));

    // ...across exactly three model turns.
    assert_eq!(report.steps, 3);
}

#[tokio::test]
async fn fatal_provider_error_terminates() {
    let provider = FakeProvider::new(vec![Err(agent::ProviderError::Transport("offline".into()))]);
    let planner = ModelPlanner::new(Box::new(provider));

    let report = Agent::new(Budget::default())
        .run("anything", &planner, &default_registry())
        .await;

    assert!(matches!(report.outcome, TerminalReason::Fatal(_)));
}
