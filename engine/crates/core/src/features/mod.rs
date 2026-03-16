//! Incremental feature computation from price bars.
//!
//! Features are the quantitative inputs to strategies. Every feature updates
//! in O(1) per bar using fixed-size stack buffers — zero heap allocation
//! in the hot path.
//!
//! # Module structure
//!
//! Each indicator is in its own module with documentation on when it's useful:
//!
//! - [`ring_buf`] — fixed-size circular buffer (foundation for SMA, RollingStats)
//! - [`rolling_stats`] — rolling mean/std (z-score, Bollinger, relative volume)
//! - [`sma`] — simple moving average (trend filter, Bollinger center)
//! - [`ema`] — exponential moving average + Wilder variant (momentum, ADX)
//! - [`adx`] — average directional index (trend strength 0-100)
//!
//! # Data flow
//!
//! ```text
//!  Bar (close, high, low, volume)
//!   │
//!   ├──► RingBuf<64>       ──► N-bar lookback returns
//!   ├──► Sma<32>           ──► SMA-32 (trend filter, Bollinger center)
//!   ├──► Sma<64>           ──► SMA-64 (long-term trend)
//!   ├──► RollingStats<32>  ──► return std → z-score
//!   ├──► RollingStats<32>  ──► volume mean → relative volume
//!   ├──► RollingStats<32>  ──► close std → Bollinger Bands
//!   ├──► RollingStats<16>  ──► true range mean → ATR
//!   ├──► Ema(10), Ema(30)  ──► fast/slow crossover (momentum)
//!   └──► Adx(14)           ──► trend strength + directional indicators
//! ```

pub mod adx;
pub mod ema;
pub mod ring_buf;
pub mod rolling_stats;
pub mod sma;

// Re-export all types so existing code (`use crate::features::*`) still works.
pub use adx::Adx;
pub use ema::{Ema, WilderEma};
pub use ring_buf::RingBuf;
pub use rolling_stats::RollingStats;
pub use sma::Sma;

// ---------------------------------------------------------------------------
// Feature output — all computed features for a single bar
// ---------------------------------------------------------------------------

/// All computed features for a single symbol at current bar.
#[derive(Debug, Clone, Default)]
pub struct FeatureValues {
    // --- V1: mean-reversion features ---
    pub return_1: f64,        // 1-bar return
    pub return_5: f64,        // 5-bar return
    pub return_20: f64,       // 20-bar return
    pub sma_20: f64, // simple moving average of close (32-bar window, power-of-2 constraint)
    pub sma_50: f64, // 64-bar simple moving average of close (trend)
    pub atr: f64,    // average true range (16-bar window)
    pub return_std_20: f64, // 32-bar rolling std dev of 1-bar returns
    pub return_z_score: f64, // return_1 / return_std_20
    pub relative_volume: f64, // current volume / 32-bar avg volume
    pub bar_range: f64, // high - low
    pub close_location: f64, // (close - low) / (high - low)
    pub trend_up: bool, // true when close > sma_50 (bullish trend)
    pub warmed_up: bool, // true once all features have enough data

    // --- V2: momentum features ---
    pub ema_fast: f64,             // EMA(10) — fast exponential moving average
    pub ema_slow: f64,             // EMA(30) — slow exponential moving average
    pub ema_fast_above_slow: bool, // true when EMA(10) > EMA(30) (level, not event)
    pub adx: f64,                  // trend strength 0-100
    pub plus_di: f64,              // +DI: bullish directional indicator
    pub minus_di: f64,             // -DI: bearish directional indicator

    // --- V2: Bollinger Band features ---
    pub bollinger_upper: f64,     // SMA(32) + 2 × std_dev(close, 32)
    pub bollinger_lower: f64,     // SMA(32) - 2 × std_dev(close, 32)
    pub bollinger_pct_b: f64,     // (close - lower) / (upper - lower), 0-1 normally
    pub bollinger_bandwidth: f64, // (upper - lower) / SMA(32), normalized width
}

