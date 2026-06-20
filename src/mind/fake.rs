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
    /// When true, `decide` never resolves (for the mid-decide cancellation test).
    never_resolves: bool,
}

impl FakeMind {
    /// Create a new FakeMind with a script of decisions.
    pub fn new(script: Vec<Decision>, budget_summary: BudgetSummary) -> Self {
        Self {
            script: Arc::new(Mutex::new(script.into())),
            budget_summary,
            never_resolves: false,
        }
    }

    /// Create a FakeMind with default budget summary (for simple tests).
    pub fn with_script_only(script: Vec<Decision>) -> Self {
        let now = Instant::now();
        Self::new(
            script,
            BudgetSummary {
                tokens_remaining: 100_000,
                next_reset: now + std::time::Duration::from_secs(86400),
            },
        )
    }

    /// Create a FakeMind whose `decide` never resolves (for the mid-decide
    /// cancellation test, SC 12).
    pub fn pending() -> Self {
        let mut mind = Self::with_script_only(Vec::new());
        mind.never_resolves = true;
        mind
    }
}

#[async_trait]
impl Mind for FakeMind {
    async fn decide(&mut self, _perception: Perception) -> Decision {
        if self.never_resolves {
            // Park forever; the brainstem's select! cancels/answers Status around it.
            std::future::pending::<()>().await;
        }
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
