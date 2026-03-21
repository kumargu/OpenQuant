//! Pair validation pipeline.
//!
//! Orchestrates the full validation flow:
//! 1. ETF exclusion filter (instant reject)
//! 2. OLS regression → beta (hedge ratio)
//! 3. Engle-Granger cointegration (ADF on spread residuals)
//! 4. OU half-life estimation
//! 5. Beta stability (rolling CV + CUSUM)
//! 6. Composite scoring
//!
//! Reads `pair_candidates.json`, validates each pair against daily price data,
//! writes `active_pairs.json` with passing pairs sorted by score.

use crate::etf_filter::is_etf_component_pair;
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
pub const MIN_HISTORY_BARS: usize = 200;

/// Price data for a single symbol: ordered daily close prices.
pub type PriceData = Vec<f64>;

/// Price data provider trait — allows testing with synthetic data.
pub trait PriceProvider {
    /// Get daily close prices for a symbol.
    /// Returns at least `MIN_HISTORY_BARS` prices, ordered oldest-to-newest.
    fn get_prices(&self, symbol: &str) -> Option<PriceData>;
}

/// In-memory price provider for testing.
pub struct InMemoryPrices {
    pub data: HashMap<String, PriceData>,
}

impl PriceProvider for InMemoryPrices {
    fn get_prices(&self, symbol: &str) -> Option<PriceData> {
        self.data.get(symbol).cloned()
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

    // Use min length
    let n = prices_a.len().min(prices_b.len());
    let prices_a = &prices_a[prices_a.len() - n..];
    let prices_b = &prices_b[prices_b.len() - n..];

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

    result.beta = Some(ols.beta);
    result.beta_r_squared = Some(ols.r_squared);

    // Step 4: Engle-Granger cointegration
    // Spread residuals = log_a - beta * log_b
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
            result.cusum_break = bs.cusum_break;
            result.beta_stable = bs.is_stable;
            if !bs.is_stable {
                let mut reasons = Vec::new();
                if bs.cv >= 0.20 {
                    reasons.push(format!("Beta CV={:.3} >= 0.20", bs.cv));
                }
                if bs.cusum_break {
                    reasons.push("CUSUM structural break detected".into());
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

    // Step 7: Compute score and determine pass/fail
    result.score = compute_score(
        result.adf_pvalue.unwrap_or(1.0),
        result.half_life.unwrap_or(0.0),
        result.beta_cv.unwrap_or(1.0),
        result.beta_r_squared.unwrap_or(0.0),
        result.cusum_break,
    );

    // Pass criteria: cointegrated + valid half-life + stable beta
    result.passed = result.is_cointegrated
        && result.half_life_valid
        && result.beta_stable
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
    use crate::types::PairCandidate;
    use tempfile::TempDir;

    /// Generate a cointegrated pair: a = beta * b + stationary_noise
    fn cointegrated_pair(n: usize, beta: f64, half_life: f64, seed: u64) -> (PriceData, PriceData) {
        let phi = (-f64::ln(2.0) / half_life).exp();
        let mut state = seed;
        let mut next_noise = |scale: f64| -> f64 {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((state >> 33) as f64 / u32::MAX as f64 - 0.5) * scale
        };

        let mut log_b = Vec::with_capacity(n);
        let mut b_val = 4.0; // ln(~55)
        for _ in 0..n {
            b_val += next_noise(0.02);
            log_b.push(b_val);
        }

        let mut spread = 0.0;
        let mut log_a = Vec::with_capacity(n);
        for i in 0..n {
            spread = phi * spread + next_noise(0.01);
            log_a.push(beta * log_b[i] + 1.0 + spread); // alpha = 1.0
        }

        let prices_a: Vec<f64> = log_a.iter().map(|x| x.exp()).collect();
        let prices_b: Vec<f64> = log_b.iter().map(|x| x.exp()).collect();
        (prices_a, prices_b)
    }

    /// Generate two independent random walks (not cointegrated).
    fn independent_walks(n: usize, seed: u64) -> (PriceData, PriceData) {
        let mut state = seed;
        let mut next_noise = || -> f64 {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((state >> 33) as f64 / u32::MAX as f64 - 0.5) * 0.02
        };

        let mut a = Vec::with_capacity(n);
        let mut b = Vec::with_capacity(n);
        let mut va = 4.0;
        let mut vb = 4.0;
        for _ in 0..n {
            va += next_noise();
            vb += next_noise();
            a.push(va.exp());
            b.push(vb.exp());
        }
        (a, b)
    }

    fn make_provider(pairs: Vec<(&str, PriceData)>) -> InMemoryPrices {
        InMemoryPrices {
            data: pairs.into_iter().map(|(s, p)| (s.to_string(), p)).collect(),
        }
    }

    #[test]
    fn test_cointegrated_pair_passes() {
        let (pa, pb) = cointegrated_pair(500, 1.5, 10.0, 42);
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
        let (pa, pb) = independent_walks(500, 42);
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
        let (pa, pb) = cointegrated_pair(500, 1.5, 10.0, 42);
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

        let (pa, pb) = cointegrated_pair(500, 1.5, 10.0, 42);
        let (px, py) = independent_walks(500, 99);
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
}
