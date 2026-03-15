//! Mean-reversion strategy.
//!
//! Idea: unusually large price drops (measured by z-score of returns)
//! tend to revert toward the mean. Buy the dip, sell the rip.
//!
//! ```text
//!  Price
//!   │          ╱╲
//!   │    ╱╲  ╱    ╲  ← SELL: z > +2.0 (overbought)
//!   │  ╱    ╲       ╲
//!   │╱        ╲       ── mean
//!   │          ╲╱
//!   │              ← BUY: z < -2.0 (oversold) + volume > 1.2x
//!   └──────────────── Time
//! ```
//!
//! Entry conditions (all must be true):
//!   1. z-score < buy_threshold  (price dropped unusually far)
//!   2. relative_volume > min_volume  (drop has participation)
//!   3. not already holding a position
//!
//! Exit conditions:
//!   1. z-score > sell_threshold  (price reverted past the mean)
//!   2. currently holding a position
//!
//! Score formula (higher = more conviction):
//!   buy:  0.6 × |z - threshold| + 0.4 × (relative_volume - 1.0)
//!   sell: |z - threshold|

use crate::features::FeatureValues;
use super::{Side, SignalOutput, SignalReason, Strategy};

/// Configuration for mean-reversion strategy.
#[derive(Debug, Clone)]
pub struct Config {
    /// Z-score below this triggers a buy (negative value). Default: -2.2
    /// Tightened from -2.0 to reduce false entries in low-conviction dips.
    pub buy_z_threshold: f64,
    /// Z-score above this triggers a sell (positive value). Default: 2.0
    pub sell_z_threshold: f64,
    /// Minimum relative volume to confirm a buy signal. Default: 1.2
    pub min_relative_volume: f64,
    /// Minimum score to act on. Default: 0.5
    pub min_score: f64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            buy_z_threshold: -2.2,
            sell_z_threshold: 2.0,
            min_relative_volume: 1.2,
            min_score: 0.5,
        }
    }
}

/// Mean-reversion strategy instance.
pub struct MeanReversion {
    config: Config,
}

impl MeanReversion {
    pub fn new(config: Config) -> Self {
        Self { config }
    }
}

impl Strategy for MeanReversion {
    fn score(&self, features: &FeatureValues, has_position: bool) -> Option<SignalOutput> {
        if !features.warmed_up {
            return None;
        }

        // Buy: oversold + volume confirmation + not already holding
        if features.return_z_score < self.config.buy_z_threshold
            && features.relative_volume > self.config.min_relative_volume
            && !has_position
        {
            let z_strength = (self.config.buy_z_threshold - features.return_z_score).abs();
            let vol_strength = features.relative_volume - 1.0;
            let score = 0.6 * z_strength + 0.4 * vol_strength;

            if score >= self.config.min_score {
                return Some(SignalOutput {
                    side: Side::Buy,
                    score,
                    reason: SignalReason::MeanReversionBuy,
                    z_score: features.return_z_score,
                    relative_volume: features.relative_volume,
                });
            }
        }

        // Sell: overbought + holding position
        if features.return_z_score > self.config.sell_z_threshold && has_position {
            let z_strength = (features.return_z_score - self.config.sell_z_threshold).abs();

            if z_strength >= self.config.min_score {
                return Some(SignalOutput {
                    side: Side::Sell,
                    score: z_strength,
                    reason: SignalReason::MeanReversionSell,
                    z_score: features.return_z_score,
                    relative_volume: features.relative_volume,
                });
            }
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn features(z: f64, rel_vol: f64) -> FeatureValues {
        FeatureValues {
            return_z_score: z,
            relative_volume: rel_vol,
            warmed_up: true,
            ..Default::default()
        }
    }

    fn strategy() -> MeanReversion {
        MeanReversion::new(Config::default())
    }

    // --- Buy signal tests ---

    #[test]
    fn buy_fires_on_oversold_with_volume() {
        let sig = strategy().score(&features(-3.0, 1.5), false);
        assert!(sig.is_some());
        let sig = sig.unwrap();
        assert_eq!(sig.side, Side::Buy);
        assert!(sig.score >= 0.5);
        assert_eq!(sig.reason, SignalReason::MeanReversionBuy);
    }

    #[test]
    fn buy_blocked_when_already_holding() {
        assert!(strategy().score(&features(-3.0, 1.5), true).is_none());
    }

    #[test]
    fn buy_blocked_when_z_above_threshold() {
        assert!(strategy().score(&features(-1.0, 1.5), false).is_none());
    }

    #[test]
    fn buy_blocked_when_volume_too_low() {
        assert!(strategy().score(&features(-3.0, 0.8), false).is_none());
    }

    #[test]
    fn buy_score_increases_with_stronger_z() {
        let weak = strategy().score(&features(-3.0, 1.5), false).unwrap();
        let strong = strategy().score(&features(-4.0, 1.5), false).unwrap();
        assert!(strong.score > weak.score);
    }

    #[test]
    fn buy_score_increases_with_higher_volume() {
        let low = strategy().score(&features(-3.0, 1.3), false).unwrap();
        let high = strategy().score(&features(-3.0, 3.0), false).unwrap();
        assert!(high.score > low.score);
    }

    #[test]
    fn buy_at_exact_threshold_does_not_fire() {
        assert!(strategy().score(&features(-2.2, 1.5), false).is_none());
    }

    // --- Sell signal tests ---

    #[test]
    fn sell_fires_on_overbought_when_holding() {
        let sig = strategy().score(&features(3.0, 1.5), true).unwrap();
        assert_eq!(sig.side, Side::Sell);
        assert_eq!(sig.reason, SignalReason::MeanReversionSell);
    }

    #[test]
    fn sell_blocked_when_not_holding() {
        assert!(strategy().score(&features(3.0, 1.5), false).is_none());
    }

    #[test]
    fn sell_blocked_when_z_below_threshold() {
        assert!(strategy().score(&features(1.5, 1.5), true).is_none());
    }

    #[test]
    fn sell_score_increases_with_stronger_z() {
        let weak = strategy().score(&features(2.5, 1.5), true).unwrap();
        let strong = strategy().score(&features(4.0, 1.5), true).unwrap();
        assert!(strong.score > weak.score);
    }

    // --- Edge cases ---

    #[test]
    fn no_signal_when_not_warmed_up() {
        let mut f = features(-5.0, 2.0);
        f.warmed_up = false;
        assert!(strategy().score(&f, false).is_none());
    }

    #[test]
    fn custom_thresholds_respected() {
        let s = MeanReversion::new(Config {
            buy_z_threshold: -1.0,
            sell_z_threshold: 1.0,
            min_relative_volume: 0.0,
            min_score: 0.0,
        });
        assert!(s.score(&features(-1.5, 0.5), false).is_some());
        assert!(s.score(&features(1.5, 0.5), true).is_some());
    }

    #[test]
    fn zero_volume_blocks_even_with_zero_filter() {
        let s = MeanReversion::new(Config {
            min_relative_volume: 0.0,
            min_score: 0.0,
            ..Config::default()
        });
        // 0.0 > 0.0 is false — documented behavior
        assert!(s.score(&features(-3.0, 0.0), false).is_none());
        // any positive volume passes
        assert!(s.score(&features(-3.0, 0.01), false).is_some());
    }

    #[test]
    fn signal_carries_feature_snapshot() {
        let sig = strategy().score(&features(-3.5, 1.8), false).unwrap();
        assert!((sig.z_score - (-3.5)).abs() < 1e-10);
        assert!((sig.relative_volume - 1.8).abs() < 1e-10);
    }
}
