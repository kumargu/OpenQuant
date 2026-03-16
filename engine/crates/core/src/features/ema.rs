//! Exponential Moving Average — O(1) per update, no buffer needed.
//!
//! Two variants:
//! - [`Ema`]: standard EMA with α = 2/(N+1). Used for momentum crossovers.
//! - [`WilderEma`]: Wilder's smoothing with α = 1/N. Used by ADX, RSI, ATR.
//!
//! # When is EMA useful?
//!
//! - **Momentum / trend-following**: dual EMA crossover (fast EMA crosses
//!   above slow EMA = bullish signal). EMA responds faster than SMA to
//!   recent price changes, catching trends earlier.
//!
//! - **MACD**: the classic MACD indicator is EMA(12) - EMA(26), with a
//!   signal line of EMA(9) on the MACD values.
//!
//! - **Dynamic support/resistance**: price often bounces off EMA levels
//!   in trending markets (e.g., 21-EMA pullback entries).
//!
//! # EMA vs SMA
//!
//! ```text
//!  Weight Distribution (period=10):
//!
//!  EMA: ██████████ (latest bar)    SMA: █████ (all bars equal)
//!       ████████                        █████
//!       ██████                          █████
//!       █████                           █████
//!       ████                            █████
//!       ███                             █████
//!       ██                              █████
//!       ██                              █████
//!       █                               █████
//!       █                               █████
//!
//!  EMA: recent prices dominate     SMA: all prices weighted equally
//!  → faster reaction to moves     → smoother, more lagged
//! ```
//!
//! # Mathematics
//!
//! ```text
//!  Standard EMA:
//!    α = 2 / (period + 1)
//!    EMA(t) = α × Price(t) + (1 - α) × EMA(t-1)
//!
//!  Wilder's smoothing:
//!    α = 1 / period
//!    Wilder(t) = α × value(t) + (1 - α) × Wilder(t-1)
//!
//!  Difference for period=14:
//!    Standard: α = 2/15 = 0.133 (faster, more reactive)
//!    Wilder:   α = 1/14 = 0.071 (slower, smoother)
//! ```
//!
//! # Why WilderEma matters
//!
//! Welles Wilder invented ADX, RSI, and ATR using his own smoothing method.
//! Using standard EMA for ADX produces values that diverge from Bloomberg,
//! TradingView, and TA-Lib references. If you calibrate strategy thresholds
//! against literature (e.g., "ADX > 25 = strong trend"), you need Wilder's
//! smoothing to get matching values.
//!
//! # Memory
//!
//! Both variants store just one running value + metadata — 40 bytes total.
//! Compare to `Sma<32>` which needs a 256-byte ring buffer.

/// Standard EMA with α = 2/(N+1).
#[derive(Clone)]
pub struct Ema {
    pub(crate) alpha: f64,
    one_minus_alpha: f64,
    value: f64,
    count: usize,
    period: usize,
}

impl Ema {
    pub fn new(period: usize) -> Self {
        let alpha = 2.0 / (period as f64 + 1.0);
        Self {
            alpha,
            one_minus_alpha: 1.0 - alpha,
            value: 0.0,
            count: 0,
            period,
        }
    }

    /// Push a new value and return the updated EMA.
    /// First value seeds the EMA (no smoothing applied).
    #[inline]
    pub fn push(&mut self, value: f64) -> f64 {
        self.count += 1;
        if self.count == 1 {
            self.value = value;
        } else {
            self.value = self.alpha * value + self.one_minus_alpha * self.value;
        }
        self.value
    }

    #[inline]
    pub fn value(&self) -> f64 {
        self.value
    }

    /// Ready after `period` bars. EMA is technically valid from bar 1,
    /// but needs ~period bars for the exponential weights to stabilize.
    #[inline]
    pub fn is_ready(&self) -> bool {
        self.count >= self.period
    }
}

/// Wilder's smoothing with α = 1/N. Used by ADX, RSI, and ATR.
///
/// Slower than standard EMA — approximately equivalent to a standard
/// EMA with period 2N-1. For period=14: Wilder ≈ EMA(27).
#[derive(Clone)]
pub struct WilderEma {
    alpha: f64,
    one_minus_alpha: f64,
    value: f64,
    count: usize,
    period: usize,
}

impl WilderEma {
    pub fn new(period: usize) -> Self {
        let alpha = 1.0 / period as f64;
        Self {
            alpha,
            one_minus_alpha: 1.0 - alpha,
            value: 0.0,
            count: 0,
            period,
        }
    }

    #[inline]
    pub fn push(&mut self, value: f64) -> f64 {
        self.count += 1;
        if self.count == 1 {
            self.value = value;
        } else {
            self.value = self.alpha * value + self.one_minus_alpha * self.value;
        }
        self.value
    }

