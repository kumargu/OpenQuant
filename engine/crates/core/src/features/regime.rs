//! BOCPD regime detection — Bayesian Online Changepoint Detection + vol regime.
//!
//! Detects structural breaks in the return process and classifies the current
//! market regime from GARCH vol percentile and changepoint probability.
//!
//! The BOCPD implementation follows Adams & MacKay (2007) with run-length
//! truncation for O(1) amortized updates. Uses a Normal-Gamma conjugate prior
//! so the predictive distribution is closed-form (Student-t).
//!
//! No external dependencies — the algorithm is ~60 lines of math.
//!
//! Reference: Adams & MacKay (2007) "Bayesian Online Changepoint Detection"

/// Market regime classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum MarketRegime {
    #[default]
    Unknown,
    /// Calm, range-bound. Favor mean-reversion, VWAP reversion.
    LowVol,
    /// Mixed conditions. Run all strategies with base weights.
    Normal,
    /// Trending/volatile. Favor momentum; widen stops.
    HighVol,
    /// Extreme vol + drawdown. Reduce all positions.
    Crisis,
}

/// Regime detection configuration (serializable for TOML).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct RegimeConfig {
    /// Expected bars between regime changes (BOCPD hazard = 1/lambda).
    pub bocpd_hazard: f64,
    /// Max run lengths to track (truncation for O(1) amortized cost).
    pub bocpd_max_run: usize,
    /// Vol percentile below which → LowVol.
    pub vol_percentile_low: f64,
    /// Vol percentile above which → HighVol.
    pub vol_percentile_high: f64,
    /// Drawdown threshold for Crisis regime.
    pub crisis_drawdown: f64,
    /// Vol percentile threshold for Crisis (must exceed both drawdown AND vol).
    pub crisis_vol_percentile: f64,
}

impl Default for RegimeConfig {
    fn default() -> Self {
        Self {
            bocpd_hazard: 250.0,
            bocpd_max_run: 300,
            vol_percentile_low: 0.30,
            vol_percentile_high: 0.70,
            crisis_drawdown: -0.05,
            crisis_vol_percentile: 0.90,
        }
    }
}

// ---------------------------------------------------------------------------
// BOCPD — Bayesian Online Changepoint Detection
// ---------------------------------------------------------------------------

/// Online Normal-Gamma sufficient statistics for conjugate updating.
/// Tracks per run-length sufficient stats for the predictive Student-t.
#[derive(Debug, Clone)]
struct NormalGammaSS {
    mu0: f64,    // prior mean
    kappa0: f64, // prior pseudo-observations for mean
    alpha0: f64, // prior shape for variance
    beta0: f64,  // prior rate for variance
    // Per run-length accumulators
    n: Vec<f64>,      // observation count
    sum_x: Vec<f64>,  // sum of observations
    sum_x2: Vec<f64>, // sum of squared observations
}

impl NormalGammaSS {
    fn new(mu0: f64, kappa0: f64, alpha0: f64, beta0: f64, max_run: usize) -> Self {
        Self {
            mu0,
            kappa0,
            alpha0,
            beta0,
            n: vec![0.0; max_run],
            sum_x: vec![0.0; max_run],
            sum_x2: vec![0.0; max_run],
        }
    }

    /// Predictive log-probability of x under run-length r's posterior.
    /// The predictive is a Student-t distribution (conjugate result).
    fn log_predictive(&self, r: usize, x: f64) -> f64 {
        let n = self.n[r];
        let kappa_n = self.kappa0 + n;
        let alpha_n = self.alpha0 + n / 2.0;
        let mu_n = if n > 0.0 {
            (self.kappa0 * self.mu0 + self.sum_x[r]) / kappa_n
        } else {
            self.mu0
        };
        let beta_n = if n > 0.0 {
            self.beta0
                + 0.5 * (self.sum_x2[r] - self.sum_x[r].powi(2) / n)
                + self.kappa0 * n * (self.sum_x[r] / n - self.mu0).powi(2) / (2.0 * kappa_n)
        } else {
            self.beta0
        };

        // Student-t: df = 2*alpha_n, loc = mu_n, scale = sqrt(beta_n*(kappa_n+1)/(alpha_n*kappa_n))
        let df = 2.0 * alpha_n;
        let scale_sq = beta_n * (kappa_n + 1.0) / (alpha_n * kappa_n);
        let scale = scale_sq.sqrt();

        // Log PDF of Student-t
        log_student_t_pdf(x, df, mu_n, scale)
    }

