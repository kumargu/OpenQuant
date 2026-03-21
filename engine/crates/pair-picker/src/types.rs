//! Data types for pair picker input/output.
//!
//! ## Input: `pair_candidates.json`
//! List of candidate pairs with economic rationale (manually curated or AI-generated).
//!
//! ## Output: `active_pairs.json`
//! Validated pairs with statistical properties, ready for the trading engine.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Input: a candidate pair to validate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairCandidate {
    pub leg_a: String,
    pub leg_b: String,
    #[serde(default)]
    pub economic_rationale: String,
}

/// Input file format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairCandidatesFile {
    pub pairs: Vec<PairCandidate>,
}

/// Output: a validated pair with statistical properties.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivePair {
    pub leg_a: String,
    pub leg_b: String,
    /// OLS intercept: log_a = alpha + beta * log_b.
    /// The trading engine needs alpha for correct spread computation.
    pub alpha: f64,
    /// Hedge ratio from OLS on log-prices.
    pub beta: f64,
    /// OU half-life in days.
    pub half_life_days: f64,
    /// ADF test statistic.
    pub adf_statistic: f64,
    /// ADF p-value (Engle-Granger).
    pub adf_pvalue: f64,
    /// Beta coefficient of variation (rolling 60-day).
    pub beta_cv: f64,
    /// Whether a structural break was detected in beta.
    pub structural_break: bool,
    /// Regime robustness score [0, 1]: 1.0 = cointegrated in both calm and volatile periods.
    pub regime_robustness: f64,
    /// Economic rationale (passed through from candidates).
    pub economic_rationale: String,
    /// Composite score [0, 1] combining all statistical properties.
    pub score: f64,
}

/// Output file format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivePairsFile {
    pub generated_at: DateTime<Utc>,
    pub pairs: Vec<ActivePair>,
}

/// Detailed validation result for a single pair (internal, not serialized).
#[derive(Debug, Clone)]
pub struct ValidationResult {
    pub leg_a: String,
    pub leg_b: String,
    pub economic_rationale: String,

    // OLS
    pub alpha: Option<f64>,
    pub beta: Option<f64>,
    pub beta_r_squared: Option<f64>,

    // Cointegration (Engle-Granger)
    pub adf_statistic: Option<f64>,
    pub adf_pvalue: Option<f64>,
    pub is_cointegrated: bool,

    // Half-life
    pub half_life: Option<f64>,
    pub half_life_valid: bool,

    // Beta stability
    pub beta_cv: Option<f64>,
    pub structural_break: bool,
    pub beta_stable: bool,

    // Regime
    pub regime_robustness: Option<f64>,

    // ETF filter
    pub etf_excluded: bool,

    // Overall
    pub passed: bool,
    pub score: f64,
    pub rejection_reasons: Vec<String>,
}

impl ValidationResult {
    pub fn new(candidate: &PairCandidate) -> Self {
        Self {
            leg_a: candidate.leg_a.clone(),
            leg_b: candidate.leg_b.clone(),
            economic_rationale: candidate.economic_rationale.clone(),
            alpha: None,
            beta: None,
            beta_r_squared: None,
            adf_statistic: None,
            adf_pvalue: None,
            is_cointegrated: false,
            half_life: None,
            half_life_valid: false,
            beta_cv: None,
            structural_break: false,
            beta_stable: false,
            regime_robustness: None,
            etf_excluded: false,
            passed: false,
            score: 0.0,
            rejection_reasons: Vec::new(),
        }
    }

    pub fn to_active_pair(&self) -> Option<ActivePair> {
        if !self.passed {
            return None;
        }
        Some(ActivePair {
            leg_a: self.leg_a.clone(),
            leg_b: self.leg_b.clone(),
            alpha: self.alpha.unwrap_or(0.0),
            beta: self.beta.unwrap_or(0.0),
            half_life_days: self.half_life.unwrap_or(0.0),
            adf_statistic: self.adf_statistic.unwrap_or(0.0),
            adf_pvalue: self.adf_pvalue.unwrap_or(1.0),
            beta_cv: self.beta_cv.unwrap_or(1.0),
            structural_break: self.structural_break,
            regime_robustness: self.regime_robustness.unwrap_or(-1.0),
            economic_rationale: self.economic_rationale.clone(),
            score: self.score,
        })
    }
}
