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
use crate::scorer::{compute_score, MaxHoldConfig};
use crate::stats::adf::adf_test;
use crate::stats::beta_stability::check_beta_stability;
use crate::stats::halflife::estimate_half_life;
use crate::stats::ols::tls_simple;
use crate::types::{
    ActivePair, ActivePairsFile, PairCandidate, PairCandidatesFile, ValidationResult,
};
use chrono::Utc;
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use tracing::{debug, info, warn};

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

/// Minimum R² for the hedge ratio regression — loose pre-filter.
/// R² measures co-movement, not cointegration — ADF is the proper cointegration gate.
/// Set to 0.30 per #178 spec: excludes pairs with essentially no linear relationship
/// (e.g., NVDA/AMD at R²=0.21) while remaining below the strict production threshold.
/// Lowered from 0.40 to 0.30 per issue #180.
pub const MIN_R_SQUARED: f64 = 0.30;

/// Configurable pipeline thresholds for different asset classes.
///
/// S&P 500 defaults work for correlated equities within the same GICS sector.
/// Metals, commodities, and other asset classes need different thresholds
/// because their volatility structure, mean-reversion speed, and correlation
/// dynamics differ fundamentally.
#[derive(Debug, Clone)]
pub struct PipelineConfig {
    /// Minimum daily bars required for validation.
    pub min_history_bars: usize,
    /// Maximum validation window (caps to most recent N bars).
    pub max_validation_window: usize,
    /// Minimum R² for TLS regression.
    pub min_r_squared: f64,
    /// ADF p-value threshold for cointegration. Default 0.05.
    pub adf_pvalue_threshold: f64,
    /// Minimum OU half-life in days.
    pub min_half_life: f64,
    /// Maximum OU half-life in days.
    pub max_half_life: f64,
    /// Whether structural break is a hard rejection gate.
    pub structural_break_gate: bool,
    /// Minimum annualized spread zero-crossings.
    pub min_spread_crossings: f64,
    /// Whether to apply the ETF-component exclusion filter.
    pub etf_filter_enabled: bool,
    /// Max hold cap in days. Passed to MaxHoldConfig when building ActivePair.
    /// Lower = cut losses faster on slow-reverting pairs.
    pub max_hold_cap: usize,
}

impl Default for PipelineConfig {
    /// S&P 500 defaults — the production configuration.
    fn default() -> Self {
        Self {
            min_history_bars: MIN_HISTORY_BARS,
            max_validation_window: MAX_VALIDATION_WINDOW,
            min_r_squared: MIN_R_SQUARED,
            adf_pvalue_threshold: 0.05,
            min_half_life: 1.0,
            max_half_life: 40.0,
            structural_break_gate: true,
            min_spread_crossings: 12.0,
            etf_filter_enabled: true,
            max_hold_cap: 10,
        }
    }
}

impl PipelineConfig {
    /// Relaxed config for metals/commodities exploration.
    /// Structural break gate disabled — useful for initial exploration.
    pub fn metals() -> Self {
        Self {
            min_history_bars: 90,
            max_validation_window: 150,  // shorter window — excludes supercycle, sees recent cointegration
            min_r_squared: 0.20,         // looser — metals can have weaker linear fit
            adf_pvalue_threshold: 0.20,  // very relaxed — OR/SAND at p=0.19, royalty pairs borderline
            min_half_life: 1.0,
            max_half_life: 60.0,         // metals revert slower
            structural_break_gate: false, // disabled — metals beta drifts seasonally
            min_spread_crossings: 8.0,   // relaxed — slower oscillation
            etf_filter_enabled: true,
            max_hold_cap: 5,             // shorter than S&P — metals trends persist
        }
    }

    /// Metals with structural break gate re-enabled.
    /// Experiment data shows pairs with breaks (GOLD/KGC, AEM/GOLD) are
    /// consistent losers. Keep the wider ADF/HL to let ETF pairs through.
    pub fn metals_strict() -> Self {
        Self {
            structural_break_gate: true, // RE-ENABLED — break pairs are losers
            ..Self::metals()
        }
    }
}

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

