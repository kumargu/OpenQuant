//! VWAP — volume-weighted average price, cumulative from session open.
//!
//! VWAP = sum(typical_price * volume) / sum(volume)
//!
//! Unlike moving averages, VWAP anchors to the current session's actual traded
//! prices weighted by volume. Institutional traders benchmark execution against
//! VWAP, creating a gravitational pull toward it.
//!
//! Resets at each new trading session (day boundary detected from timestamps).

use super::RollingStats;

/// VWAP state — cumulative accumulators that reset daily.
///
/// O(1) per bar: just accumulates sums. The rolling std of deviation
/// reuses our existing `RollingStats`.
#[derive(Clone)]
pub struct VwapState {
    cum_tp_vol: f64,             // cumulative (typical_price * volume)
    cum_vol: f64,                // cumulative volume
    dev_stats: RollingStats<32>, // rolling std of (close - vwap)
    last_day: Option<i32>,       // day ordinal for session reset detection
    bar_count: usize,
}

/// VWAP feature values for a single bar.
#[derive(Debug, Clone, Copy, Default)]
pub struct VwapValues {
    /// Current VWAP price.
    pub vwap: f64,
    /// close - vwap.
    pub deviation: f64,
    /// deviation / rolling_std(deviation). 0.0 when std is near zero.
    pub z_score: f64,
    /// Minutes elapsed since session start (estimated from bar count).
    /// Used by strategy to gate on session windows.
    pub session_bars: usize,
}

impl Default for VwapState {
    fn default() -> Self {
        Self::new()
    }
}

impl VwapState {
    pub fn new() -> Self {
        Self {
            cum_tp_vol: 0.0,
            cum_vol: 0.0,
            dev_stats: RollingStats::new(),
            last_day: None,
            bar_count: 0,
        }
    }

    /// Reset cumulative accumulators for a new session.
    fn reset_session(&mut self) {
        self.cum_tp_vol = 0.0;
        self.cum_vol = 0.0;
        self.bar_count = 0;
        // Keep dev_stats — rolling window adapts naturally
    }

    /// Check if timestamp indicates a new day (UTC) and reset if so.
    /// Returns true if a reset occurred.
    fn maybe_reset(&mut self, timestamp_ms: i64) -> bool {
        if timestamp_ms <= 0 {
            return false; // no timestamp (backtesting with ts=0)
        }
        let day = (timestamp_ms / 86_400_000) as i32; // ms -> days since epoch
        match self.last_day {
            Some(prev) if prev == day => false,
            _ => {
                self.last_day = Some(day);
                self.reset_session();
                true
            }
        }
    }

    /// Update VWAP with a new bar. O(1), zero allocation.
    #[inline]
    pub fn update(
        &mut self,
        high: f64,
        low: f64,
        close: f64,
        volume: f64,
        timestamp_ms: i64,
    ) -> VwapValues {
        self.maybe_reset(timestamp_ms);
        self.bar_count += 1;

        let typical_price = (high + low + close) / 3.0;
        self.cum_tp_vol += typical_price * volume;
        self.cum_vol += volume;

        let vwap = if self.cum_vol > 1e-10 {
            self.cum_tp_vol / self.cum_vol
        } else {
            close // fallback when no volume
        };

        let deviation = close - vwap;
        self.dev_stats.push(deviation);
        let std_dev = self.dev_stats.std_dev();

        let z_score = if std_dev > 1e-10 {
            deviation / std_dev
        } else {
            0.0
        };

        VwapValues {
            vwap,
            deviation,
            z_score,
            session_bars: self.bar_count,
        }
    }

    /// Whether VWAP has enough data to be meaningful (at least 10 bars in session).
    pub fn is_ready(&self) -> bool {
        self.bar_count >= 10
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY1_MS: i64 = 86_400_000 * 19000; // some arbitrary day
    const DAY2_MS: i64 = DAY1_MS + 86_400_000;

    #[test]
    fn vwap_equals_close_on_first_bar() {
        let mut state = VwapState::new();
        let v = state.update(102.0, 98.0, 100.0, 1000.0, DAY1_MS);
        // typical = (102+98+100)/3 = 100.0, vwap = 100.0
        assert!((v.vwap - 100.0).abs() < 1e-10);
        assert!((v.deviation - 0.0).abs() < 1e-10);
    }

    #[test]
    fn vwap_weighted_by_volume() {
        let mut state = VwapState::new();
        // Bar 1: typical=100, volume=1000
        state.update(102.0, 98.0, 100.0, 1000.0, DAY1_MS);
        // Bar 2: typical=110, volume=3000
        let v = state.update(112.0, 108.0, 110.0, 3000.0, DAY1_MS + 60_000);
        // vwap = (100*1000 + 110*3000) / (1000+3000) = 430000/4000 = 107.5
        assert!((v.vwap - 107.5).abs() < 1e-10);
    }

    #[test]
    fn session_resets_on_new_day() {
        let mut state = VwapState::new();
        for i in 0..20 {
            state.update(102.0, 98.0, 100.0, 1000.0, DAY1_MS + i * 60_000);
        }
        assert_eq!(state.bar_count, 20);

        // New day — should reset
        let v = state.update(202.0, 198.0, 200.0, 1000.0, DAY2_MS);
        assert_eq!(state.bar_count, 1);
        // typical = (202+198+200)/3 = 200.0
        assert!((v.vwap - 200.0).abs() < 1e-10);
    }

    #[test]
    fn no_reset_with_zero_timestamp() {
        let mut state = VwapState::new();
        state.update(102.0, 98.0, 100.0, 1000.0, 0);
        state.update(102.0, 98.0, 100.0, 1000.0, 0);
        // Should accumulate, no reset
        assert_eq!(state.bar_count, 2);
    }

    #[test]
    fn z_score_negative_below_vwap() {
        let mut state = VwapState::new();
        // Build some VWAP history
        for i in 0..20 {
            state.update(102.0, 98.0, 100.0, 1000.0, DAY1_MS + i * 60_000);
        }
        // Price drops well below VWAP
        let v = state.update(92.0, 88.0, 90.0, 1000.0, DAY1_MS + 20 * 60_000);
        assert!(v.z_score < 0.0, "z_score should be negative below VWAP");
    }

    #[test]
    fn z_score_positive_above_vwap() {
        let mut state = VwapState::new();
        for i in 0..20 {
            state.update(102.0, 98.0, 100.0, 1000.0, DAY1_MS + i * 60_000);
        }
        // Price rises well above VWAP
        let v = state.update(112.0, 108.0, 110.0, 1000.0, DAY1_MS + 20 * 60_000);
        assert!(v.z_score > 0.0, "z_score should be positive above VWAP");
    }

    #[test]
    fn is_ready_after_10_bars() {
        let mut state = VwapState::new();
        for i in 0..9 {
            state.update(102.0, 98.0, 100.0, 1000.0, DAY1_MS + i * 60_000);
            assert!(!state.is_ready());
        }
        state.update(102.0, 98.0, 100.0, 1000.0, DAY1_MS + 9 * 60_000);
        assert!(state.is_ready());
    }

    #[test]
    fn zero_volume_uses_close_as_fallback() {
        let mut state = VwapState::new();
        let v = state.update(102.0, 98.0, 100.0, 0.0, DAY1_MS);
        assert!((v.vwap - 100.0).abs() < 1e-10);
    }
}
