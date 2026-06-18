//! Explicit error types. No `Box<dyn Error>` — every failure is a typed enum so
//! the loop can classify it (fatal vs recoverable). Recoverable *domain* values
//! live in [`crate::action::RecoverableError`]; this module holds the `Result`
//! error types returned by providers, tools, and planners.

use thiserror::Error;

/// A failure talking to the model provider.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ProviderError {
    /// Network / connection failure reaching the provider.
    #[error("transport error: {0}")]
    Transport(String),
    /// The provider returned a non-success HTTP status or error body.
    #[error("provider api error: {0}")]
    Api(String),
    /// The response body could not be decoded into the expected shape.
    #[error("could not decode provider response: {0}")]
    Decode(String),
}

/// A tool rejected its input or failed during execution. Both are **recoverable**
/// at the loop level — surfaced back to the model, never fatal.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ToolError {
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("execution failed: {0}")]
    ExecutionFailed(String),
}

/// A planner failed to produce a plan. The loop classifies these: [`Self::Provider`]
/// is fatal (terminates the run); [`Self::Malformed`] is recoverable (observed,
/// the loop continues).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum PlannerError {
    #[error(transparent)]
    Provider(#[from] ProviderError),
    #[error("malformed model output: {0}")]
    Malformed(String),
}

/// A fatal, run-terminating error. Carried by [`crate::budget::TerminalReason::Fatal`].
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AgentError {
    #[error("fatal provider error: {0}")]
    Provider(#[from] ProviderError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_error_into_planner_error() {
        let p = PlannerError::from(ProviderError::Transport("down".into()));
        assert_eq!(
            p,
            PlannerError::Provider(ProviderError::Transport("down".into()))
        );
    }

    #[test]
    fn provider_error_into_agent_error() {
        let a = AgentError::from(ProviderError::Api("401".into()));
        assert_eq!(
            a.to_string(),
            "fatal provider error: provider api error: 401"
        );
    }
}