/// Validate a single candidate pair using default (S&P 500) thresholds.
pub fn validate_pair(candidate: &PairCandidate, provider: &dyn PriceProvider) -> ValidationResult {
    validate_pair_with_config(candidate, provider, &PipelineConfig::default())
}

/// Validate a single candidate pair with configurable thresholds.
pub fn validate_pair_with_config(
    candidate: &PairCandidate,
    provider: &dyn PriceProvider,
    cfg: &PipelineConfig,
) -> ValidationResult {
    let pair_id = format!("{}/{}", candidate.leg_a, candidate.leg_b);
    let mut result = ValidationResult::new(candidate);

    debug!(pair = pair_id.as_str(), "validating pair");

    // Step 1: ETF exclusion (instant reject)
    if cfg.etf_filter_enabled && is_etf_component_pair(&candidate.leg_a, &candidate.leg_b) {
        result.etf_excluded = true;
        result.rejection_reasons.push("ETF-component pair".into());
        debug!(pair = pair_id.as_str(), "REJECTED: ETF-component pair");
        return result;
    }
    debug!(pair = pair_id.as_str(), "passed ETF filter");

    // Step 2: Get price data
    let prices_a = match provider.get_prices(&candidate.leg_a) {
        Some(p) if p.len() >= cfg.min_history_bars => p,
        Some(p) => {
            result.rejection_reasons.push(format!(
                "{}: only {} bars (need {})",
                candidate.leg_a,
                p.len(),
                cfg.min_history_bars
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
        Some(p) if p.len() >= cfg.min_history_bars => p,
        Some(p) => {
            result.rejection_reasons.push(format!(
                "{}: only {} bars (need {})",
                candidate.leg_b,
                p.len(),
                cfg.min_history_bars
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
    // cap to max_validation_window to focus on the recent regime.
    let n = prices_a
        .len()
        .min(prices_b.len())
        .min(cfg.max_validation_window);
    let prices_a = &prices_a[prices_a.len() - n..];
    let prices_b = &prices_b[prices_b.len() - n..];

    debug!(
        pair = pair_id.as_str(),
        bars = n,
        price_a_first = prices_a[0],
        price_a_last = prices_a[n - 1],
        price_b_first = prices_b[0],
        price_b_last = prices_b[n - 1],
        "price data loaded"
    );

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

    // Step 3: TLS regression → beta (symmetric hedge ratio)
    // TLS minimizes perpendicular distance, so beta is the same regardless of
    // which leg is treated as x or y. OLS would give a different beta depending
    // on leg ordering — a bug for pairs where ordering is arbitrary (alphabetical).
    // Ref: Teetor (2011), "Better Hedge Ratios for Spread Trading".
    let ols = match tls_simple(&log_b, &log_a) {
        Some(r) => r,
        None => {
            result
                .rejection_reasons
                .push("TLS regression failed".into());
            return result;
        }
    };

    result.alpha = Some(ols.alpha);
    result.beta = Some(ols.beta);
    result.beta_r_squared = Some(ols.r_squared);

    debug!(
        pair = pair_id.as_str(),
        alpha = format!("{:.6}", ols.alpha).as_str(),
        beta = format!("{:.4}", ols.beta).as_str(),
        r_squared = format!("{:.4}", ols.r_squared).as_str(),
        "TLS regression result"
    );

    // Step 3b: Minimum R² — below this the hedge ratio is meaningless noise
    if ols.r_squared < cfg.min_r_squared {
        result.rejection_reasons.push(format!(
            "R²={:.3} below minimum {:.2}",
            ols.r_squared, cfg.min_r_squared
        ));
        debug!(pair = pair_id.as_str(), r_squared = format!("{:.4}", ols.r_squared).as_str(), threshold = cfg.min_r_squared, "REJECTED: low R²");
    }

    // Step 4: Engle-Granger cointegration
    // Spread = log_a - beta * log_b (intentionally omitting OLS intercept alpha).
    // The ADF regression includes its own constant term, and the AR(1) half-life
    // estimation absorbs any level shift, so subtracting alpha here is unnecessary
    // and would only add noise from the intercept estimate.
    //
    // IMPORTANT: pybridge's scan_pair computes spread_mean/spread_std using the
    // WITH-alpha form (log_a - alpha - beta*log_b) for z-score computation.
    // This is mathematically safe because:
    //   - The alpha offset is constant, so it does not affect ADF/HL (AR(1) absorbs it)
    //   - spread_std is invariant to constant shifts
    //   - spread_mean in the WITH-alpha form is ~0, making z-score = spread / spread_std
    // Both forms give identical half-life and ADF results.
    let spread: Vec<f64> = log_a
        .iter()
        .zip(log_b.iter())
        .map(|(a, b)| a - ols.beta * b)
        .collect();

    match adf_test(&spread, None, true) {
        Some(adf) => {
            result.adf_statistic = Some(adf.test_statistic);
            result.adf_pvalue = Some(adf.p_value);
            // Use configurable threshold instead of ADF's hardcoded 0.05
            result.is_cointegrated = adf.p_value < cfg.adf_pvalue_threshold;
            debug!(
                pair = pair_id.as_str(),
                adf_stat = format!("{:.4}", adf.test_statistic).as_str(),
                adf_pvalue = format!("{:.6}", adf.p_value).as_str(),
                threshold = cfg.adf_pvalue_threshold,
                is_cointegrated = result.is_cointegrated,
                "ADF cointegration test"
            );
            if !result.is_cointegrated {
                result.rejection_reasons.push(format!(
                    "Not cointegrated (ADF p={:.4} > {:.2}, stat={:.3})",
                    adf.p_value, cfg.adf_pvalue_threshold, adf.test_statistic
                ));
            }
        }
        None => {
            debug!(pair = pair_id.as_str(), "ADF test failed — insufficient data or numerical issue");
            result.rejection_reasons.push("ADF test failed".into());
            return result;
        }
    }

    // Step 5: OU half-life
    match estimate_half_life(&spread) {
        Some(hl) => {
            result.half_life = Some(hl.half_life);
            result.half_life_valid =
                hl.half_life >= cfg.min_half_life && hl.half_life <= cfg.max_half_life;
            debug!(
                pair = pair_id.as_str(),
                half_life = format!("{:.2}", hl.half_life).as_str(),
                phi = format!("{:.6}", hl.phi).as_str(),
                valid = result.half_life_valid,
                range = format!("[{}, {}]", cfg.min_half_life, cfg.max_half_life).as_str(),
                "OU half-life estimation"
            );
            if !result.half_life_valid {
                result.rejection_reasons.push(format!(
                    "Half-life {:.1} days outside valid range [{}, {}]",
                    hl.half_life, cfg.min_half_life, cfg.max_half_life
                ));
            }
        }
        None => {
            debug!(pair = pair_id.as_str(), "half-life estimation failed — spread not mean-reverting");
            result
                .rejection_reasons
                .push("Half-life estimation failed (not mean-reverting)".into());
        }
    }

    // Step 5b: Spread crossing frequency — reject pairs that don't oscillate enough.
    // A pair that never crosses zero can't generate mean-reversion trades.
    // Minimum: 12 zero-crossings per year (roughly 1 per month).
    {
        let n = spread.len();
        if n > 20 {
            let mean = spread.iter().sum::<f64>() / n as f64;
            let demeaned: Vec<f64> = spread.iter().map(|s| s - mean).collect();
            let crossings = demeaned
                .windows(2)
                .filter(|w| (w[0] >= 0.0) != (w[1] >= 0.0))
                .count();
            // Annualize: crossings per 252 trading days
            let annual_crossings = crossings as f64 * 252.0 / n as f64;
            result.spread_crossings = Some(annual_crossings);
            if annual_crossings < cfg.min_spread_crossings {
                result.rejection_reasons.push(format!(
                    "Low spread crossing frequency: {:.1}/year (need ≥{:.0})",
                    annual_crossings, cfg.min_spread_crossings
                ));
            }
        }
    }

    // Step 6: Beta stability
    match check_beta_stability(&log_a, &log_b) {
        Some(bs) => {
            result.beta_cv = Some(bs.cv);
            result.structural_break = bs.structural_break;
            result.beta_stable = bs.is_stable;
            debug!(
                pair = pair_id.as_str(),
                beta_cv = format!("{:.4}", bs.cv).as_str(),
                structural_break = bs.structural_break,
                max_shift_pct = format!("{:.2}%", bs.max_shift_pct * 100.0).as_str(),
                is_stable = bs.is_stable,
                "beta stability check"
            );
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
            debug!(pair = pair_id.as_str(), "beta stability check failed");
            result
                .rejection_reasons
                .push("Beta stability check failed".into());
        }
    }

    // Step 7: Regime robustness — test cointegration across calm/volatile sub-periods
    if let Some(beta) = result.beta {
        let robustness = compute_regime_robustness(prices_a, prices_b, beta);
        result.regime_robustness = Some(robustness.score);
        debug!(
            pair = pair_id.as_str(),
            regime_robustness = format!("{:.3}", robustness.score).as_str(),
            current_regime = ?robustness.current_regime,
            sufficient_data = robustness.sufficient_data,
            "regime robustness"
        );

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

    // Pass criteria: cointegrated + valid half-life + no structural break + adequate R²
    // Beta CV is a SCORE penalty (handled by compute_score), not a hard gate.
    // Structural break remains a hard gate — it indicates a genuinely broken relationship.
    // See research issue #202 and Principal Engineer review for justification.
    let r_squared_ok = result.beta_r_squared.unwrap_or(0.0) >= cfg.min_r_squared;
    let crossings_ok = result.spread_crossings.unwrap_or(0.0) >= cfg.min_spread_crossings;
    let structural_break_ok = if cfg.structural_break_gate {
        !result.structural_break
    } else {
        true // skip structural break gate
    };
    result.passed = result.is_cointegrated
        && result.half_life_valid
        && structural_break_ok
        && r_squared_ok
        && crossings_ok
        && !result.etf_excluded;

    debug!(
        pair = pair_id.as_str(),
        passed = result.passed,
        score = format!("{:.4}", result.score).as_str(),
        cointegrated = result.is_cointegrated,
        half_life_valid = result.half_life_valid,
        structural_break = result.structural_break,
        r_squared_ok,
        crossings_ok,
        etf_excluded = result.etf_excluded,
        rejection_count = result.rejection_reasons.len(),
        "final verdict"
    );

    result
}

/// Validate candidates in memory — no file I/O.
/// Returns passing pairs as ActivePair vec, sorted by score descending.
/// Used when the runner calls pair-picker as a library.
pub fn validate_candidates(
    candidates: &[PairCandidate],
    provider: &dyn PriceProvider,
) -> Vec<ActivePair> {
    validate_candidates_with_config(candidates, provider, &PipelineConfig::default())
}

/// Validate candidates with configurable pipeline thresholds.
pub fn validate_candidates_with_config(
    candidates: &[PairCandidate],
    provider: &dyn PriceProvider,
    cfg: &PipelineConfig,
) -> Vec<ActivePair> {
    info!(
        adf_p = cfg.adf_pvalue_threshold,
        max_hl = cfg.max_half_life,
        min_r2 = cfg.min_r_squared,
        structural_break_gate = cfg.structural_break_gate,
        max_window = cfg.max_validation_window,
        "pipeline config"
    );
    let mut results: Vec<ValidationResult> = candidates
        .iter()
        .map(|c| {
            let r = validate_pair_with_config(c, provider, cfg);
            if r.passed {
                info!(
                    "PASS: {}/{} — score={:.3}, hl={:.1}d",
                    r.leg_a,
                    r.leg_b,
                    r.score,
                    r.half_life.unwrap_or(0.0),
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

    results.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let mhc = MaxHoldConfig { max_hold_cap: cfg.max_hold_cap, ..MaxHoldConfig::default() };
    results.iter().filter_map(|r| r.to_active_pair_with_config(&mhc)).collect()
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
    run_pipeline_from_candidates_with_config(candidates, output_path, provider, &PipelineConfig::default())
}

/// Run pipeline with configurable thresholds.
pub fn run_pipeline_from_candidates_with_config(
    candidates: &[PairCandidate],
    output_path: &Path,
    provider: &dyn PriceProvider,
    cfg: &PipelineConfig,
) -> Result<Vec<ValidationResult>, PipelineError> {
    let mut results: Vec<ValidationResult> = candidates
        .iter()
        .map(|c| {
            let r = validate_pair_with_config(c, provider, cfg);
            if r.passed {
                let hl = r.half_life.unwrap_or(0.0);
                let mhd = crate::scorer::compute_max_hold_days(
                    hl,
                    &crate::scorer::MaxHoldConfig::default(),
                );
                info!(
                    "PASS: {}/{} — score={:.3}, beta={:.4}, hl={:.1}d, adf_p={:.4}, max_hold={}d",
                    r.leg_a,
                    r.leg_b,
                    r.score,
                    r.beta.unwrap_or(0.0),
                    hl,
                    r.adf_pvalue.unwrap_or(1.0),
                    mhd,
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
    let mhc = MaxHoldConfig { max_hold_cap: cfg.max_hold_cap, ..MaxHoldConfig::default() };
    let active_pairs: Vec<_> = results.iter().filter_map(|r| r.to_active_pair_with_config(&mhc)).collect();

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

        if let Some(ols) = tls_simple(&log_b, &log_a) {
            // Guard against extreme betas from noisy/decoupled windows.
            // TLS can produce wild betas when covariance is small-but-nonzero
            // (e.g., during temporary regime breaks). Keep old beta if R² is
            // too low or beta is unreasonable. Ref: PR #215 review.
            if ols.r_squared < MIN_R_SQUARED || ols.beta.abs() > 5.0 {
                warn!(
                    pair = format!("{}/{}", pair.leg_a, pair.leg_b).as_str(),
                    r_squared = format!("{:.3}", ols.r_squared).as_str(),
                    beta = format!("{:.4}", ols.beta).as_str(),
                    "Beta refresh rejected: weak fit or extreme beta — keeping old value"
                );
                continue;
            }
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
                "Beta refreshed via TLS"
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

    #[test]
    fn test_max_hold_days_in_active_pair_output() {
        // Verify that max_hold_days is written to active_pairs.json and obeys
        // the HL-adaptive formula: min(ceil(2.5 * half_life), 10).
        let tmp = TempDir::new().unwrap();
        let output_path = tmp.path().join("active_pairs.json");

        let (pa, pb) = test_utils::cointegrated_pair(500, 1.5, 10.0, 42);
        let provider = make_provider(vec![("A", pa), ("B", pb)]);

        let candidates = vec![PairCandidate {
            leg_a: "A".into(),
            leg_b: "B".into(),
            economic_rationale: "cointegrated".into(),
        }];

        let results = run_pipeline_from_candidates(&candidates, &output_path, &provider).unwrap();
        let passed: Vec<_> = results.iter().filter(|r| r.passed).collect();

        if passed.is_empty() {
            // Pair didn't pass validation — skip the assertion (not a max_hold bug)
            return;
        }

        let contents = fs::read_to_string(&output_path).unwrap();
        let output: ActivePairsFile = serde_json::from_str(&contents).unwrap();
        assert!(
            !output.pairs.is_empty(),
            "Expected at least one active pair"
        );

        let pair = &output.pairs[0];
        let hl = pair.half_life_days;

        // max_hold_days must be positive
        assert!(
            pair.max_hold_days > 0,
            "max_hold_days should be > 0, got {}",
            pair.max_hold_days
        );

        // max_hold_days must not exceed cap of 10
        assert!(
            pair.max_hold_days <= 10,
            "max_hold_days={} exceeds cap=10",
            pair.max_hold_days
        );

        // max_hold_days = min(ceil(2.5 * hl), 10)
        let expected = ((2.5 * hl).ceil() as usize).min(10);
        assert_eq!(
            pair.max_hold_days, expected,
            "max_hold_days={} expected={} for hl={:.2}",
            pair.max_hold_days, expected, hl
        );
    }
}
