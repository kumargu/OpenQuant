//! Pair validation pipeline.
//!
//! Orchestrates the full validation flow:
//! 1. ETF exclusion filter (instant reject)
//! 2. OLS regression → beta (hedge ratio)
//! 3. Engle-Granger cointegration (ADF on spread residuals)
//! 4. OU half-life estimation
//! 5. Beta stability (rolling CV + structural break detection)
//! 6. Composite scoring
//!
//! Reads `pair_candidates.json`, validates each pair against daily price data,
//! writes `active_pairs.json` with passing pairs sorted by score.

use crate::etf_filter::is_etf_component_pair;
use crate::regime::{compute_regime_robustness, RegimeAdjustedThresholds};
use crate::scorer::compute_score;
use crate::stats::adf::adf_test;
use crate::stats::beta_stability::check_beta_stability;
use crate::stats::halflife::{estimate_half_life, is_half_life_valid};
use crate::stats::ols::ols_simple;
use crate::types::{ActivePairsFile, PairCandidate, PairCandidatesFile, ValidationResult};
use chrono::Utc;
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use tracing::{info, warn};

/// Minimum number of daily bars required for validation.
/// Lowered from 200 to 90: captures recent regime while still providing
/// sufficient observations for ADF (needs ~50+) and rolling beta (30-bar windows).
/// Trade-off: shorter window = more responsive to regime changes but less
/// statistical power. 90 days is ~4.5 months of daily data.
pub const MIN_HISTORY_BARS: usize = 90;

/// Maximum window for validation. Caps data to the most recent N bars
/// even when more history is available. Keeps validation focused on the
/// current regime rather than averaging across historical regime changes.
pub const MAX_VALIDATION_WINDOW: usize = 150;

/// Minimum R² for the hedge ratio OLS — below this the beta is meaningless noise.
pub const MIN_R_SQUARED: f64 = 0.50;

/// Price data for a single symbol: ordered daily close prices.
pub type PriceData = Vec<f64>;

/// Price data provider trait — allows testing with synthetic data.
pub trait PriceProvider {
    /// Get daily close prices for a symbol.
    /// Returns at least `MIN_HISTORY_BARS` prices, ordered oldest-to-newest.
    /// Returns a reference to avoid cloning 500+ f64s per pair.
    fn get_prices(&self, symbol: &str) -> Option<&[f64]>;
}

/// In-memory price provider for testing.
pub struct InMemoryPrices {
    pub data: HashMap<String, PriceData>,
}

impl PriceProvider for InMemoryPrices {
    fn get_prices(&self, symbol: &str) -> Option<&[f64]> {
        self.data.get(symbol).map(|v| v.as_slice())
    }
}

