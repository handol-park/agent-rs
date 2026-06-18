//! Run budgets and terminal reasons. [`Budget::exceeded`] is a pure function of
//! `(step, tokens_used, elapsed)` so the loop's stopping logic is unit-testable
//! without touching the clock.

use std::time::Duration;

use crate::error::AgentError;

/// Hard limits on a single run. Any one being exceeded ends the run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Budget {
    pub max_steps: usize,
    pub max_tokens: u64,
    pub wall_clock: Duration,
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            max_steps: 8,
            max_tokens: 100_000,
            wall_clock: Duration::from_secs(120),
        }
    }
}

/// Why a run stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalReason {
    /// The model finished with this message.
    Finished(String),
    /// The step budget was reached without finishing.
    MaxSteps,
    /// The token budget was reached.
    TokenBudget,
    /// The wall-clock budget elapsed.
    TimedOut,
    /// A fatal, non-recoverable error.
    Fatal(AgentError),
}

impl Budget {
    /// Returns the terminal reason if any limit is exceeded at the *start* of
    /// `step` (1-based), else `None`. Token and time limits are inclusive.
    pub fn exceeded(
        &self,
        step: usize,
        tokens_used: u64,
        elapsed: Duration,
    ) -> Option<TerminalReason> {
        if step > self.max_steps {
            return Some(TerminalReason::MaxSteps);
        }
        if tokens_used >= self.max_tokens {
            return Some(TerminalReason::TokenBudget);
        }
        if elapsed >= self.wall_clock {
            return Some(TerminalReason::TimedOut);
        }
        None
    }

    /// Time left on the wall-clock budget (saturating at zero), for sizing the
    /// per-step provider `timeout`.
    pub fn remaining_time(&self, elapsed: Duration) -> Duration {
        self.wall_clock.saturating_sub(elapsed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn budget() -> Budget {
        Budget {
            max_steps: 3,
            max_tokens: 1000,
            wall_clock: Duration::from_secs(10),
        }
    }

    #[test]
    fn within_limits_returns_none() {
        assert_eq!(budget().exceeded(1, 0, Duration::ZERO), None);
        assert_eq!(budget().exceeded(3, 999, Duration::from_secs(9)), None);
    }

    #[test]
    fn step_over_limit_is_max_steps() {
        assert_eq!(
            budget().exceeded(4, 0, Duration::ZERO),
            Some(TerminalReason::MaxSteps)
        );
    }

    #[test]
    fn tokens_at_limit_is_token_budget() {
        assert_eq!(
            budget().exceeded(1, 1000, Duration::ZERO),
            Some(TerminalReason::TokenBudget)
        );
    }

    #[test]
    fn elapsed_at_limit_is_timed_out() {
        assert_eq!(
            budget().exceeded(1, 0, Duration::from_secs(10)),
            Some(TerminalReason::TimedOut)
        );
    }

    #[test]
    fn remaining_time_saturates() {
        assert_eq!(
            budget().remaining_time(Duration::from_secs(4)),
            Duration::from_secs(6)
        );
        assert_eq!(
            budget().remaining_time(Duration::from_secs(99)),
            Duration::ZERO
        );
    }
}
