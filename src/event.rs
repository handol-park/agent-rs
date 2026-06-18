//! Structured run events. A consumer that receives the full `RunEvent` stream
//! can reconstruct exactly what happened in a run — every plan, tool call,
//! result, recovery, and the finish.

use serde_json::Value;

use crate::action::{Action, RecoverableError};

/// One observable moment in a run, emitted in order.
#[derive(Debug, Clone, PartialEq)]
pub enum RunEvent {
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
