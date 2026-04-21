//! Bertram (2010) optimal thresholds for OU spread trading.
//!
//! Paper: Bertram, W. K. (2010). Analytic solutions for optimal statistical
//! arbitrage trading. Physica A 389(11): 2234-2243.
//!
//! Given OU process dX = -κ(X - θ) dt + σ_c dW, the symmetric-trade strategy
//! enters at a = θ - k·σ_eq and exits at m = θ + k·σ_eq.

use serde::{Deserialize, Serialize};

use crate::ou::OuFit;

/// Result of Bertram threshold optimization.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BertramResult {
    /// Entry threshold (below mean).
    pub a: f64,
    /// Exit threshold (above mean).
    pub m: f64,
    /// Threshold in z-score units: k = (θ - a) / σ_eq.
    pub k: f64,
    /// Expected return per unit time (spread-units per year).
    pub expected_return_rate: f64,
    /// Expected trade length in trading days.
    pub expected_trade_length_days: f64,
    /// Continuous-time volatility (σ_c = σ_eq * √(2κ)).
    pub sigma_cont: f64,
}

/// Compute erfi(x) = -i * erf(i*x) using the Maclaurin series.
/// For small-to-moderate |x|, this converges well.
fn erfi(x: f64) -> f64 {
    // erfi(x) = (2/√π) * Σ_{n=0}^∞ x^(2n+1) / (n! * (2n+1))
    // We'll use enough terms for good precision
    if !x.is_finite() {
        return f64::NAN;
    }
    if x.abs() > 6.0 {
        // For large |x|, erfi grows like exp(x²)/(x√π)
        // Use asymptotic expansion
        let x2 = x * x;
        let sign = x.signum();
        return sign * x2.exp() / (x.abs() * std::f64::consts::PI.sqrt());
    }

    let two_over_sqrt_pi = 2.0 / std::f64::consts::PI.sqrt();
    let mut sum = 0.0;
    let mut term = x; // First term: x^1 / (0! * 1)
    let mut n_fact = 1.0;

    for n in 0..100 {
        sum += term;
        if term.abs() < 1e-15 * sum.abs() {
            break;
        }
        n_fact *= (n + 1) as f64;
        let exp = 2 * n + 3;
        term = x.powi(exp) / (n_fact * exp as f64);
    }

    two_over_sqrt_pi * sum
}

/// Bertram eq (9): expected first-passage time from a to m under OU.
/// Returns time in years (kappa is annualized, so result is in years).
fn expected_trade_length(a: f64, m: f64, theta: f64, kappa: f64, sigma_cont: f64) -> f64 {
    if kappa <= 0.0 || sigma_cont <= 0.0 {
        return f64::NAN;
    }
    let scale = kappa.sqrt() / sigma_cont;
    let term_m = erfi((m - theta) * scale);
    let term_a = erfi((a - theta) * scale);
    (std::f64::consts::PI / kappa) * (term_m - term_a)
}

/// Bertram eq (5): expected return per unit time = (m - a - c) / E[T].
fn expected_return_rate(a: f64, m: f64, c: f64, theta: f64, kappa: f64, sigma_cont: f64) -> f64 {
    let t = expected_trade_length(a, m, theta, kappa, sigma_cont);
    if t <= 0.0 || !t.is_finite() {
        return f64::NEG_INFINITY;
    }
    (m - a - c) / t
}

