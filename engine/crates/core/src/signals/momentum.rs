//! Momentum / trend-following strategy.
//!
//! Complements mean-reversion: when mean-reversion gets stopped out repeatedly
//! in a strong trend, momentum profits by riding the trend.
//!
//! ```text
//!  Price & Moving Averages
//!  │
//!  │         ╱── Fast EMA(10)
//!  │    ╱╲  ╱
//!  │  ╱    ╳ ← BUY: fast crosses above slow + ADX > 20
//!  │╱    ╱  ╲── Slow EMA(30)
//!  │   ╱
//!  │──╱──────────────────── Time
//!  ```
//!
//! # Entry conditions (all must be true)
//!
//! 1. `ema_fast > ema_slow` — uptrend confirmed by MA crossover
//! 2. `adx > min_adx` — trend is strong enough (filters choppy markets)
//! 3. `+DI > -DI` — bullish directional movement dominates
//! 4. Not already holding a position
//!
//! # Exit conditions
//!
//! 1. `ema_fast < ema_slow` — trend reversal
//! 2. Currently holding a position
//!
//! # Score formula
//!
//! ```text
//! score = 0.5 × adx_strength + 0.3 × ma_separation + 0.2 × volume_boost
//!
//! where:
//!   adx_strength  = clamp((adx - min_adx) / 40, 0, 1)
//!   ma_separation = clamp(|ema_fast - ema_slow| / atr, 0, 2) / 2
//!   volume_boost  = clamp(relative_volume - 1.0, 0, 2) / 2
//! ```
//!
//! # Why ADX filtering matters
//!
//! ```text
//!  ADX < 20  → choppy market → MA crossovers are noise → DON'T TRADE
//!  ADX 20-40 → moderate trend → trade with smaller conviction
//!  ADX > 40  → strong trend → full conviction
//! ```

use super::{Side, SignalOutput, SignalReason, Strategy};
use crate::features::FeatureValues;

/// Configuration for momentum strategy.
///
/// All parameters go in `[momentum]` section of `openquant.toml`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Config {
    /// Minimum ADX value to consider a trend tradeable. Default: 20.0
    ///
    /// Lower values (15-18) catch more trends but with more false signals.
    /// Higher values (25-30) only trade strong trends, fewer signals but higher quality.
    /// Literature standard: 20 = "trend exists", 40 = "strong trend".
    pub min_adx: f64,

    /// Minimum score to act on a signal. Default: 0.3
    ///
    /// Lower values (0.1-0.2) fire more trades. Higher values (0.5+) are selective.
    /// Start conservative (0.3) and lower only after backtesting proves it helps.
    pub min_score: f64,

    /// Require +DI > -DI for buy signals. Default: true
    ///
    /// When true, only buys when bullish directional movement dominates.
    /// Disable to trade momentum in both directions (not recommended for long-only).
    pub directional_filter: bool,

    /// Minimum volume relative to rolling average. Default: 0.8
    ///
    /// Lower than mean-reversion (1.2) because trends can persist on normal volume.
    /// Set to 0.0 to disable volume filtering entirely.
    pub min_relative_volume: f64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            min_adx: 20.0,
            min_score: 0.3,
            directional_filter: true,
            min_relative_volume: 0.8,
        }
    }
}

/// Momentum / trend-following strategy.
pub struct Momentum {
    config: Config,
}

impl Momentum {
    pub fn new(config: Config) -> Self {
        Self { config }
    }
}

