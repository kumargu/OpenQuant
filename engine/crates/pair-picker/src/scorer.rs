//! Pair scoring — combines statistical test results into a composite score.
//!
//! Score components (all normalized to [0, 1]):
//! - Cointegration strength: lower ADF p-value → higher score
//! - Half-life quality: closer to ideal range (5-15 days) → higher score
//! - Beta stability: lower CV → higher score
//! - R² of hedge ratio: higher → higher score

/// Configuration for HL-adaptive max hold time.
///
/// OU theory: after `k` half-lives, the expected reversion is `1 - 2^{-k}`.
/// Setting `max_hold = multiplier * half_life` targets a fixed fraction of
/// expected reversion before the timeout.  The cap prevents excessive hold
/// times for slow pairs.
///
/// Default: 2.5× half-life, capped at 10 days → ~82% expected reversion.
///
/// Reference: Ornstein-Uhlenbeck process theory; see also Krauss (2017),
/// "Statistical Arbitrage Pairs Trading Strategies: Review and Outlook".
#[derive(Debug, Clone)]
pub struct MaxHoldConfig {
    /// Multiplier applied to the OU half-life to derive max hold duration.
    /// Default: 2.5 (targets ~82% reversion: 1 - 2^{-2.5} ≈ 0.82).
    pub hold_multiplier: f64,
    /// Hard cap on max hold in days regardless of half-life.
    /// Default: 10 days.
    pub max_hold_cap: usize,
}

impl Default for MaxHoldConfig {
    fn default() -> Self {
        Self {
            hold_multiplier: 2.5,
            max_hold_cap: 10,
        }
    }
}

/// Compute per-pair max hold days from OU half-life.
///
/// Formula: `min(ceil(hold_multiplier * half_life), max_hold_cap)`
///
/// # Panics / edge cases
/// - `half_life` must be finite and > 0; returns `max_hold_cap` otherwise
///   (guards against NaN/Inf propagating into the trading engine).
/// - Fractional results are rounded **up** (`ceil`) so the pair always gets
///   at least the full expected reversion window.
///
/// # Examples
/// ```
/// use pair_picker::scorer::{MaxHoldConfig, compute_max_hold_days};
/// let cfg = MaxHoldConfig::default(); // multiplier=2.5, cap=10
/// assert_eq!(compute_max_hold_days(2.0, &cfg), 5);  // 2.5*2.0=5.0 → 5
/// assert_eq!(compute_max_hold_days(5.0, &cfg), 10); // 2.5*5.0=12.5 → capped at 10
/// ```
pub fn compute_max_hold_days(half_life: f64, config: &MaxHoldConfig) -> usize {
    if !half_life.is_finite() || half_life <= 0.0 {
        // Defensive: bad half-life → use the cap (safe upper bound)
        return config.max_hold_cap;
    }
    let raw = config.hold_multiplier * half_life;
    let days = raw.ceil() as usize;
    days.min(config.max_hold_cap)
}

/// Configurable scoring weights. Will be superseded by Thompson sampling (#119),
/// but kept configurable so they can be tuned via CLI or config file in the meantime.
#[derive(Debug, Clone)]
pub struct ScoringConfig {
    pub w_cointegration: f64,
    pub w_half_life: f64,
    pub w_stability: f64,
    pub w_fit: f64,
}

impl Default for ScoringConfig {
    fn default() -> Self {
        Self {
            w_cointegration: 0.35,
            w_half_life: 0.25,
            w_stability: 0.25,
            w_fit: 0.15,
        }
    }
}

/// Compute composite score from validation results using default weights.
///
/// Returns a score in [0, 1] where higher = better pair.
pub fn compute_score(
    adf_pvalue: f64,
    half_life: f64,
    beta_cv: f64,
    r_squared: f64,
    structural_break: bool,
) -> f64 {
    compute_score_with_config(
        adf_pvalue,
        half_life,
        beta_cv,
        r_squared,
        structural_break,
        &ScoringConfig::default(),
    )
}