    /// Shift sufficient stats: run-length r inherits from r-1, then add x.
    fn grow_and_add(&mut self, x: f64, max_r: usize) {
        // Shift stats right (newest run-length at index 0)
        let limit = max_r.min(self.n.len() - 1);
        for r in (1..=limit).rev() {
            self.n[r] = self.n[r - 1];
            self.sum_x[r] = self.sum_x[r - 1];
            self.sum_x2[r] = self.sum_x2[r - 1];
        }
        // Run-length 0: new segment starts with this observation
        self.n[0] = 0.0;
        self.sum_x[0] = 0.0;
        self.sum_x2[0] = 0.0;

        // Add x to all run lengths
        for r in 0..=limit {
            self.n[r] += 1.0;
            self.sum_x[r] += x;
            self.sum_x2[r] += x * x;
        }
    }
}

/// Log PDF of Student-t distribution.
fn log_student_t_pdf(x: f64, df: f64, mu: f64, scale: f64) -> f64 {
    let z = (x - mu) / scale;
    // log Γ((df+1)/2) - log Γ(df/2) - 0.5*log(df*π) - log(scale) - ((df+1)/2)*log(1 + z²/df)
    ln_gamma((df + 1.0) / 2.0)
        - ln_gamma(df / 2.0)
        - 0.5 * (df * std::f64::consts::PI).ln()
        - scale.ln()
        - ((df + 1.0) / 2.0) * (1.0 + z * z / df).ln()
}

/// Stirling's approximation for ln(Γ(x)), accurate for x > 0.5.
fn ln_gamma(x: f64) -> f64 {
    if x <= 0.0 {
        return f64::INFINITY;
    }
    // Lanczos approximation (g=7, n=9) for better accuracy at small x
    const G: f64 = 7.0;
    const C: [f64; 9] = [
        0.99999999999980993,
        676.5203681218851,
        -1259.1392167224028,
        771.32342877765313,
        -176.61502916214059,
        12.507343278686905,
        -0.13857109526572012,
        9.9843695780195716e-6,
        1.5056327351493116e-7,
    ];

    if x < 0.5 {
        // Reflection formula
        let sin_val = (std::f64::consts::PI * x).sin();
        if sin_val.abs() < 1e-300 {
            return f64::INFINITY;
        }
        return std::f64::consts::PI.ln() - sin_val.abs().ln() - ln_gamma(1.0 - x);
    }

    let x = x - 1.0;
    let mut sum = C[0];
    for (i, &c) in C[1..].iter().enumerate() {
        sum += c / (x + i as f64 + 1.0);
    }
    let t = x + G + 0.5;
    0.5 * (2.0 * std::f64::consts::PI).ln() + (t.ln() * (x + 0.5)) - t + sum.ln()
}

/// BOCPD online changepoint detector with run-length truncation.
#[derive(Debug, Clone)]
pub struct Bocpd {
    /// Hazard rate: 1/expected_run_length.
    hazard: f64,
    max_run: usize,
    /// Log run-length probabilities (indexed by run length).
    log_probs: Vec<f64>,
    /// Sufficient statistics for each run length.
    ss: NormalGammaSS,
    /// Number of observations processed.
    count: usize,
    /// Current changepoint probability (run-length 0 posterior mass).
    changepoint_prob: f64,
    /// MAP (most probable) run-length. Short = recent regime change.
    map_run_length: usize,
}

