//! Ordinary Least Squares regression.
//!
//! Implements simple linear regression y = alpha + beta * x using the
//! closed-form normal equations. Returns beta, alpha, residuals, and R².
//!
//! Used for:
//! - Hedge ratio (beta) estimation between log-prices of two assets
//! - AR(1) regression for OU half-life estimation
//! - ADF test regressions

/// Result of an OLS regression y = alpha + beta * x.
#[derive(Debug, Clone)]
pub struct OlsResult {
    /// Slope coefficient.
    pub beta: f64,
    /// Intercept.
    pub alpha: f64,
    /// R-squared (coefficient of determination).
    pub r_squared: f64,
    /// Residuals (y - y_hat).
    pub residuals: Vec<f64>,
    /// Standard error of beta.
    pub beta_std_err: f64,
    /// Number of observations.
    pub n: usize,
}

/// Run simple OLS regression: y = alpha + beta * x.
///
/// Returns `None` if fewer than 3 observations or zero variance in x.
pub fn ols_simple(x: &[f64], y: &[f64]) -> Option<OlsResult> {
    let n = x.len().min(y.len());
    if n < 3 {
        return None;
    }

    let n_f = n as f64;
    let mut sum_x = 0.0;
    let mut sum_y = 0.0;
    let mut sum_xx = 0.0;
    let mut sum_xy = 0.0;

    for i in 0..n {
        sum_x += x[i];
        sum_y += y[i];
        sum_xx += x[i] * x[i];
        sum_xy += x[i] * y[i];
    }

    let mean_x = sum_x / n_f;
    let mean_y = sum_y / n_f;
    let ss_xx = sum_xx - n_f * mean_x * mean_x;

    if ss_xx.abs() < 1e-15 {
        return None; // zero variance in x
    }

    let ss_xy = sum_xy - n_f * mean_x * mean_y;
    let beta = ss_xy / ss_xx;
    let alpha = mean_y - beta * mean_x;

    // Compute residuals and R²
    let mut residuals = Vec::with_capacity(n);
    let mut ss_res = 0.0;
    let mut ss_tot = 0.0;

    for i in 0..n {
        let y_hat = alpha + beta * x[i];
        let e = y[i] - y_hat;
        residuals.push(e);
        ss_res += e * e;
        let dy = y[i] - mean_y;
        ss_tot += dy * dy;
    }

    let r_squared = if ss_tot > 1e-15 {
        1.0 - ss_res / ss_tot
    } else {
        0.0
    };

    // Standard error of beta: se(beta) = sqrt(s² / SS_xx)
    // where s² = SS_res / (n - 2)
    let s_squared = if n > 2 { ss_res / (n - 2) as f64 } else { 0.0 };
    let beta_std_err = (s_squared / ss_xx).sqrt();

    Some(OlsResult {
        beta,
        alpha,
        r_squared,
        residuals,
        beta_std_err,
        n,
    })
}

/// Run multiple OLS regression with a design matrix X (including intercept column)
/// and dependent variable y. Used by ADF test which needs lagged differences.
///
/// X is column-major: `x_cols[j][i]` is observation i of regressor j.
/// First column should be ones (intercept) if desired.
///
/// Solves via normal equations: beta = (X'X)^{-1} X'y
/// Returns coefficients and residuals.
#[derive(Debug, Clone)]
pub struct MultiOlsResult {
    /// Regression coefficients (one per column of X).
    pub coefficients: Vec<f64>,
    /// Residuals (y - X * beta).
    pub residuals: Vec<f64>,
    /// Standard errors of coefficients.
    pub std_errors: Vec<f64>,
    /// Number of observations.
    pub n: usize,
}

#[allow(clippy::needless_range_loop)]
pub fn ols_multiple(x_cols: &[&[f64]], y: &[f64]) -> Option<MultiOlsResult> {
    let k = x_cols.len(); // number of regressors
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

    // Build X'X (k x k) and X'y (k x 1)
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

    // Solve via Cholesky decomposition (X'X is symmetric positive definite)
    let coefficients = solve_symmetric(&xtx, &xty, k)?;

    // Compute residuals
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

    // Standard errors: diag((X'X)^{-1}) * s²
    let s_squared = ss_res / (n - k) as f64;
    let inv_diag = invert_diagonal(&xtx, k);
    let std_errors = inv_diag
        .iter()
        .map(|&d| (d * s_squared).max(0.0).sqrt())
        .collect();

    Some(MultiOlsResult {
        coefficients,
        residuals,
        std_errors,
        n,
    })
}

/// Solve Ax = b for symmetric positive definite A via Cholesky decomposition.
fn solve_symmetric(a: &[f64], b: &[f64], n: usize) -> Option<Vec<f64>> {
    // Cholesky: A = L * L'
    let mut l = vec![0.0; n * n];

    for j in 0..n {
        let mut sum = 0.0;
        for k in 0..j {
            sum += l[j * n + k] * l[j * n + k];
        }
        let diag = a[j * n + j] - sum;
        if diag <= 0.0 {
            // Not positive definite — fall back to LU with pivoting
            return solve_lu(a, b, n);
        }
        l[j * n + j] = diag.sqrt();

        for i in (j + 1)..n {
            let mut sum = 0.0;
            for k in 0..j {
                sum += l[i * n + k] * l[j * n + k];
            }
            l[i * n + j] = (a[i * n + j] - sum) / l[j * n + j];
        }
    }

    // Forward substitution: L * z = b
    let mut z = vec![0.0; n];
    for i in 0..n {
        let mut sum = 0.0;
        for k in 0..i {
            sum += l[i * n + k] * z[k];
        }
        z[i] = (b[i] - sum) / l[i * n + i];
    }

    // Back substitution: L' * x = z
    let mut x = vec![0.0; n];
    for i in (0..n).rev() {
        let mut sum = 0.0;
        for k in (i + 1)..n {
            sum += l[k * n + i] * x[k];
        }
        x[i] = (z[i] - sum) / l[i * n + i];
    }

    Some(x)
}

