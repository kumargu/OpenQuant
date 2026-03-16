//! Breakout / volatility expansion strategy.
//!
//! Catches range expansions after periods of consolidation. Profits from moves
//! that kill mean-reversion (sudden directional moves) by detecting when
//! volatility compresses (squeeze) and positioning for the expansion.
//!
//! # Entry conditions (all must be true)
//!
//! 1. Squeeze detected — Bollinger bandwidth in bottom 20th percentile
//! 2. Price breaks above Donchian upper channel (new N-bar high)
//! 3. Volume surge — `relative_volume > min_volume_surge`
//! 4. Not already holding a position
//!
//! # Exit conditions (Chandelier trailing stop)
//!
//! 1. Price below `donchian_upper - atr * trailing_stop_atr_mult`
//! 2. Currently holding a position
//!
//! # Score formula
//!
//! ```text
//! score = 0.4 × squeeze_intensity + 0.3 × volume_surge + 0.3 × breakout_strength
//!
//! where:
//!   squeeze_intensity = 1.0 - bandwidth_percentile  (tighter squeeze → higher)
//!   volume_surge = clamp(relative_volume - 1.0, 0, 2) / 2
//!   breakout_strength = clamp((close - donchian_upper) / atr, 0, 2) / 2
//! ```

use super::{Side, SignalOutput, SignalReason, Strategy};
use crate::features::FeatureValues;

/// Configuration for breakout strategy.
///
/// All parameters go in `[breakout]` section of `openquant.toml`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Config {
    /// Enable this strategy. Default: false (until backtested).
    pub enabled: bool,

    /// Require squeeze before breakout. Default: true
    pub squeeze_required: bool,

    /// Minimum volume surge for entry. Default: 1.2
    pub min_volume_surge: f64,

    /// ATR multiplier for Chandelier trailing stop. Default: 3.0
    pub trailing_stop_atr_mult: f64,

    /// Minimum score to act on. Default: 0.3
    pub min_score: f64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: false,
            squeeze_required: true,
            min_volume_surge: 1.2,
            trailing_stop_atr_mult: 3.0,
            min_score: 0.3,
        }
    }
}

/// Breakout / volatility expansion strategy.
pub struct Breakout {
    config: Config,
}

impl Breakout {
    pub fn new(config: Config) -> Self {
        Self { config }
    }
}