// ---------------------------------------------------------------------------
// Per-symbol feature state — orchestrates all indicators
// ---------------------------------------------------------------------------

/// Per-symbol feature state. All buffers are stack-allocated, fixed-size.
///
/// Buffer sizes use power-of-2 capacity for bitwise index wrapping.
/// This means actual window sizes are 32 and 64, not 20 and 50.
#[derive(Clone)]
pub struct FeatureState {
    // V1 state
    closes: RingBuf<64>,            // last 64 closes for lookback returns
    sma: Sma<32>,                   // 32-bar SMA
    sma_long: Sma<64>,              // 64-bar SMA for trend detection
    atr_stats: RollingStats<16>,    // 16-bar ATR via rolling mean of true range
    return_stats: RollingStats<32>, // 32-bar rolling std of 1-bar returns
    volume_stats: RollingStats<32>, // 32-bar rolling avg of volume
    prev_close: Option<f64>,        // previous close for true range calculation
    bar_count: usize,
    warmup_period: usize,

    // V2 state: momentum indicators
    ema_fast: Ema, // EMA(10) for momentum crossover
    ema_slow: Ema, // EMA(30) for momentum crossover
    adx: Adx,      // ADX(14) for trend strength

    // V2 state: Bollinger Bands
    close_stats: RollingStats<32>, // rolling std of close prices
}

impl Default for FeatureState {
    fn default() -> Self {
        Self::new()
    }
}

impl FeatureState {
    pub fn new() -> Self {
        // Warmup must exceed all indicator requirements:
        // Sma<64> needs 64 bars (binding constraint).
        // EMA(30) needs ~30, ADX(14) needs 28, RollingStats<32> needs 32.
        const WARMUP: usize = 64;
        const EMA_SLOW_PERIOD: usize = 30;
        const ADX_PERIOD: usize = 14;
        // Guard: warmup must cover all indicators (compile-time)
        const {
            assert!(WARMUP >= 64, "must cover Sma<64>");
            assert!(WARMUP >= EMA_SLOW_PERIOD, "must cover EMA slow period");
            assert!(WARMUP >= ADX_PERIOD * 2, "must cover ADX (2×period)");
            assert!(WARMUP >= 32, "must cover RollingStats<32>");
        }

        Self {
            closes: RingBuf::new(),
            sma: Sma::new(),
            sma_long: Sma::new(),
            atr_stats: RollingStats::new(),
            return_stats: RollingStats::new(),
            volume_stats: RollingStats::new(),
            prev_close: None,
            bar_count: 0,
            warmup_period: WARMUP,

            ema_fast: Ema::new(10),
            ema_slow: Ema::new(30),
            adx: Adx::new(14),

            close_stats: RollingStats::new(),
        }
    }

