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
//! 2. Extract the longest contiguous run of calm and volatile bars
//! 3. Run cointegration test on each contiguous sub-period separately
//! 4. Score regime robustness: 1.0 if cointegrated in both, 0.3 if calm-only, 0.0 if neither
//! 5. Adjust validation thresholds when current regime is stressed

use crate::stats::adf::adf_test;
use tracing::info;

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
    /// 0.3 = cointegrated only in calm periods (fragile)
    /// 0.0 = not cointegrated in either
    /// -1.0 = insufficient data (not tested)
    pub score: f64,
    /// ADF p-value during calm periods.
    pub calm_adf_pvalue: Option<f64>,
    /// ADF p-value during volatile periods.
    pub volatile_adf_pvalue: Option<f64>,
    /// Fraction of bars classified as calm.
    pub calm_fraction: f64,
    /// Whether there are enough contiguous bars in each regime for testing.
    pub sufficient_data: bool,
    /// Current regime classification (mode of last 10 bars).
    pub current_regime: PairRegime,
}

/// Minimum contiguous bars in a regime sub-period to run cointegration test.
/// ADF requires consecutive observations to correctly compute Δy_t = y_t - y_{t-1}.
const MIN_REGIME_BARS: usize = 50;

/// Rolling window for volatility estimation.
const VOL_WINDOW: usize = 20;

/// Buffer factor for regime classification around median volatility.
/// Bars with vol within ±(median * BUFFER * 0.3) are classified as Normal.
/// At 0.50: Normal band is [0.85*median, 1.15*median], giving roughly
/// 30-40% Calm, 20-30% Normal, 30-40% Volatile for typical vol distributions.
const VOL_BUFFER_FACTOR: f64 = 0.50;

/// Number of recent bars to use for current regime estimation (mode).
const CURRENT_REGIME_LOOKBACK: usize = 10;

/// Compute regime robustness for a pair.
///
/// Classifies each bar's regime based on rolling return volatility,
/// extracts the longest contiguous run of calm/volatile bars, and
/// runs cointegration tests on each. Using contiguous runs preserves
/// the time-series structure that ADF requires (consecutive Δy_t).
pub fn compute_regime_robustness(
    prices_a: &[f64],
    prices_b: &[f64],
    beta: f64,
) -> RegimeRobustness {
    let n = prices_a.len().min(prices_b.len());
    if n < VOL_WINDOW + MIN_REGIME_BARS {
        return RegimeRobustness {
            score: -1.0, // insufficient data → not tested
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

    // Classify bars by volatility
    let regimes = classify_regimes(&vol);
    let calm_fraction =
        regimes.iter().filter(|&&r| r == PairRegime::Calm).count() as f64 / regimes.len() as f64;

    // Current regime: mode of last N bars for stability
    let current_regime = regime_mode(&regimes);

    // Extract longest contiguous runs for each regime.
    // ADF requires consecutive observations — using scattered indices would
    // compute multi-day differences instead of 1-day Δy_t, biasing the test
    // toward over-detecting stationarity (fails unsafe for a risk control).
    let offset = VOL_WINDOW; // spread indices are offset by vol window
    let calm_spread = longest_contiguous_run(&regimes, PairRegime::Calm, &spread, offset);
    let volatile_spread = longest_contiguous_run(&regimes, PairRegime::Volatile, &spread, offset);

    let sufficient_data =
        calm_spread.len() >= MIN_REGIME_BARS && volatile_spread.len() >= MIN_REGIME_BARS;

    // Run ADF on each contiguous sub-period
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
        -1.0 // insufficient data: not tested
    };

    info!(
        calm_run = calm_spread.len(),
        volatile_run = volatile_spread.len(),
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
/// to avoid entering fragile pairs. Consumed by the pipeline for ADF
/// threshold and by PairsEngine (#121) for position sizing and entry z.
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
            PairRegime::Calm | PairRegime::Normal => Self {
                // Match the base ADF threshold (0.10). Regime tightening only
                // applies in volatile conditions — calm/normal uses the standard gate.
                adf_pvalue_threshold: 0.10,
                position_size_mult: 1.0,
                entry_z_mult: 1.0,
            },
            PairRegime::Volatile => Self {
                // Softened from 0.01 to 0.03 (issue #225): p<0.01 demands 99% confidence
                // during the noisiest data when ADF has lowest power. p<0.03 is still
                // conservative while not being statistically backwards.
                adf_pvalue_threshold: 0.03,
                position_size_mult: 0.5,
                entry_z_mult: 1.25,
            },
        }
    }
}