/// Compute composite score with custom weights.
pub fn compute_score_with_config(
    adf_pvalue: f64,
    half_life: f64,
    beta_cv: f64,
    r_squared: f64,
    structural_break: bool,
    config: &ScoringConfig,
) -> f64 {
    let coint_score = cointegration_score(adf_pvalue);
    let hl_score = half_life_score(half_life);
    let stability_score = beta_stability_score(beta_cv, structural_break);
    let fit_score = if r_squared.is_finite() {
        r_squared.clamp(0.0, 1.0)
    } else {
        0.0
    };

    let raw = config.w_cointegration * coint_score
        + config.w_half_life * hl_score
        + config.w_stability * stability_score
        + config.w_fit * fit_score;

    raw.clamp(0.0, 1.0)
}

/// ADF p-value → cointegration score.
/// p < 0.01 → 1.0, p = 0.05 → 0.5, p > 0.10 → 0.0
fn cointegration_score(p_value: f64) -> f64 {
    if p_value <= 0.01 {
        1.0
    } else if p_value >= 0.10 {
        0.0
    } else {
        // Linear interpolation: [0.01, 0.10] → [1.0, 0.0]
        1.0 - (p_value - 0.01) / (0.10 - 0.01)
    }
}

/// Half-life → quality score.
/// Ideal range: 2-15 days (fast enough to trade intraday, slow enough to execute).
/// Valid range: 1-40 days (lowered from 3 to admit fast mean-reverting pairs).
fn half_life_score(hl: f64) -> f64 {
    if !(1.0..=40.0).contains(&hl) {
        return 0.0;
    }
    if (2.0..=15.0).contains(&hl) {
        return 1.0; // ideal
    }
    if hl < 2.0 {
        // 1-2: ramp up (very fast reversion — may be noise)
        hl - 1.0
    } else {
        // 15-40: ramp down
        1.0 - (hl - 15.0) / 25.0
    }
}

