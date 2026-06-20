//! Run budgets and terminal reasons. For spec 001 (one-shot agent), [`Budget::exceeded`]
//! is a pure function of `(step, tokens_used, elapsed)`. For spec 002 (actor agent),
//! renewable budgets over recurring windows.

use std::time::Duration;
use tokio::time::Instant;

use crate::error::AgentError;

// ============================================================================
// Spec 001: One-shot budget (still in use by the old Agent::run)
// ============================================================================

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

// ============================================================================
// Spec 002: Renewable token budgets over recurring windows (goals 14-16)
// ============================================================================

/// A recurring window period for the renewable budget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Period {
    /// 24 hours.
    Daily,
    /// 7 days.
    Weekly,
    /// Custom period.
    Every(Duration),
}

impl Period {
    /// The duration of one window. Used for window calculation and reset timing.
    pub fn duration(&self) -> Duration {
        match self {
            Period::Daily => Duration::from_secs(24 * 60 * 60),
            Period::Weekly => Duration::from_secs(7 * 24 * 60 * 60),
            Period::Every(d) => *d,
        }
    }
}

/// A renewable token budget: `max_tokens` per `period`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenewableBudget {
    pub period: Period,
    pub max_tokens: u64,
}

impl Default for RenewableBudget {
    fn default() -> Self {
        Self {
            period: Period::Daily,
            max_tokens: 100_000,
        }
    }
}

/// The consumption state for a renewable budget. Tracks which window we're in
/// and how many tokens have been used in the current window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BudgetState {
    start: Instant,
    window: u64,
    used: u64,
}

impl BudgetState {
    /// Create a new budget state starting at `now`.
    pub fn new(now: Instant) -> Self {
        Self {
            start: now,
            window: 0,
            used: 0,
        }
    }

    /// Calculate which window `now` falls into. Window N = [start + N·period, start + (N+1)·period).
    /// Uses integer division of nanoseconds (exact, no float rounding).
    fn window(&self, now: Instant, period_duration: Duration) -> u64 {
        let elapsed = now.saturating_duration_since(self.start);
        let elapsed_nanos = elapsed.as_nanos();
        let period_nanos = period_duration.as_nanos();
        if period_nanos == 0 {
            0 // avoid division by zero
        } else {
            (elapsed_nanos / period_nanos) as u64
        }
    }

    /// Refresh: if we've crossed into a new window since the last check, reset consumption.
    /// Returns true if the window rolled (for event emission, goal 17).
    pub fn refresh(&mut self, now: Instant, budget: &RenewableBudget) -> bool {
        let current_window = self.window(now, budget.period.duration());
        if current_window > self.window {
            self.window = current_window;
            self.used = 0;
            true
        } else {
            false
        }
    }

    /// Charge `tokens` to the current window (after refreshing). Saturating add to prevent overflow.
    pub fn charge(&mut self, now: Instant, budget: &RenewableBudget, tokens: u64) {
        self.refresh(now, budget);
        self.used = self.used.saturating_add(tokens);
    }

    /// Tokens remaining in the current window. Saturating subtraction.
    pub fn remaining(&self, now: Instant, budget: &RenewableBudget) -> u64 {
        // Compute the current window without mutating (callers may not own &mut).
        let mut temp = self.clone();
        temp.refresh(now, budget);
        budget.max_tokens.saturating_sub(temp.used)
    }

    /// Is the current window exhausted? (used >= max_tokens)
    pub fn exhausted(&self, now: Instant, budget: &RenewableBudget) -> bool {
        let mut temp = self.clone();
        temp.refresh(now, budget);
        temp.used >= budget.max_tokens
    }

    /// When does the current window reset? start + (window + 1) * period
    pub fn next_reset(&self, now: Instant, budget: &RenewableBudget) -> Instant {
        let current_window = self.window(now, budget.period.duration());
        let period_dur = budget.period.duration();
        self.start + period_dur * (current_window as u32 + 1)
    }
}

/// Summary of budget state for external observers (goal 12).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BudgetSummary {
    pub tokens_remaining: u64,
    pub next_reset: Instant,
}