/// Validate a single candidate pair.
pub fn validate_pair(candidate: &PairCandidate, provider: &dyn PriceProvider) -> ValidationResult {
    let mut result = ValidationResult::new(candidate);

    // Step 1: ETF exclusion (instant reject)
    if is_etf_component_pair(&candidate.leg_a, &candidate.leg_b) {
        result.etf_excluded = true;
        result.rejection_reasons.push("ETF-component pair".into());
        return result;
    }

    // Step 2: Get price data
    let prices_a = match provider.get_prices(&candidate.leg_a) {
        Some(p) if p.len() >= MIN_HISTORY_BARS => p,
        Some(p) => {
            result.rejection_reasons.push(format!(
                "{}: only {} bars (need {})",
                candidate.leg_a,
                p.len(),
                MIN_HISTORY_BARS
            ));
            return result;
        }
        None => {
            result
                .rejection_reasons
                .push(format!("{}: no price data", candidate.leg_a));
            return result;
        }
    };

    let prices_b = match provider.get_prices(&candidate.leg_b) {
        Some(p) if p.len() >= MIN_HISTORY_BARS => p,
        Some(p) => {
            result.rejection_reasons.push(format!(
                "{}: only {} bars (need {})",
                candidate.leg_b,
                p.len(),
                MIN_HISTORY_BARS
            ));
            return result;
        }
        None => {
            result
                .rejection_reasons
                .push(format!("{}: no price data", candidate.leg_b));
            return result;
        }
    };

    // Use the most recent observations. If more data is available than needed,
    // cap to MAX_VALIDATION_WINDOW to focus on the recent regime.
    let n = prices_a
        .len()
        .min(prices_b.len())
        .min(MAX_VALIDATION_WINDOW);
    let prices_a = &prices_a[prices_a.len() - n..];
    let prices_b = &prices_b[prices_b.len() - n..];

    // Guard: reject non-positive prices before ln() — data corruption, bad API
    // response, or stock split artifacts would produce -inf/NaN that silently
    // propagates through OLS, ADF, and scoring.
    if prices_a.iter().any(|&p| !p.is_finite() || p <= 0.0) {
        result.rejection_reasons.push(format!(
            "{}: non-positive or NaN prices detected",
            candidate.leg_a
        ));
        return result;
    }
    if prices_b.iter().any(|&p| !p.is_finite() || p <= 0.0) {
        result.rejection_reasons.push(format!(
            "{}: non-positive or NaN prices detected",
            candidate.leg_b
        ));
        return result;
    }

    // Log-prices for regression
    let log_a: Vec<f64> = prices_a.iter().map(|p| p.ln()).collect();
    let log_b: Vec<f64> = prices_b.iter().map(|p| p.ln()).collect();

    // Step 3: OLS regression → beta
    let ols = match ols_simple(&log_b, &log_a) {
        Some(r) => r,
        None => {
            result
                .rejection_reasons
                .push("OLS regression failed".into());
            return result;
        }
    };

    result.alpha = Some(ols.alpha);
    result.beta = Some(ols.beta);
    result.beta_r_squared = Some(ols.r_squared);

    // Step 3b: Minimum R² — below this the hedge ratio is meaningless noise
    if ols.r_squared < MIN_R_SQUARED {
        result.rejection_reasons.push(format!(
            "R²={:.3} below minimum {MIN_R_SQUARED}",
            ols.r_squared
        ));
    }

    // Step 4: Engle-Granger cointegration
    // Spread = log_a - beta * log_b (intentionally omitting OLS intercept alpha).
    // The ADF regression includes its own constant term, and the AR(1) half-life
    // estimation absorbs any level shift, so subtracting alpha here is unnecessary
    // and would only add noise from the intercept estimate.
    let spread: Vec<f64> = log_a
        .iter()
        .zip(log_b.iter())
        .map(|(a, b)| a - ols.beta * b)
        .collect();

    match adf_test(&spread, None, true) {
        Some(adf) => {
            result.adf_statistic = Some(adf.test_statistic);
            result.adf_pvalue = Some(adf.p_value);
            result.is_cointegrated = adf.is_stationary;
            if !adf.is_stationary {
                result.rejection_reasons.push(format!(
                    "Not cointegrated (ADF p={:.4}, stat={:.3})",
                    adf.p_value, adf.test_statistic
                ));
            }
        }
        None => {
            result.rejection_reasons.push("ADF test failed".into());
            return result;
        }
    }

    // Step 5: OU half-life
    match estimate_half_life(&spread) {
        Some(hl) => {
            result.half_life = Some(hl.half_life);
            result.half_life_valid = is_half_life_valid(hl.half_life);
            if !result.half_life_valid {
                result.rejection_reasons.push(format!(
                    "Half-life {:.1} days outside valid range [3, 40]",
                    hl.half_life
                ));
            }
        }
        None => {
            result
                .rejection_reasons
                .push("Half-life estimation failed (not mean-reverting)".into());
        }
    }

    // Step 6: Beta stability
    match check_beta_stability(&log_a, &log_b) {
        Some(bs) => {
            result.beta_cv = Some(bs.cv);
            result.structural_break = bs.structural_break;
            result.beta_stable = bs.is_stable;
            if !bs.is_stable {
                let mut reasons = Vec::new();
                if bs.cv >= 0.20 {
                    reasons.push(format!("Beta CV={:.3} >= 0.20", bs.cv));
                }
                if bs.structural_break {
                    reasons.push(format!(
                        "Structural break: shift={:.1}% > threshold={:.1}%",
                        bs.max_shift_pct * 100.0,
                        crate::stats::beta_stability::structural_break_threshold() * 100.0,
                    ));
                }
                result.rejection_reasons.extend(reasons);
            }
        }
        None => {
            result
                .rejection_reasons
                .push("Beta stability check failed".into());
        }
    }

    // Step 7: Regime robustness — test cointegration across calm/volatile sub-periods
    if let Some(beta) = result.beta {
        let robustness = compute_regime_robustness(prices_a, prices_b, beta);
        result.regime_robustness = Some(robustness.score);

        // Use regime-adjusted ADF threshold (p<0.01 in volatile vs p<0.05 in calm)
        let thresholds = RegimeAdjustedThresholds::from_regime(robustness.current_regime);
        if let Some(p) = result.adf_pvalue {
            if p > thresholds.adf_pvalue_threshold && result.is_cointegrated {
                // ADF passed at 0.05 but fails the tighter volatile threshold
                result.is_cointegrated = false;
                result.rejection_reasons.push(format!(
                    "Regime-tightened: ADF p={p:.4} > {:.2} (volatile regime threshold)",
                    thresholds.adf_pvalue_threshold
                ));
            }
        }

        if robustness.sufficient_data && robustness.score >= 0.0 && robustness.score < 0.3 {
            result.rejection_reasons.push(format!(
                "Regime-fragile: robustness={:.2} (cointegration breaks in volatile periods)",
                robustness.score
            ));
        }
    }

    // Step 8: Compute score and determine pass/fail
    result.score = compute_score(
        result.adf_pvalue.unwrap_or(1.0),
        result.half_life.unwrap_or(0.0),
        result.beta_cv.unwrap_or(1.0),
        result.beta_r_squared.unwrap_or(0.0),
        result.structural_break,
    );

    // Pass criteria: cointegrated + valid half-life + stable beta + adequate R²
    let r_squared_ok = result.beta_r_squared.unwrap_or(0.0) >= MIN_R_SQUARED;
    result.passed = result.is_cointegrated
        && result.half_life_valid
        && result.beta_stable
        && r_squared_ok
        && !result.etf_excluded;

    result
}

