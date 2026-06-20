//! Mind trait and cognitive types (spec 002 goals 1-5).

pub mod fake;
pub mod model;

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::mpsc::UnboundedSender;
use tokio::time::Instant;

use crate::budget::BudgetSummary;
use crate::error::AgentError;
use crate::event::RunEvent;
use crate::observation::{Observation, Outcome};
use crate::tool::ToolSchema;

/// A perception passed to the mind: either a new task, an observation from the
/// previous command, or a resume signal after throttling.
#[derive(Debug, Clone, PartialEq)]
pub enum Perception {
    /// Start a new task (resets working memory).
    NewTask { goal: String },
    /// Observation from the previous command.
    Observation(Observation),
    /// Resume after throttling (no new stimulus, working memory unchanged).
    Resume,
}

/// A command the mind wants the brainstem to actuate.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    CallTool {
        call_id: String,
        name: String,
        input: Value,
    },
}

/// A decision from the mind.
#[derive(Debug, Clone, PartialEq)]
pub enum Decision {
    /// Actuate this command.
    Act(Command),
    /// Task completed successfully.
    Done(Outcome),
    /// Task or service failed.
    Failed(Reason),
    /// Token budget exhausted; throttle until this instant.
    Throttle(Instant),
}

/// Why a decision failed: task-scoped or service-scoped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reason {
    /// Task-scoped failure (task ends, service continues).
    Task(TaskFault),
    /// Service-fatal failure (run terminates).
    Service(AgentError),
}

/// Task-scoped failure reasons.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskFault {
    /// Step-liveness budget exceeded (non-convergence).
    NoProgress,
    /// The budget window is too small to fund even one decision.
    BudgetTooSmall,
    /// Bad request (400, 422 HTTP errors).
    BadRequest(String),
    /// Malformed model output (unparseable, unusable).
    Malformed(String),
}

/// The Mind trait: given a perception, decide the next action.
#[async_trait]
pub trait Mind: Send + Sync {
    /// Decide the next action given a perception. Accumulates perceptions into
    /// working memory (NewTask resets; Observation appends; Resume does not fold).
    async fn decide(&mut self, perception: Perception) -> Decision;

    /// Read the budget summary (tokens remaining, next reset) as of the last decision.
    fn budget_summary(&self) -> BudgetSummary;

    /// Inject the brainstem's `RunEvent` sink so cognitive events (`RetryScheduled`,
    /// `WindowReset`) emitted *inside* a `decide` interleave on the brainstem's
    /// single event stream (plan 002: both ends are producers on one channel).
    /// The default is a no-op for minds (e.g. `FakeMind`) that emit nothing.
    fn set_event_sink(&mut self, _events: UnboundedSender<RunEvent>) {}

    /// Inject the tool schemas the brainstem's registry advertises, so a
    /// model-backed mind can tell the provider which tools exist. Default is a
    /// no-op for minds that don't talk to a provider (e.g. `FakeMind`).
    fn set_tools(&mut self, _tools: Vec<ToolSchema>) {}
}