    #[inline]
    pub fn value(&self) -> f64 {
        self.value
    }

    #[inline]
    pub fn is_ready(&self) -> bool {
        self.count >= self.period
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Standard EMA tests ---

    #[test]
    fn ema_first_value_equals_input() {
        let mut ema = Ema::new(10);
        assert_eq!(ema.push(100.0), 100.0);
    }

    #[test]
    fn ema_converges_to_constant() {
        let mut ema = Ema::new(10);
        for _ in 0..100 {
            ema.push(50.0);
        }
        assert!((ema.value() - 50.0).abs() < 1e-10);
    }

    #[test]
    fn ema_weights_recent_more() {
        let mut ema = Ema::new(10);
        for _ in 0..20 {
            ema.push(100.0);
        }
        ema.push(110.0);
        assert!(ema.value() > 100.0, "EMA should move toward spike");
        assert!(ema.value() < 110.0, "EMA should not reach spike in one bar");
    }

    #[test]
    fn ema_is_ready_after_period() {
        let mut ema = Ema::new(10);
        for i in 0..10 {
            ema.push(i as f64);
            if i < 9 {
                assert!(!ema.is_ready());
            }
        }
        assert!(ema.is_ready());
    }

    #[test]
    fn ema_alpha_calculation() {
        let ema = Ema::new(10);
        // α = 2 / (10 + 1) ≈ 0.1818
        assert!((ema.alpha - 2.0 / 11.0).abs() < 1e-10);
    }

    #[test]
    fn ema_exact_two_values() {
        let mut ema = Ema::new(10);
        ema.push(100.0); // seeds to 100
        let v = ema.push(110.0);
        // α = 2/11, v = (2/11)*110 + (9/11)*100 = 220/11 + 900/11 = 1120/11
        let expected = 2.0 / 11.0 * 110.0 + 9.0 / 11.0 * 100.0;
        assert!((v - expected).abs() < 1e-10);
    }

    #[test]
    fn ema_exact_three_values_period_3() {
        // EMA(3) of [10, 20, 30]: α = 2/4 = 0.5
        // After [10]: 10.0
        // After [20]: 0.5*20 + 0.5*10 = 15.0
        // After [30]: 0.5*30 + 0.5*15 = 22.5
        let mut ema = Ema::new(3);
        ema.push(10.0);
        ema.push(20.0);
        let v = ema.push(30.0);
        assert!((v - 22.5).abs() < 1e-10, "got {v}, expected 22.5");
    }

    // --- WilderEma tests ---

    #[test]
    fn wilder_first_value_equals_input() {
        let mut w = WilderEma::new(14);
        assert_eq!(w.push(100.0), 100.0);
    }

    #[test]
    fn wilder_converges_to_constant() {
        let mut w = WilderEma::new(14);
        for _ in 0..200 {
            w.push(50.0);
        }
        assert!((w.value() - 50.0).abs() < 1e-10);
    }

    #[test]
    fn wilder_alpha_is_one_over_n() {
        let w = WilderEma::new(14);
        assert!((w.alpha - 1.0 / 14.0).abs() < 1e-10);
    }

    #[test]
    fn wilder_slower_than_standard_ema() {
        // Both start at 100, then get a spike to 200.
        // Wilder should react less (stay closer to 100).
        let mut ema = Ema::new(14);
        let mut wilder = WilderEma::new(14);

        for _ in 0..20 {
            ema.push(100.0);
            wilder.push(100.0);
        }
        ema.push(200.0);
        wilder.push(200.0);

        // Both should move toward 200, but Wilder less so
        assert!(wilder.value() < ema.value(),
            "Wilder ({}) should react less than EMA ({})", wilder.value(), ema.value());
        assert!(wilder.value() > 100.0, "Wilder should still move toward spike");
    }

    #[test]
    fn wilder_exact_two_values() {
        let mut w = WilderEma::new(14);
        w.push(100.0); // seeds to 100
        let v = w.push(114.0);
        // α = 1/14, v = (1/14)*114 + (13/14)*100 = 114/14 + 1300/14 = 1414/14 = 101.0
        let expected = 1.0 / 14.0 * 114.0 + 13.0 / 14.0 * 100.0;
        assert!((v - expected).abs() < 1e-10, "got {v}, expected {expected}");
    }

    #[test]
    fn wilder_is_ready_after_period() {
        let mut w = WilderEma::new(14);
        for i in 0..14 {
            w.push(i as f64);
            if i < 13 {
                assert!(!w.is_ready());
            }
        }
        assert!(w.is_ready());
    }
}
