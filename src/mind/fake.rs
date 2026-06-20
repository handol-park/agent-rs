//! FakeMind for testing the brainstem (scripted Decisions).

use async_trait::async_trait;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use tokio::time::Instant;

use crate::budget::BudgetSummary;
use crate::mind::{Decision, Mind, Perception};

/// A fake mind that returns scripted decisions. For brainstem tests.
pub struct FakeMind {
    script: Arc<Mutex<VecDeque<Decision>>>,
    budget_summary: BudgetSummary,
}

impl FakeMind {
    /// Create a new FakeMind with a script of decisions.
    pub fn new(script: Vec<Decision>, budget_summary: BudgetSummary) -> Self {
        Self {
            script: Arc::new(Mutex::new(script.into())),
            budget_summary,
        }
    }

    /// Create a FakeMind with default budget summary (for simple tests).
    pub fn with_script(script: Vec<Decision>) -> Self {
        let now = Instant::now();
        Self::new(
            script,
            BudgetSummary {
                tokens_remaining: 100_000,
                next_reset: now + std::time::Duration::from_secs(86400),
            },
        )
    }

    /// Create a FakeMind with default budget summary (for simple tests).
    pub fn with_script_only(script: Vec<Decision>) -> Self {
        Self::with_script(script)
    }

    /// Create a FakeMind that never completes decide (for cancellation tests).
    /// Returns an empty script that will panic when decide is called.
    pub fn pending() -> Self {
        Self::with_script(Vec::new())
    }
}

#[async_trait]
impl Mind for FakeMind {
    async fn decide(&mut self, _perception: Perception) -> Decision {
        self.script
            .lock()
            .expect("not poisoned")
            .pop_front()
            .expect("FakeMind script exhausted")
    }

    fn budget_summary(&self) -> BudgetSummary {
        self.budget_summary
    }
}
