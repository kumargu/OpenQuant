//! Per-basket runtime state.

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;

/// Per-basket runtime state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BasketState {
    /// Current position: -1 (short), 0 (flat), +1 (long).
    pub position: i8,
    /// Date when position was entered (None if flat).
    pub entry_date: Option<NaiveDate>,
    /// Spread value when position was entered.
    pub entry_spread: Option<f64>,
    /// z-score at the moment we entered the current position. Lets the
    /// transition log compare entry_z vs exit_z so we can tell
    /// "real mean-reversion (entered at +k, exited at -k)" from
    /// "whipsaw (entered at +k, exited at +k+epsilon)".
    #[serde(default)]
    pub entry_z: Option<f64>,
    /// Ring buffer of recent spread observations (for diagnostics).
    #[serde(default)]
    pub spread_history: VecDeque<f64>,
    /// Most recent z-score.
    pub last_z: Option<f64>,
    /// Stop-loss re-arm gate. Set to true after a stop-out; while
    /// suspended, the engine refuses to enter new positions on this
    /// basket. Cleared once `|z| < k` (the spread is back inside the
    /// Bertram band, i.e. the regime has plausibly returned). Mirrors
    /// quant-lab's `simulate_bertram_with_stop` re-arm logic so a
    /// stopped-out basket can't immediately re-enter at the same
    /// adverse z and re-stop on the next bar.
    #[serde(default)]
    pub suspended: bool,
}

impl Default for BasketState {
    fn default() -> Self {
        Self {
            position: 0,
            entry_date: None,
            entry_spread: None,
            entry_z: None,
            spread_history: VecDeque::with_capacity(60),
            last_z: None,
            suspended: false,
        }
    }
}

impl BasketState {
    /// Create a new flat state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a new spread observation.
    pub fn record_spread(&mut self, spread: f64) {
        const MAX_HISTORY: usize = 60;
        if self.spread_history.len() >= MAX_HISTORY {
            self.spread_history.pop_front();
        }
        self.spread_history.push_back(spread);
    }

    /// Check if currently in a position (long or short).
    pub fn is_in_position(&self) -> bool {
        self.position != 0
    }

    /// Enter a position.
    pub fn enter(&mut self, position: i8, date: NaiveDate, spread: f64, z: f64) {
        self.position = position;
        self.entry_date = Some(date);
        self.entry_spread = Some(spread);
        self.entry_z = Some(z);
    }

    /// Flip to opposite position.
    pub fn flip(&mut self, date: NaiveDate, spread: f64, z: f64) {
        self.position = -self.position;
        self.entry_date = Some(date);
        self.entry_spread = Some(spread);
        self.entry_z = Some(z);
    }

    /// Flatten the position while preserving diagnostics.
    pub fn flatten(&mut self) {
        self.position = 0;
        self.entry_date = None;
        self.entry_spread = None;
        self.entry_z = None;
    }

    /// Flatten and arm the suspended re-arm gate. Called by the engine
    /// when an open position breaches its z-score adverse-move stop
    /// (cointegration plausibly broken).
    pub fn stop_out(&mut self) {
        self.flatten();
        self.suspended = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_state_is_flat() {
        let state = BasketState::new();
        assert_eq!(state.position, 0);
        assert!(!state.is_in_position());
    }

    #[test]
    fn test_enter_position() {
        let mut state = BasketState::new();
        let date = NaiveDate::from_ymd_opt(2026, 4, 21).unwrap();
        state.enter(1, date, 0.05, 1.5);
        assert_eq!(state.position, 1);
        assert!(state.is_in_position());
        assert_eq!(state.entry_date, Some(date));
        assert_eq!(state.entry_spread, Some(0.05));
    }

    #[test]
    fn test_flip_position() {
        let mut state = BasketState::new();
        let date1 = NaiveDate::from_ymd_opt(2026, 4, 20).unwrap();
        let date2 = NaiveDate::from_ymd_opt(2026, 4, 21).unwrap();
        state.enter(1, date1, 0.05, 1.6);
        state.flip(date2, 0.08, -1.7);
        assert_eq!(state.position, -1);
        assert_eq!(state.entry_date, Some(date2));
        assert_eq!(state.entry_spread, Some(0.08));
    }

    #[test]
    fn test_spread_history_ring_buffer() {
        let mut state = BasketState::new();
        for i in 0..100 {
            state.record_spread(i as f64);
        }
        assert_eq!(state.spread_history.len(), 60);
        assert_eq!(state.spread_history.front(), Some(&40.0));
        assert_eq!(state.spread_history.back(), Some(&99.0));
    }

    #[test]
    fn test_stop_out_arms_suspended_gate() {
        let mut state = BasketState::new();
        let date = NaiveDate::from_ymd_opt(2026, 4, 21).unwrap();
        state.enter(1, date, 0.05, 1.5);
        assert!(!state.suspended);
        state.stop_out();
        assert_eq!(state.position, 0);
        assert_eq!(state.entry_date, None);
        assert!(state.suspended, "stop_out must arm the re-entry gate");
    }

    #[test]
    fn test_flatten_does_not_arm_suspended_gate() {
        // Plain flatten() (used by portfolio admission, snapshot
        // restore, etc.) must NOT touch suspended — only stop_out()
        // arms the re-entry gate. Mixing the two would suspend baskets
        // that simply got demoted by the portfolio cap.
        let mut state = BasketState::new();
        let date = NaiveDate::from_ymd_opt(2026, 4, 21).unwrap();
        state.enter(1, date, 0.05, 1.5);
        state.flatten();
        assert!(!state.suspended);
    }

    #[test]
    fn test_flatten_clears_position_fields() {
        let mut state = BasketState::new();
        let date = NaiveDate::from_ymd_opt(2026, 4, 21).unwrap();
        state.enter(1, date, 0.05, 1.5);
        state.flatten();
        assert_eq!(state.position, 0);
        assert_eq!(state.entry_date, None);
        assert_eq!(state.entry_spread, None);
    }
}
