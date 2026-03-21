//! Pair scoring — combines statistical test results into a composite score.
//!
//! Score components (all normalized to [0, 1]):
//! - Cointegration strength: lower ADF p-value → higher score
//! - Half-life quality: closer to ideal range (5-15 days) → higher score
//! - Beta stability: lower CV → higher score
//! - R² of hedge ratio: higher → higher score

/// Compute composite score from validation results.
///
/// Returns a score in [0, 1] where higher = better pair.
pub fn compute_score(
    adf_pvalue: f64,
    half_life: f64,
    beta_cv: f64,
    r_squared: f64,
    cusum_break: bool,
) -> f64 {
    let coint_score = cointegration_score(adf_pvalue);
    let hl_score = half_life_score(half_life);
    let stability_score = beta_stability_score(beta_cv, cusum_break);
    let fit_score = r_squared.clamp(0.0, 1.0);

    // Weighted combination
    const W_COINT: f64 = 0.35;
    const W_HALFLIFE: f64 = 0.25;
    const W_STABILITY: f64 = 0.25;
    const W_FIT: f64 = 0.15;

    let raw = W_COINT * coint_score
        + W_HALFLIFE * hl_score
        + W_STABILITY * stability_score
        + W_FIT * fit_score;

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
/// Ideal range: 5-15 days (fast enough to trade, slow enough to execute).
/// Valid range: 3-40 days.
fn half_life_score(hl: f64) -> f64 {
    if !(3.0..=40.0).contains(&hl) {
        return 0.0;
    }
    if (5.0..=15.0).contains(&hl) {
        return 1.0; // ideal
    }
    if hl < 5.0 {
        // 3-5: ramp up
        (hl - 3.0) / 2.0
    } else {
        // 15-40: ramp down
        1.0 - (hl - 15.0) / 25.0
    }
}

/// Beta stability → score.
/// CV < 0.05 → 1.0, CV = 0.20 → 0.0 (at threshold)
fn beta_stability_score(cv: f64, cusum_break: bool) -> f64 {
    if cusum_break {
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
    fn test_cusum_break_penalizes() {
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
        assert_eq!(half_life_score(2.0), 0.0);
        assert_eq!(half_life_score(10.0), 1.0);
        assert!(half_life_score(4.0) > 0.0 && half_life_score(4.0) < 1.0);
        assert!(half_life_score(30.0) > 0.0 && half_life_score(30.0) < 1.0);
        assert_eq!(half_life_score(50.0), 0.0);
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
}