/// Find optimal symmetric thresholds maximizing expected return rate.
///
/// Uses grid search (robust) followed by Newton refinement.
/// Returns `None` if no valid solution found.
pub fn optimize_symmetric_thresholds(ou: &OuFit, cost: f64) -> Option<BertramResult> {
    let theta = ou.mu;
    let kappa = ou.kappa;
    let sigma_eq = ou.sigma_eq;

    if kappa <= 0.0 || sigma_eq <= 0.0 {
        return None;
    }

    // Convert to continuous-time volatility
    let sigma_cont = sigma_eq * (2.0 * kappa).sqrt();

    // Grid search over k values
    let mut best_k = 1.0;
    let mut best_rate = f64::NEG_INFINITY;

    for i in 1..200 {
        let k = 0.1 + (i as f64) * 0.02; // k from 0.12 to ~4.1
        let a = theta - k * sigma_eq;
        let m = theta + k * sigma_eq;
        let rate = expected_return_rate(a, m, cost, theta, kappa, sigma_cont);
        if rate > best_rate && rate.is_finite() {
            best_rate = rate;
            best_k = k;
        }
    }

    if !best_rate.is_finite() || best_rate <= 0.0 {
        return None;
    }

    // Newton refinement around best_k (optional, grid is usually sufficient)
    let k = refine_k_newton(theta, kappa, sigma_eq, sigma_cont, cost, best_k);

    let a = theta - k * sigma_eq;
    let m = theta + k * sigma_eq;
    let t_years = expected_trade_length(a, m, theta, kappa, sigma_cont);
    let t_days = t_years * 252.0;
    let rate = expected_return_rate(a, m, cost, theta, kappa, sigma_cont);

    if !k.is_finite() || !t_days.is_finite() || !rate.is_finite() {
        return None;
    }

    Some(BertramResult {
        a,
        m,
        k,
        expected_return_rate: rate,
        expected_trade_length_days: t_days,
        sigma_cont,
    })
}

/// Newton-Raphson refinement of k around an initial guess.
fn refine_k_newton(
    theta: f64,
    kappa: f64,
    sigma_eq: f64,
    sigma_cont: f64,
    cost: f64,
    k_init: f64,
) -> f64 {
    let mut k = k_init;
    let dk = 0.001;

    for _ in 0..10 {
        let a = theta - k * sigma_eq;
        let m = theta + k * sigma_eq;
        let rate = expected_return_rate(a, m, cost, theta, kappa, sigma_cont);

        let a_plus = theta - (k + dk) * sigma_eq;
        let m_plus = theta + (k + dk) * sigma_eq;
        let rate_plus = expected_return_rate(a_plus, m_plus, cost, theta, kappa, sigma_cont);

        let a_minus = theta - (k - dk) * sigma_eq;
        let m_minus = theta + (k - dk) * sigma_eq;
        let rate_minus = expected_return_rate(a_minus, m_minus, cost, theta, kappa, sigma_cont);

        let grad = (rate_plus - rate_minus) / (2.0 * dk);
        let hess = (rate_plus - 2.0 * rate + rate_minus) / (dk * dk);

        if hess.abs() < 1e-12 || !grad.is_finite() || !hess.is_finite() {
            break;
        }

        let step = -grad / hess;
        if step.abs() < 1e-6 {
            break;
        }

        k += step.clamp(-0.5, 0.5);
        k = k.clamp(0.1, 5.0);
    }

    k
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_erfi_small() {
        // erfi(0) = 0
        assert!((erfi(0.0)).abs() < 1e-10);
        // erfi is odd
        assert!((erfi(0.5) + erfi(-0.5)).abs() < 1e-10);
    }

    #[test]
    fn test_bertram_basic() {
        let ou = OuFit {
            a: 0.0,
            b: 0.95,
            kappa: 12.92, // -ln(0.95) * 252
            mu: 0.0,
            sigma: 0.01,
            sigma_eq: 0.032,
            half_life_days: 13.51,
        };
        let result = optimize_symmetric_thresholds(&ou, 0.0005);
        assert!(result.is_some());
        let bt = result.unwrap();
        assert!(bt.k > 0.0 && bt.k < 5.0);
        assert!(bt.expected_trade_length_days > 0.0);
    }

    #[test]
    fn test_bertram_zero_cost() {
        let ou = OuFit {
            a: 0.0,
            b: 0.95,
            kappa: 12.92,
            mu: 0.0,
            sigma: 0.01,
            sigma_eq: 0.032,
            half_life_days: 13.51,
        };
        let result = optimize_symmetric_thresholds(&ou, 0.0);
        assert!(result.is_some());
        let bt = result.unwrap();
        // With zero cost, optimal k should be close to 0 (trade frequently)
        assert!(bt.k > 0.0);
    }
}