impl BudgetSummary {
    pub fn from_state(state: &BudgetState, now: Instant, budget: &RenewableBudget) -> Self {
        Self {
            tokens_remaining: state.remaining(now, budget),
            next_reset: state.next_reset(now, budget),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ========================================================================
    // Spec 001 tests (existing)
    // ========================================================================

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

    // ========================================================================
    // Spec 002 renewable budget tests
    // ========================================================================

    #[tokio::test(start_paused = true)]
    async fn window_calculation_from_zero() {
        let now = Instant::now();
        let budget = RenewableBudget {
            period: Period::Every(Duration::from_secs(10)),
            max_tokens: 100,
        };
        let state = BudgetState::new(now);

        // At t=0, window 0
        assert_eq!(state.window(now, budget.period.duration()), 0);

        // At t=10s, window 1
        tokio::time::advance(Duration::from_secs(10)).await;
        let now = Instant::now();
        assert_eq!(state.window(now, budget.period.duration()), 1);

        // At t=25s, window 2
        tokio::time::advance(Duration::from_secs(15)).await;
        let now = Instant::now();
        assert_eq!(state.window(now, budget.period.duration()), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn refresh_rolls_window_and_resets_used() {
        let now = Instant::now();
        let budget = RenewableBudget {
            period: Period::Every(Duration::from_secs(10)),
            max_tokens: 100,
        };
        let mut state = BudgetState::new(now);

        // Use 50 tokens in window 0
        state.charge(now, &budget, 50);
        assert_eq!(state.used, 50);
        assert_eq!(state.window, 0);

        // Advance to window 1
        tokio::time::advance(Duration::from_secs(10)).await;
        let now = Instant::now();

        // Refresh should roll the window and reset used
        let rolled = state.refresh(now, &budget);
        assert!(rolled);
        assert_eq!(state.window, 1);
        assert_eq!(state.used, 0);
    }

    #[tokio::test(start_paused = true)]
    async fn charge_saturates_on_overflow() {
        let now = Instant::now();
        let budget = RenewableBudget {
            period: Period::Daily,
            max_tokens: 100,
        };
        let mut state = BudgetState::new(now);

        state.used = u64::MAX - 10;
        state.charge(now, &budget, 50);
        assert_eq!(state.used, u64::MAX); // saturated
    }

    #[tokio::test(start_paused = true)]
    async fn remaining_saturates_at_zero() {
        let now = Instant::now();
        let budget = RenewableBudget {
            period: Period::Daily,
            max_tokens: 100,
        };
        let mut state = BudgetState::new(now);

        state.used = 100;
        assert_eq!(state.remaining(now, &budget), 0);

        state.used = 150;
        assert_eq!(state.remaining(now, &budget), 0); // saturates
    }

    #[tokio::test(start_paused = true)]
    async fn exhausted_when_used_gte_max() {
        let now = Instant::now();
        let budget = RenewableBudget {
            period: Period::Daily,
            max_tokens: 100,
        };
        let mut state = BudgetState::new(now);

        state.used = 99;
        assert!(!state.exhausted(now, &budget));

        state.used = 100;
        assert!(state.exhausted(now, &budget));

        state.used = 101;
        assert!(state.exhausted(now, &budget));
    }

    #[tokio::test(start_paused = true)]
    async fn next_reset_is_start_plus_next_window_boundary() {
        let now = Instant::now();
        let budget = RenewableBudget {
            period: Period::Every(Duration::from_secs(10)),
            max_tokens: 100,
        };
        let state = BudgetState::new(now);

        // At t=0 (window 0), next reset is at t=10
        let expected = now + Duration::from_secs(10);
        assert_eq!(state.next_reset(now, &budget), expected);

        // At t=5 (still window 0), next reset is still at t=10
        tokio::time::advance(Duration::from_secs(5)).await;
        let now = Instant::now();
        assert_eq!(state.next_reset(now, &budget), expected);

        // At t=10 (window 1), next reset is at t=20
        tokio::time::advance(Duration::from_secs(5)).await;
        let now = Instant::now();
        let expected = state.start + Duration::from_secs(20);
        assert_eq!(state.next_reset(now, &budget), expected);
    }

    #[tokio::test(start_paused = true)]
    async fn charge_straddles_window_boundary() {
        let now = Instant::now();
        let budget = RenewableBudget {
            period: Period::Every(Duration::from_secs(10)),
            max_tokens: 100,
        };
        let mut state = BudgetState::new(now);

        // Charge 30 in window 0
        state.charge(now, &budget, 30);
        assert_eq!(state.used, 30);

        // Advance to window 1 and charge 20
        tokio::time::advance(Duration::from_secs(10)).await;
        let now = Instant::now();
        state.charge(now, &budget, 20);
        // Should be in window 1 with used=20 (window 0's 30 is forgotten)
        assert_eq!(state.window, 1);
        assert_eq!(state.used, 20);
    }
}
