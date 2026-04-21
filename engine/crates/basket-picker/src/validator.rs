//! Basket candidate validation.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::bertram::optimize_symmetric_thresholds;
use crate::ou::fit_ou_ar1;
use crate::schema::{BasketCandidate, BasketFit};
use crate::spread::build_spread;

/// Configuration for the validator.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidatorConfig {
    /// Number of days to use for OU fitting.
    pub residual_window: usize,
    /// Minimum allowed k after Bertram optimization.
    pub k_clip_min: f64,
    /// Maximum allowed k after Bertram optimization.
    pub k_clip_max: f64,
    /// Transaction cost per round-trip (as a decimal, e.g., 0.0005 for 5 bps).
    pub cost: f64,
}

impl Default for ValidatorConfig {
    fn default() -> Self {
        Self {
            residual_window: 60,
            k_clip_min: 0.15,
            k_clip_max: 2.5,
            cost: 0.0005,
        }
    }
}

/// Validate a basket candidate against historical bars.
///
/// # Arguments
/// * `candidate` - The basket candidate to validate
/// * `bars` - Map from symbol to price series (daily closes, aligned)
/// * `config` - Validator configuration
///
/// # Returns
/// A `BasketFit` with validation result.
pub fn validate(
    candidate: &BasketCandidate,
    bars: &HashMap<String, Vec<f64>>,
    config: &ValidatorConfig,
) -> BasketFit {
    // Get target prices
    let target_prices = match bars.get(&candidate.target) {
        Some(p) => p,
        None => {
            return BasketFit::rejected(
                candidate.clone(),
                format!("missing target symbol: {}", candidate.target),
            )
        }
    };

    // Get peer prices
    let mut peer_prices: Vec<&[f64]> = Vec::new();
    for member in &candidate.members {
        match bars.get(member) {
            Some(p) => {
                if p.len() != target_prices.len() {
                    return BasketFit::rejected(
                        candidate.clone(),
                        format!("misaligned bars for peer: {}", member),
                    );
                }
                peer_prices.push(p);
            }
            None => {
                return BasketFit::rejected(
                    candidate.clone(),
                    format!("missing peer symbol: {}", member),
                )
            }
        }
    }

    // Build spread
    let spread = match build_spread(target_prices, &peer_prices) {
        Some(s) => s,
        None => {
            return BasketFit::rejected(
                candidate.clone(),
                "failed to build spread (invalid prices)",
            )
        }
    };

    // Use only the last residual_window days for fitting
    if spread.len() < config.residual_window {
        return BasketFit::rejected(
            candidate.clone(),
            format!(
                "insufficient data: {} bars < {} required",
                spread.len(),
                config.residual_window
            ),
        );
    }
    let fit_window = &spread[spread.len() - config.residual_window..];

    // Fit OU
    let ou = match fit_ou_ar1(fit_window) {
        Some(fit) => fit,
        None => return BasketFit::rejected(candidate.clone(), "OU fit failed (b not in (0,1))"),
    };

    // Compute Bertram thresholds
    let bertram = match optimize_symmetric_thresholds(&ou, config.cost) {
        Some(bt) => bt,
        None => {
            return BasketFit::rejected(candidate.clone(), "Bertram optimization failed");
        }
    };

    // Validate clip bounds (clamp panics if min > max)
    if config.k_clip_min > config.k_clip_max {
        return BasketFit::rejected(
            candidate.clone(),
            format!(
                "invalid config: k_clip_min ({}) > k_clip_max ({})",
                config.k_clip_min, config.k_clip_max
            ),
        );
    }

    // Clip k to allowed range
    let k_raw = bertram.k;
    let k_clipped = k_raw.clamp(config.k_clip_min, config.k_clip_max);

    // Check validity: k must be within range
    let (valid, reject_reason) = if k_raw < config.k_clip_min {
        (
            true,
            Some(format!(
                "k={:.3} clipped up to {:.3}",
                k_raw, config.k_clip_min
            )),
        )
    } else if k_raw > config.k_clip_max {
        (
            true,
            Some(format!(
                "k={:.3} clipped down to {:.3}",
                k_raw, config.k_clip_max
            )),
        )
    } else {
        (true, None)
    };

    BasketFit {
        candidate: candidate.clone(),
        ou: Some(ou),
        bertram: Some(bertram),
        threshold_k: k_clipped,
        valid,
        reject_reason,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_cointegrated_prices(n: usize, seed: u64) -> (Vec<f64>, Vec<f64>, Vec<f64>) {
        // Generate cointegrated prices: target = alpha + beta * peer1 + beta * peer2 + noise
        // The spread (log target - mean(log peers)) will be mean-reverting
        let mut state = seed;

        let mut peer1 = Vec::with_capacity(n);
        let mut peer2 = Vec::with_capacity(n);
        let mut target = Vec::with_capacity(n);

        let mut p1 = 100.0_f64;
        let mut p2 = 150.0_f64;
        let mut spread = 0.0_f64;

        for _ in 0..n {
            // Random walk for peers
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            let r1 = ((state >> 33) as f64 / (1u64 << 31) as f64) - 0.5;
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            let r2 = ((state >> 33) as f64 / (1u64 << 31) as f64) - 0.5;
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            let noise = ((state >> 33) as f64 / (1u64 << 31) as f64) - 0.5;

            p1 *= 1.0 + 0.01 * r1;
            p2 *= 1.0 + 0.01 * r2;

            // Mean-reverting spread
            spread = 0.9 * spread + 0.01 * noise;

            // Target follows basket + mean-reverting spread
            let log_basket = (p1.ln() + p2.ln()) / 2.0;
            let t = (log_basket + spread).exp();

            peer1.push(p1);
            peer2.push(p2);
            target.push(t);
        }

        (target, peer1, peer2)
    }

    #[test]
    fn test_validate_basic() {
        let candidate = BasketCandidate {
            target: "AAPL".to_string(),
            members: vec!["MSFT".to_string(), "GOOGL".to_string()],
            sector: "tech".to_string(),
            fit_date: chrono::NaiveDate::from_ymd_opt(2026, 4, 20).unwrap(),
        };

        let (target, peer1, peer2) = make_cointegrated_prices(100, 12345);

        let mut bars = HashMap::new();
        bars.insert("AAPL".to_string(), target);
        bars.insert("MSFT".to_string(), peer1);
        bars.insert("GOOGL".to_string(), peer2);

        let config = ValidatorConfig::default();
        let fit = validate(&candidate, &bars, &config);

        // Should succeed with cointegrated data
        assert!(fit.ou.is_some(), "OU fit should succeed");
        assert!(fit.valid, "validation should pass");
    }

    fn make_simple_prices(n: usize, base: f64) -> Vec<f64> {
        (0..n).map(|i| base * (1.0 + 0.001 * i as f64)).collect()
    }

    #[test]
    fn test_validate_missing_symbol() {
        let candidate = BasketCandidate {
            target: "AAPL".to_string(),
            members: vec!["MSFT".to_string()],
            sector: "tech".to_string(),
            fit_date: chrono::NaiveDate::from_ymd_opt(2026, 4, 20).unwrap(),
        };

        let mut bars = HashMap::new();
        bars.insert("AAPL".to_string(), make_simple_prices(100, 100.0));
        // MSFT missing

        let config = ValidatorConfig::default();
        let fit = validate(&candidate, &bars, &config);

        assert!(!fit.valid);
        assert!(fit.reject_reason.unwrap().contains("missing peer symbol"));
    }

    #[test]
    fn test_validate_insufficient_data() {
        let candidate = BasketCandidate {
            target: "AAPL".to_string(),
            members: vec!["MSFT".to_string()],
            sector: "tech".to_string(),
            fit_date: chrono::NaiveDate::from_ymd_opt(2026, 4, 20).unwrap(),
        };

        let mut bars = HashMap::new();
        bars.insert("AAPL".to_string(), make_simple_prices(30, 100.0));
        bars.insert("MSFT".to_string(), make_simple_prices(30, 150.0));

        let config = ValidatorConfig {
            residual_window: 60, // More than we have
            ..Default::default()
        };
        let fit = validate(&candidate, &bars, &config);

        assert!(!fit.valid);
        assert!(fit.reject_reason.unwrap().contains("insufficient data"));
    }

    #[test]
    fn test_validate_invalid_config_clip_bounds() {
        let candidate = BasketCandidate {
            target: "AAPL".to_string(),
            members: vec!["MSFT".to_string(), "GOOGL".to_string()],
            sector: "tech".to_string(),
            fit_date: chrono::NaiveDate::from_ymd_opt(2026, 4, 20).unwrap(),
        };

        let (target, peer1, peer2) = make_cointegrated_prices(100, 12345);

        let mut bars = HashMap::new();
        bars.insert("AAPL".to_string(), target);
        bars.insert("MSFT".to_string(), peer1);
        bars.insert("GOOGL".to_string(), peer2);

        let config = ValidatorConfig {
            k_clip_min: 2.5,
            k_clip_max: 0.15, // Invalid: min > max
            ..Default::default()
        };
        let fit = validate(&candidate, &bars, &config);

        assert!(!fit.valid);
        assert!(fit.reject_reason.unwrap().contains("invalid config"));
    }
}
