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
    /// Z-score at the bar that triggered entry (None if flat).
    /// Persisted to keep the closed-trade diagnostic faithful across
    /// state-snapshot reload boundaries.
    #[serde(default)]
    pub entry_z: Option<f64>,
    /// Running max of `-position * (z - entry_z)` since entry. None until
    /// the first post-entry diagnostic update; 0.0 after entry, monotonically
    /// non-decreasing while in position.
    #[serde(default)]
    pub max_adverse_z: Option<f64>,
    /// Date the running `max_adverse_z` was last advanced.
    #[serde(default)]
    pub max_adverse_date: Option<NaiveDate>,
    /// Running max of `position * (z - entry_z)` since entry.
    #[serde(default)]
    pub max_favorable_z: Option<f64>,
    /// Number of bars observed since entry (incremented by `update_diagnostics`).
    #[serde(default)]
    pub bars_held: u32,
    /// Stage-2 re-entry block. Set to `Some(p)` after a `MaxHoldExit` on
    /// position `p` (where `p ∈ {-1, +1}`). Cleared as soon as `z * p >= 0`,
    /// i.e. once the spread has mean-reverted past zero on the same side we
    /// had been positioned. While set, the state machine refuses to enter a
    /// position in direction `p`. Prevents the "exit-then-re-enter-at-worse-
    /// price" pathology that the first Stage 2 implementation tripped on
    /// (NVDA cycling -0.81 → -4.4 → -4.1 → -4.9 → -4.4 → -6.9 → -6.4 on
    /// April hl15).
    #[serde(default)]
    pub entry_block_direction: Option<i8>,
    /// Ring buffer of recent spread observations (for diagnostics).
    #[serde(default)]
    pub spread_history: VecDeque<f64>,
    /// Most recent z-score.
    pub last_z: Option<f64>,
}

