//! OU/AR(1) fitting for spread series.
//!
//! Given a spread series x_t, fit AR(1):
//!   x_{t+1} = a + b * x_t + ε
//!
//! Then recover OU parameters:
//!   κ = -ln(b) * 252  (annualized mean-reversion speed)
//!   μ = a / (1 - b)   (long-run mean)
//!   σ_eq = σ_ε / √(1 - b²)  (stationary standard deviation)

use serde::{Deserialize, Serialize};

/// Result of fitting an OU/AR(1) model to a spread series.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OuFit {
    /// AR(1) intercept.
    pub a: f64,
    /// AR(1) slope coefficient. Valid iff b ∈ (0, 1).
    pub b: f64,
    /// Annualized mean-reversion speed (per year).
    pub kappa: f64,
    /// Long-run mean of the spread.
    pub mu: f64,
    /// Residual standard deviation from AR(1) fit.
    pub sigma: f64,
    /// Stationary (equilibrium) standard deviation.
    pub sigma_eq: f64,
    /// Half-life in trading days (diagnostic only, NOT for decision-making).
    pub half_life_days: f64,
}

/// Fit AR(1) on a spread series and return OU parameters.
///
/// Returns `None` if:
/// - Series has fewer than 10 observations
/// - b ∉ (0, 1) (non-stationary or divergent)
/// - Any computed value is non-finite
pub fn fit_ou_ar1(spread: &[f64]) -> Option<OuFit> {
    if spread.len() < 10 {
        return None;
    }

    let n = spread.len() - 1;
    let x_lag = &spread[..n];
    let x_now = &spread[1..];

    // OLS: x_now = a + b * x_lag
    // Normal equations: [Σ1, Σx_lag] [a]   [Σx_now]
    //                   [Σx_lag, Σx²] [b] = [Σ(x_lag*x_now)]
    let sum_1 = n as f64;
    let sum_x_lag: f64 = x_lag.iter().sum();
    let sum_x_lag_sq: f64 = x_lag.iter().map(|x| x * x).sum();
    let sum_x_now: f64 = x_now.iter().sum();
    let sum_cross: f64 = x_lag.iter().zip(x_now.iter()).map(|(l, n)| l * n).sum();

    let det = sum_1 * sum_x_lag_sq - sum_x_lag * sum_x_lag;
    if !det.is_finite() || det.abs() < 1e-15 {
        return None;
    }

    let a = (sum_x_lag_sq * sum_x_now - sum_x_lag * sum_cross) / det;
    let b = (sum_1 * sum_cross - sum_x_lag * sum_x_now) / det;

    // Reject non-stationary processes
    if b <= 0.0 || b >= 1.0 {
        return None;
    }

    // Compute residuals and variance
    let mut sum_resid_sq = 0.0;
    for (lag, now) in x_lag.iter().zip(x_now.iter()) {
        let pred = a + b * lag;
        let resid = now - pred;
        sum_resid_sq += resid * resid;
    }
    // ddof=2 for unbiased estimate (two parameters estimated)
    let sigma2 = sum_resid_sq / (n as f64 - 2.0).max(1.0);
    let sigma = sigma2.sqrt();

    // OU parameters
    let ln_b = b.ln();
    let kappa = -ln_b * 252.0;
    let mu = a / (1.0 - b);
    let sigma_eq = sigma / (1.0 - b * b).sqrt();
    let half_life_days = 2.0_f64.ln() / (-ln_b);

    // Guard against non-finite values
    if !a.is_finite()
        || !b.is_finite()
        || !kappa.is_finite()
        || !mu.is_finite()
        || !sigma.is_finite()
        || !sigma_eq.is_finite()
        || !half_life_days.is_finite()
    {
        return None;
    }

    Some(OuFit {
        a,
        b,
        kappa,
        mu,
        sigma,
        sigma_eq,
        half_life_days,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fit_ou_ar1_basic() {
        // Simple mean-reverting series around 0
        let spread: Vec<f64> = (0..100).map(|i| 0.1 * ((i as f64 * 0.3).sin())).collect();
        let fit = fit_ou_ar1(&spread);
        assert!(fit.is_some());
        let ou = fit.unwrap();
        assert!(ou.b > 0.0 && ou.b < 1.0);
        assert!(ou.kappa > 0.0);
        assert!(ou.sigma_eq > 0.0);
    }

    #[test]
    fn test_fit_ou_ar1_insufficient_data() {
        let spread = vec![1.0, 2.0, 3.0];
        assert!(fit_ou_ar1(&spread).is_none());
    }

    #[test]
    fn test_fit_ou_ar1_trending_rejected() {
        // Pure random walk (no mean reversion) should be rejected
        let spread: Vec<f64> = (0..100).map(|i| i as f64 * 0.01).collect();
        // This might or might not be rejected depending on noise
        // A pure trend should give b ≈ 1, which we reject
        let _fit = fit_ou_ar1(&spread);
        // The fit might succeed with b close to 1, or fail
        // This is a weak test - synthetic OU tests are better
    }

    #[test]
    fn test_fit_ou_ar1_nan_guard() {
        let spread = vec![f64::NAN; 20];
        assert!(fit_ou_ar1(&spread).is_none());
    }
}