/// Fallback LU solver with partial pivoting.
fn solve_lu(a: &[f64], b: &[f64], n: usize) -> Option<Vec<f64>> {
    let mut aug = vec![0.0; n * (n + 1)];
    for i in 0..n {
        for j in 0..n {
            aug[i * (n + 1) + j] = a[i * n + j];
        }
        aug[i * (n + 1) + n] = b[i];
    }

    for col in 0..n {
        // Partial pivoting
        let mut max_val = aug[col * (n + 1) + col].abs();
        let mut max_row = col;
        for row in (col + 1)..n {
            let val = aug[row * (n + 1) + col].abs();
            if val > max_val {
                max_val = val;
                max_row = row;
            }
        }
        if max_val < 1e-14 {
            return None; // singular
        }
        if max_row != col {
            for j in 0..=n {
                aug.swap(col * (n + 1) + j, max_row * (n + 1) + j);
            }
        }
        let pivot = aug[col * (n + 1) + col];
        for row in (col + 1)..n {
            let factor = aug[row * (n + 1) + col] / pivot;
            for j in col..=n {
                aug[row * (n + 1) + j] -= factor * aug[col * (n + 1) + j];
            }
        }
    }

    // Back substitution
    let mut x = vec![0.0; n];
    for i in (0..n).rev() {
        let mut sum = aug[i * (n + 1) + n];
        for j in (i + 1)..n {
            sum -= aug[i * (n + 1) + j] * x[j];
        }
        x[i] = sum / aug[i * (n + 1) + i];
    }

    Some(x)
}

/// Extract diagonal of the inverse of a symmetric matrix (for standard errors).
fn invert_diagonal(a: &[f64], n: usize) -> Vec<f64> {
    // Compute full inverse via LU, extract diagonal
    let mut result = vec![0.0; n];
    for i in 0..n {
        let mut e = vec![0.0; n];
        e[i] = 1.0;
        if let Some(col) = solve_lu(a, &e, n) {
            result[i] = col[i];
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ols_simple_perfect_fit() {
        // y = 2 + 3x (perfect linear)
        let x = vec![1.0, 2.0, 3.0, 4.0, 5.0];
        let y: Vec<f64> = x.iter().map(|&xi| 2.0 + 3.0 * xi).collect();
        let result = ols_simple(&x, &y).unwrap();

        assert!((result.beta - 3.0).abs() < 1e-10);
        assert!((result.alpha - 2.0).abs() < 1e-10);
        assert!((result.r_squared - 1.0).abs() < 1e-10);
        assert!(result.residuals.iter().all(|e| e.abs() < 1e-10));
    }

    #[test]
    fn test_ols_simple_noisy() {
        // y ≈ 1 + 2x with some noise
        let x = vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0];
        let y = vec![3.1, 4.9, 7.2, 8.8, 11.1, 13.0, 14.8, 17.1, 19.0, 21.2];
        let result = ols_simple(&x, &y).unwrap();

        assert!((result.beta - 2.0).abs() < 0.1, "beta={}", result.beta);
        assert!((result.alpha - 1.0).abs() < 0.3, "alpha={}", result.alpha);
        assert!(result.r_squared > 0.99);
    }

    #[test]
    fn test_ols_returns_none_for_insufficient_data() {
        assert!(ols_simple(&[1.0], &[2.0]).is_none());
        assert!(ols_simple(&[1.0, 2.0], &[2.0, 3.0]).is_none());
    }

    #[test]
    fn test_ols_returns_none_for_zero_variance() {
        let x = vec![5.0, 5.0, 5.0, 5.0];
        let y = vec![1.0, 2.0, 3.0, 4.0];
        assert!(ols_simple(&x, &y).is_none());
    }

    #[test]
    fn test_ols_multiple_with_intercept() {
        // y = 2 + 3x (same as simple, but using multiple OLS with explicit intercept)
        let ones = vec![1.0; 5];
        let x = vec![1.0, 2.0, 3.0, 4.0, 5.0];
        let y: Vec<f64> = x.iter().map(|&xi| 2.0 + 3.0 * xi).collect();

        let cols: Vec<&[f64]> = vec![&ones, &x];
        let result = ols_multiple(&cols, &y).unwrap();

        assert!((result.coefficients[0] - 2.0).abs() < 1e-10, "intercept");
        assert!((result.coefficients[1] - 3.0).abs() < 1e-10, "slope");
    }

    #[test]
    fn test_ols_beta_std_err() {
        let x = vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0];
        let y = vec![3.1, 4.9, 7.2, 8.8, 11.1, 13.0, 14.8, 17.1, 19.0, 21.2];
        let result = ols_simple(&x, &y).unwrap();
        // Standard error should be small for a tight fit
        assert!(result.beta_std_err < 0.1, "se={}", result.beta_std_err);
    }
}
