//! agent-rs — a production-shaped LLM agent framework.
//!
//! v0.2: `Mind + Brainstem` — cognition and runtime are split. The
//! `Brainstem` drives a perpetual task loop; `Mind` owns the LLM provider and
//! resilience logic. See `AGENTS.md` and `docs/` for the full architecture.

pub mod brainstem;
pub mod budget;
pub mod error;
pub mod event;
pub mod mind;
pub mod observation;
pub mod provider;
pub mod recoverable;
pub mod tool;

pub use brainstem::{Brainstem, Lifecycle, Snapshot, Task};
pub use budget::{BudgetState, BudgetSummary, Period, RenewableBudget};
pub use error::ErrorClass;
pub use error::{AgentError, ProviderError, ToolError};
pub use event::{RunEvent, Termination};
pub use mind::fake::FakeMind;
pub use mind::model::ModelMind;
pub use mind::{Command, Decision, Mind, Perception, Reason, TaskFault};
pub use observation::{Observation, Outcome, TaskOutcome};
pub use provider::{fake::FakeProvider, openai::OpenAiProvider};
pub use provider::{Message, ModelRequest, ModelResponse, Provider, ToolCall, Usage};
pub use recoverable::RecoverableError;
pub use tool::{default_registry, Tool, ToolRegistry, ToolSchema};
