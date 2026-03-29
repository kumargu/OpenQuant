//! Beta (hedge ratio) stability analysis.
//!
//! Two complementary checks:
//! 1. **Rolling window**: compute beta over rolling 60-day windows, check std/mean < 0.20
//! 2. **Structural break detection**: max mean-shift test on rolling betas

use super::ols::tls_simple;

/// Result of beta stability analysis.
#[derive(Debug, Clone)]
pub struct BetaStabilityResult {
    /// Current beta (from full-sample OLS).
    pub beta: f64,
    /// Mean of rolling betas.
    pub rolling_mean: f64,
    /// Std of rolling betas.
    pub rolling_std: f64,
    /// Coefficient of variation: std / |mean|.
    pub cv: f64,
    /// Whether beta is stable (cv < threshold).
    pub is_stable: bool,
    /// Whether a structural break was detected in the hedge ratio.
    pub structural_break: bool,
    /// Max mean-shift percentage (for logging/calibration).
    pub max_shift_pct: f64,
    /// Number of rolling windows computed.
    pub n_windows: usize,
}

/// Maximum acceptable coefficient of variation for beta.
/// Raised from 0.20 to 0.35: the 0.20 threshold was calibrated for 60-bar windows
/// but ROLLING_WINDOW was halved to 30 without recalibrating CV. With 30-bar OLS,
/// estimation noise inflates CV by ~sqrt(2), so 0.20 was below the noise floor.
/// 0.35 allows moderately stable pairs through while still catching high-CV garbage.
/// Beta CV also feeds into compute_score() as a continuous penalty — it does not need
/// to be a strict hard gate. See research issue #202.
pub const MAX_BETA_CV: f64 = 0.35;

/// Rolling window size for beta estimation.
/// Lowered from 60 to 30 to work with 90-day validation windows.
/// 30 bars gives ~60 rolling windows for CV estimation, sufficient
/// for detecting structural breaks while fitting within shorter data.
pub const ROLLING_WINDOW: usize = 30;

/// Check beta stability using rolling windows and structural break detection.
///
/// `log_prices_a` and `log_prices_b` are log-prices of the two legs.
pub fn check_beta_stability(
    log_prices_a: &[f64],
    log_prices_b: &[f64],
) -> Option<BetaStabilityResult> {
    let n = log_prices_a.len().min(log_prices_b.len());
    if n < ROLLING_WINDOW + 10 {
        return None;
    }

    // Full-sample beta (TLS for symmetric hedge ratio)
    let full_ols = tls_simple(log_prices_b, log_prices_a)?;
    let beta = full_ols.beta;

    // Rolling betas
    let n_windows = n - ROLLING_WINDOW + 1;
    let mut rolling_betas = Vec::with_capacity(n_windows);

    for start in 0..n_windows {
        let end = start + ROLLING_WINDOW;
        let x = &log_prices_b[start..end];
        let y = &log_prices_a[start..end];
        if let Some(result) = tls_simple(x, y) {
            rolling_betas.push(result.beta);
        }
    }

    if rolling_betas.is_empty() {
        return None;
    }

    let rb_n = rolling_betas.len() as f64;
    let rolling_mean: f64 = rolling_betas.iter().sum::<f64>() / rb_n;
    let rolling_std = {
        let var = rolling_betas
            .iter()
            .map(|b| (b - rolling_mean).powi(2))
            .sum::<f64>()
            / (rb_n - 1.0); // sample variance (Bessel's correction)
        var.sqrt()
    };

    let cv = if rolling_mean.abs() > 1e-10 {
        rolling_std / rolling_mean.abs()
    } else {
        f64::INFINITY
    };

    let shift = max_mean_shift(&rolling_betas);
    let threshold = structural_break_threshold();
    let structural_break = shift > threshold;

    let is_stable = cv < MAX_BETA_CV && !structural_break;

    Some(BetaStabilityResult {
        beta,
        rolling_mean,
        rolling_std,
        cv,
        is_stable,
        structural_break,
        max_shift_pct: shift,
        n_windows: rolling_betas.len(),
    })
}

/// Structural break threshold calibrated by rolling window size.
///
/// Shorter rolling windows produce noisier beta estimates, inflating the
/// apparent max mean-shift. The threshold scales with estimation noise:
/// - 60-bar windows: 15% (original, low noise)
/// - 30-bar windows: 45% (higher noise floor from smaller OLS samples)
///
/// Calibrated on known-good/known-bad pairs (#168):
/// - DUK/SO (stable utility): 41.8% at 30-bar → should PASS
/// - BAC/WFC (stable bank): 42.3% at 30-bar → should PASS
/// - LMT/NOC (unstable): 155.9% → should FAIL
/// - KO/PEP (drifting): 73.0% → should FAIL
pub fn structural_break_threshold() -> f64 {
    // Scale threshold with rolling window noise:
    // At 60 bars: 0.15 (tight). At 30 bars: 0.45 (loose).
    // Linear interpolation: threshold = 0.75 - 0.01 * ROLLING_WINDOW
    (0.75 - 0.01 * ROLLING_WINDOW as f64).clamp(0.15, 0.50)
}