impl Default for BasketState {
    fn default() -> Self {
        Self {
            position: 0,
            entry_date: None,
            entry_spread: None,
            entry_z: None,
            max_adverse_z: None,
            max_adverse_date: None,
            max_favorable_z: None,
            bars_held: 0,
            entry_block_direction: None,
            spread_history: VecDeque::with_capacity(60),
            last_z: None,
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

    /// Update running adverse / favorable excursion trackers. Caller must
    /// only invoke this when in a position. Adverse / favorable use the
    /// reviewer-corrected sign convention from issue #325:
    ///   adverse  = -position * (z - entry_z)   (loss-side excursion)
    ///   favorable =  position * (z - entry_z)   (profit-side excursion)
    pub fn update_diagnostics(&mut self, z: f64, date: NaiveDate) {
        let (Some(p), Some(z0)) = (
            (self.position != 0).then_some(self.position),
            self.entry_z,
        ) else {
            return;
        };
        if !z.is_finite() || !z0.is_finite() {
            return;
        }
        self.bars_held = self.bars_held.saturating_add(1);
        let adverse = -(p as f64) * (z - z0);
        let favorable = (p as f64) * (z - z0);
        match self.max_adverse_z {
            Some(prev) if prev >= adverse => {}
            _ => {
                self.max_adverse_z = Some(adverse);
                self.max_adverse_date = Some(date);
            }
        }
        match self.max_favorable_z {
            Some(prev) if prev >= favorable => {}
            _ => self.max_favorable_z = Some(favorable),
        }
    }

    /// Enter a position from flat. Resets diagnostic trackers and clears any
    /// re-entry block (the block is meant to gate this very call; if the
    /// caller decided to enter, the block is logically cleared).
    pub fn enter(&mut self, position: i8, date: NaiveDate, spread: f64, entry_z: f64) {
        self.position = position;
        self.entry_date = Some(date);
        self.entry_spread = Some(spread);
        self.entry_z = Some(entry_z);
        self.max_adverse_z = Some(0.0);
        self.max_adverse_date = Some(date);
        self.max_favorable_z = Some(0.0);
        self.bars_held = 0;
        self.entry_block_direction = None;
    }

    /// Flip to opposite position. Resets diagnostic trackers (the prior
    /// trade's diagnostics must be snapshotted before calling this).
    pub fn flip(&mut self, date: NaiveDate, spread: f64, entry_z: f64) {
        self.position = -self.position;
        self.entry_date = Some(date);
        self.entry_spread = Some(spread);
        self.entry_z = Some(entry_z);
        self.max_adverse_z = Some(0.0);
        self.max_adverse_date = Some(date);
        self.max_favorable_z = Some(0.0);
        self.bars_held = 0;
    }

    /// Flatten the position. Clears entry context and diagnostics.
    pub fn flatten(&mut self) {
        self.position = 0;
        self.entry_date = None;
        self.entry_spread = None;
        self.entry_z = None;
        self.max_adverse_z = None;
        self.max_adverse_date = None;
        self.max_favorable_z = None;
        self.bars_held = 0;
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
        state.enter(1, date, 0.05, -1.5);
        assert_eq!(state.position, 1);
        assert!(state.is_in_position());
        assert_eq!(state.entry_date, Some(date));
        assert_eq!(state.entry_spread, Some(0.05));
        assert_eq!(state.entry_z, Some(-1.5));
        assert_eq!(state.max_adverse_z, Some(0.0));
        assert_eq!(state.max_favorable_z, Some(0.0));
        assert_eq!(state.bars_held, 0);
    }

    #[test]
    fn test_flip_position() {
        let mut state = BasketState::new();
        let date1 = NaiveDate::from_ymd_opt(2026, 4, 20).unwrap();
        let date2 = NaiveDate::from_ymd_opt(2026, 4, 21).unwrap();
        state.enter(1, date1, 0.05, -1.5);
        state.flip(date2, 0.08, 1.6);
        assert_eq!(state.position, -1);
        assert_eq!(state.entry_date, Some(date2));
        assert_eq!(state.entry_spread, Some(0.08));
        assert_eq!(state.entry_z, Some(1.6));
        assert_eq!(state.bars_held, 0);
    }

    #[test]
    fn test_update_diagnostics_long_adverse() {
        let mut state = BasketState::new();
        let d0 = NaiveDate::from_ymd_opt(2026, 4, 1).unwrap();
        state.enter(1, d0, 0.05, -1.5);
        // z drifts further negative — adverse for long
        state.update_diagnostics(-2.0, d0.succ_opt().unwrap());
        assert_eq!(state.max_adverse_z, Some(0.5));
        assert_eq!(state.max_favorable_z, Some(0.0));
        // worse adverse — should advance
        state.update_diagnostics(-4.0, d0.succ_opt().unwrap().succ_opt().unwrap());
        assert_eq!(state.max_adverse_z, Some(2.5));
        // back toward entry — should not retreat the running max
        state.update_diagnostics(-1.0, d0.succ_opt().unwrap().succ_opt().unwrap().succ_opt().unwrap());
        assert_eq!(state.max_adverse_z, Some(2.5));
        assert_eq!(state.max_favorable_z, Some(0.5));
        assert_eq!(state.bars_held, 3);
    }

    #[test]
    fn test_update_diagnostics_short_adverse() {
        let mut state = BasketState::new();
        let d0 = NaiveDate::from_ymd_opt(2026, 4, 1).unwrap();
        state.enter(-1, d0, 0.10, 1.5);
        // z drifts further positive — adverse for short
        state.update_diagnostics(4.0, d0.succ_opt().unwrap());
        assert_eq!(state.max_adverse_z, Some(2.5));
        // mean revert — favorable side expands
        state.update_diagnostics(0.5, d0.succ_opt().unwrap().succ_opt().unwrap());
        assert_eq!(state.max_favorable_z, Some(1.0));
    }

    #[test]
    fn test_update_diagnostics_ignores_nan() {
        let mut state = BasketState::new();
        let d0 = NaiveDate::from_ymd_opt(2026, 4, 1).unwrap();
        state.enter(1, d0, 0.05, -1.5);
        state.update_diagnostics(f64::NAN, d0);
        assert_eq!(state.max_adverse_z, Some(0.0));
        assert_eq!(state.bars_held, 0);
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
    fn test_flatten_clears_position_fields() {
        let mut state = BasketState::new();
        let date = NaiveDate::from_ymd_opt(2026, 4, 21).unwrap();
        state.enter(1, date, 0.05, -1.5);
        state.update_diagnostics(-2.0, date.succ_opt().unwrap());
        state.flatten();
        assert_eq!(state.position, 0);
        assert_eq!(state.entry_date, None);
        assert_eq!(state.entry_spread, None);
        assert_eq!(state.entry_z, None);
        assert_eq!(state.max_adverse_z, None);
        assert_eq!(state.max_favorable_z, None);
        assert_eq!(state.bars_held, 0);
    }
}
