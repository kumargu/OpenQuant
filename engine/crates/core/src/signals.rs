/// Signal scoring: features → trade decision.
///
/// V1: Simple mean-reversion strategy.
/// - Buy when price drops unusually (z-score < -2) with high volume confirmation
/// - Sell when price spikes unusually (z-score > 2) and holding a position

use crate::features::FeatureValues;

/// Side of a trade.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Side {
    Buy,
    Sell,
}

/// What the signal scorer thinks we should do.
#[derive(Debug, Clone)]
pub struct SignalOutput {
    pub side: Side,
    pub score: f64,    // conviction strength, higher = stronger
    pub reason: String,
}

/// Configuration for the signal scorer.
#[derive(Debug, Clone)]
pub struct SignalConfig {
    /// Z-score threshold for buy (negative = oversold). Default: -2.0
    pub buy_z_threshold: f64,
    /// Z-score threshold for sell (positive = overbought). Default: 2.0
    pub sell_z_threshold: f64,
    /// Minimum relative volume to confirm signal. Default: 1.2
    pub min_relative_volume: f64,
    /// Minimum score to act. Default: 0.5
    pub min_score: f64,
}

impl Default for SignalConfig {
    fn default() -> Self {
        Self {
            buy_z_threshold: -2.0,
            sell_z_threshold: 2.0,
            min_relative_volume: 1.2,
            min_score: 0.5,
        }
    }
}

/// Score the current features and return a signal if thresholds are met.
///
/// Returns None if no trade signal, Some(SignalOutput) if a signal fires.
pub fn score(features: &FeatureValues, has_position: bool, config: &SignalConfig) -> Option<SignalOutput> {
    if !features.warmed_up {
        return None;
    }

    // Buy signal: oversold + volume confirmation
    if features.return_z_score < config.buy_z_threshold
        && features.relative_volume > config.min_relative_volume
        && !has_position
    {
        // Score: how far past threshold * volume confirmation
        let z_strength = (config.buy_z_threshold - features.return_z_score).abs();
        let vol_strength = features.relative_volume - 1.0;
        let score = 0.6 * z_strength + 0.4 * vol_strength;

        if score >= config.min_score {
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

    // Sell signal: overbought + holding position
    if features.return_z_score > config.sell_z_threshold && has_position {
        let z_strength = (features.return_z_score - config.sell_z_threshold).abs();
        let score = z_strength;

        if score >= config.min_score {
            return Some(SignalOutput {
                side: Side::Sell,
                score,
                reason: format!(
                    "mean-reversion sell: z={:.2}, rel_vol={:.2}",
                    features.return_z_score, features.relative_volume
                ),
            });
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn features_with_z(z: f64, rel_vol: f64) -> FeatureValues {
        FeatureValues {
            return_z_score: z,
            relative_volume: rel_vol,
            warmed_up: true,
            ..Default::default()
        }
    }

    #[test]
    fn test_buy_signal_fires() {
        let f = features_with_z(-3.0, 1.5);
        let sig = score(&f, false, &SignalConfig::default());
        assert!(sig.is_some());
        let sig = sig.unwrap();
        assert_eq!(sig.side, Side::Buy);
        assert!(sig.score >= 0.5);
    }

    #[test]
    fn test_no_buy_when_already_holding() {
        let f = features_with_z(-3.0, 1.5);
        let sig = score(&f, true, &SignalConfig::default());
        assert!(sig.is_none());
    }

    #[test]
    fn test_sell_signal_fires() {
        let f = features_with_z(3.0, 1.5);
        let sig = score(&f, true, &SignalConfig::default());
        assert!(sig.is_some());
        assert_eq!(sig.unwrap().side, Side::Sell);
    }

    #[test]
    fn test_no_sell_when_not_holding() {
        let f = features_with_z(3.0, 1.5);
        let sig = score(&f, false, &SignalConfig::default());
        assert!(sig.is_none());
    }

    #[test]
    fn test_no_signal_when_not_warmed_up() {
        let mut f = features_with_z(-5.0, 2.0);
        f.warmed_up = false;
        assert!(score(&f, false, &SignalConfig::default()).is_none());
    }

    #[test]
    fn test_no_signal_weak_z() {
        let f = features_with_z(-1.0, 1.5);
        assert!(score(&f, false, &SignalConfig::default()).is_none());
    }

    #[test]
    fn test_no_signal_low_volume() {
        let f = features_with_z(-3.0, 0.8);
        assert!(score(&f, false, &SignalConfig::default()).is_none());
    }
}