/// Beta stability → score.
/// CV < 0.05 → 1.0, CV = 0.20 → 0.0 (at threshold)
fn beta_stability_score(cv: f64, structural_break: bool) -> f64 {
    if structural_break {
        return 0.0;
    }
    if cv <= 0.05 {
        1.0
    } else if cv >= 0.20 {
        0.0
    } else {
        // Linear: [0.05, 0.20] → [1.0, 0.0]
        1.0 - (cv - 0.05) / (0.20 - 0.05)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_excellent_pair() {
        let score = compute_score(0.001, 8.0, 0.03, 0.95, false);
        assert!(score > 0.85, "score={score}");
    }

    #[test]
    fn test_mediocre_pair() {
        let score = compute_score(0.04, 25.0, 0.15, 0.70, false);
        assert!(score > 0.2 && score < 0.6, "score={score}");
    }

    #[test]
    fn test_structural_break_penalizes() {
        let no_break = compute_score(0.01, 10.0, 0.10, 0.90, false);
        let with_break = compute_score(0.01, 10.0, 0.10, 0.90, true);
        assert!(
            with_break < no_break,
            "no_break={no_break}, with_break={with_break}"
        );
    }

    #[test]
    fn test_cointegration_score() {
        assert!((cointegration_score(0.001) - 1.0).abs() < 0.01);
        assert!((cointegration_score(0.05) - 0.556).abs() < 0.01);
        assert!((cointegration_score(0.15) - 0.0).abs() < 0.01);
    }

    #[test]
    fn test_half_life_score() {
        assert_eq!(half_life_score(0.5), 0.0); // below MIN_HALF_LIFE
        assert!(half_life_score(1.5) > 0.0 && half_life_score(1.5) < 1.0); // ramp-up
        assert_eq!(half_life_score(2.0), 1.0); // ideal range
        assert_eq!(half_life_score(10.0), 1.0); // ideal range
        assert!(half_life_score(30.0) > 0.0 && half_life_score(30.0) < 1.0); // ramp-down
        assert_eq!(half_life_score(50.0), 0.0); // above MAX_HALF_LIFE
    }

    #[test]
    fn test_score_range() {
        // Score should always be in [0, 1]
        for p in [0.001, 0.01, 0.05, 0.10, 0.50] {
            for hl in [3.0, 5.0, 10.0, 20.0, 40.0] {
                for cv in [0.01, 0.10, 0.20, 0.50] {
                    for r2 in [0.5, 0.8, 0.95] {
                        let s = compute_score(p, hl, cv, r2, false);
                        assert!(
                            s >= 0.0 && s <= 1.0,
                            "p={p}, hl={hl}, cv={cv}, r2={r2}, s={s}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn test_custom_config() {
        let config = ScoringConfig {
            w_cointegration: 1.0,
            w_half_life: 0.0,
            w_stability: 0.0,
            w_fit: 0.0,
        };
        let score = compute_score_with_config(0.001, 50.0, 1.0, 0.0, false, &config);
        // Only cointegration matters, and p=0.001 → 1.0
        assert!((score - 1.0).abs() < 0.01, "score={score}");
    }

    #[test]
    fn test_nan_r_squared() {
        let score = compute_score(0.01, 10.0, 0.05, f64::NAN, false);
        assert!(score.is_finite(), "NaN R² should not produce NaN score");
    }

    // -----------------------------------------------------------------------
    // MaxHoldConfig / compute_max_hold_days tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_max_hold_hl2_gives_5() {
        // HL=2d → 2.5*2=5.0 → ceil(5.0)=5 (no cap)
        let cfg = MaxHoldConfig::default();
        assert_eq!(compute_max_hold_days(2.0, &cfg), 5);
    }

    #[test]
    fn test_max_hold_hl5_capped_at_10() {
        // HL=5d → 2.5*5=12.5 → ceil=13, capped at 10
        let cfg = MaxHoldConfig::default();
        assert_eq!(compute_max_hold_days(5.0, &cfg), 10);
    }

    #[test]
    fn test_max_hold_hl3_rounds_up() {
        // HL=3d → 2.5*3=7.5 → ceil(7.5)=8
        let cfg = MaxHoldConfig::default();
        assert_eq!(compute_max_hold_days(3.0, &cfg), 8);
    }

    #[test]
    fn test_max_hold_exactly_at_cap() {
        // HL=4d → 2.5*4=10.0 → ceil=10, cap=10 → exactly 10
        let cfg = MaxHoldConfig::default();
        assert_eq!(compute_max_hold_days(4.0, &cfg), 10);
    }

    #[test]
    fn test_max_hold_nan_halflife_uses_cap() {
        let cfg = MaxHoldConfig::default();
        assert_eq!(compute_max_hold_days(f64::NAN, &cfg), 10);
    }

    #[test]
    fn test_max_hold_inf_halflife_uses_cap() {
        let cfg = MaxHoldConfig::default();
        assert_eq!(compute_max_hold_days(f64::INFINITY, &cfg), 10);
    }

    #[test]
    fn test_max_hold_zero_halflife_uses_cap() {
        let cfg = MaxHoldConfig::default();
        assert_eq!(compute_max_hold_days(0.0, &cfg), 10);
    }

    #[test]
    fn test_max_hold_negative_halflife_uses_cap() {
        let cfg = MaxHoldConfig::default();
        assert_eq!(compute_max_hold_days(-1.0, &cfg), 10);
    }

    #[test]
    fn test_max_hold_custom_config() {
        // multiplier=3.0, cap=15
        let cfg = MaxHoldConfig {
            hold_multiplier: 3.0,
            max_hold_cap: 15,
        };
        // HL=4d → 3*4=12 → ceil=12 (under cap)
        assert_eq!(compute_max_hold_days(4.0, &cfg), 12);
        // HL=6d → 3*6=18 → capped at 15
        assert_eq!(compute_max_hold_days(6.0, &cfg), 15);
    }
}