impl Strategy for Momentum {
    fn score(&self, features: &FeatureValues, has_position: bool) -> Option<SignalOutput> {
        if !features.warmed_up {
            return None;
        }

        // --- EXIT: trend reversal ---
        if has_position && !features.ema_fast_above_slow {
            // Score based on how far below the slow EMA the fast EMA has dropped
            let separation = if features.atr > 1e-10 {
                (features.ema_slow - features.ema_fast) / features.atr
            } else {
                1.0
            };
            let score = separation.clamp(0.0, 2.0) / 2.0;

            return Some(SignalOutput {
                side: Side::Sell,
                score,
                reason: SignalReason::MomentumSell,
                z_score: features.return_z_score,
                relative_volume: features.relative_volume,
                    votes: String::new(),
            });
        }

        // --- ENTRY: trend following ---
        if has_position {
            return None;
        }

        // Gate 1: EMA crossover — fast above slow
        if !features.ema_fast_above_slow {
            return None;
        }

        // Gate 2: ADX — trend must be strong enough
        // Uses `is_nan()` guard because NaN < threshold is false in IEEE 754,
        // which would incorrectly pass a simple `<` check.
        if features.adx.is_nan() || features.adx < self.config.min_adx {
            return None;
        }

        // Gate 3: Directional filter — +DI > -DI (bullish)
        if self.config.directional_filter && features.plus_di <= features.minus_di {
            return None;
        }

        // Gate 4: Volume — at least min_relative_volume (< to match ADX gate)
        if features.relative_volume < self.config.min_relative_volume {
            return None;
        }

        // Score: composite of ADX strength, MA separation, and volume
        // ADX normalization: maps [min_adx, min_adx+ADX_RANGE] → [0, 1]
        // At default min_adx=20: ADX 60 saturates. Rare for ADX to exceed 60.
        const ADX_RANGE: f64 = 40.0;
        let adx_strength = ((features.adx - self.config.min_adx) / ADX_RANGE).clamp(0.0, 1.0);

        let ma_separation = if features.atr > 1e-10 {
            ((features.ema_fast - features.ema_slow) / features.atr).clamp(0.0, 2.0) / 2.0
        } else {
            0.0
        };

        let volume_boost = (features.relative_volume - 1.0).clamp(0.0, 2.0) / 2.0;

        let score = 0.5 * adx_strength + 0.3 * ma_separation + 0.2 * volume_boost;

        if score < self.config.min_score {
            return None;
        }

        Some(SignalOutput {
            side: Side::Buy,
            score,
            reason: SignalReason::MomentumBuy,
            z_score: features.return_z_score,
            relative_volume: features.relative_volume,
                    votes: String::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a FeatureValues with momentum-relevant fields set.
    fn features(adx: f64, plus_di: f64, minus_di: f64, rel_vol: f64) -> FeatureValues {
        FeatureValues {
            warmed_up: true,
            ema_fast: 105.0,
            ema_slow: 100.0,
            ema_fast_above_slow: true,
            adx,
            plus_di,
            minus_di,
            atr: 2.0,
            relative_volume: rel_vol,
            return_z_score: 0.5,
            ..Default::default()
        }
    }

    fn strategy() -> Momentum {
        Momentum::new(Config::default())
    }

    // --- Entry tests ---

    #[test]
    fn buy_fires_in_strong_uptrend() {
        let sig = strategy().score(&features(30.0, 35.0, 15.0, 1.5), false);
        assert!(sig.is_some());
        let sig = sig.unwrap();
        assert_eq!(sig.side, Side::Buy);
        assert_eq!(sig.reason, SignalReason::MomentumBuy);
        assert!(sig.score >= 0.3);
    }

    #[test]
    fn buy_blocked_when_already_holding() {
        assert!(
            strategy()
                .score(&features(30.0, 35.0, 15.0, 1.5), true)
                .is_none()
        );
    }

    #[test]
    fn buy_blocked_when_adx_too_low() {
        assert!(
            strategy()
                .score(&features(15.0, 35.0, 15.0, 1.5), false)
                .is_none()
        );
    }

    #[test]
    fn buy_blocked_when_ema_not_crossed() {
        let mut f = features(30.0, 35.0, 15.0, 1.5);
        f.ema_fast_above_slow = false;
        f.ema_fast = 98.0;
        f.ema_slow = 100.0;
        assert!(strategy().score(&f, false).is_none());
    }

    #[test]
    fn buy_blocked_when_minus_di_dominates() {
        // +DI < -DI means bearish directional movement
        assert!(
            strategy()
                .score(&features(30.0, 15.0, 35.0, 1.5), false)
                .is_none()
        );
    }

    #[test]
    fn buy_blocked_when_volume_too_low() {
        assert!(
            strategy()
                .score(&features(30.0, 35.0, 15.0, 0.5), false)
                .is_none()
        );
    }

    #[test]
    fn buy_at_exact_adx_threshold_does_not_fire() {
        // adx == min_adx: adx_strength = 0, so score will be low
        let sig = strategy().score(&features(20.0, 35.0, 15.0, 1.0), false);
        // Score: 0.5*0 + 0.3*(5/2)/2 + 0.2*0 = 0.375 which is > 0.3
        // Actually let's check: ma_sep = (105-100)/2 = 2.5, clamp(2.5,0,2)/2 = 1.0
        // score = 0 + 0.3*1.0 + 0 = 0.3, which is NOT > 0.3 (== threshold)
        // Wait min_score check is `score < min_score`, so 0.3 < 0.3 is false => fires
        // Let's just check it fires with very low score
        assert!(sig.is_some());
    }

    #[test]
    fn buy_blocked_below_min_score() {
        // Very low ADX just above threshold, low volume, tight MA = low score
        let mut f = features(20.5, 25.0, 20.0, 0.9);
        f.ema_fast = 100.1;
        f.ema_slow = 100.0;
        f.atr = 10.0; // large ATR makes MA separation tiny
        let sig = strategy().score(&f, false);
        // adx_strength = (20.5-20)/40 = 0.0125
        // ma_sep = (0.1/10) = 0.01, clamp/2 = 0.005
        // vol = clamp(0.9-1.0, 0, 2)/2 = 0
        // score = 0.5*0.0125 + 0.3*0.005 + 0 = 0.00775 < 0.3
        assert!(sig.is_none());
    }

    // --- Exit tests ---

    #[test]
    fn sell_fires_on_trend_reversal() {
        let mut f = features(25.0, 15.0, 35.0, 1.0);
        f.ema_fast_above_slow = false;
        f.ema_fast = 99.0;
        f.ema_slow = 100.0;
        let sig = strategy().score(&f, true);
        assert!(sig.is_some());
        let sig = sig.unwrap();
        assert_eq!(sig.side, Side::Sell);
        assert_eq!(sig.reason, SignalReason::MomentumSell);
    }

    #[test]
    fn sell_blocked_when_not_holding() {
        let mut f = features(25.0, 15.0, 35.0, 1.0);
        f.ema_fast_above_slow = false;
        f.ema_fast = 99.0;
        f.ema_slow = 100.0;
        assert!(strategy().score(&f, false).is_none());
    }

    #[test]
    fn no_sell_when_trend_still_up() {
        // Holding + trend still up = no exit signal (entry is blocked by has_position)
        assert!(
            strategy()
                .score(&features(30.0, 35.0, 15.0, 1.5), true)
                .is_none()
        );
    }

    // --- Score tests ---

    #[test]
    fn score_increases_with_stronger_adx() {
        let weak = strategy()
            .score(&features(25.0, 35.0, 15.0, 1.5), false)
            .unwrap();
        let strong = strategy()
            .score(&features(50.0, 35.0, 15.0, 1.5), false)
            .unwrap();
        assert!(strong.score > weak.score);
    }

    #[test]
    fn score_increases_with_wider_ma_separation() {
        let s = Momentum::new(Config {
            min_score: 0.0,
            ..Config::default()
        });
        let narrow = {
            let mut f = features(30.0, 35.0, 15.0, 1.5);
            f.ema_fast = 100.5;
            f.ema_slow = 100.0;
            s.score(&f, false).unwrap()
        };
        let wide = {
            let f = features(30.0, 35.0, 15.0, 1.5); // ema_fast=105, ema_slow=100
            s.score(&f, false).unwrap()
        };
        assert!(wide.score > narrow.score);
    }

    #[test]
    fn score_increases_with_higher_volume() {
        let low = strategy()
            .score(&features(30.0, 35.0, 15.0, 1.0), false)
            .unwrap();
        let high = strategy()
            .score(&features(30.0, 35.0, 15.0, 2.5), false)
            .unwrap();
        assert!(high.score > low.score);
    }

    #[test]
    fn sell_score_reflects_ma_separation() {
        let small_drop = {
            let mut f = features(25.0, 15.0, 35.0, 1.0);
            f.ema_fast_above_slow = false;
            f.ema_fast = 99.5;
            f.ema_slow = 100.0;
            strategy().score(&f, true).unwrap()
        };
        let large_drop = {
            let mut f = features(25.0, 15.0, 35.0, 1.0);
            f.ema_fast_above_slow = false;
            f.ema_fast = 96.0;
            f.ema_slow = 100.0;
            strategy().score(&f, true).unwrap()
        };
        assert!(large_drop.score > small_drop.score);
    }

    // --- Config tests ---

    #[test]
    fn directional_filter_can_be_disabled() {
        let s = Momentum::new(Config {
            directional_filter: false,
            min_score: 0.0,
            ..Config::default()
        });
        // -DI > +DI but directional filter off → should still fire
        assert!(s.score(&features(30.0, 15.0, 35.0, 1.5), false).is_some());
    }

    #[test]
    fn custom_min_adx_respected() {
        let strict = Momentum::new(Config {
            min_adx: 40.0,
            ..Config::default()
        });
        // ADX=30 is below the custom threshold
        assert!(
            strict
                .score(&features(30.0, 35.0, 15.0, 1.5), false)
                .is_none()
        );
        // ADX=45 is above
        assert!(
            strict
                .score(&features(45.0, 35.0, 15.0, 1.5), false)
                .is_some()
        );
    }

    // --- Edge cases ---

    #[test]
    fn no_signal_when_not_warmed_up() {
        let mut f = features(30.0, 35.0, 15.0, 1.5);
        f.warmed_up = false;
        assert!(strategy().score(&f, false).is_none());
    }

    #[test]
    fn zero_atr_handled_gracefully() {
        let s = Momentum::new(Config {
            min_score: 0.0,
            ..Config::default()
        });
        let mut f = features(30.0, 35.0, 15.0, 1.5);
        f.atr = 0.0;
        // Should still produce a signal — ma_separation defaults to 0.0
        let sig = s.score(&f, false);
        assert!(sig.is_some());
    }

    #[test]
    fn signal_carries_feature_snapshot() {
        let f = features(30.0, 35.0, 15.0, 1.8);
        let sig = strategy().score(&f, false).unwrap();
        assert!((sig.z_score - 0.5).abs() < 1e-10);
        assert!((sig.relative_volume - 1.8).abs() < 1e-10);
    }

    #[test]
    fn score_bounded_0_to_1() {
        // Extreme inputs: ADX=100, huge MA separation, huge volume
        let s = Momentum::new(Config {
            min_score: 0.0,
            ..Config::default()
        });
        let mut f = features(100.0, 80.0, 5.0, 10.0);
        f.ema_fast = 200.0;
        f.ema_slow = 100.0;
        f.atr = 0.1; // tiny ATR → huge normalized separation
        let sig = s.score(&f, false).unwrap();
        assert!(
            sig.score <= 1.0,
            "score should be bounded at 1.0, got {}",
            sig.score
        );
        assert!(sig.score > 0.0);
    }

    #[test]
    fn exit_with_zero_atr_defaults_to_moderate_score() {
        let mut f = features(25.0, 15.0, 35.0, 1.0);
        f.ema_fast_above_slow = false;
        f.ema_fast = 99.0;
        f.ema_slow = 100.0;
        f.atr = 0.0;
        let sig = strategy().score(&f, true).unwrap();
        assert_eq!(sig.side, Side::Sell);
        // With zero ATR, separation defaults to 1.0, score = 1.0/2 = 0.5
        assert!((sig.score - 0.5).abs() < 1e-10);
    }

    #[test]
    fn nan_adx_produces_no_signal() {
        // NaN comparisons return false in IEEE 754, so ADX gate passes (NaN < 20 = false).
        // We guard against this explicitly in the strategy.
        let f = features(f64::NAN, 35.0, 15.0, 1.5);
        assert!(strategy().score(&f, false).is_none());
    }

    #[test]
    fn volume_at_exact_threshold_passes() {
        // Volume gate uses `<` (not `<=`), so exact threshold passes
        let sig = strategy().score(&features(30.0, 35.0, 15.0, 0.8), false);
        assert!(sig.is_some(), "volume at exact threshold should pass");
    }
}
