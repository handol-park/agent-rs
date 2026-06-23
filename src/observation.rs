//! Observations and outcomes (spec 002 types).

use serde_json::Value;

use crate::mind::TaskFault;
use crate::recoverable::RecoverableError;

/// The result of actuating a command: a tool result or a recoverable error.
#[derive(Debug, Clone, PartialEq)]
pub enum Observation {
    /// A tool call succeeded and returned this output.
    ToolResult { call_id: String, output: Value },
    /// A recoverable error occurred (tool failed, unknown tool, etc.).
    Recoverable {
        call_id: Option<String>,
        error: RecoverableError,
    },
}

/// A successful task result (the final answer).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub message: String,
}

/// The result of a task: completed or failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskOutcome {
    Completed(Outcome),
    Failed(TaskFault),
}
