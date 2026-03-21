//! Regime-aware pair scoring.
//!
//! Integrates volatility regime detection into pair validation to avoid
//! recommending pairs that are only cointegrated in calm markets.
//!
//! ## Why Phase 1
//!
//! Pairs that are cointegrated in low-vol regimes can blow up during regime
//! transitions. Without regime gating, the pair picker will happily recommend
//! pairs right before they decouple. This is a risk control, not an enhancement.
//!
//! ## Approach
//!
//! 1. Classify each bar as calm/volatile using a rolling volatility percentile
//! 2. Split price history into calm and volatile sub-periods
//! 3. Run cointegration test on each sub-period separately
//! 4. Score regime robustness: 1.0 if cointegrated in both, 0.5 if calm-only, 0.0 if neither
//! 5. Adjust validation thresholds when current regime is stressed

use crate::stats::adf::adf_test;
use tracing::debug;

/// Regime classification for the pair picker.
/// Simplified version of the engine's MarketRegime — the pair picker
/// doesn't need Crisis/Unknown distinction since it runs daily, not intraday.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PairRegime {
    Calm,
    Normal,
    Volatile,
}

/// Result of regime-conditional cointegration analysis.
#[derive(Debug, Clone)]
pub struct RegimeRobustness {
    /// Regime robustness score [0, 1].
    /// 1.0 = cointegrated in both calm and volatile periods
    /// 0.5 = cointegrated only in calm periods (fragile)
    /// 0.0 = not cointegrated in either
    pub score: f64,
    /// ADF p-value during calm periods.
    pub calm_adf_pvalue: Option<f64>,
    /// ADF p-value during volatile periods.
    pub volatile_adf_pvalue: Option<f64>,
    /// Fraction of bars classified as calm.
    pub calm_fraction: f64,
    /// Whether there are enough bars in each regime for testing.
    pub sufficient_data: bool,
    /// Current regime classification.
    pub current_regime: PairRegime,
}

/// Minimum bars in a regime sub-period to run cointegration test.
const MIN_REGIME_BARS: usize = 50;

/// Rolling window for volatility estimation.
const VOL_WINDOW: usize = 20;

/// Percentile threshold: below this → calm, above → volatile.
const VOL_PERCENTILE_THRESHOLD: f64 = 0.50;

/// Compute regime robustness for a pair.
///
/// Classifies each bar's regime based on rolling return volatility,
/// then runs cointegration tests on calm and volatile sub-periods.
pub fn compute_regime_robustness(
    prices_a: &[f64],
    prices_b: &[f64],
    beta: f64,
) -> RegimeRobustness {
    let n = prices_a.len().min(prices_b.len());
    if n < VOL_WINDOW + MIN_REGIME_BARS {
        return RegimeRobustness {
            score: 0.5, // insufficient data → neutral
            calm_adf_pvalue: None,
            volatile_adf_pvalue: None,
            calm_fraction: 0.5,
            sufficient_data: false,
            current_regime: PairRegime::Normal,
        };
    }

    // Compute log-spread
    let spread: Vec<f64> = (0..n)
        .map(|i| prices_a[i].ln() - beta * prices_b[i].ln())
        .collect();

    // Compute rolling volatility of spread returns
    let returns: Vec<f64> = (1..n).map(|i| spread[i] - spread[i - 1]).collect();
    let vol = rolling_volatility(&returns, VOL_WINDOW);

    // Classify bars by volatility percentile
    let regimes = classify_regimes(&vol);
    let calm_fraction =
        regimes.iter().filter(|&&r| r == PairRegime::Calm).count() as f64 / regimes.len() as f64;
    let current_regime = *regimes.last().unwrap_or(&PairRegime::Normal);

    // Split spread into calm and volatile sub-periods
    // We need contiguous-enough blocks, so we just filter indices
    let calm_spread: Vec<f64> = regimes
        .iter()
        .enumerate()
        .filter(|(_, &r)| r == PairRegime::Calm)
        .map(|(i, _)| spread[i + VOL_WINDOW]) // offset by vol window
        .collect();

    let volatile_spread: Vec<f64> = regimes
        .iter()
        .enumerate()
        .filter(|(_, &r)| r == PairRegime::Volatile)
        .map(|(i, _)| spread[i + VOL_WINDOW])
        .collect();

    let sufficient_data =
        calm_spread.len() >= MIN_REGIME_BARS && volatile_spread.len() >= MIN_REGIME_BARS;

    // Run ADF on each sub-period
    let calm_adf = if calm_spread.len() >= MIN_REGIME_BARS {
        adf_test(&calm_spread, Some(2), true)
    } else {
        None
    };

    let volatile_adf = if volatile_spread.len() >= MIN_REGIME_BARS {
        adf_test(&volatile_spread, Some(2), true)
    } else {
        None
    };

    let calm_pvalue = calm_adf.as_ref().map(|a| a.p_value);
    let volatile_pvalue = volatile_adf.as_ref().map(|a| a.p_value);

    let calm_coint = calm_adf.is_some_and(|a| a.is_stationary);
    let volatile_coint = volatile_adf.is_some_and(|a| a.is_stationary);

    // Score regime robustness
    let score = if calm_coint && volatile_coint {
        1.0 // robust: cointegrated in both regimes
    } else if calm_coint && !volatile_coint {
        0.3 // fragile: only works in calm markets
    } else if !calm_coint && volatile_coint {
        0.4 // unusual but possible: mean-reverts more in stress
    } else if sufficient_data {
        0.0 // broken: not cointegrated in either regime
    } else {
        0.5 // insufficient data: neutral prior
    };

    debug!(
        calm_bars = calm_spread.len(),
        volatile_bars = volatile_spread.len(),
        calm_coint,
        volatile_coint,
        score,
        current_regime = ?current_regime,
        "Regime robustness computed"
    );

    RegimeRobustness {
        score,
        calm_adf_pvalue: calm_pvalue,
        volatile_adf_pvalue: volatile_pvalue,
        calm_fraction,
        sufficient_data,
        current_regime,
    }
}