impl Bocpd {
    pub fn from_config(config: &RegimeConfig) -> Self {
        let max_run = config.bocpd_max_run;
        Self {
            hazard: 1.0 / config.bocpd_hazard,
            max_run,
            log_probs: vec![f64::NEG_INFINITY; max_run],
            ss: NormalGammaSS::new(0.0, 0.1, 1.0, 0.001, max_run),
            count: 0,
            changepoint_prob: 0.0,
            map_run_length: 0,
        }
    }

    /// Feed a new observation (log return). Returns changepoint probability.
    pub fn update(&mut self, x: f64) -> f64 {
        let h = self.hazard;
        let log_h = h.ln();
        let log_1mh = (1.0 - h).ln();

        if self.count == 0 {
            // Initialize: all mass at run-length 0
            self.log_probs[0] = 0.0; // log(1.0)
            self.ss.n[0] = 1.0;
            self.ss.sum_x[0] = x;
            self.ss.sum_x2[0] = x * x;
            self.count = 1;
            self.changepoint_prob = 0.0;
            return 0.0;
        }

        let limit = self.count.min(self.max_run - 1);

        // 1. Compute predictive log-probabilities for each run length
        let mut log_pred = vec![f64::NEG_INFINITY; limit + 1];
        for r in 0..=limit {
            if self.log_probs[r] > f64::NEG_INFINITY {
                log_pred[r] = self.ss.log_predictive(r, x);
            }
        }

        // 2. Compute growth probabilities: P(r_t = r+1) = P(r_{t-1} = r) * (1-H) * pred(x|r)
        let mut new_log_probs = vec![f64::NEG_INFINITY; self.max_run];
        for r in 0..limit {
            if self.log_probs[r] > f64::NEG_INFINITY {
                let new_r = r + 1;
                if new_r < self.max_run {
                    new_log_probs[new_r] = self.log_probs[r] + log_1mh + log_pred[r];
                }
            }
        }

        // 3. Changepoint probability: P(r_t = 0) = Σ P(r_{t-1} = r) * H * pred(x|r)
        let mut log_cp_terms = Vec::new();
        for r in 0..=limit {
            if self.log_probs[r] > f64::NEG_INFINITY {
                log_cp_terms.push(self.log_probs[r] + log_h + log_pred[r]);
            }
        }
        new_log_probs[0] = log_sum_exp(&log_cp_terms);

        // 4. Normalize (log-space)
        let log_total = log_sum_exp(
            &new_log_probs
                .iter()
                .copied()
                .filter(|&v| v > f64::NEG_INFINITY)
                .collect::<Vec<_>>(),
        );
        for p in &mut new_log_probs {
            if *p > f64::NEG_INFINITY {
                *p -= log_total;
            }
        }

        // 5. Update sufficient statistics
        self.ss.grow_and_add(x, self.count.min(self.max_run - 2));

        self.log_probs = new_log_probs;
        self.count += 1;
        self.changepoint_prob = self.log_probs[0].exp().min(1.0);

        // MAP run-length: the most probable current run length
        self.map_run_length = self
            .log_probs
            .iter()
            .enumerate()
            .filter(|(_, p)| **p > f64::NEG_INFINITY)
            .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(i, _)| i)
            .unwrap_or(0);

        self.changepoint_prob
    }

    pub fn changepoint_prob(&self) -> f64 {
        self.changepoint_prob
    }

    /// Most probable current run length. Short values indicate a recent regime change.
    pub fn map_run_length(&self) -> usize {
        self.map_run_length
    }

    pub fn count(&self) -> usize {
        self.count
    }
}

/// Log-sum-exp for numerical stability.
fn log_sum_exp(log_vals: &[f64]) -> f64 {
    if log_vals.is_empty() {
        return f64::NEG_INFINITY;
    }
    let max = log_vals.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    if max == f64::NEG_INFINITY {
        return f64::NEG_INFINITY;
    }
    max + log_vals.iter().map(|&v| (v - max).exp()).sum::<f64>().ln()
}