/// Run the full pipeline: read candidates, validate, write results.
pub fn run_pipeline(
    candidates_path: &Path,
    output_path: &Path,
    provider: &dyn PriceProvider,
) -> Result<Vec<ValidationResult>, PipelineError> {
    // Read candidates
    let contents = fs::read_to_string(candidates_path).map_err(PipelineError::Io)?;
    let candidates: PairCandidatesFile =
        serde_json::from_str(&contents).map_err(PipelineError::Json)?;

    info!(
        "Loaded {} candidate pairs from {}",
        candidates.pairs.len(),
        candidates_path.display()
    );

    run_pipeline_from_candidates(&candidates.pairs, output_path, provider)
}

/// Run pipeline from an in-memory list of candidates (used by tests and external callers).
pub fn run_pipeline_from_candidates(
    candidates: &[PairCandidate],
    output_path: &Path,
    provider: &dyn PriceProvider,
) -> Result<Vec<ValidationResult>, PipelineError> {
    let mut results: Vec<ValidationResult> = candidates
        .iter()
        .map(|c| {
            let r = validate_pair(c, provider);
            if r.passed {
                info!(
                    "PASS: {}/{} — score={:.3}, beta={:.4}, hl={:.1}d, adf_p={:.4}",
                    r.leg_a,
                    r.leg_b,
                    r.score,
                    r.beta.unwrap_or(0.0),
                    r.half_life.unwrap_or(0.0),
                    r.adf_pvalue.unwrap_or(1.0),
                );
            } else {
                warn!(
                    "REJECT: {}/{} — {:?}",
                    r.leg_a, r.leg_b, r.rejection_reasons
                );
            }
            r
        })
        .collect();

    // Sort by score descending
    results.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // Build output
    let active_pairs: Vec<_> = results.iter().filter_map(|r| r.to_active_pair()).collect();

    let output = ActivePairsFile {
        generated_at: Utc::now(),
        pairs: active_pairs,
    };

    // Write output
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent).map_err(PipelineError::Io)?;
    }
    let json = serde_json::to_string_pretty(&output).map_err(PipelineError::Json)?;
    fs::write(output_path, json).map_err(PipelineError::Io)?;

    info!(
        "Wrote {} active pairs to {}",
        output.pairs.len(),
        output_path.display()
    );

    Ok(results)
}

/// Minimum bars for beta refresh (much less than full validation).
const MIN_REFRESH_BARS: usize = 30;

