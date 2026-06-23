//! Structured run events for the v0.2 actor service. A consumer that receives
//! the full `RunEvent` stream can reconstruct exactly what happened.
//!
//! Events: TaskReceived, Command, CommandResult, RecoverableObservation,
//! RetryScheduled, TaskCompleted, TaskFailed, WindowReset, ThrottleSleep, Terminated.

use std::time::Duration;
use tokio::time::Instant;

use crate::error::ProviderError;
use crate::mind::TaskFault;
use crate::observation::{Outcome, RecoverableError};

/// One observable moment in a run, emitted in order.
#[derive(Debug, Clone, PartialEq)]
pub enum RunEvent {
    /// A new task was received from the inbox (goal 17).
    TaskReceived { goal: String },
    /// The mind decided to issue a command (goal 17).
    Command { call_id: String, name: String },
    /// A command was actuated and produced a result (goal 17).
    CommandResult { call_id: String, ok: bool },
    /// A recoverable error was observed.
    RecoverableObservation { error: RecoverableError },
    /// A transient provider error triggered a retry (goal 17, emitted by ModelMind).
    RetryScheduled {
        attempt: usize,
        delay: Duration,
        error: ProviderError,
    },
    /// A task completed successfully (goal 17).
    TaskCompleted { outcome: Outcome },
    /// A task failed (task-scoped, service continues) (goal 17).
    TaskFailed { reason: TaskFault },
    /// The token budget window rolled to a new window (goal 17, emitted by ModelMind).
    WindowReset { window: u64 },
    /// The brainstem is sleeping until the budget resets (goal 17).
    ThrottleSleep { wake: Instant },
    /// The run terminated (goal 17).
    Terminated { reason: Termination },
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
        let a = RunEvent::TaskReceived {
            goal: "test".into(),
        };
        let b = RunEvent::TaskReceived {
            goal: "test".into(),
        };
        assert_eq!(a, b);
        assert_ne!(
            a,
            RunEvent::TaskReceived {
                goal: "other".into()
            }
        );
    }
}