// ---------------------------------------------------------------------------
// Vol percentile tracker (rolling percentile of GARCH vol)
// ---------------------------------------------------------------------------

/// Rolling percentile tracker for volatility regime classification.
/// Maintains a sorted window of recent GARCH vol values.
#[derive(Debug, Clone)]
pub struct VolPercentile {
    buffer: Vec<f64>,
    capacity: usize,
    pos: usize,
    full: bool,
}

impl VolPercentile {
    pub fn new(window: usize) -> Self {
        Self {
            buffer: vec![0.0; window],
            capacity: window,
            pos: 0,
            full: false,
        }
    }

    /// Push a new vol value, return its percentile rank (0.0-1.0).
    pub fn push(&mut self, vol: f64) -> f64 {
        self.buffer[self.pos] = vol;
        self.pos += 1;
        if self.pos >= self.capacity {
            self.pos = 0;
            self.full = true;
        }

        let count = if self.full { self.capacity } else { self.pos };
        if count < 2 {
            return 0.5;
        }

        // Count how many values are below current vol
        let below = self.buffer[..count].iter().filter(|&&v| v < vol).count();
        below as f64 / (count - 1) as f64
    }
}

// ---------------------------------------------------------------------------
// Regime classifier — combines BOCPD + vol percentile + drawdown
// ---------------------------------------------------------------------------

