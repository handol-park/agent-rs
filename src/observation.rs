//! Observations and outcomes (spec 002 types).
//!
//! A bad action or a failed tool becomes a [`RecoverableError`] — recorded and
//! fed back to the model — not a terminal state.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::mind::TaskFault;

/// A failure that the loop **recovers** from: it is observed into memory and the
/// model gets another turn. Never terminates the run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RecoverableError {
    /// The model called a tool that is not registered.
    UnknownTool(String),
    /// A registered tool rejected its input or failed.
    ToolFailed { name: String, error: String },
}

impl RecoverableError {
    /// A short, model-facing rendering used when feeding the error back as an
    /// observation.
    pub fn message(&self) -> String {
        match self {
            RecoverableError::UnknownTool(name) => {
                format!("error: no tool named '{name}' is available")
            }
            RecoverableError::ToolFailed { name, error } => {
                format!("error: tool '{name}' failed: {error}")
            }
        }
    }
}

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
