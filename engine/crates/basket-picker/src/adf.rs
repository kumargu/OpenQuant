//! Lightweight Augmented Dickey-Fuller test for spread admission.

use tracing::debug;

/// ADF result for a spread series.
#[derive(Debug, Clone, Copy)]
pub struct AdfResult {
    pub test_statistic: f64,
    pub p_value: f64,
    pub is_stationary: bool,
}

// Standard ADF critical values with constant (no time trend), MacKinnon (1996)
// asymptotic table. Appropriate when the tested series is built from a fixed
// formula (e.g. `log(target) - mean(log(peers))`) and is NOT the residual of
// an OLS-estimated cointegrating regression. Using the Engle-Granger table
// here would over-state p-values by ~0.5 critical-value units.
//
// Ref: MacKinnon, J. G. (1996). "Numerical Distribution Functions for Unit
// Root and Cointegration Tests." Journal of Applied Econometrics, 11(6).
const ADF_CONSTANT_CRITICAL_VALUES: [(f64, f64); 4] = [
    (0.01, -3.435),
    (0.05, -2.864),
    (0.10, -2.568),
    (0.25, -2.196),
];

pub fn adf_test(series: &[f64], max_lags: Option<usize>) -> Option<AdfResult> {
    let n = series.len();
    if n < 20 {
        return None;
    }

    let max_p = max_lags.unwrap_or_else(|| {
        let suggested = (12.0 * (n as f64 / 100.0).powf(0.25)).floor() as usize;
        suggested.min(n / 4).max(1)
    });

    let mut best_aic = f64::INFINITY;
    let mut best_lag = 0usize;
    for p in 0..=max_p {
        if let Some(aic) = adf_aic(series, p) {
            if aic < best_aic {
                best_aic = aic;
                best_lag = p;
            }
        }
    }

    debug!(n, max_lags = max_p, best_lag, "basket ADF lag selection");
    adf_regression(series, best_lag)
}

fn adf_aic(series: &[f64], p: usize) -> Option<f64> {
    let n = series.len();
    if n <= p + 2 {
        return None;
    }
    let start = p + 1;
    let t_len = n - start;
    if t_len < p + 3 {
        return None;
    }

    let dy: Vec<f64> = (start..n).map(|t| series[t] - series[t - 1]).collect();
    let ones = vec![1.0; t_len];
    let y_lag: Vec<f64> = (start..n).map(|t| series[t - 1]).collect();

    let mut lag_diffs: Vec<Vec<f64>> = Vec::with_capacity(p);
    for lag in 1..=p {
        lag_diffs.push(
            (start..n)
                .map(|t| series[t - lag] - series[t - lag - 1])
                .collect(),
        );
    }

    let mut cols: Vec<&[f64]> = vec![&ones, &y_lag];
    for ld in &lag_diffs {
        cols.push(ld);
    }

    let result = ols_multiple(&cols, &dy)?;
    let rss: f64 = result.residuals.iter().map(|e| e * e).sum();
    let k = cols.len() as f64;
    let n_f = t_len as f64;
    Some(n_f * (rss / n_f).ln() + 2.0 * k)
}

fn adf_regression(series: &[f64], p: usize) -> Option<AdfResult> {
    let n = series.len();
    let start = p + 1;
    let t_len = n - start;
    if t_len < p + 3 {
        return None;
    }

    let dy: Vec<f64> = (start..n).map(|t| series[t] - series[t - 1]).collect();
    let ones = vec![1.0; t_len];
    let y_lag: Vec<f64> = (start..n).map(|t| series[t - 1]).collect();

    let mut lag_diffs: Vec<Vec<f64>> = Vec::with_capacity(p);
    for lag in 1..=p {
        lag_diffs.push(
            (start..n)
                .map(|t| series[t - lag] - series[t - lag - 1])
                .collect(),
        );
    }

    let mut cols: Vec<&[f64]> = vec![&ones, &y_lag];
    for ld in &lag_diffs {
        cols.push(ld);
    }

    let result = ols_multiple(&cols, &dy)?;
    let gamma = *result.coefficients.get(1)?;
    let se_gamma = *result.std_errors.get(1)?;
    if se_gamma <= 1e-15 || !se_gamma.is_finite() {
        return None;
    }
    let test_statistic = gamma / se_gamma;
    let p_value = interpolate_p_value(test_statistic, &ADF_CONSTANT_CRITICAL_VALUES);

    Some(AdfResult {
        test_statistic,
        p_value,
        is_stationary: p_value < 0.05,
    })
}

