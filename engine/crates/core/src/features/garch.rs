//! GJR-GARCH(1,1) online volatility estimator with asymmetric leverage effect.
//!
//! Captures the empirical observation that negative returns increase volatility
//! more than positive returns of the same magnitude.
//!
//! ```text
//! σ²_t = ω + (α + γ × I_{r<0}) × r²_{t-1} + β × σ²_{t-1}
//!
//! where:
//!   ω = long-run variance weight (intercept)
//!   α = symmetric shock reaction
//!   β = variance persistence
//!   γ = asymmetry coefficient (leverage effect)
//!   I_{r<0} = 1 if r_{t-1} < 0, else 0
//! ```
//!
//! Only 4 parameters. Computationally identical to GARCH(1,1) — one extra
//! multiply per bar. O(1) time and space.
//!
//! Reference: Glosten, Jagannathan & Runkle (1993).

/// GJR-GARCH(1,1) online estimator.
#[derive(Debug, Clone)]
pub struct GjrGarch {
    omega: f64,
    alpha: f64,
    beta: f64,
    gamma: f64,
    sigma_sq: f64,
    prev_return: f64,
    count: usize,
}

/// GJR-GARCH configuration (serializable for TOML).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct GarchConfig {
    pub omega: f64,
    pub alpha: f64,
    pub beta: f64,
    pub gamma: f64,
}

impl Default for GarchConfig {
    fn default() -> Self {
        // Sensible defaults for equity/crypto intraday.
        // α + β + γ/2 = 0.06 + 0.87 + 0.05 = 0.98 (high persistence, asymmetric).
        Self {
            omega: 0.000005,
            alpha: 0.06,
            beta: 0.87,
            gamma: 0.10,
        }
    }
}

impl GarchConfig {
    /// Check stationarity constraint: α + β + γ/2 < 1.
    pub fn is_stationary(&self) -> bool {
        self.alpha + self.beta + self.gamma / 2.0 < 1.0
    }
}

impl GjrGarch {
    /// Create a new GJR-GARCH estimator from config.
    ///
    /// # Panics
    /// # Panics
    /// Panics if the stationarity constraint α + β + γ/2 < 1 is violated.
    pub fn from_config(config: &GarchConfig) -> Self {
        assert!(
            config.is_stationary(),
            "GJR-GARCH not stationary: α + β + γ/2 = {} ≥ 1",
            config.alpha + config.beta + config.gamma / 2.0
        );

        // Long-run unconditional variance: ω / (1 - α - β - γ/2)
        let denom = (1.0 - config.alpha - config.beta - config.gamma / 2.0).max(0.01);
        let long_run_var = config.omega / denom;

        Self {
            omega: config.omega,
            alpha: config.alpha,
            beta: config.beta,
            gamma: config.gamma,
            sigma_sq: long_run_var,
            prev_return: 0.0,
            count: 0,
        }
    }

    /// Feed a new log return, update conditional variance, return σ_t.
    ///
    /// The first call initializes `prev_return`; variance updates start
    /// from the second call onward.
    #[inline]
    pub fn update(&mut self, log_return: f64) -> f64 {
        if self.count > 0 {
            let shock_sq = self.prev_return * self.prev_return;
            let leverage = if self.prev_return < 0.0 {
                self.gamma
            } else {
                0.0
            };
            self.sigma_sq =
                self.omega + (self.alpha + leverage) * shock_sq + self.beta * self.sigma_sq;
        }
        self.prev_return = log_return;
        self.count += 1;
        self.sigma_sq.sqrt()
    }

    /// Current conditional volatility σ_t (standard deviation of returns).
    pub fn volatility(&self) -> f64 {
        self.sigma_sq.sqrt()
    }

    /// Current conditional variance σ²_t.
    pub fn variance(&self) -> f64 {
        self.sigma_sq
    }