/// Lightweight beta/alpha refresh — only runs OLS on existing pairs.
///
/// Unlike full validation (which requires 200+ bars, ADF, half-life, etc.),
/// this only needs ~30 bars to compute a reliable hedge ratio. Useful when
/// you have insufficient data for full validation but want to keep
/// alpha/beta current.
///
/// Reads existing `active_pairs.json`, re-estimates OLS on available price
/// data, and writes updated file. Pairs that don't have enough data are
/// left unchanged.
pub fn refresh_beta(
    active_pairs_path: &Path,
    provider: &dyn PriceProvider,
) -> Result<usize, PipelineError> {
    let contents = fs::read_to_string(active_pairs_path).map_err(PipelineError::Io)?;
    let mut file: crate::types::ActivePairsFile =
        serde_json::from_str(&contents).map_err(PipelineError::Json)?;

    let mut refreshed = 0;

    for pair in &mut file.pairs {
        let prices_a = match provider.get_prices(&pair.leg_a) {
            Some(p) if p.len() >= MIN_REFRESH_BARS => p,
            _ => continue,
        };
        let prices_b = match provider.get_prices(&pair.leg_b) {
            Some(p) if p.len() >= MIN_REFRESH_BARS => p,
            _ => continue,
        };

        let n = prices_a.len().min(prices_b.len());
        let pa = &prices_a[prices_a.len() - n..];
        let pb = &prices_b[prices_b.len() - n..];

        // Guard non-positive prices
        if pa.iter().any(|&p| !p.is_finite() || p <= 0.0)
            || pb.iter().any(|&p| !p.is_finite() || p <= 0.0)
        {
            continue;
        }

        let log_a: Vec<f64> = pa.iter().map(|p| p.ln()).collect();
        let log_b: Vec<f64> = pb.iter().map(|p| p.ln()).collect();

        if let Some(ols) = ols_simple(&log_b, &log_a) {
            let old_beta = pair.beta;
            let old_alpha = pair.alpha;
            pair.alpha = ols.alpha;
            pair.beta = ols.beta;
            refreshed += 1;

            info!(
                pair = format!("{}/{}", pair.leg_a, pair.leg_b).as_str(),
                old_beta = format!("{old_beta:.4}").as_str(),
                new_beta = format!("{:.4}", ols.beta).as_str(),
                old_alpha = format!("{old_alpha:.4}").as_str(),
                new_alpha = format!("{:.4}", ols.alpha).as_str(),
                r_squared = format!("{:.3}", ols.r_squared).as_str(),
                bars = n,
                "Beta refreshed via OLS"
            );
        }
    }

    // Update timestamp and write back
    file.generated_at = Utc::now();
    let json = serde_json::to_string_pretty(&file).map_err(PipelineError::Json)?;
    fs::write(active_pairs_path, json).map_err(PipelineError::Io)?;

    info!(refreshed, total = file.pairs.len(), "Beta refresh complete");

    Ok(refreshed)
}

#[derive(Debug)]
pub enum PipelineError {
    Io(std::io::Error),
    Json(serde_json::Error),
}

impl std::fmt::Display for PipelineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "IO error: {e}"),
            Self::Json(e) => write!(f, "JSON error: {e}"),
        }
    }
}

impl std::error::Error for PipelineError {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils;
    use crate::types::PairCandidate;
    use tempfile::TempDir;

    fn make_provider(pairs: Vec<(&str, PriceData)>) -> InMemoryPrices {
        InMemoryPrices {
            data: pairs.into_iter().map(|(s, p)| (s.to_string(), p)).collect(),
        }
    }

    #[test]
    fn test_cointegrated_pair_passes() {
        // Generate exactly MAX_VALIDATION_WINDOW bars so the cap doesn't truncate
        let (pa, pb) = test_utils::cointegrated_pair(MAX_VALIDATION_WINDOW, 1.5, 10.0, 42);
        let provider = make_provider(vec![("A", pa), ("B", pb)]);
        let candidate = PairCandidate {
            leg_a: "A".into(),
            leg_b: "B".into(),
            economic_rationale: "test pair".into(),
        };

        let result = validate_pair(&candidate, &provider);
        assert!(
            result.passed,
            "Expected cointegrated pair to pass. Rejections: {:?}",
            result.rejection_reasons
        );
        assert!(result.score > 0.5, "score={}", result.score);
        assert!(result.beta.unwrap() > 1.0, "beta={:?}", result.beta);
    }