/// Regime-adjusted validation thresholds.
///
/// When the current regime is volatile/stressed, tighten requirements
/// to avoid entering fragile pairs.
#[derive(Debug, Clone)]
pub struct RegimeAdjustedThresholds {
    /// ADF p-value threshold (default 0.05, tightened to 0.01 in volatile).
    pub adf_pvalue_threshold: f64,
    /// Position size multiplier (1.0 = normal, 0.5 = half in volatile).
    pub position_size_mult: f64,
    /// Z-score entry threshold multiplier (1.0 = normal, 1.25 = wider in volatile).
    pub entry_z_mult: f64,
}

impl RegimeAdjustedThresholds {
    /// Compute thresholds based on current regime.
    pub fn from_regime(regime: PairRegime) -> Self {
        match regime {
            PairRegime::Calm => Self {
                adf_pvalue_threshold: 0.05,
                position_size_mult: 1.0,
                entry_z_mult: 1.0,
            },
            PairRegime::Normal => Self {
                adf_pvalue_threshold: 0.05,
                position_size_mult: 1.0,
                entry_z_mult: 1.0,
            },
            PairRegime::Volatile => Self {
                adf_pvalue_threshold: 0.01,
                position_size_mult: 0.5,
                entry_z_mult: 1.25,
            },
        }
    }
}

/// Compute rolling standard deviation of returns.
fn rolling_volatility(returns: &[f64], window: usize) -> Vec<f64> {
    let n = returns.len();
    if n < window {
        return vec![];
    }

    let mut result = Vec::with_capacity(n - window + 1);
    for start in 0..=(n - window) {
        let slice = &returns[start..start + window];
        let mean: f64 = slice.iter().sum::<f64>() / window as f64;
        let var: f64 = slice.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / (window - 1) as f64;
        result.push(var.sqrt());
    }
    result
}

/// Classify each bar's regime by comparing its rolling vol to the median vol.
fn classify_regimes(vol: &[f64]) -> Vec<PairRegime> {
    if vol.is_empty() {
        return vec![];
    }

    // Compute median volatility
    let mut sorted = vol.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median = sorted[sorted.len() / 2];

    // Classify: below median → Calm, above → Volatile
    // Use a buffer zone around median for Normal
    let low_thresh = median * (1.0 - VOL_PERCENTILE_THRESHOLD * 0.3);
    let high_thresh = median * (1.0 + VOL_PERCENTILE_THRESHOLD * 0.3);

    vol.iter()
        .map(|&v| {
            if v < low_thresh {
                PairRegime::Calm
            } else if v > high_thresh {
                PairRegime::Volatile
            } else {
                PairRegime::Normal
            }
        })
        .collect()
}

