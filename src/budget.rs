//! Renewable token budgets over recurring windows (spec 002).

use std::time::Duration;
use tokio::time::Instant;

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
        elapsed_nanos
            .checked_div(period_nanos)
            .map(|w| w as u64)
            .unwrap_or(0)
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

    /// The current window index (0-based), as of the last `refresh`/`charge`.
    /// Used to label the `WindowReset` event (goal 17).
    pub fn current_window(&self) -> u64 {
        self.window
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

    /// When does the current window reset? start + (window + 1) * period.
    /// Uses u128 nanosecond arithmetic to avoid the silent truncation that
    /// `Duration * u32` would force for large window indices (e.g.
    /// `Period::Every(1ms)` overflows `u32` windows after ~49 days).
    pub fn next_reset(&self, now: Instant, budget: &RenewableBudget) -> Instant {
        let current_window = self.window(now, budget.period.duration());
        let reset_nanos = budget
            .period
            .duration()
            .as_nanos()
            .saturating_mul(current_window as u128 + 1);
        self.start + Duration::from_nanos(reset_nanos.min(u64::MAX as u128) as u64)
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
impl BudgetState {
    /// Test helper: directly set the used token count.
    pub fn set_used_for_test(&mut self, used: u64) {
        self.used = used;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weekly_period_is_seven_days() {
        assert_eq!(
            Period::Weekly.duration(),
            Duration::from_secs(7 * 24 * 60 * 60)
        );
    }

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
    async fn next_reset_does_not_truncate_large_window_index() {
        // With a 1ms period the window index passes u32::MAX after ~49 days.
        // The old `Duration * u32` cast truncated it and returned a past instant.
        let start = Instant::now();
        let budget = RenewableBudget {
            period: Period::Every(Duration::from_millis(1)),
            max_tokens: 100,
        };
        let mut state = BudgetState::new(start);

        tokio::time::advance(Duration::from_millis(u32::MAX as u64 + 5)).await;
        let now = Instant::now();
        state.refresh(now, &budget);

        let reset = state.next_reset(now, &budget);
        assert!(
            reset > now,
            "next_reset must stay in the future past u32::MAX windows, not truncate to the past"
        );
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
