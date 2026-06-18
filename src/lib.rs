//! agent-rs — a production-shaped LLM agent loop.
//!
//! `perceive -> plan -> act -> observe`, with recoverable errors, native LLM
//! tool-use, and budgets/observability. See `AGENTS.md` and `docs/`.
//!
//! Build order: P1 ships the pure types below; the loop, tools, provider, and
//! planner land in subsequent phases.

pub mod action;
pub mod budget;
pub mod error;
pub mod event;
pub mod memory;
pub mod provider;
pub mod tool;

pub use action::{Action, ActionOutcome, RecoverableError};
pub use budget::{Budget, TerminalReason};
pub use error::{AgentError, PlannerError, ProviderError, ToolError};
pub use event::RunEvent;
pub use memory::{Memory, MemorySnapshot, Record};
pub use provider::{fake::FakeProvider, openai::OpenAiProvider};
pub use provider::{Message, ModelRequest, ModelResponse, Provider, ToolCall, Usage};
pub use tool::{default_registry, Tool, ToolRegistry, ToolSchema};
