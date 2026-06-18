//! Planning: turn the current run state into the next batch of actions. The
//! `Planner` trait is the seam the core loop dispatches through — a deterministic
//! [`rule::RulePlanner`] for offline runs, or a [`model::ModelPlanner`] backed by
//! a real provider.

use async_trait::async_trait;

use crate::action::Action;
use crate::error::PlannerError;
use crate::memory::Memory;
use crate::provider::Usage;
use crate::tool::ToolSchema;

pub mod model;
pub mod rule;

/// Everything a planner needs to decide the next step.
pub struct PlanContext<'a> {
    pub step: usize,
    pub max_steps: usize,
    pub memory: &'a Memory,
    pub tools: &'a [ToolSchema],
}

/// A planner's decision: optional reasoning, the actions to execute, and the
/// token usage incurred producing it (zero for non-model planners).
#[derive(Debug, Clone, PartialEq)]
pub struct PlanOutput {
    pub thought: Option<String>,
    pub actions: Vec<Action>,
    pub usage: Usage,
}

/// Produces the next [`PlanOutput`]. Runtime-dispatched (`&dyn Planner`).
#[async_trait]
pub trait Planner: Send + Sync {
    async fn plan_next(&self, ctx: &PlanContext<'_>) -> Result<PlanOutput, PlannerError>;
}
