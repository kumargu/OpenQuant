//! Basket candidate validation.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::adf::adf_test;
use crate::bertram::optimize_symmetric_thresholds;
use crate::dominance::max_component_dominance;
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
    /// Whether to reject baskets that fail ADF stationarity on the fit window.
    pub adf_gate_enabled: bool,
    /// Maximum allowed ADF p-value when the gate is enabled.
    pub adf_pvalue_max: f64,
    /// Whether to reject baskets dominated by a single component.
    pub dominance_gate_enabled: bool,
    /// Maximum allowed absolute component variance contribution share.
    pub dominance_max: f64,
}

impl Default for ValidatorConfig {
    fn default() -> Self {
        Self {
            residual_window: 60,
            k_clip_min: 0.15,
            k_clip_max: 2.5,
            cost: 0.0005,
            adf_gate_enabled: false,
            adf_pvalue_max: 0.05,
            dominance_gate_enabled: false,
            dominance_max: 0.60,
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
    let target_window = &target_prices[target_prices.len() - config.residual_window..];
    let peer_windows: Vec<&[f64]> = peer_prices
        .iter()
        .map(|p| &p[p.len() - config.residual_window..])
        .collect();

    let adf = adf_test(fit_window, None);
    if config.adf_gate_enabled {
        match adf {
            Some(result) if result.p_value <= config.adf_pvalue_max => {}
            Some(result) => {
                return BasketFit::rejected(
                    candidate.clone(),
                    format!(
                        "ADF gate failed: p={:.4} > {:.4} (stat={:.3})",
                        result.p_value, config.adf_pvalue_max, result.test_statistic
                    ),
                );
            }
            None => {
                return BasketFit::rejected(candidate.clone(), "ADF gate failed: test unavailable")
            }
        }
    }

    let dominance_score = max_component_dominance(target_window, &peer_windows);
    if config.dominance_gate_enabled {
        match dominance_score {
            Some(score) if score <= config.dominance_max => {}
            Some(score) => {
                return BasketFit::rejected(
                    candidate.clone(),
                    format!(
                        "dominance gate failed: score={:.3} > {:.3}",
                        score, config.dominance_max
                    ),
                );
            }
            None => {
                return BasketFit::rejected(
                    candidate.clone(),
                    "dominance gate failed: score unavailable",
                )
            }
        }
    }

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
        adf_statistic: adf.map(|r| r.test_statistic),
        adf_pvalue: adf.map(|r| r.p_value),
        dominance_score,
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

    fn make_random_walk(n: usize, seed: u64, drift: f64) -> Vec<f64> {
        let mut state = seed;
        let mut p = 100.0_f64;
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            let r = ((state >> 33) as f64 / (1u64 << 31) as f64) - 0.5;
            p *= 1.0 + drift + 0.02 * r;
            out.push(p);
        }
        out
    }

    /// Stationary AR(1) spread on top of a shared random walk.
    /// `kappa` near 1 = slow reversion; near 0 = fast reversion.
    fn make_strongly_cointegrated(
        n: usize,
        seed: u64,
        kappa: f64,
    ) -> (Vec<f64>, Vec<f64>, Vec<f64>) {
        let mut state = seed;
        let mut basket_lp = (100.0_f64).ln();
        let mut spread = 0.0_f64;
        let mut t = Vec::with_capacity(n);
        let mut p1 = Vec::with_capacity(n);
        let mut p2 = Vec::with_capacity(n);
        for _ in 0..n {
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            let r1 = ((state >> 33) as f64 / (1u64 << 31) as f64) - 0.5;
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            let r2 = ((state >> 33) as f64 / (1u64 << 31) as f64) - 0.5;
            state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
            let noise = ((state >> 33) as f64 / (1u64 << 31) as f64) - 0.5;
            basket_lp += 0.01 * (r1 + r2) * 0.5;
            spread = kappa * spread + 0.005 * noise;
            let lp1 = basket_lp + 0.005 * r1;
            let lp2 = basket_lp + 0.005 * r2;
            p1.push(lp1.exp());
            p2.push(lp2.exp());
            t.push((basket_lp + spread).exp());
        }
        (t, p1, p2)
    }

    #[test]
    fn test_adf_gate_rejects_non_stationary_spread() {
        let candidate = BasketCandidate {
            target: "AAPL".to_string(),
            members: vec!["MSFT".to_string(), "GOOGL".to_string()],
            sector: "tech".to_string(),
            fit_date: chrono::NaiveDate::from_ymd_opt(2026, 4, 20).unwrap(),
        };

        let mut bars = HashMap::new();
        bars.insert("AAPL".to_string(), make_random_walk(120, 1, 0.0010));
        bars.insert("MSFT".to_string(), make_random_walk(120, 2, 0.0));
        bars.insert("GOOGL".to_string(), make_random_walk(120, 3, 0.0));

        let config = ValidatorConfig {
            adf_gate_enabled: true,
            adf_pvalue_max: 0.05,
            ..Default::default()
        };
        let fit = validate(&candidate, &bars, &config);

        assert!(!fit.valid, "non-stationary spread must fail ADF gate");
        assert!(
            fit.reject_reason.as_deref().unwrap().contains("ADF gate"),
            "reason should mention ADF gate, got: {:?}",
            fit.reject_reason
        );
    }

    #[test]
    fn test_adf_gate_admits_stationary_spread() {
        let candidate = BasketCandidate {
            target: "AAPL".to_string(),
            members: vec!["MSFT".to_string(), "GOOGL".to_string()],
            sector: "tech".to_string(),
            fit_date: chrono::NaiveDate::from_ymd_opt(2026, 4, 20).unwrap(),
        };

        // kappa=0.3 → fast mean-reversion; spread is clearly stationary
        let (target, peer1, peer2) = make_strongly_cointegrated(120, 12345, 0.3);

        let mut bars = HashMap::new();
        bars.insert("AAPL".to_string(), target);
        bars.insert("MSFT".to_string(), peer1);
        bars.insert("GOOGL".to_string(), peer2);

        let config = ValidatorConfig {
            adf_gate_enabled: true,
            adf_pvalue_max: 0.10,
            ..Default::default()
        };
        let fit = validate(&candidate, &bars, &config);
        assert!(
            fit.valid,
            "strongly mean-reverting spread should pass ADF gate, got reject: {:?}",
            fit.reject_reason
        );
    }

    #[test]
    fn test_dominance_gate_rejects_target_dominated_basket() {
        let candidate = BasketCandidate {
            target: "BIG".to_string(),
            members: vec!["TINY1".to_string(), "TINY2".to_string()],
            sector: "x".to_string(),
            fit_date: chrono::NaiveDate::from_ymd_opt(2026, 4, 20).unwrap(),
        };

        let big = make_random_walk(120, 11, 0.0);
        let mut bars = HashMap::new();
        bars.insert("BIG".to_string(), big);
        bars.insert(
            "TINY1".to_string(),
            (0..120).map(|i| 100.0 + i as f64 * 1e-6).collect(),
        );
        bars.insert(
            "TINY2".to_string(),
            (0..120).map(|i| 100.0 - i as f64 * 1e-6).collect(),
        );

        let config = ValidatorConfig {
            dominance_gate_enabled: true,
            dominance_max: 0.50,
            ..Default::default()
        };
        let fit = validate(&candidate, &bars, &config);

        assert!(
            !fit.valid,
            "single-name-dominated basket must fail dominance gate"
        );
        assert!(
            fit.reject_reason
                .as_deref()
                .unwrap()
                .contains("dominance gate"),
            "reason should mention dominance gate, got: {:?}",
            fit.reject_reason
        );
    }

    #[test]
    fn test_dominance_gate_admits_balanced_basket() {
        let candidate = BasketCandidate {
            target: "AAPL".to_string(),
            members: vec!["MSFT".to_string(), "GOOGL".to_string()],
            sector: "tech".to_string(),
            fit_date: chrono::NaiveDate::from_ymd_opt(2026, 4, 20).unwrap(),
        };

        // Balanced random walks — no member concentrates spread variance
        let mut bars = HashMap::new();
        bars.insert("AAPL".to_string(), make_random_walk(120, 1, 0.0));
        bars.insert("MSFT".to_string(), make_random_walk(120, 2, 0.0));
        bars.insert("GOOGL".to_string(), make_random_walk(120, 3, 0.0));

        // Loose threshold (3.0) — well above any realistic balanced score; the
        // metric is a normalized risk-budget share that can exceed 1.0 due to
        // dollar-neutral weights, but any single member shouldn't reach 3.0
        // when all three series are independent random walks of the same vol.
        let config = ValidatorConfig {
            dominance_gate_enabled: true,
            dominance_max: 3.0,
            ..Default::default()
        };
        let fit = validate(&candidate, &bars, &config);

        // We don't care if OU/Bertram pass downstream — only that the
        // dominance gate did not reject.
        let reason = fit.reject_reason.as_deref().unwrap_or("");
        assert!(
            !reason.contains("dominance gate"),
            "balanced basket should not be dominance-rejected, got: {reason}"
        );
    }

    #[test]
    fn test_dominance_gate_rejects_when_score_unavailable() {
        let candidate = BasketCandidate {
            target: "AAPL".to_string(),
            members: vec!["MSFT".to_string(), "GOOGL".to_string()],
            sector: "tech".to_string(),
            fit_date: chrono::NaiveDate::from_ymd_opt(2026, 4, 20).unwrap(),
        };

        let mut bars = HashMap::new();
        bars.insert("AAPL".to_string(), vec![100.0; 120]);
        bars.insert("MSFT".to_string(), vec![100.0; 120]);
        bars.insert("GOOGL".to_string(), vec![100.0; 120]);

        let config = ValidatorConfig {
            dominance_gate_enabled: true,
            dominance_max: 0.50,
            ..Default::default()
        };
        let fit = validate(&candidate, &bars, &config);

        assert!(!fit.valid, "zero-variance spread must reject");
        let reason = fit.reject_reason.unwrap();
        assert!(
            reason.contains("dominance gate") || reason.contains("OU"),
            "reason should mention dominance or OU failure, got: {reason}"
        );
    }
}