    /// Update features with a new bar. Returns computed values.
    /// This is the hot path — zero heap allocation, O(1) per call.
    #[inline]
    pub fn update(&mut self, close: f64, high: f64, low: f64, volume: f64) -> FeatureValues {
        let prev_close = self.closes.last();
        self.closes.push(close);
        self.bar_count += 1;

        // 1-bar return
        let return_1 = match prev_close {
            Some(pc) if pc != 0.0 => (close - pc) / pc,
            _ => 0.0,
        };
        self.return_stats.push(return_1);

        // N-bar returns via lookback
        let return_5 = self
            .closes
            .ago(5)
            .filter(|&pc| pc != 0.0)
            .map(|pc| (close - pc) / pc)
            .unwrap_or(0.0);

        let return_20 = self
            .closes
            .ago(19)
            .filter(|&pc| pc != 0.0)
            .map(|pc| (close - pc) / pc)
            .unwrap_or(0.0);

        // SMA via running sum (O(1))
        let sma_20 = self.sma.push(close);
        let sma_50 = self.sma_long.push(close);

        // ATR: True Range = max(H-L, |H-prev_close|, |L-prev_close|)
        let true_range = match self.prev_close {
            Some(pc) => {
                let hl = high - low;
                let hc = (high - pc).abs();
                let lc = (low - pc).abs();
                hl.max(hc).max(lc)
            }
            None => high - low,
        };
        self.atr_stats.push(true_range);
        self.prev_close = Some(close);
        let atr = self.atr_stats.mean();

        // Volume
        self.volume_stats.push(volume);
        let avg_volume = self.volume_stats.mean();
        let relative_volume = if avg_volume > 0.0 {
            volume / avg_volume
        } else {
            1.0
        };

        // Z-score
        let std_dev = self.return_stats.std_dev();
        let return_z_score = if std_dev > 1e-10 {
            return_1 / std_dev
        } else {
            0.0
        };

        // Bar shape
        let range = high - low;
        let close_location = if range > 0.0 {
            (close - low) / range
        } else {
            0.5
        };

        // Trend: close above SMA-64 = bullish
        let trend_up = close > sma_50;

        // --- V2: Momentum indicators ---
        let ema_fast = self.ema_fast.push(close);
        let ema_slow = self.ema_slow.push(close);
        let ema_fast_above_slow = ema_fast > ema_slow;
        let (adx_val, plus_di, minus_di) = self.adx.update(high, low, close);

        // --- V2: Bollinger Bands ---
        // Standard: SMA(N) ± 2 × std_dev(close_prices, N)
        self.close_stats.push(close);
        let close_std = self.close_stats.std_dev();
        let bollinger_upper = sma_20 + 2.0 * close_std;
        let bollinger_lower = sma_20 - 2.0 * close_std;
        let bb_width = bollinger_upper - bollinger_lower;
        let bollinger_pct_b = if bb_width > 1e-10 {
            (close - bollinger_lower) / bb_width
        } else {
            0.5
        };
        let bollinger_bandwidth = if sma_20 > 1e-10 {
            bb_width / sma_20
        } else {
            0.0
        };

        FeatureValues {
            return_1,
            return_5,
            return_20,
            sma_20,
            sma_50,
            atr,
            return_std_20: std_dev,
            return_z_score,
            relative_volume,
            bar_range: range,
            close_location,
            trend_up,
            warmed_up: self.bar_count >= self.warmup_period,
            ema_fast,
            ema_slow,
            ema_fast_above_slow,
            adx: adx_val,
            plus_di,
            minus_di,
            bollinger_upper,
            bollinger_lower,
            bollinger_pct_b,
            bollinger_bandwidth,
        }
    }
}

