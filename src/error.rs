//! Explicit error types. No `Box<dyn Error>` — every failure is a typed enum so
//! the loop can classify it (fatal vs recoverable). Recoverable *domain* values
//! live in [`crate::action::RecoverableError`]; this module holds the `Result`
//! error types returned by providers and tools.

use thiserror::Error;

/// How a [`ProviderError`] should be handled by the mind (spec 002 goal 3).
/// Classification is **total**: every `ProviderError` maps to exactly one class.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorClass {
    /// Retry with backoff, unbounded (network blip, timeout, 429, 5xx, decode).
    Transient,
    /// Auth / endpoint-or-model config (401, 403, 404). Terminates the run.
    ServiceFatal,
    /// Request shaped by this task's content (400, 422, any other rejected
    /// status). Fails the task; the service keeps serving.
    TaskFatal,
}

/// A failure talking to the model provider.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ProviderError {
    /// Network / connection failure reaching the provider.
    #[error("transport error: {0}")]
    Transport(String),
    /// The provider returned a non-success HTTP status. Carries the status code
    /// (drives classification, goal 3) and the response body.
    #[error("provider api error: {status}: {body}")]
    Api { status: u16, body: String },
    /// The response body could not be decoded into the expected shape.
    #[error("could not decode provider response: {0}")]
    Decode(String),
}

impl ProviderError {
    /// Classify this error for the mind's resilience layer (spec 002 goal 3).
    ///
    /// Total by construction — the `_` arm covers every unlisted status: an
    /// unlisted 5xx is transient (server-side, may self-heal); every other
    /// status (unlisted 4xx, 1xx, 3xx) is task-fatal (the server actively
    /// rejected this request — don't blindly retry it).
    pub fn class(&self) -> ErrorClass {
        match self {
            ProviderError::Transport(_) | ProviderError::Decode(_) => ErrorClass::Transient,
            ProviderError::Api { status, .. } => match status {
                429 | 500..=599 => ErrorClass::Transient,
                401 | 403 | 404 => ErrorClass::ServiceFatal,
                _ => ErrorClass::TaskFatal,
            },
        }
    }
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

/// A fatal, run-terminating error.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AgentError {
    #[error("fatal provider error: {0}")]
    Provider(#[from] ProviderError),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn api(status: u16) -> ProviderError {
        ProviderError::Api {
            status,
            body: "err".into(),
        }
    }

    #[test]
    fn provider_error_into_agent_error() {
        let a = AgentError::from(api(401));
        assert_eq!(
            a.to_string(),
            "fatal provider error: provider api error: 401: err"
        );
    }

    #[test]
    fn transport_and_decode_are_transient() {
        assert_eq!(
            ProviderError::Transport("x".into()).class(),
            ErrorClass::Transient
        );
        assert_eq!(
            ProviderError::Decode("x".into()).class(),
            ErrorClass::Transient
        );
    }

    #[test]
    fn rate_limit_and_5xx_are_transient() {
        assert_eq!(api(429).class(), ErrorClass::Transient);
        assert_eq!(api(500).class(), ErrorClass::Transient);
        assert_eq!(api(503).class(), ErrorClass::Transient);
        assert_eq!(api(599).class(), ErrorClass::Transient);
    }

    #[test]
    fn auth_and_config_are_service_fatal() {
        assert_eq!(api(401).class(), ErrorClass::ServiceFatal);
        assert_eq!(api(403).class(), ErrorClass::ServiceFatal);
        assert_eq!(api(404).class(), ErrorClass::ServiceFatal);
    }

    #[test]
    fn bad_request_is_task_fatal() {
        assert_eq!(api(400).class(), ErrorClass::TaskFatal);
        assert_eq!(api(422).class(), ErrorClass::TaskFatal);
    }

    /// The `_` arm: any unlisted status is total. An unlisted 4xx is task-fatal;
    /// an unlisted 5xx falls into the transient range; 1xx/3xx are task-fatal.
    #[test]
    fn unclassified_statuses_are_total() {
        assert_eq!(api(418).class(), ErrorClass::TaskFatal); // unlisted 4xx
        assert_eq!(api(451).class(), ErrorClass::TaskFatal); // unlisted 4xx
        assert_eq!(api(502).class(), ErrorClass::Transient); // unlisted 5xx
        assert_eq!(api(100).class(), ErrorClass::TaskFatal); // 1xx
        assert_eq!(api(301).class(), ErrorClass::TaskFatal); // 3xx
    }
}