    /// Number of observations processed.
    pub fn count(&self) -> usize {
        self.count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_garch() -> GjrGarch {
        GjrGarch::from_config(&GarchConfig::default())
    }

    #[test]
    fn asymmetry_negative_shock_increases_vol_more() {
        let mut g1 = default_garch();
        let mut g2 = default_garch();

        // Feed identical magnitude shocks with different signs
        let shock = 0.02; // 2% move
        for _ in 0..5 {
            g1.update(0.0); // calm
            g2.update(0.0);
        }

        // Negative shock
        g1.update(-shock);
        let vol_after_neg = g1.update(0.0);

        // Positive shock
        g2.update(shock);
        let vol_after_pos = g2.update(0.0);

        assert!(
            vol_after_neg > vol_after_pos,
            "vol after negative shock ({vol_after_neg:.6}) should exceed \
             vol after positive shock ({vol_after_pos:.6})"
        );
    }

    #[test]
    fn vol_decays_toward_long_run_mean() {
        let config = GarchConfig::default();
        let mut g = GjrGarch::from_config(&config);

        // Big shock, then calm bars
        g.update(0.05); // 5% shock
        let vol_after_shock = g.update(0.0);

        // Let it decay for 50 calm bars
        let mut vol = vol_after_shock;
        for _ in 0..50 {
            vol = g.update(0.0);
        }

        assert!(
            vol < vol_after_shock,
            "vol should decay: {vol:.6} < {vol_after_shock:.6}"
        );

        // Should approach long-run vol
        let long_run_vol =
            (config.omega / (1.0 - config.alpha - config.beta - config.gamma / 2.0)).sqrt();
        // After 50 calm bars, should be within 2x of long-run
        assert!(
            vol < long_run_vol * 2.0,
            "vol ({vol:.6}) should approach long-run ({long_run_vol:.6})"
        );
    }

    #[test]
    #[should_panic(expected = "not stationary")]
    fn stationarity_violation_panics_in_debug() {
        let config = GarchConfig {
            omega: 0.001,
            alpha: 0.5,
            beta: 0.5,
            gamma: 0.2, // α + β + γ/2 = 1.1 ≥ 1
        };
        GjrGarch::from_config(&config);
    }

    #[test]
    fn stationarity_check() {
        assert!(GarchConfig::default().is_stationary());

        let bad = GarchConfig {
            omega: 0.001,
            alpha: 0.5,
            beta: 0.5,
            gamma: 0.2,
        };
        assert!(!bad.is_stationary());
    }

    #[test]
    fn first_bar_uses_long_run_variance() {
        let config = GarchConfig::default();
        let g = GjrGarch::from_config(&config);

        let long_run_var = config.omega / (1.0 - config.alpha - config.beta - config.gamma / 2.0);
        assert!(
            (g.variance() - long_run_var).abs() < 1e-12,
            "initial variance should be long-run: {} vs {}",
            g.variance(),
            long_run_var
        );
    }

    #[test]
    fn vol_increases_after_large_shock() {
        let mut g = default_garch();
        let vol_before = g.update(0.001); // tiny move
        g.update(0.05); // big shock
        let vol_after = g.update(0.0);

        assert!(
            vol_after > vol_before,
            "vol should increase after shock: {vol_after:.6} > {vol_before:.6}"
        );
    }

    #[test]
    fn zero_gamma_equals_plain_garch() {
        let symmetric = GarchConfig {
            gamma: 0.0,
            ..GarchConfig::default()
        };
        let mut g1 = GjrGarch::from_config(&symmetric);
        let mut g2 = GjrGarch::from_config(&symmetric);

        g1.update(-0.02);
        let v1 = g1.update(0.0);

        g2.update(0.02);
        let v2 = g2.update(0.0);

        assert!(
            (v1 - v2).abs() < 1e-12,
            "with γ=0, pos and neg shocks should give same vol: {v1} vs {v2}"
        );
    }

    #[test]
    fn count_tracks_observations() {
        let mut g = default_garch();
        assert_eq!(g.count(), 0);
        g.update(0.01);
        assert_eq!(g.count(), 1);
        g.update(-0.01);
        assert_eq!(g.count(), 2);
    }
}
