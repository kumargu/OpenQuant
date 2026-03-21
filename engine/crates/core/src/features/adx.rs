//! Average Directional Index (ADX) — measures trend strength, not direction.
//!
//! # When is ADX useful?
//!
//! ADX is the **regime detector** — it tells you *whether* to trade momentum
//! or mean-reversion, not which direction to trade.
//!
//! ```text
//!  ADX value │ Market regime     │ Best strategy
//!  ──────────┼───────────────────┼──────────────────────
//!  < 20      │ No trend (choppy) │ Mean-reversion shines
//!  20 - 40   │ Moderate trend    │ Momentum starts working
//!  > 40      │ Strong trend      │ Momentum's sweet spot
//!  > 60      │ Extreme trend     │ Rare; often near reversal
//! ```
//!
//! **Key insight**: ADX doesn't tell you if the trend is up or down —
//! that's what +DI and -DI are for:
//! - +DI > -DI → bullish trend
//! - -DI > +DI → bearish trend
//! - ADX tells you how *strong* that trend is
//!
//! # Mathematics
//!
//! ```text
//!  Step 1 — Directional Movement (per bar):
//!    +DM = max(High(t) - High(t-1), 0)  if > -DM, else 0
//!    -DM = max(Low(t-1) - Low(t), 0)    if > +DM, else 0
//!
//!    Only the LARGER direction counts. Inside bars (both small)
//!    contribute zero. This captures the dominant move direction.
//!
//!  Step 2 — Smooth with Wilder's method (α = 1/N):
//!    +DI = 100 × WilderEMA(+DM) / WilderEMA(TrueRange)
//!    -DI = 100 × WilderEMA(-DM) / WilderEMA(TrueRange)
//!
//!    Normalizing by ATR makes DI values comparable across
//!    different price levels and volatility regimes.
//!
//!  Step 3 — Directional Index:
//!    DX = 100 × |+DI - -DI| / (+DI + -DI)
//!
//!    High DX = one direction dominates (trending).
//!    Low DX = up and down movement roughly equal (ranging).
//!
//!  Step 4 — Smooth again:
//!    ADX = WilderEMA(DX)
//!
//!    Double smoothing removes noise. ADX is always 0-100.
//! ```
//!
//! # Implementation
//!
//! Uses 4 Wilder EMAs (α = 1/N) matching Wilder's original specification
//! and Bloomberg/TradingView/TA-Lib reference values. Total: ~200 bytes.

use super::ema::WilderEma;

#[derive(Clone)]
pub struct Adx {
    plus_dm_ema: WilderEma,
    minus_dm_ema: WilderEma,
    tr_ema: WilderEma,
    adx_ema: WilderEma,
    prev_high: f64,
    prev_low: f64,
    prev_close: f64,
    count: usize,
    period: usize,
}

impl Adx {
    pub fn new(period: usize) -> Self {
        Self {
            plus_dm_ema: WilderEma::new(period),
            minus_dm_ema: WilderEma::new(period),
            tr_ema: WilderEma::new(period),
            adx_ema: WilderEma::new(period),
            prev_high: 0.0,
            prev_low: 0.0,
            prev_close: 0.0,
            count: 0,
            period,
        }
    }

    /// Update with a new bar. Returns `(adx, +DI, -DI)`.
    #[inline]
    pub fn update(&mut self, high: f64, low: f64, close: f64) -> (f64, f64, f64) {
        self.count += 1;
        if self.count == 1 {
            self.prev_high = high;
            self.prev_low = low;
            self.prev_close = close;
            return (0.0, 0.0, 0.0);
        }

        // Directional Movement: only the larger direction counts
        let up_move = high - self.prev_high;
        let down_move = self.prev_low - low;

        let plus_dm = if up_move > down_move && up_move > 0.0 {
            up_move
        } else {
            0.0
        };
        let minus_dm = if down_move > up_move && down_move > 0.0 {
            down_move
        } else {
            0.0
        };

        // True Range
        let hl = high - low;
        let hc = (high - self.prev_close).abs();
        let lc = (low - self.prev_close).abs();
        let tr = hl.max(hc).max(lc);

        self.prev_high = high;
        self.prev_low = low;
        self.prev_close = close;

        // Smooth with Wilder EMAs
        let smoothed_plus_dm = self.plus_dm_ema.push(plus_dm);
        let smoothed_minus_dm = self.minus_dm_ema.push(minus_dm);
        let smoothed_tr = self.tr_ema.push(tr);

        if smoothed_tr < 1e-10 {
            return (0.0, 0.0, 0.0);
        }

        // Directional Indicators
        let plus_di = 100.0 * smoothed_plus_dm / smoothed_tr;
        let minus_di = 100.0 * smoothed_minus_dm / smoothed_tr;

        // Directional Index → ADX
        let di_sum = plus_di + minus_di;
        let dx = if di_sum > 1e-10 {
            100.0 * (plus_di - minus_di).abs() / di_sum
        } else {
            0.0
        };

        let adx = self.adx_ema.push(dx);

        (adx, plus_di, minus_di)
    }