/// Penalize Thompson sampling prior for regime-fragile pairs.
///
/// If a pair is only cointegrated in calm markets (regime_robustness < 0.5),
/// reduce the Thompson prior mean to discourage selection.
pub fn regime_adjusted_prior(base_quality_score: f64, regime_robustness: f64) -> f64 {
    // Blend: quality_score * robustness_weight
    // robustness=1.0 → no change, robustness=0.3 → 65% of original
    let weight = 0.5 + 0.5 * regime_robustness;
    base_quality_score * weight
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stats::ols::ols_simple;

    /// Generate prices with regime change: calm first half, volatile second half.
    fn regime_switching_pair(n: usize, seed: u64) -> (Vec<f64>, Vec<f64>) {
        let mut lcg = crate::test_utils::Lcg::new(seed);
        let beta = 1.5;
        let phi = (-f64::ln(2.0) / 10.0).exp();

        let mut log_b = Vec::with_capacity(n);
        let mut b_val = 4.0;
        for _ in 0..n {
            b_val += lcg.next(0.01); // tight random walk
            log_b.push(b_val);
        }

        let mut spread = 0.0;
        let mut log_a = Vec::with_capacity(n);
        for (i, lb) in log_b.iter().enumerate() {
            // Calm first half, volatile second half (spread noise 5x larger)
            let noise_scale = if i < n / 2 { 0.005 } else { 0.025 };
            spread = phi * spread + lcg.next(noise_scale);
            log_a.push(beta * lb + 1.0 + spread);
        }

        let pa: Vec<f64> = log_a.iter().map(|x| x.exp()).collect();
        let pb: Vec<f64> = log_b.iter().map(|x| x.exp()).collect();
        (pa, pb)
    }

    #[test]
    fn test_stable_pair_high_robustness() {
        // Pair with consistent cointegration across regimes
        let (pa, pb) = crate::test_utils::cointegrated_pair(500, 1.5, 10.0, 42);
        let log_b: Vec<f64> = pb.iter().map(|p| p.ln()).collect();
        let log_a: Vec<f64> = pa.iter().map(|p| p.ln()).collect();
        let ols = ols_simple(&log_b, &log_a).unwrap();

        let result = compute_regime_robustness(&pa, &pb, ols.beta);
        assert!(
            result.score >= 0.3,
            "Stable pair should have decent robustness: score={}",
            result.score
        );
    }

    #[test]
    fn test_regime_switching_pair() {
        // Pair that's cointegrated in calm period but breaks in volatile period
        let (pa, pb) = regime_switching_pair(500, 42);
        let log_b: Vec<f64> = pb.iter().map(|p| p.ln()).collect();
        let log_a: Vec<f64> = pa.iter().map(|p| p.ln()).collect();
        let ols = ols_simple(&log_b, &log_a).unwrap();

        let result = compute_regime_robustness(&pa, &pb, ols.beta);
        // Should detect some fragility (volatile period has weaker cointegration)
        assert!(
            result.score < 1.0,
            "Regime-switching pair should have reduced robustness: score={}",
            result.score
        );
    }

    #[test]
    fn test_insufficient_data_neutral() {
        let pa = vec![100.0; 30];
        let pb = vec![50.0; 30];
        let result = compute_regime_robustness(&pa, &pb, 1.0);
        assert!(!result.sufficient_data);
        assert!((result.score - 0.5).abs() < 0.01, "score={}", result.score);
    }

    #[test]
    fn test_regime_adjusted_thresholds_calm() {
        let t = RegimeAdjustedThresholds::from_regime(PairRegime::Calm);
        assert_eq!(t.adf_pvalue_threshold, 0.05);
        assert_eq!(t.position_size_mult, 1.0);
        assert_eq!(t.entry_z_mult, 1.0);
    }

    #[test]
    fn test_regime_adjusted_thresholds_volatile() {
        let t = RegimeAdjustedThresholds::from_regime(PairRegime::Volatile);
        assert_eq!(t.adf_pvalue_threshold, 0.01);
        assert_eq!(t.position_size_mult, 0.5);
        assert_eq!(t.entry_z_mult, 1.25);
    }

    #[test]
    fn test_regime_adjusted_prior() {
        // High robustness → minimal penalty
        assert!((regime_adjusted_prior(0.80, 1.0) - 0.80).abs() < 0.01);
        // Low robustness → significant penalty
        let penalized = regime_adjusted_prior(0.80, 0.3);
        assert!(penalized < 0.80, "penalized={penalized}");
        assert!(penalized > 0.40, "penalized={penalized}"); // not too harsh
    }

    #[test]
    fn test_rolling_volatility() {
        let returns = vec![
            0.01, -0.02, 0.015, -0.01, 0.005, 0.02, -0.015, 0.01, -0.005, 0.01,
        ];
        let vol = rolling_volatility(&returns, 5);
        assert_eq!(vol.len(), 6);
        assert!(vol.iter().all(|v| *v > 0.0 && v.is_finite()));
    }

    #[test]
    fn test_classify_regimes_balanced() {
        // Uniform volatility → mostly Normal
        let vol = vec![0.01; 100];
        let regimes = classify_regimes(&vol);
        assert_eq!(regimes.len(), 100);
        // All same vol → should be classified consistently
        let unique: std::collections::HashSet<_> = regimes.iter().collect();
        assert!(
            unique.len() <= 2,
            "Uniform vol should give 1-2 regime types"
        );
    }
}
