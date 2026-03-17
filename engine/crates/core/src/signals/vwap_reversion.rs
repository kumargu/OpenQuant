//! VWAP reversion strategy — mean-reversion anchored to volume-weighted price.
//!
//! VWAP represents where institutional money actually traded. When price deviates
//! significantly from VWAP, institutional execution benchmarking creates a
//! gravitational pull back toward it.
//!
//! # Entry conditions (all must be true)
//!
//! 1. `vwap_z_score < buy_z_threshold` — price is N std devs below VWAP
//! 2. `relative_volume > min_relative_volume` — real participation
//! 3. VWAP is ready (enough session bars)
//! 4. Within valid session window (skip first/last N bars)
//! 5. Not already holding a position
//!
//! # Exit conditions
//!
//! 1. `vwap_z_score > sell_z_threshold` — price reverted past VWAP
//! 2. Currently holding a position
//!
//! # Score formula
//!
//! ```text
//! score = 0.5 × |z_vwap - threshold| + 0.3 × volume_boost + 0.2 × vwap_proximity
//! ```

use super::{Side, SignalOutput, SignalReason, Strategy};
use crate::features::FeatureValues;

/// Configuration for VWAP reversion strategy.
///
/// All parameters go in `[vwap_reversion]` section of `openquant.toml`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Config {
    /// Enable this strategy. Default: false (until backtested).
    pub enabled: bool,

    /// VWAP Z-score below which a buy signal fires. Default: -2.0
    pub buy_z_threshold: f64,

    /// VWAP Z-score above which a sell/exit signal fires. Default: 1.5
    pub sell_z_threshold: f64,

    /// Minimum relative volume to confirm entry. Default: 1.0
    pub min_relative_volume: f64,

    /// Minimum score to act on. Default: 0.3
    pub min_score: f64,

    /// Session bars to skip at start (VWAP unstable early). Default: 30
    pub session_start_skip_bars: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: false,
            buy_z_threshold: -2.0,
            sell_z_threshold: 1.5,
            min_relative_volume: 1.0,
            min_score: 0.3,
            session_start_skip_bars: 30,
        }
    }
}

/// VWAP reversion strategy.
pub struct VwapReversion {
    config: Config,
}

impl VwapReversion {
    pub fn new(config: Config) -> Self {
        Self { config }
    }
}