fn interpolate_p_value(stat: f64, table: &[(f64, f64)]) -> f64 {
    if stat <= table[0].1 {
        return table[0].0 * 0.5;
    }
    if stat >= table[table.len() - 1].1 {
        let last = table[table.len() - 1];
        let ratio = (stat - last.1) / (-last.1);
        return (last.0 + ratio * (1.0 - last.0)).min(1.0);
    }
    for i in 0..(table.len() - 1) {
        let (p_lo, cv_lo) = table[i];
        let (p_hi, cv_hi) = table[i + 1];
        if stat >= cv_lo && stat <= cv_hi {
            let frac = (stat - cv_lo) / (cv_hi - cv_lo);
            return p_lo + frac * (p_hi - p_lo);
        }
    }
    0.5
}

#[derive(Debug, Clone)]
struct MultiOlsResult {
    coefficients: Vec<f64>,
    residuals: Vec<f64>,
    std_errors: Vec<f64>,
}

#[allow(clippy::needless_range_loop)]
fn ols_multiple(x_cols: &[&[f64]], y: &[f64]) -> Option<MultiOlsResult> {
    let k = x_cols.len();
    if k == 0 {
        return None;
    }
    let n = y.len();
    for col in x_cols {
        if col.len() != n {
            return None;
        }
    }
    if n <= k {
        return None;
    }

    let mut xtx = vec![0.0; k * k];
    let mut xty = vec![0.0; k];
    for j in 0..k {
        for i in 0..k {
            let mut s = 0.0;
            for t in 0..n {
                s += x_cols[j][t] * x_cols[i][t];
            }
            xtx[j * k + i] = s;
        }
        let mut s = 0.0;
        for t in 0..n {
            s += x_cols[j][t] * y[t];
        }
        xty[j] = s;
    }

    let coefficients = solve_symmetric(&xtx, &xty, k)?;

    let mut residuals = Vec::with_capacity(n);
    let mut ss_res = 0.0;
    for t in 0..n {
        let mut y_hat = 0.0;
        for j in 0..k {
            y_hat += coefficients[j] * x_cols[j][t];
        }
        let e = y[t] - y_hat;
        residuals.push(e);
        ss_res += e * e;
    }

    let s_squared = ss_res / (n - k) as f64;
    let inv_diag = invert_diagonal(&xtx, k)?;
    let std_errors = inv_diag.iter().map(|d| (d * s_squared).sqrt()).collect();

    Some(MultiOlsResult {
        coefficients,
        residuals,
        std_errors,
    })
}

fn solve_symmetric(a: &[f64], b: &[f64], n: usize) -> Option<Vec<f64>> {
    let mut l = vec![0.0; n * n];
    for i in 0..n {
        for j in 0..=i {
            let mut sum = a[i * n + j];
            for k in 0..j {
                sum -= l[i * n + k] * l[j * n + k];
            }
            if i == j {
                if sum <= 1e-15 || !sum.is_finite() {
                    return None;
                }
                l[i * n + j] = sum.sqrt();
            } else {
                l[i * n + j] = sum / l[j * n + j];
            }
        }
    }

    let mut y = vec![0.0; n];
    for i in 0..n {
        let mut sum = b[i];
        for k in 0..i {
            sum -= l[i * n + k] * y[k];
        }
        y[i] = sum / l[i * n + i];
    }

    let mut x = vec![0.0; n];
    for i in (0..n).rev() {
        let mut sum = y[i];
        for k in (i + 1)..n {
            sum -= l[k * n + i] * x[k];
        }
        x[i] = sum / l[i * n + i];
    }
    Some(x)
}

fn invert_diagonal(a: &[f64], n: usize) -> Option<Vec<f64>> {
    let mut diag = Vec::with_capacity(n);
    for i in 0..n {
        let mut e = vec![0.0; n];
        e[i] = 1.0;
        let x = solve_symmetric(a, &e, n)?;
        diag.push(x[i]);
    }
    Some(diag)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stationary_series_rejects_unit_root() {
        let mut series = Vec::new();
        let mut x = 0.0;
        for i in 0..300 {
            let noise = ((i * 17 % 29) as f64 - 14.0) / 200.0;
            x = 0.75 * x + noise;
            series.push(x);
        }
        let result = adf_test(&series, None).unwrap();
        assert!(result.is_stationary, "{result:?}");
    }
}