    /// Ready after 2×period bars (DM smoothing + ADX smoothing).
    #[inline]
    pub fn is_ready(&self) -> bool {
        self.count >= self.period * 2
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn returns_zero_on_first_bar() {
        let mut adx = Adx::new(14);
        let (val, pdi, mdi) = adx.update(100.0, 98.0, 99.0);
        assert_eq!(val, 0.0);
        assert_eq!(pdi, 0.0);
        assert_eq!(mdi, 0.0);
    }

    #[test]
    fn rises_in_strong_uptrend() {
        let mut adx = Adx::new(14);
        for i in 0..50 {
            let base = 100.0 + i as f64 * 2.0;
            adx.update(base + 1.0, base - 1.0, base);
        }
        let (val, pdi, mdi) = adx.update(201.0, 199.0, 200.0);
        assert!(val > 20.0, "ADX should be high in uptrend, got {val}");
        assert!(pdi > mdi, "+DI should exceed -DI in uptrend");
    }

    #[test]
    fn low_in_ranging_market() {
        let mut adx = Adx::new(14);
        for i in 0..50 {
            let offset = if i % 2 == 0 { 1.0 } else { -1.0 };
            adx.update(100.0 + offset, 99.0 + offset, 99.5 + offset);
        }
        let (val, _, _) = adx.update(100.0, 99.0, 99.5);
        assert!(val < 25.0, "ADX should be low in ranging market, got {val}");
    }

    #[test]
    fn is_ready_check() {
        let mut adx = Adx::new(14);
        for i in 0..27 {
            adx.update(100.0 + i as f64, 99.0 + i as f64, 99.5 + i as f64);
        }
        assert!(!adx.is_ready(), "should not be ready at 27 bars");
        adx.update(130.0, 128.0, 129.0);
        assert!(adx.is_ready(), "should be ready at 28 bars (2×14)");
    }

    #[test]
    fn plus_di_dominates_in_uptrend() {
        let mut adx = Adx::new(14);
        // Consistent uptrend
        for i in 0..40 {
            let base = 100.0 + i as f64;
            adx.update(base + 0.5, base - 0.5, base);
        }
        let (_, pdi, mdi) = adx.update(140.5, 139.5, 140.0);
        assert!(pdi > mdi, "+DI ({pdi}) should > -DI ({mdi}) in uptrend");
    }

    #[test]
    fn minus_di_dominates_in_downtrend() {
        let mut adx = Adx::new(14);
        // Consistent downtrend
        for i in 0..40 {
            let base = 200.0 - i as f64;
            adx.update(base + 0.5, base - 0.5, base);
        }
        let (_, pdi, mdi) = adx.update(160.5, 159.5, 160.0);
        assert!(mdi > pdi, "-DI ({mdi}) should > +DI ({pdi}) in downtrend");
    }

    #[test]
    fn adx_bounded_0_100() {
        let mut adx = Adx::new(14);
        for i in 0..100 {
            let base = 100.0 + i as f64 * 5.0; // extreme trend
            adx.update(base + 1.0, base - 1.0, base);
        }
        let (val, pdi, mdi) = adx.update(600.0, 598.0, 599.0);
        assert!(
            (0.0..=100.0).contains(&val),
            "ADX should be 0-100, got {val}"
        );
        assert!(pdi >= 0.0, "+DI should be non-negative");
        assert!(mdi >= 0.0, "-DI should be non-negative");
    }
}