impl Strategy for VwapReversion {
    fn score(&self, features: &FeatureValues, has_position: bool) -> Option<SignalOutput> {
        if !features.warmed_up || !features.vwap_ready {
            return None;
        }

        // Session gate: skip early bars where VWAP is unstable
        if features.vwap_session_bars < self.config.session_start_skip_bars {
            return None;
        }

        // --- EXIT: price reverted above VWAP ---
        if has_position && features.vwap_z_score > self.config.sell_z_threshold {
            let z_excess = features.vwap_z_score - self.config.sell_z_threshold;
            let score = z_excess.clamp(0.0, 2.0) / 2.0;
            return Some(SignalOutput {
                side: Side::Sell,
                score,
                reason: SignalReason::VwapReversionSell,
                z_score: features.vwap_z_score,
                relative_volume: features.relative_volume,
                votes: String::new(),
            });
        }

        // --- ENTRY: price dropped below VWAP ---
        if has_position {
            return None;
        }

        if features.vwap_z_score.is_nan() || features.vwap_z_score > self.config.buy_z_threshold {
            return None;
        }

        // Volume gate
        if features.relative_volume < self.config.min_relative_volume {
            return None;
        }

        // Score: how far below threshold + volume strength
        let z_excess = (features.vwap_z_score - self.config.buy_z_threshold)
            .abs()
            .clamp(0.0, 3.0)
            / 3.0;
        let volume_boost = (features.relative_volume - 1.0).clamp(0.0, 2.0) / 2.0;
        // Proximity to VWAP mid-session (higher in mid-session)
        let session_factor: f64 = if features.vwap_session_bars > 200 {
            0.5 // late session, VWAP solidified
        } else if features.vwap_session_bars > 60 {
            1.0 // mid-session, VWAP most reliable
        } else {
            0.3 // early session
        };
        let vwap_quality = session_factor.clamp(0.0, 1.0);

        let score = 0.5 * z_excess + 0.3 * volume_boost + 0.2 * vwap_quality;

        if score < self.config.min_score {
            return None;
        }

        Some(SignalOutput {
            side: Side::Buy,
            score,
            reason: SignalReason::VwapReversionBuy,
            z_score: features.vwap_z_score,
            relative_volume: features.relative_volume,
            votes: String::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vwap_features(vwap_z: f64, rel_vol: f64, session_bars: usize) -> FeatureValues {
        FeatureValues {
            warmed_up: true,
            vwap: 100.0,
            vwap_deviation: vwap_z * 2.0, // approximate
            vwap_z_score: vwap_z,
            vwap_session_bars: session_bars,
            vwap_ready: true,
            relative_volume: rel_vol,
            ..Default::default()
        }
    }

    fn strategy() -> VwapReversion {
        VwapReversion::new(Config {
            enabled: true,
            ..Config::default()
        })
    }

    // --- Entry tests ---

    #[test]
    fn buy_fires_below_vwap() {
        let sig = strategy().score(&vwap_features(-2.5, 1.5, 100), false);
        assert!(sig.is_some());
        let sig = sig.unwrap();
        assert_eq!(sig.side, Side::Buy);
        assert_eq!(sig.reason, SignalReason::VwapReversionBuy);
    }

    #[test]
    fn buy_blocked_above_threshold() {
        assert!(
            strategy()
                .score(&vwap_features(-1.5, 1.5, 100), false)
                .is_none()
        );
    }

    #[test]
    fn buy_blocked_when_holding() {
        assert!(
            strategy()
                .score(&vwap_features(-2.5, 1.5, 100), true)
                .is_none()
        );
    }

    #[test]
    fn buy_blocked_low_volume() {
        assert!(
            strategy()
                .score(&vwap_features(-2.5, 0.5, 100), false)
                .is_none()
        );
    }

    #[test]
    fn buy_blocked_early_session() {
        assert!(
            strategy()
                .score(&vwap_features(-2.5, 1.5, 10), false)
                .is_none()
        );
    }

    #[test]
    fn buy_blocked_when_not_warmed_up() {
        let mut f = vwap_features(-2.5, 1.5, 100);
        f.warmed_up = false;
        assert!(strategy().score(&f, false).is_none());
    }

    #[test]
    fn buy_blocked_when_vwap_not_ready() {
        let mut f = vwap_features(-2.5, 1.5, 100);
        f.vwap_ready = false;
        assert!(strategy().score(&f, false).is_none());
    }

    #[test]
    fn nan_vwap_z_blocked() {
        assert!(
            strategy()
                .score(&vwap_features(f64::NAN, 1.5, 100), false)
                .is_none()
        );
    }

    // --- Exit tests ---

    #[test]
    fn sell_fires_above_vwap() {
        let sig = strategy().score(&vwap_features(2.0, 1.0, 100), true);
        assert!(sig.is_some());
        let sig = sig.unwrap();
        assert_eq!(sig.side, Side::Sell);
        assert_eq!(sig.reason, SignalReason::VwapReversionSell);
    }

    #[test]
    fn no_sell_when_not_holding() {
        // Below buy threshold, not holding — should fire buy, not sell
        let sig = strategy().score(&vwap_features(2.0, 1.0, 100), false);
        assert!(sig.is_none()); // above buy threshold, no entry
    }

    #[test]
    fn no_sell_when_still_below_vwap() {
        // Holding but z still below sell threshold
        assert!(
            strategy()
                .score(&vwap_features(0.5, 1.0, 100), true)
                .is_none()
        );
    }

    // --- Score tests ---

    #[test]
    fn score_increases_with_deeper_z() {
        let s = VwapReversion::new(Config {
            enabled: true,
            min_score: 0.0,
            ..Config::default()
        });
        let shallow = s.score(&vwap_features(-2.1, 1.5, 100), false).unwrap();
        let deep = s.score(&vwap_features(-3.5, 1.5, 100), false).unwrap();
        assert!(deep.score > shallow.score);
    }

    #[test]
    fn score_increases_with_higher_volume() {
        let s = VwapReversion::new(Config {
            enabled: true,
            min_score: 0.0,
            ..Config::default()
        });
        let low = s.score(&vwap_features(-2.5, 1.0, 100), false).unwrap();
        let high = s.score(&vwap_features(-2.5, 2.5, 100), false).unwrap();
        assert!(high.score > low.score);
    }

    #[test]
    fn score_bounded() {
        let s = VwapReversion::new(Config {
            enabled: true,
            min_score: 0.0,
            ..Config::default()
        });
        let sig = s.score(&vwap_features(-10.0, 5.0, 100), false).unwrap();
        assert!(
            sig.score <= 1.0,
            "score should be bounded, got {}",
            sig.score
        );
        assert!(sig.score > 0.0);
    }

    #[test]
    fn signal_carries_vwap_z_snapshot() {
        let sig = strategy()
            .score(&vwap_features(-2.5, 1.5, 100), false)
            .unwrap();
        assert!((sig.z_score - (-2.5)).abs() < 1e-10);
        assert!((sig.relative_volume - 1.5).abs() < 1e-10);
    }
}