/// Compute max mean-shift percentage across split points.
///
/// Returns the maximum |mean(left) - mean(right)| / |overall_mean|
/// across all candidate split points (20%-80% of series).
fn max_mean_shift(series: &[f64]) -> f64 {
    let n = series.len();
    if n < 20 {
        return 0.0;
    }

    let n_f = n as f64;
    let mean: f64 = series.iter().sum::<f64>() / n_f;

    if mean.abs() < 1e-15 {
        return 0.0;
    }

    // Check max shift across multiple split points (20%-80%)
    let start = (n_f * 0.2) as usize;
    let end = (n_f * 0.8) as usize;
    let total_sum: f64 = series.iter().sum();
    let mut max_shift = 0.0_f64;

    let mut prefix_sum = 0.0;
    for (i, val) in series.iter().enumerate() {
        prefix_sum += val;
        if i >= start && i < end {
            let n1 = (i + 1) as f64;
            let n2 = (n - i - 1) as f64;
            let mean1 = prefix_sum / n1;
            let mean2 = (total_sum - prefix_sum) / n2;
            let shift_pct = ((mean1 - mean2) / mean.abs()).abs();
            max_shift = max_shift.max(shift_pct);
        }
    }

    max_shift
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stable_beta() {
        // Two series with a stable relationship: a = 1.5 * b + noise
        let n = 200;
        let mut log_a = Vec::with_capacity(n);
        let mut log_b = Vec::with_capacity(n);
        let mut state: u64 = 42;
        let mut b_val = 4.0; // ln(~55)

        for _ in 0..n {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let noise_b = ((state >> 33) as f64 / u32::MAX as f64 - 0.5) * 0.01;
            b_val += noise_b;
            log_b.push(b_val);

            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let noise_a = ((state >> 33) as f64 / u32::MAX as f64 - 0.5) * 0.005;
            log_a.push(1.5 * b_val + noise_a);
        }

        let result = check_beta_stability(&log_a, &log_b).unwrap();
        assert!(
            result.is_stable,
            "cv={}, structural_break={}",
            result.cv, result.structural_break
        );
        assert!((result.beta - 1.5).abs() < 0.1, "beta={}", result.beta);
        assert!(result.cv < MAX_BETA_CV, "cv={}", result.cv);
    }

    #[test]
    fn test_structural_break() {
        // Beta changes from 1.5 to 0.5 halfway through
        let n = 200;
        let mut log_a = Vec::with_capacity(n);
        let mut log_b = Vec::with_capacity(n);
        let mut state: u64 = 42;
        let mut b_val = 4.0;

        for i in 0..n {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let noise_b = ((state >> 33) as f64 / u32::MAX as f64 - 0.5) * 0.01;
            b_val += noise_b;
            log_b.push(b_val);

            let beta = if i < n / 2 { 1.5 } else { 0.5 };
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let noise_a = ((state >> 33) as f64 / u32::MAX as f64 - 0.5) * 0.005;
            log_a.push(beta * b_val + noise_a);
        }

        let result = check_beta_stability(&log_a, &log_b).unwrap();
        // Should detect instability via either CV or CUSUM
        assert!(
            !result.is_stable,
            "cv={}, structural_break={}",
            result.cv, result.structural_break
        );
    }

    #[test]
    fn test_too_short() {
        let a = vec![1.0; 35];
        let b = vec![1.0; 35];
        assert!(check_beta_stability(&a, &b).is_none());
    }

    #[test]
    fn test_no_structural_break() {
        // Constant series — zero shift
        let series = vec![1.0; 50];
        assert!(max_mean_shift(&series) < 0.01);
    }

    #[test]
    fn test_structural_break_detected() {
        // Clear level shift — should be very large
        let mut series = vec![1.0; 50];
        series.extend(vec![5.0; 50]);
        let shift = max_mean_shift(&series);
        assert!(shift > 1.0, "expected large shift, got {shift}");
    }

    #[test]
    fn test_threshold_scales_with_window() {
        // At ROLLING_WINDOW=30: threshold = 0.75 - 0.30 = 0.45
        let t = structural_break_threshold();
        assert!(
            (t - 0.45).abs() < 0.01,
            "expected ~0.45 for ROLLING_WINDOW={}, got {t}",
            ROLLING_WINDOW
        );
    }

    #[test]
    fn test_max_shift_pct_exposed() {
        // Verify max_shift_pct is populated in BetaStabilityResult
        let n = 200;
        let mut log_a = Vec::with_capacity(n);
        let mut log_b = Vec::with_capacity(n);
        let mut state: u64 = 42;
        let mut b_val = 4.0;
        for _ in 0..n {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let noise = ((state >> 33) as f64 / u32::MAX as f64 - 0.5) * 0.01;
            b_val += noise;
            log_b.push(b_val);
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let noise_a = ((state >> 33) as f64 / u32::MAX as f64 - 0.5) * 0.005;
            log_a.push(1.5 * b_val + noise_a);
        }
        let result = check_beta_stability(&log_a, &log_b).unwrap();
        assert!(
            result.max_shift_pct >= 0.0,
            "max_shift_pct should be non-negative"
        );
    }
}
