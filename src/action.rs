//! Recoverable errors: failures the loop observes and feeds back to the model
//! rather than treating as terminal.
//!
//! The central design choice lives here: a bad action or a failed tool becomes a
//! [`RecoverableError`] — recorded and fed back to the model — not a terminal
//! state.

use serde::{Deserialize, Serialize};

/// A failure that the loop **recovers** from: it is observed into memory and the
/// model gets another turn. Never terminates the run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RecoverableError {
    /// The model called a tool that is not registered.
    UnknownTool(String),
    /// A registered tool rejected its input or failed.
    ToolFailed { name: String, error: String },
    /// The model produced an action that failed validation.
    MalformedPlan(String),
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
            RecoverableError::MalformedPlan(why) => format!("error: malformed action: {why}"),
        }
    }
}