// ---------------------------------------------------------------------------
// Integration tests — FeatureState orchestrating all indicators together
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feature_warmup() {
        let mut state = FeatureState::new();
        for i in 0..63 {
            let f = state.update(100.0 + i as f64, 101.0 + i as f64, 99.0 + i as f64, 1000.0);
            assert!(!f.warmed_up, "should not be warmed up at bar {i}");
        }
        let f = state.update(120.0, 121.0, 119.0, 1000.0);
        assert!(f.warmed_up, "should be warmed up at bar 64");
    }

    #[test]
    fn return_1_computation() {
        let mut state = FeatureState::new();
        state.update(100.0, 101.0, 99.0, 1000.0);
        let f = state.update(105.0, 106.0, 104.0, 1000.0);
        assert!((f.return_1 - 0.05).abs() < 1e-10, "expected 5% return");
    }

    #[test]
    fn return_1_first_bar_is_zero() {
        let mut state = FeatureState::new();
        let f = state.update(100.0, 101.0, 99.0, 1000.0);
        assert_eq!(f.return_1, 0.0, "first bar has no previous close");
    }

    #[test]
    fn relative_volume_spike() {
        let mut state = FeatureState::new();
        for _ in 0..20 {
            state.update(100.0, 101.0, 99.0, 1000.0);
        }
        let f = state.update(100.0, 101.0, 99.0, 2000.0);
        assert!(
            f.relative_volume > 1.5,
            "expected high relative volume, got {}",
            f.relative_volume
        );
    }

    #[test]
    fn z_score_extreme_drop() {
        let mut state = FeatureState::new();
        for _ in 0..20 {
            state.update(100.0, 100.5, 99.5, 1000.0);
        }
        let f = state.update(95.0, 100.0, 94.0, 1500.0);
        assert!(
            f.return_z_score < -2.0,
            "expected z < -2, got {}",
            f.return_z_score
        );
    }

    #[test]
    fn z_score_zero_for_constant_prices() {
        let mut state = FeatureState::new();
        for _ in 0..25 {
            let f = state.update(100.0, 100.0, 100.0, 1000.0);
            assert!(
                f.return_z_score.abs() < 1e-10,
                "constant prices should give z=0"
            );
        }
    }

    #[test]
    fn bar_range_and_close_location() {
        let mut state = FeatureState::new();
        let f = state.update(110.0, 110.0, 90.0, 1000.0);
        assert!((f.bar_range - 20.0).abs() < 1e-10);
        assert!((f.close_location - 1.0).abs() < 1e-10);

        let f = state.update(90.0, 110.0, 90.0, 1000.0);
        assert!((f.close_location - 0.0).abs() < 1e-10);

        let f = state.update(100.0, 110.0, 90.0, 1000.0);
        assert!((f.close_location - 0.5).abs() < 1e-10);
    }

    #[test]
    fn zero_range_bar_close_location() {
        let mut state = FeatureState::new();
        let f = state.update(100.0, 100.0, 100.0, 1000.0);
        assert!((f.close_location - 0.5).abs() < 1e-10);
    }

    #[test]
    fn sma_matches_manual_calculation() {
        let mut state = FeatureState::new();
        let prices = [100.0, 102.0, 104.0, 103.0, 101.0];
        let mut f = FeatureValues::default();
        for &p in &prices {
            f = state.update(p, p + 1.0, p - 1.0, 1000.0);
        }
        assert!((f.sma_20 - 102.0).abs() < 1e-10);
    }

    #[test]
    fn ema_fast_above_slow_detected_in_uptrend() {
        let mut state = FeatureState::new();
        for i in 0..60 {
            let price = 100.0 + i as f64 * 0.5;
            state.update(price, price + 0.5, price - 0.5, 1000.0);
        }
        let f = state.update(130.0, 130.5, 129.5, 1000.0);
        assert!(
            f.ema_fast > f.ema_slow,
            "fast EMA should be above slow in uptrend"
        );
        assert!(f.ema_fast_above_slow);
    }

    #[test]
    fn adx_available_in_feature_state() {
        let mut state = FeatureState::new();
        for i in 0..60 {
            let base = 100.0 + i as f64 * 2.0;
            state.update(base, base + 1.0, base - 1.0, 1000.0);
        }
        let f = state.update(220.0, 221.0, 219.0, 1000.0);
        assert!(
            f.adx > 0.0,
            "ADX should be positive after warmup, got {}",
            f.adx
        );
    }

    #[test]
    fn bollinger_bands_computed() {
        let mut state = FeatureState::new();
        for _ in 0..50 {
            state.update(100.0, 101.0, 99.0, 1000.0);
        }
        let f = state.update(100.0, 101.0, 99.0, 1000.0);
        assert!(f.bollinger_upper >= f.sma_20);
        assert!(f.bollinger_lower <= f.sma_20);
        assert!(
            (f.bollinger_pct_b - 0.5).abs() < 0.2,
            "expected %B near 0.5 for constant price, got {}",
            f.bollinger_pct_b
        );
    }

    #[test]
    fn bollinger_pct_b_above_one_for_breakout() {
        let mut state = FeatureState::new();
        for _ in 0..50 {
            state.update(100.0, 100.5, 99.5, 1000.0);
        }
        let f = state.update(115.0, 116.0, 114.0, 2000.0);
        assert!(
            f.bollinger_pct_b > 1.0,
            "expected %B > 1.0 for breakout, got {}",
            f.bollinger_pct_b
        );
    }
}