/// Extract the longest contiguous run of bars matching `target` regime.
///
/// Returns consecutive spread values preserving the time-series structure
/// that ADF requires. Non-contiguous filtering would produce multi-day
/// gaps in Δy_t, biasing the test toward over-detecting stationarity.
fn longest_contiguous_run(
    regimes: &[PairRegime],
    target: PairRegime,
    spread: &[f64],
    offset: usize,
) -> Vec<f64> {
    let mut best_start = 0;
    let mut best_len = 0;
    let mut cur_start = 0;
    let mut cur_len = 0;

    for (i, &r) in regimes.iter().enumerate() {
        if r == target {
            if cur_len == 0 {
                cur_start = i;
            }
            cur_len += 1;
            if cur_len > best_len {
                best_start = cur_start;
                best_len = cur_len;
            }
        } else {
            cur_len = 0;
        }
    }

    (best_start..best_start + best_len)
        .filter_map(|i| spread.get(i + offset).copied())
        .collect()
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
///
/// Buffer zone around median: [0.85*median, 1.15*median] → Normal.
/// Below → Calm, above → Volatile.
fn classify_regimes(vol: &[f64]) -> Vec<PairRegime> {
    if vol.is_empty() {
        return vec![];
    }

    let mut sorted = vol.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median = sorted[sorted.len() / 2];

    let low_thresh = median * (1.0 - VOL_BUFFER_FACTOR * 0.3);
    let high_thresh = median * (1.0 + VOL_BUFFER_FACTOR * 0.3);

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

/// Compute the mode (most common regime) of the last N bars.
fn regime_mode(regimes: &[PairRegime]) -> PairRegime {
    if regimes.is_empty() {
        return PairRegime::Normal;
    }
    let start = regimes.len().saturating_sub(CURRENT_REGIME_LOOKBACK);
    let recent = &regimes[start..];

    let mut calm = 0;
    let mut normal = 0;
    let mut volatile = 0;
    for &r in recent {
        match r {
            PairRegime::Calm => calm += 1,
            PairRegime::Normal => normal += 1,
            PairRegime::Volatile => volatile += 1,
        }
    }

    if volatile >= calm && volatile >= normal {
        PairRegime::Volatile
    } else if calm >= normal {
        PairRegime::Calm
    } else {
        PairRegime::Normal
    }
}

/// Penalize Thompson sampling prior for regime-fragile pairs.
///
/// If a pair is only cointegrated in calm markets (regime_robustness < 0.5),
/// reduce the Thompson prior mean to discourage selection.
pub fn regime_adjusted_prior(base_quality_score: f64, regime_robustness: f64) -> f64 {
    // For untested pairs (score = -1.0), use neutral weight
    let robustness = if regime_robustness < 0.0 {
        0.5
    } else {
        regime_robustness
    };
    // Blend: quality_score * robustness_weight
    // robustness=1.0 → no change, robustness=0.3 → 65% of original
    let weight = 0.5 + 0.5 * robustness;
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
            b_val += lcg.next(0.01);
            log_b.push(b_val);
        }

        let mut spread = 0.0;
        let mut log_a = Vec::with_capacity(n);
        for (i, lb) in log_b.iter().enumerate() {
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
        let (pa, pb) = crate::test_utils::cointegrated_pair(500, 1.5, 10.0, 42);
        let log_b: Vec<f64> = pb.iter().map(|p| p.ln()).collect();
        let log_a: Vec<f64> = pa.iter().map(|p| p.ln()).collect();
        let ols = ols_simple(&log_b, &log_a).unwrap();

        let result = compute_regime_robustness(&pa, &pb, ols.beta);
        assert!(
            result.score >= 0.3 || result.score == -1.0,
            "Stable pair should have decent robustness or insufficient data: score={}",
            result.score
        );
    }

    #[test]
    fn test_regime_switching_pair() {
        let (pa, pb) = regime_switching_pair(500, 42);
        let log_b: Vec<f64> = pb.iter().map(|p| p.ln()).collect();
        let log_a: Vec<f64> = pa.iter().map(|p| p.ln()).collect();
        let ols = ols_simple(&log_b, &log_a).unwrap();

        let result = compute_regime_robustness(&pa, &pb, ols.beta);
        assert!(
            result.score < 1.0,
            "Regime-switching pair should have reduced robustness: score={}",
            result.score
        );
    }

    #[test]
    fn test_insufficient_data_not_tested() {
        let pa = vec![100.0; 30];
        let pb = vec![50.0; 30];
        let result = compute_regime_robustness(&pa, &pb, 1.0);
        assert!(!result.sufficient_data);
        assert!((result.score - -1.0).abs() < 0.01, "score={}", result.score);
    }

    #[test]
    fn test_regime_adjusted_thresholds_calm() {
        let t = RegimeAdjustedThresholds::from_regime(PairRegime::Calm);
        assert_eq!(t.adf_pvalue_threshold, 0.10);
        assert_eq!(t.position_size_mult, 1.0);
        assert_eq!(t.entry_z_mult, 1.0);
    }

    #[test]
    fn test_regime_adjusted_thresholds_volatile() {
        let t = RegimeAdjustedThresholds::from_regime(PairRegime::Volatile);
        assert_eq!(t.adf_pvalue_threshold, 0.03);
        assert_eq!(t.position_size_mult, 0.5);
        assert_eq!(t.entry_z_mult, 1.25);
    }

    #[test]
    fn test_regime_adjusted_prior() {
        assert!((regime_adjusted_prior(0.80, 1.0) - 0.80).abs() < 0.01);
        let penalized = regime_adjusted_prior(0.80, 0.3);
        assert!(penalized < 0.80, "penalized={penalized}");
        assert!(penalized > 0.40, "penalized={penalized}");
        // Untested pairs (score = -1.0) get neutral treatment
        let untested = regime_adjusted_prior(0.80, -1.0);
        assert!((untested - 0.60).abs() < 0.01, "untested={untested}");
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
        let vol = vec![0.01; 100];
        let regimes = classify_regimes(&vol);
        assert_eq!(regimes.len(), 100);
        let unique: std::collections::HashSet<_> = regimes.iter().collect();
        assert!(
            unique.len() <= 2,
            "Uniform vol should give 1-2 regime types"
        );
    }

    #[test]
    fn test_longest_contiguous_run() {
        let regimes = vec![
            PairRegime::Calm,
            PairRegime::Calm,
            PairRegime::Volatile,
            PairRegime::Calm,
            PairRegime::Calm,
            PairRegime::Calm,
            PairRegime::Volatile,
        ];
        let spread = vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0];
        let run = longest_contiguous_run(&regimes, PairRegime::Calm, &spread, 0);
        // Longest calm run: indices 3,4,5 → spread values 4.0, 5.0, 6.0
        assert_eq!(run, vec![4.0, 5.0, 6.0]);
    }

    #[test]
    fn test_regime_mode() {
        let regimes = vec![
            PairRegime::Calm,
            PairRegime::Calm,
            PairRegime::Volatile,
            PairRegime::Volatile,
            PairRegime::Volatile,
        ];
        assert_eq!(regime_mode(&regimes), PairRegime::Volatile);
    }
}