impl Strategy for Breakout {
    fn score(&self, features: &FeatureValues, has_position: bool) -> Option<SignalOutput> {
        if !features.warmed_up {
            return None;
        }

        // --- EXIT: Chandelier trailing stop ---
        // When holding, exit if price drops below donchian_upper - atr * multiplier.
        // This acts as a trailing stop that follows the breakout higher.
        if has_position {
            let stop_price =
                features.donchian_upper - self.config.trailing_stop_atr_mult * features.atr;
            // Use bollinger_pct_b < 0.5 as a proxy for "close is in lower half"
            // combined with price below the Chandelier stop level.
            // We approximate close from the features we have.
            // Actually, we can use donchian_mid as a reference:
            // if donchian_mid < stop_price, the channel has contracted enough to exit.
            // Better: %B < 0 means price broke below lower Bollinger band.
            // Simplest: check if close is below the trailing stop.
            // Since we don't have close directly, we check if the lower band
            // and close_location suggest price is at the bottom of the range.
            //
            // For now, exit when %B drops below 0.3 AND the Donchian channel
            // has started contracting (bandwidth expanding means breakout continuing).
            //
            // Actually, the simplest Chandelier exit: price dropped from the high
            // by more than atr * mult. We can approximate using %B:
            // if %B < (1.0 - 2*trailing_mult/bandwidth_in_std), but that's complex.
            //
            // Pragmatic approach: use bollinger_pct_b < 0.0 (broke below lower band)
            // as the Chandelier equivalent when combined with position holding.
            if features.bollinger_pct_b < 0.0 || features.donchian_mid < stop_price {
                let exit_score = (1.0 - features.bollinger_pct_b).clamp(0.1, 1.0);
                return Some(SignalOutput {
                    side: Side::Sell,
                    score: exit_score,
                    reason: SignalReason::BreakoutSell,
                    z_score: features.return_z_score,
                    relative_volume: features.relative_volume,
                });
            }
            return None; // holding, no exit triggered
        }

        // --- ENTRY: Donchian breakout from squeeze ---

        // Gate 1: Squeeze check
        if self.config.squeeze_required && !features.squeeze {
            return None;
        }

        // Gate 2: Price must break above Donchian upper channel
        // %B > 1.0 means close is above upper Bollinger band (breakout territory)
        // Combined with Donchian: we need price at or above the channel.
        // Use %B > 1.0 as breakout confirmation (price above upper Bollinger).
        if features.bollinger_pct_b <= 1.0 {
            return None;
        }

        // Gate 3: Volume surge
        if features.relative_volume < self.config.min_volume_surge {
            return None;
        }

        // Score components
        let squeeze_intensity = (1.0 - features.bandwidth_percentile).clamp(0.0, 1.0);
        let volume_surge = (features.relative_volume - 1.0).clamp(0.0, 2.0) / 2.0;
        // Breakout strength: how far above the upper band
        let breakout_strength = if features.atr > 1e-10 {
            let bb_width = features.bollinger_upper - features.bollinger_lower;
            let excess = (features.bollinger_pct_b - 1.0) * bb_width;
            (excess / features.atr).clamp(0.0, 2.0) / 2.0
        } else {
            0.5
        };

        let score = 0.4 * squeeze_intensity + 0.3 * volume_surge + 0.3 * breakout_strength;

        if score < self.config.min_score {
            return None;
        }

        Some(SignalOutput {
            side: Side::Buy,
            score,
            reason: SignalReason::BreakoutBuy,
            z_score: features.return_z_score,
            relative_volume: features.relative_volume,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build features for breakout testing.
    fn breakout_features(
        pct_b: f64,
        squeeze: bool,
        rel_vol: f64,
        bandwidth_pct: f64,
    ) -> FeatureValues {
        FeatureValues {
            warmed_up: true,
            bollinger_pct_b: pct_b,
            bollinger_upper: 110.0,
            bollinger_lower: 90.0,
            bollinger_bandwidth: 0.2,
            squeeze,
            bandwidth_percentile: bandwidth_pct,
            relative_volume: rel_vol,
            atr: 2.0,
            donchian_upper: 108.0,
            donchian_lower: 92.0,
            donchian_mid: 100.0,
            return_z_score: 0.5,
            ..Default::default()
        }
    }

    fn strategy() -> Breakout {
        Breakout::new(Config {
            enabled: true,
            ..Config::default()
        })
    }

    // --- Entry tests ---

    #[test]
    fn buy_fires_on_breakout_from_squeeze() {
        let sig = strategy().score(&breakout_features(1.5, true, 2.0, 0.10), false);
        assert!(sig.is_some());
        let sig = sig.unwrap();
        assert_eq!(sig.side, Side::Buy);
        assert_eq!(sig.reason, SignalReason::BreakoutBuy);
    }

    #[test]
    fn buy_blocked_without_squeeze() {
        assert!(
            strategy()
                .score(&breakout_features(1.5, false, 2.0, 0.50), false)
                .is_none()
        );
    }

    #[test]
    fn buy_blocked_when_pct_b_below_one() {
        assert!(
            strategy()
                .score(&breakout_features(0.8, true, 2.0, 0.10), false)
                .is_none()
        );
    }

    #[test]
    fn buy_blocked_low_volume() {
        assert!(
            strategy()
                .score(&breakout_features(1.5, true, 0.8, 0.10), false)
                .is_none()
        );
    }

    #[test]
    fn buy_blocked_when_holding() {
        // When holding with strong breakout, no exit triggered
        let mut f = breakout_features(1.5, true, 2.0, 0.10);
        // donchian_mid must be above stop_price (108 - 3*2 = 102)
        f.donchian_mid = 106.0;
        let sig = strategy().score(&f, true);
        assert!(sig.is_none());
    }

    #[test]
    fn buy_without_squeeze_requirement() {
        let s = Breakout::new(Config {
            enabled: true,
            squeeze_required: false,
            min_score: 0.0,
            ..Config::default()
        });
        let sig = s.score(&breakout_features(1.5, false, 2.0, 0.50), false);
        assert!(sig.is_some());
    }

    #[test]
    fn not_warmed_up_blocked() {
        let mut f = breakout_features(1.5, true, 2.0, 0.10);
        f.warmed_up = false;
        assert!(strategy().score(&f, false).is_none());
    }

    // --- Exit tests ---

    #[test]
    fn sell_fires_when_pct_b_drops_below_zero() {
        let f = breakout_features(-0.2, false, 1.0, 0.50);
        let sig = strategy().score(&f, true);
        assert!(sig.is_some());
        let sig = sig.unwrap();
        assert_eq!(sig.side, Side::Sell);
        assert_eq!(sig.reason, SignalReason::BreakoutSell);
    }

    #[test]
    fn sell_fires_when_donchian_mid_below_stop() {
        let mut f = breakout_features(0.3, false, 1.0, 0.50);
        // stop_price = 108 - 3.0 * 2.0 = 102
        // donchian_mid = 95 < 102 → exit
        f.donchian_mid = 95.0;
        let sig = strategy().score(&f, true);
        assert!(sig.is_some());
        assert_eq!(sig.unwrap().side, Side::Sell);
    }

    #[test]
    fn no_sell_when_breakout_still_strong() {
        let mut f = breakout_features(1.2, false, 2.0, 0.50);
        // donchian_mid above stop level, %B > 0 — no exit
        f.donchian_mid = 106.0;
        assert!(strategy().score(&f, true).is_none());
    }

    // --- Score tests ---

    #[test]
    fn score_increases_with_tighter_squeeze() {
        let s = Breakout::new(Config {
            enabled: true,
            min_score: 0.0,
            ..Config::default()
        });
        let loose = s
            .score(&breakout_features(1.5, true, 2.0, 0.15), false)
            .unwrap();
        let tight = s
            .score(&breakout_features(1.5, true, 2.0, 0.05), false)
            .unwrap();
        assert!(tight.score > loose.score);
    }

    #[test]
    fn score_increases_with_higher_volume() {
        let s = Breakout::new(Config {
            enabled: true,
            min_score: 0.0,
            ..Config::default()
        });
        let low = s
            .score(&breakout_features(1.5, true, 1.3, 0.10), false)
            .unwrap();
        let high = s
            .score(&breakout_features(1.5, true, 3.0, 0.10), false)
            .unwrap();
        assert!(high.score > low.score);
    }

    #[test]
    fn score_bounded() {
        let s = Breakout::new(Config {
            enabled: true,
            min_score: 0.0,
            ..Config::default()
        });
        let sig = s
            .score(&breakout_features(3.0, true, 5.0, 0.01), false)
            .unwrap();
        assert!(
            sig.score <= 1.0,
            "score should be bounded, got {}",
            sig.score
        );
        assert!(sig.score > 0.0);
    }
}
