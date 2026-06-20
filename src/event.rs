//! Structured run events. A consumer that receives the full `RunEvent` stream
//! can reconstruct exactly what happened in a run.
//!
//! Spec 001 events: StepStarted, Planned, ToolCalled, ToolSucceeded, Recovered, Finished.
//! Spec 002 adds: TaskReceived, Command, CommandResult, RetryScheduled, TaskCompleted,
//! TaskFailed, WindowReset, ThrottleSleep, Terminated.

use serde_json::Value;
use std::time::Duration;
use tokio::time::Instant;

use crate::action::{Action, RecoverableError};
use crate::error::ProviderError;
use crate::mind::TaskFault;
use crate::observation::Outcome;

/// One observable moment in a run, emitted in order.
#[derive(Debug, Clone, PartialEq)]
pub enum RunEvent {
    // ========================================================================
    // Spec 001 events (one-shot agent)
    // ========================================================================
    StepStarted {
        step: usize,
    },
    Planned {
        step: usize,
        thought: Option<String>,
        actions: Vec<Action>,
    },
    ToolCalled {
        step: usize,
        name: String,
        input: Value,
    },
    ToolSucceeded {
        step: usize,
        name: String,
        output: Value,
    },
    /// A recoverable error occurred and was fed back into memory.
    Recovered {
        step: usize,
        error: RecoverableError,
    },
    Finished {
        step: usize,
        message: String,
    },

    // ========================================================================
    // Spec 002 events (actor agent)
    // ========================================================================
    /// A new task was received from the inbox (goal 17).
    TaskReceived {
        goal: String,
    },
    /// The mind decided to issue a command (goal 17).
    Command {
        call_id: String,
        name: String,
    },
    /// A command was actuated and produced a result (goal 17).
    CommandResult {
        call_id: String,
        ok: bool,
    },
    /// A recoverable error was observed (spec 002 variant, no step).
    RecoverableObservation {
        error: RecoverableError,
    },
    /// A transient provider error triggered a retry (goal 17, emitted by ModelMind).
    RetryScheduled {
        attempt: usize,
        delay: Duration,
        error: ProviderError,
    },
    /// A task completed successfully (goal 17).
    TaskCompleted {
        outcome: Outcome,
    },
    /// A task failed (task-scoped, service continues) (goal 17).
    TaskFailed {
        reason: TaskFault,
    },
    /// The token budget window rolled to a new window (goal 17, emitted by ModelMind).
    WindowReset {
        window: u64,
    },
    /// The brainstem is sleeping until the budget resets (goal 17).
    ThrottleSleep {
        wake: Instant,
    },
    /// The run terminated (goal 17).
    Terminated {
        reason: Termination,
    },
}

/// Why the brainstem run terminated (spec 002).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Termination {
    /// Cancellation token was triggered.
    Cancelled,
    /// A service-fatal error occurred.
    Fatal(String),
    /// The inbox closed (all senders dropped).
    Stopped,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_compare_by_value() {
        let a = RunEvent::StepStarted { step: 1 };
        let b = RunEvent::StepStarted { step: 1 };
        assert_eq!(a, b);
        assert_ne!(a, RunEvent::StepStarted { step: 2 });
    }
}
