//! Ornstein-Uhlenbeck half-life estimation.
//!
//! Fits AR(1) model to the spread: s_t = phi * s_{t-1} + eps
//! Half-life = -ln(2) / ln(phi)
//!
//! Valid range: 3-40 days (lower bound accounts for execution realism).

use super::ols::ols_simple;

/// OU half-life result.
#[derive(Debug, Clone)]
pub struct HalfLifeResult {
    /// AR(1) coefficient (phi). Must be in (0, 1) for mean-reversion.
    pub phi: f64,
    /// Half-life in bars (days if daily data).
    pub half_life: f64,
    /// R² of the AR(1) regression.
    pub r_squared: f64,
}

/// Valid half-life range (in bars/days).
pub const MIN_HALF_LIFE: f64 = 3.0;
pub const MAX_HALF_LIFE: f64 = 40.0;

/// Estimate OU half-life from a spread series.
///
/// Returns `None` if:
/// - Series too short (< 20)
/// - AR(1) coefficient not in (0, 1) — not mean-reverting
pub fn estimate_half_life(spread: &[f64]) -> Option<HalfLifeResult> {
    let n = spread.len();
    if n < 20 {
        return None;
    }

    // AR(1): s_t = alpha + phi * s_{t-1} + eps
    // Regress s_t on s_{t-1}
    let y: Vec<f64> = spread[1..].to_vec();
    let x: Vec<f64> = spread[..n - 1].to_vec();

    let result = ols_simple(&x, &y)?;
    let phi = result.beta;

    // phi must be in (0, 1) for mean-reversion
    if phi <= 0.0 || phi >= 1.0 {
        return None;
    }

    let half_life = -f64::ln(2.0) / phi.ln();

    Some(HalfLifeResult {
        phi,
        half_life,
        r_squared: result.r_squared,
    })
}

/// Check if half-life is within the valid range for trading.
pub fn is_half_life_valid(hl: f64) -> bool {
    (MIN_HALF_LIFE..=MAX_HALF_LIFE).contains(&hl)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils;

    #[test]
    fn test_half_life_recovery() {
        // Generate OU with half_life = 10, check we recover it approximately
        let series = test_utils::ou_process(1000, 10.0, 0.1, 42);
        let result = estimate_half_life(&series).unwrap();

        assert!(
            (result.half_life - 10.0).abs() < 3.0,
            "expected ~10, got {}",
            result.half_life
        );
        assert!(result.phi > 0.0 && result.phi < 1.0);
    }

    #[test]
    fn test_half_life_short() {
        let series = test_utils::ou_process(1000, 5.0, 0.1, 99);
        let result = estimate_half_life(&series).unwrap();
        assert!(
            (result.half_life - 5.0).abs() < 2.0,
            "expected ~5, got {}",
            result.half_life
        );
    }

    #[test]
    fn test_half_life_long() {
        let series = test_utils::ou_process(2000, 30.0, 0.1, 77);
        let result = estimate_half_life(&series).unwrap();
        assert!(
            (result.half_life - 30.0).abs() < 15.0,
            "expected ~30, got {}",
            result.half_life
        );
    }

    #[test]
    fn test_random_walk_no_half_life() {
        let series = test_utils::random_walk(500, 0.1, 42);
        let result = estimate_half_life(&series);
        // Either None (phi >= 1) or half_life outside valid range
        match result {
            None => {} // expected
            Some(r) => assert!(!is_half_life_valid(r.half_life), "hl={}", r.half_life),
        }
    }

    #[test]
    fn test_too_short() {
        let series = vec![1.0; 10];
        assert!(estimate_half_life(&series).is_none());
    }

    #[test]
    fn test_validity_range() {
        assert!(!is_half_life_valid(2.0));
        assert!(is_half_life_valid(3.0));
        assert!(is_half_life_valid(20.0));
        assert!(is_half_life_valid(40.0));
        assert!(!is_half_life_valid(41.0));
    }
}