/// Classify market regime from vol percentile, BOCPD state, and drawdown.
///
/// Uses MAP run-length as the changepoint signal: a short MAP run-length
/// (< 20 bars) indicates a recent regime change → be defensive.
pub fn classify_regime(
    vol_percentile: f64,
    map_run_length: usize,
    recent_drawdown: f64,
    config: &RegimeConfig,
) -> MarketRegime {
    // Crisis: extreme vol + significant drawdown
    if recent_drawdown < config.crisis_drawdown && vol_percentile > config.crisis_vol_percentile {
        return MarketRegime::Crisis;
    }

    // Short MAP run-length = recent regime change → be defensive
    if map_run_length < 20 && vol_percentile > config.vol_percentile_low {
        return MarketRegime::HighVol;
    }

    if vol_percentile < config.vol_percentile_low {
        MarketRegime::LowVol
    } else if vol_percentile > config.vol_percentile_high {
        MarketRegime::HighVol
    } else {
        MarketRegime::Normal
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bocpd_map_run_length_grows_during_stable() {
        let config = RegimeConfig {
            bocpd_hazard: 50.0,
            bocpd_max_run: 200,
            ..Default::default()
        };
        let mut bocpd = Bocpd::from_config(&config);

        // 100 bars of stable returns → MAP run-length should grow
        for _ in 0..100 {
            bocpd.update(0.001);
        }

        assert!(
            bocpd.map_run_length() > 50,
            "MAP run-length should be long during stable period: {}",
            bocpd.map_run_length()
        );
    }

    #[test]
    fn bocpd_map_run_length_resets_after_shift() {
        let config = RegimeConfig {
            bocpd_hazard: 50.0,
            bocpd_max_run: 200,
            ..Default::default()
        };
        let mut bocpd = Bocpd::from_config(&config);

        // 100 bars of calm
        for _ in 0..100 {
            bocpd.update(0.001);
        }
        let run_before = bocpd.map_run_length();

        // Sudden regime change: 20 bars of crash
        for _ in 0..20 {
            bocpd.update(-0.05);
        }

        // Switch back to calm — MAP should reset to short run-length
        for _ in 0..5 {
            bocpd.update(0.001);
        }
        let run_after = bocpd.map_run_length();

        assert!(
            run_after < run_before,
            "MAP run-length should be shorter after regime changes: {run_before} → {run_after}"
        );
    }

    #[test]
    fn bocpd_low_prob_during_stable_period() {
        let config = RegimeConfig {
            bocpd_hazard: 100.0,
            bocpd_max_run: 150,
            ..Default::default()
        };
        let mut bocpd = Bocpd::from_config(&config);

        // 100 bars of stable returns
        for _ in 0..100 {
            bocpd.update(0.001);
        }

        assert!(
            bocpd.changepoint_prob() < 0.1,
            "stable period should have low changepoint prob: {:.4}",
            bocpd.changepoint_prob()
        );
    }

    #[test]
    fn vol_percentile_basic() {
        let mut vp = VolPercentile::new(20);

        // Push 20 increasing values
        for i in 1..=20 {
            vp.push(i as f64);
        }

        // Lowest value should be near 0th percentile
        let p_low = vp.push(0.5);
        assert!(p_low < 0.1, "lowest vol should be low percentile: {p_low}");

        // Highest value should be near 100th percentile
        let p_high = vp.push(100.0);
        assert!(
            p_high > 0.9,
            "highest vol should be high percentile: {p_high}"
        );
    }

    #[test]
    fn vol_percentile_single_value() {
        let mut vp = VolPercentile::new(10);
        let p = vp.push(1.0);
        assert_eq!(p, 0.5, "single value should be 50th percentile");
    }

    #[test]
    fn regime_crisis_requires_both_conditions() {
        let config = RegimeConfig::default();

        // High vol but no drawdown → HighVol, not Crisis
        let r = classify_regime(0.95, 100, -0.01, &config);
        assert_eq!(r, MarketRegime::HighVol);

        // Drawdown but low vol → not Crisis
        let r = classify_regime(0.5, 100, -0.10, &config);
        assert_eq!(r, MarketRegime::Normal);

        // Both → Crisis
        let r = classify_regime(0.95, 100, -0.10, &config);
        assert_eq!(r, MarketRegime::Crisis);
    }

    #[test]
    fn regime_classification_levels() {
        let config = RegimeConfig::default();

        assert_eq!(
            classify_regime(0.1, 100, 0.0, &config),
            MarketRegime::LowVol
        );
        assert_eq!(
            classify_regime(0.5, 100, 0.0, &config),
            MarketRegime::Normal
        );
        assert_eq!(
            classify_regime(0.8, 100, 0.0, &config),
            MarketRegime::HighVol
        );
    }

    #[test]
    fn regime_short_run_length_overrides_to_high_vol() {
        let config = RegimeConfig::default();
        // Low vol percentile but short MAP run = recent regime change → HighVol
        let r = classify_regime(0.5, 5, 0.0, &config);
        assert_eq!(r, MarketRegime::HighVol);
    }

    #[test]
    fn ln_gamma_known_values() {
        // Γ(1) = 1, ln(1) = 0
        assert!((ln_gamma(1.0)).abs() < 1e-6);
        // Γ(2) = 1, ln(1) = 0
        assert!((ln_gamma(2.0)).abs() < 1e-6);
        // Γ(0.5) = √π, ln(√π) ≈ 0.5724
        assert!((ln_gamma(0.5) - 0.5723649).abs() < 1e-4);
        // Γ(5) = 24, ln(24) ≈ 3.1781
        assert!((ln_gamma(5.0) - 24.0_f64.ln()).abs() < 1e-6);
    }

    #[test]
    fn log_sum_exp_accuracy() {
        let vals = vec![1.0_f64.ln(), 2.0_f64.ln(), 3.0_f64.ln()];
        let result = log_sum_exp(&vals).exp();
        assert!((result - 6.0).abs() < 1e-10, "1+2+3=6, got {result}");
    }

    #[test]
    fn log_sum_exp_empty() {
        assert_eq!(log_sum_exp(&[]), f64::NEG_INFINITY);
    }

    #[test]
    fn bocpd_count_tracks() {
        let mut bocpd = Bocpd::from_config(&RegimeConfig::default());
        assert_eq!(bocpd.count(), 0);
        bocpd.update(0.01);
        assert_eq!(bocpd.count(), 1);
        bocpd.update(-0.01);
        assert_eq!(bocpd.count(), 2);
    }
}