    #[test]
    fn test_random_walks_rejected() {
        let (pa, pb) = test_utils::independent_walks(500, 42);
        let provider = make_provider(vec![("X", pa), ("Y", pb)]);
        let candidate = PairCandidate {
            leg_a: "X".into(),
            leg_b: "Y".into(),
            economic_rationale: "test pair".into(),
        };

        let result = validate_pair(&candidate, &provider);
        assert!(
            !result.passed,
            "Expected random walks to be rejected. Score={}",
            result.score
        );
    }

    #[test]
    fn test_etf_component_rejected() {
        let (pa, pb) = test_utils::cointegrated_pair(500, 1.5, 10.0, 42);
        let provider = make_provider(vec![("XLF", pa), ("JPM", pb)]);
        let candidate = PairCandidate {
            leg_a: "XLF".into(),
            leg_b: "JPM".into(),
            economic_rationale: "ETF and component".into(),
        };

        let result = validate_pair(&candidate, &provider);
        assert!(!result.passed);
        assert!(result.etf_excluded);
    }

    #[test]
    fn test_insufficient_data_rejected() {
        let provider = make_provider(vec![("A", vec![100.0; 50]), ("B", vec![100.0; 50])]);
        let candidate = PairCandidate {
            leg_a: "A".into(),
            leg_b: "B".into(),
            economic_rationale: "test".into(),
        };

        let result = validate_pair(&candidate, &provider);
        assert!(!result.passed);
    }

    #[test]
    fn test_full_pipeline_writes_output() {
        let tmp = TempDir::new().unwrap();
        let output_path = tmp.path().join("active_pairs.json");

        let (pa, pb) = test_utils::cointegrated_pair(500, 1.5, 10.0, 42);
        let (px, py) = test_utils::independent_walks(500, 99);
        let provider = make_provider(vec![("A", pa), ("B", pb), ("X", px), ("Y", py)]);

        let candidates = vec![
            PairCandidate {
                leg_a: "A".into(),
                leg_b: "B".into(),
                economic_rationale: "cointegrated".into(),
            },
            PairCandidate {
                leg_a: "X".into(),
                leg_b: "Y".into(),
                economic_rationale: "random walks".into(),
            },
        ];

        let results = run_pipeline_from_candidates(&candidates, &output_path, &provider).unwrap();

        assert_eq!(results.len(), 2);
        assert!(output_path.exists());

        // Read and verify output
        let contents = fs::read_to_string(&output_path).unwrap();
        let output: ActivePairsFile = serde_json::from_str(&contents).unwrap();

        // Only the cointegrated pair should pass
        assert!(
            output.pairs.len() <= 1,
            "Expected at most 1 active pair, got {}",
            output.pairs.len()
        );
        if !output.pairs.is_empty() {
            assert_eq!(output.pairs[0].leg_a, "A");
            assert_eq!(output.pairs[0].leg_b, "B");
        }
    }

    #[test]
    fn test_non_positive_prices_rejected() {
        // Zero price should be caught before ln()
        let mut prices_a = vec![100.0; 300];
        prices_a[150] = 0.0; // corrupt data point
        let prices_b = vec![100.0; 300];
        let provider = make_provider(vec![("A", prices_a), ("B", prices_b)]);
        let candidate = PairCandidate {
            leg_a: "A".into(),
            leg_b: "B".into(),
            economic_rationale: "test".into(),
        };
        let result = validate_pair(&candidate, &provider);
        assert!(!result.passed);
        assert!(
            result
                .rejection_reasons
                .iter()
                .any(|r| r.contains("non-positive")),
            "Expected non-positive price rejection, got: {:?}",
            result.rejection_reasons
        );
    }

    #[test]
    fn test_nan_prices_rejected() {
        let mut prices_a = vec![100.0; 300];
        prices_a[100] = f64::NAN;
        let prices_b = vec![100.0; 300];
        let provider = make_provider(vec![("A", prices_a), ("B", prices_b)]);
        let candidate = PairCandidate {
            leg_a: "A".into(),
            leg_b: "B".into(),
            economic_rationale: "test".into(),
        };
        let result = validate_pair(&candidate, &provider);
        assert!(!result.passed);
    }
}
