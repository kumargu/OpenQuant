//! Augmented Dickey-Fuller test for unit root / stationarity.
//!
//! Tests H0: series has a unit root (non-stationary)
//! vs H1: series is stationary.
//!
//! Used in Engle-Granger two-step cointegration:
//! 1. Regress y on x → get residuals
//! 2. ADF test on residuals → reject unit root = cointegrated
//!
//! Implementation:
//! - Regress Δy_t on y_{t-1} + constant + p lagged Δy terms
//! - Test statistic = gamma_hat / se(gamma_hat)
//! - Compare against MacKinnon (1994) critical values

use super::ols::ols_multiple;
use tracing::debug;

/// ADF test result.
#[derive(Debug, Clone)]
pub struct AdfResult {
    /// ADF test statistic (negative = more stationary).
    pub test_statistic: f64,
    /// Approximate p-value interpolated from MacKinnon tables.
    pub p_value: f64,
    /// Number of lags used.
    pub lags: usize,
    /// Number of observations used in regression.
    pub n_obs: usize,
    /// Whether null hypothesis is rejected at 5% level.
    pub is_stationary: bool,
    /// Raw gamma coefficient (mean-reversion speed). More negative = faster reversion.
    /// This is the AR(1) coefficient on y_{t-1} in the ADF regression.
    /// Useful independently of statistical significance — a pair can have strong
    /// mean-reversion (large |gamma|) but fail the ADF significance test.
    pub gamma: f64,
}

/// MacKinnon (1994) approximate critical values for ADF with constant, no trend.
/// Format: (significance_level, critical_value)
/// These are for large-sample; for Engle-Granger residual-based test we use
/// slightly different values (more conservative).
const CRITICAL_VALUES: [(f64, f64); 4] =
    [(0.01, -3.43), (0.05, -2.86), (0.10, -2.57), (0.25, -1.94)];

/// For Engle-Granger cointegration residuals, critical values are more stringent.
/// MacKinnon (2010) for 2-variable cointegration at various significance levels.
const EG_CRITICAL_VALUES: [(f64, f64); 4] =
    [(0.01, -3.90), (0.05, -3.34), (0.10, -3.04), (0.25, -2.58)];

/// Run ADF test on a time series.
///
/// `max_lags`: maximum lag order. If None, uses floor(12 * (n/100)^{1/4}).
/// `engle_granger`: if true, use Engle-Granger critical values (for residual-based test).
pub fn adf_test(series: &[f64], max_lags: Option<usize>, engle_granger: bool) -> Option<AdfResult> {
    let n = series.len();
    if n < 20 {
        return None;
    }

    // Determine optimal lag length via AIC
    let max_p = max_lags.unwrap_or_else(|| {
        let suggested = (12.0 * (n as f64 / 100.0).powf(0.25)).floor() as usize;
        suggested.min(n / 4).max(1)
    });

    let mut best_aic = f64::INFINITY;
    let mut best_lag = 0;

    for p in 0..=max_p {
        if let Some(aic) = adf_aic(series, p) {
            if aic < best_aic {
                best_aic = aic;
                best_lag = p;
            }
        }
    }

    debug!(
        n = n,
        max_lags = max_p,
        best_lag,
        best_aic = format!("{best_aic:.2}").as_str(),
        engle_granger,
        "ADF lag selection"
    );

    adf_regression(series, best_lag, engle_granger)
}

/// Compute AIC for ADF regression with `p` lags.
fn adf_aic(series: &[f64], p: usize) -> Option<f64> {
    let n = series.len();
    if n <= p + 2 {
        return None;
    }

    // Δy_t = c + gamma * y_{t-1} + sum(phi_i * Δy_{t-i}) + eps
    let start = p + 1;
    let t_len = n - start;
    if t_len < p + 3 {
        return None;
    }

    // Build dependent variable: Δy_t
    let dy: Vec<f64> = (start..n).map(|t| series[t] - series[t - 1]).collect();

    // Build regressors
    let ones: Vec<f64> = vec![1.0; t_len];
    let y_lag: Vec<f64> = (start..n).map(|t| series[t - 1]).collect();

    let mut lag_diffs: Vec<Vec<f64>> = Vec::with_capacity(p);
    for lag in 1..=p {
        let col: Vec<f64> = (start..n)
            .map(|t| series[t - lag] - series[t - lag - 1])
            .collect();
        lag_diffs.push(col);
    }

    let mut cols: Vec<&[f64]> = vec![&ones, &y_lag];
    for ld in &lag_diffs {
        cols.push(ld);
    }

    let result = ols_multiple(&cols, &dy)?;

    // AIC = n * ln(RSS/n) + 2k
    let rss: f64 = result.residuals.iter().map(|e| e * e).sum();
    let k = cols.len() as f64;
    let n_f = t_len as f64;
    Some(n_f * (rss / n_f).ln() + 2.0 * k)
}

