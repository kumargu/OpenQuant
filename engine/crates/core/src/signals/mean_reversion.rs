//! Mean-reversion strategy.
//!
//! Idea: unusually large price drops (measured by z-score) tend to revert.
//! Buy when the return z-score is extremely negative with volume confirmation,
//! sell when z-score swings extremely positive.
//!
//! Entry: z-score < buy_threshold AND relative_volume > min_volume AND no position
//! Exit:  z-score > sell_threshold AND holding a position
//!
//! Score formula:
//!   buy_score  = 0.6 * |z - threshold| + 0.4 * (relative_volume - 1.0)
//!   sell_score = |z - threshold|

use crate::features::FeatureValues;
use super::{Side, SignalOutput, Strategy};

/// Configuration for mean-reversion strategy.
#[derive(Debug, Clone)]
pub struct Config {
    /// Z-score below this triggers a buy (negative value). Default: -2.0
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
            buy_z_threshold: -2.0,
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
                    reason: format!(
                        "mean-reversion buy: z={:.2}, rel_vol={:.2}, close_loc={:.2}",
                        features.return_z_score, features.relative_volume, features.close_location
                    ),
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
                    reason: format!(
                        "mean-reversion sell: z={:.2}, rel_vol={:.2}",
                        features.return_z_score, features.relative_volume
                    ),
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
        assert!(sig.reason.contains("mean-reversion buy"));
    }

    #[test]
    fn buy_blocked_when_already_holding() {
        let sig = strategy().score(&features(-3.0, 1.5), true);
        assert!(sig.is_none(), "should not buy when already holding");
    }

    #[test]
    fn buy_blocked_when_z_above_threshold() {
        let sig = strategy().score(&features(-1.0, 1.5), false);
        assert!(sig.is_none(), "z=-1.0 is above -2.0 threshold");
    }

    #[test]
    fn buy_blocked_when_volume_too_low() {
        let sig = strategy().score(&features(-3.0, 0.8), false);
        assert!(sig.is_none(), "volume 0.8x is below 1.2x minimum");
    }

    #[test]
    fn buy_score_increases_with_stronger_z() {
        let weak = strategy().score(&features(-2.5, 1.5), false).unwrap();
        let strong = strategy().score(&features(-4.0, 1.5), false).unwrap();
        assert!(strong.score > weak.score, "stronger z should give higher score");
    }

    #[test]
    fn buy_score_increases_with_higher_volume() {
        let low_vol = strategy().score(&features(-3.0, 1.3), false).unwrap();
        let high_vol = strategy().score(&features(-3.0, 3.0), false).unwrap();
        assert!(high_vol.score > low_vol.score, "higher volume should give higher score");
    }

    #[test]
    fn buy_at_exact_threshold_does_not_fire() {
        let sig = strategy().score(&features(-2.0, 1.5), false);
        assert!(sig.is_none(), "z exactly at threshold should not fire (need strictly below)");
    }

    // --- Sell signal tests ---

    #[test]
    fn sell_fires_on_overbought_when_holding() {
        let sig = strategy().score(&features(3.0, 1.5), true);
        assert!(sig.is_some());
        assert_eq!(sig.unwrap().side, Side::Sell);
    }

    #[test]
    fn sell_blocked_when_not_holding() {
        let sig = strategy().score(&features(3.0, 1.5), false);
        assert!(sig.is_none(), "should not sell when not holding");
    }

    #[test]
    fn sell_blocked_when_z_below_threshold() {
        let sig = strategy().score(&features(1.5, 1.5), true);
        assert!(sig.is_none(), "z=1.5 is below 2.0 sell threshold");
    }

    #[test]
    fn sell_score_increases_with_stronger_z() {
        let weak = strategy().score(&features(2.5, 1.5), true).unwrap();
        let strong = strategy().score(&features(4.0, 1.5), true).unwrap();
        assert!(strong.score > weak.score);
    }

    // --- Warmup tests ---

    #[test]
    fn no_signal_when_not_warmed_up() {
        let mut f = features(-5.0, 2.0);
        f.warmed_up = false;
        assert!(strategy().score(&f, false).is_none());
    }

    // --- Custom config tests ---

    #[test]
    fn custom_thresholds_respected() {
        let s = MeanReversion::new(Config {
            buy_z_threshold: -1.0,
            sell_z_threshold: 1.0,
            min_relative_volume: 0.0,
            min_score: 0.0,
        });
        // z=-1.5 should fire with relaxed threshold of -1.0
        assert!(s.score(&features(-1.5, 0.5), false).is_some());
        // z=1.5 should fire sell with relaxed threshold of 1.0
        assert!(s.score(&features(1.5, 0.5), true).is_some());
    }

    #[test]
    fn zero_volume_filter_allows_all() {
        let s = MeanReversion::new(Config {
            min_relative_volume: 0.0,
            min_score: 0.0,
            ..Config::default()
        });
        // Even zero volume should pass when filter is disabled
        // (relative_volume=0.0 > min=0.0 is false, so this still blocks)
        // This documents the behavior: 0.0 > 0.0 is false
        let sig = s.score(&features(-3.0, 0.0), false);
        assert!(sig.is_none(), "0.0 > 0.0 is false, so zero volume is always blocked");

        // But any positive volume passes
        let sig = s.score(&features(-3.0, 0.01), false);
        assert!(sig.is_some(), "tiny volume should pass when filter is 0.0");
    }
}
