//! Actions the planner can emit, and the outcomes of executing them.
//!
//! The central design choice lives here: a bad action or a failed tool becomes a
//! [`RecoverableError`] — recorded and fed back to the model — not a terminal
//! state. Only [`ActionOutcome::Finished`], an exhausted budget, or a fatal
//! provider error ends a run.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A single decision from the planner.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Action {
    /// Invoke a tool by name with JSON input. `call_id` ties the eventual result
    /// back to this request (native tool-calling `tool_call_id`).
    CallTool {
        call_id: String,
        name: String,
        input: Value,
    },
    /// End the run successfully with a final message.
    Finish { message: String },
}

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

/// The result of executing one [`Action`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ActionOutcome {
    /// A tool ran and returned JSON output.
    ToolResult {
        call_id: String,
        name: String,
        output: Value,
    },
    /// Something went wrong but the run continues. `call_id` links the failure
    /// back to the tool call that caused it (so it can be replayed to the model
    /// as a tool result); it is `None` for failures not tied to a call, such as
    /// an invalid `Finish`.
    Recoverable {
        call_id: Option<String>,
        error: RecoverableError,
    },
    /// The model chose to finish.
    Finished { message: String },
}

impl Action {
    /// Validate an action before execution. A failure is itself recoverable —
    /// an invalid action from the model is feedback, not a crash.
    pub fn validate(&self) -> Result<(), RecoverableError> {
        match self {
            Action::CallTool { name, input, .. } => {
                if name.trim().is_empty() {
                    return Err(RecoverableError::MalformedPlan("empty tool name".into()));
                }
                if !input.is_object() {
                    return Err(RecoverableError::MalformedPlan(format!(
                        "tool '{name}' input must be a JSON object"
                    )));
                }
                Ok(())
            }
            Action::Finish { message } => {
                if message.trim().is_empty() {
                    return Err(RecoverableError::MalformedPlan(
                        "empty finish message".into(),
                    ));
                }
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn call(name: &str, input: Value) -> Action {
        Action::CallTool {
            call_id: "c1".into(),
            name: name.into(),
            input,
        }
    }

    #[test]
    fn valid_call_tool_passes() {
        assert_eq!(
            call("calculator", json!({"expression": "1+1"})).validate(),
            Ok(())
        );
    }

    #[test]
    fn empty_tool_name_is_malformed() {
        assert_eq!(
            call("  ", json!({})).validate(),
            Err(RecoverableError::MalformedPlan("empty tool name".into()))
        );
    }

    #[test]
    fn non_object_input_is_malformed() {
        match call("calc", json!(["1+1"])).validate() {
            Err(RecoverableError::MalformedPlan(why)) => assert!(why.contains("JSON object")),
            other => panic!("expected MalformedPlan, got {other:?}"),
        }
    }

    #[test]
    fn empty_finish_message_is_malformed() {
        let a = Action::Finish {
            message: "   ".into(),
        };
        assert_eq!(
            a.validate(),
            Err(RecoverableError::MalformedPlan(
                "empty finish message".into()
            ))
        );
    }

    #[test]
    fn recoverable_messages_are_model_facing() {
        assert_eq!(
            RecoverableError::UnknownTool("foo".into()).message(),
            "error: no tool named 'foo' is available"
        );
    }
}