/// Run ADF regression and return test result.
fn adf_regression(series: &[f64], p: usize, engle_granger: bool) -> Option<AdfResult> {
    let n = series.len();
    let start = p + 1;
    let t_len = n - start;
    if t_len < p + 3 {
        return None;
    }

    let dy: Vec<f64> = (start..n).map(|t| series[t] - series[t - 1]).collect();
    let ones: Vec<f64> = vec![1.0; t_len];
    let y_lag: Vec<f64> = (start..n).map(|t| series[t - 1]).collect();

    let mut lag_diffs: Vec<Vec<f64>> = Vec::with_capacity(p);
    for lag in 1..=p {
        let col: Vec<f64> = (start..n)
            .map(|t| series[t - lag] - series[t - lag - 1])
            .collect();
        lag_diffs.push(col);
    }

    let mut cols: Vec<&[f64]> = vec![&ones, &y_lag];
    for ld in &lag_diffs {
        cols.push(ld);
    }

    let result = ols_multiple(&cols, &dy)?;

    // gamma is coefficient[1] (y_{t-1}), its t-statistic is the ADF stat
    let gamma = result.coefficients[1];
    let se_gamma = result.std_errors[1];
    if se_gamma < 1e-15 {
        return None;
    }

    let test_stat = gamma / se_gamma;

    let table = if engle_granger {
        &EG_CRITICAL_VALUES
    } else {
        &CRITICAL_VALUES
    };

    let p_value = interpolate_p_value(test_stat, table);
    let is_stationary = p_value < 0.05;

    debug!(
        n_obs = t_len,
        lags = p,
        gamma = format!("{gamma:.6}").as_str(),
        se_gamma = format!("{se_gamma:.6}").as_str(),
        test_stat = format!("{test_stat:.4}").as_str(),
        p_value = format!("{p_value:.4}").as_str(),
        engle_granger,
        "ADF regression result"
    );

    Some(AdfResult {
        test_statistic: test_stat,
        p_value,
        lags: p,
        n_obs: t_len,
        is_stationary,
        gamma,
    })
}

/// Interpolate p-value from critical value table.
/// Linear interpolation between known critical values.
fn interpolate_p_value(stat: f64, table: &[(f64, f64)]) -> f64 {
    // table is sorted by significance level ascending, critical values ascending (more negative)
    // If stat is more negative than the 1% critical value, p < 0.01
    if stat <= table[0].1 {
        // Extrapolate below 1%: clamp to a small value
        return table[0].0 * 0.5;
    }

    // If stat is less negative than the 25% critical value, p > 0.25
    if stat >= table[table.len() - 1].1 {
        // Extrapolate: linearly toward 1.0
        let last = table[table.len() - 1];
        let ratio = (stat - last.1) / (-last.1);
        return (last.0 + ratio * (1.0 - last.0)).min(1.0);
    }

    // Interpolate between two bracketing entries
    for i in 0..(table.len() - 1) {
        let (p_lo, cv_lo) = table[i];
        let (p_hi, cv_hi) = table[i + 1];
        if stat >= cv_lo && stat <= cv_hi {
            let frac = (stat - cv_lo) / (cv_hi - cv_lo);
            return p_lo + frac * (p_hi - p_lo);
        }
    }

    0.5 // fallback
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils;

    #[test]
    fn test_adf_stationary_series() {
        let series = test_utils::stationary_series(500, 0.5, 0.2, 42);
        let result = adf_test(&series, None, false).unwrap();
        // Stationary series should reject unit root
        assert!(
            result.is_stationary,
            "stat={}, p={}",
            result.test_statistic, result.p_value
        );
        assert!(result.p_value < 0.05);
    }

    #[test]
    fn test_adf_random_walk() {
        let series = test_utils::random_walk(500, 0.2, 42);
        let result = adf_test(&series, None, false).unwrap();
        // Random walk should NOT reject unit root
        assert!(
            !result.is_stationary,
            "stat={}, p={}",
            result.test_statistic, result.p_value
        );
        assert!(result.p_value > 0.05);
    }

    #[test]
    fn test_adf_too_short() {
        let series = vec![1.0, 2.0, 3.0];
        assert!(adf_test(&series, None, false).is_none());
    }

    #[test]
    fn test_adf_engle_granger_more_conservative() {
        // Same series should have higher p-value with EG critical values
        let series = test_utils::stationary_series(200, 0.5, 0.2, 99);
        let normal = adf_test(&series, None, false).unwrap();
        let eg = adf_test(&series, None, true).unwrap();
        // EG critical values are more stringent, so p-value should be higher (or equal)
        assert!(
            eg.p_value >= normal.p_value - 0.01,
            "eg_p={}, normal_p={}",
            eg.p_value,
            normal.p_value
        );
    }

    #[test]
    fn test_p_value_interpolation() {
        // Exact critical value
        assert!((interpolate_p_value(-2.86, &CRITICAL_VALUES) - 0.05).abs() < 0.001);
        // More negative than 1% → very small p
        assert!(interpolate_p_value(-5.0, &CRITICAL_VALUES) < 0.01);
        // Less negative than 25% → large p
        assert!(interpolate_p_value(-1.0, &CRITICAL_VALUES) > 0.25);
    }
}
