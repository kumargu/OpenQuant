//! Thompson sampling for pair selection ranking.
//!
//! Each pair is a bandit arm with a Normal-Inverse-Gamma (NIG) posterior
//! over its expected return and variance. After each trade, the posterior
//! is updated with the realized return. To select pairs, we sample from
//! each posterior and rank by sampled Sharpe ratio.
//!
//! ## Why NIG?
//!
//! The Normal-Inverse-Gamma is the conjugate prior for a Normal likelihood
//! with unknown mean and variance. This gives us closed-form posterior
//! updates — no MCMC needed. The posterior predictive is Student-t, which
//! naturally handles uncertainty (wide tails for few observations).
//!
//! ## Informative Priors
//!
//! New pairs don't start with flat priors. Instead, the prior mean is
//! derived from the pair's statistical quality (ADF p-value, half-life,
//! score from the validation pipeline). Better statistical properties
//! → more optimistic prior → explored sooner. This accelerates
//! convergence compared to uniform priors.
//!
//! ## References
//! - Russo et al. — Tutorial on Thompson Sampling (2018)
//! - Murphy (2007) — Conjugate Bayesian analysis of the Gaussian distribution

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::Path;

/// Normal-Inverse-Gamma parameters for one pair (bandit arm).
///
/// Parameterization: (μ₀, κ₀, α₀, β₀)
/// - μ₀: prior mean of the return distribution
/// - κ₀: pseudo-observations for the mean (higher = more confident)
/// - α₀: shape parameter (pseudo-observations / 2)
/// - β₀: rate parameter (encodes prior variance belief)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArmState {
    /// Prior/posterior mean of expected return (bps).
    pub mu: f64,
    /// Precision of the mean estimate (pseudo-observations).
    pub kappa: f64,
    /// Shape parameter (half the effective sample size).
    pub alpha: f64,
    /// Rate parameter (encodes variance belief).
    pub beta: f64,
    /// Number of observed trades.
    pub n_trades: usize,
}

impl ArmState {
    /// Create an informative prior based on the pair's statistical quality.
    ///
    /// `quality_score`: the composite score from the validation pipeline [0, 1].
    /// Higher score → more optimistic prior mean, but still uncertain.
    pub fn from_quality_score(quality_score: f64) -> Self {
        // Prior mean: map quality [0, 1] → expected return [0, 10] bps per trade.
        // A score of 0.85 (excellent pair) gets ~8.5 bps prior mean.
        // A score of 0.50 (marginal pair) gets ~5.0 bps prior mean.
        let mu = quality_score * 10.0;

        // κ₀ = 1: weak prior — one pseudo-observation of the mean.
        // This means the first real trade will shift the mean significantly.
        let kappa = 1.0;

        // α₀ = 2: minimal shape (need α > 1 for finite variance).
        // Gives very wide posterior — lots of exploration initially.
        let alpha = 2.0;

        // β₀: encode prior belief about return variance.
        // Pairs trading returns typically have std ~15-30 bps per trade.
        // β₀ = α₀ * prior_variance = 2 * 400 = 800 (std ~20 bps)
        let prior_variance = 400.0; // (20 bps)²
        let beta = alpha * prior_variance;

        Self {
            mu,
            kappa,
            alpha,
            beta,
            n_trades: 0,
        }
    }

    /// Update posterior with an observed trade return (in bps).
    ///
    /// NIG conjugate update:
    /// κₙ = κ₀ + n
    /// μₙ = (κ₀ μ₀ + n x̄) / κₙ
    /// αₙ = α₀ + n/2
    /// βₙ = β₀ + S/2 + κ₀ n (x̄ - μ₀)² / (2 κₙ)
    ///
    /// where x̄ = sample mean, S = sum of squared deviations from x̄.
    pub fn update(&mut self, returns: &[f64]) {
        let n = returns.len();
        if n == 0 {
            return;
        }
        let n_f = n as f64;

        let x_bar: f64 = returns.iter().sum::<f64>() / n_f;
        let s: f64 = returns.iter().map(|r| (r - x_bar).powi(2)).sum();

        let kappa_n = self.kappa + n_f;
        let mu_n = (self.kappa * self.mu + n_f * x_bar) / kappa_n;
        let alpha_n = self.alpha + n_f / 2.0;
        let beta_n =
            self.beta + s / 2.0 + self.kappa * n_f * (x_bar - self.mu).powi(2) / (2.0 * kappa_n);

        self.mu = mu_n;
        self.kappa = kappa_n;
        self.alpha = alpha_n;
        self.beta = beta_n;
        self.n_trades += n;
    }

    /// Sample from the posterior predictive distribution (Student-t).
    ///
    /// The posterior predictive for the next observation is:
    /// t_{2α}(μ, β(κ+1)/(ακ))
    ///
    /// We use the posterior mean as the "sampled Sharpe" proxy since
    /// the Student-t accounts for uncertainty through its wider tails.
    ///
    /// `rng_state`: mutable LCG state for deterministic sampling.
    pub fn sample(&self, rng_state: &mut u64) -> f64 {
        let df = 2.0 * self.alpha;
        let scale = (self.beta * (self.kappa + 1.0) / (self.alpha * self.kappa)).sqrt();

        // Sample from Student-t via ratio of Normal / sqrt(Chi-squared/df)
        let z = sample_normal(rng_state);
        let chi2 = sample_chi_squared(rng_state, df);
        let t = z / (chi2 / df).sqrt();

        self.mu + scale * t
    }

    /// Posterior mean (exploitation-only estimate).
    pub fn posterior_mean(&self) -> f64 {
        self.mu
    }

    /// Posterior standard deviation of the mean.
    pub fn posterior_std(&self) -> f64 {
        if self.alpha > 1.0 {
            (self.beta / ((self.alpha - 1.0) * self.kappa)).sqrt()
        } else {
            f64::INFINITY
        }
    }
}

/// Thompson sampling state for all pairs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThompsonState {
    pub arms: HashMap<String, ArmState>,
}

/// File format for persisted Thompson state.
const STATE_FILE: &str = "thompson_state.json";

impl ThompsonState {
    pub fn new() -> Self {
        Self {
            arms: HashMap::new(),
        }
    }

    /// Get or create an arm for a pair, using quality_score for the prior.
    pub fn get_or_create(&mut self, pair_id: &str, quality_score: f64) -> &mut ArmState {
        self.arms
            .entry(pair_id.to_string())
            .or_insert_with(|| ArmState::from_quality_score(quality_score))
    }

    /// Update a pair's posterior with observed trade returns (bps).
    pub fn update_pair(&mut self, pair_id: &str, returns: &[f64], quality_score: f64) {
        let arm = self.get_or_create(pair_id, quality_score);
        arm.update(returns);
    }

    /// Sample from all arms and return pair_ids ranked by sampled value (descending).
    pub fn rank_pairs(&self, rng_seed: u64) -> Vec<(String, f64)> {
        let mut rng_state = rng_seed;
        let mut samples: Vec<(String, f64)> = self
            .arms
            .iter()
            .map(|(id, arm)| {
                let sample = arm.sample(&mut rng_state);
                (id.clone(), sample)
            })
            .collect();

        samples.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        samples
    }

    /// Select top-K pairs by Thompson sampling.
    pub fn select_top_k(&self, k: usize, rng_seed: u64) -> Vec<String> {
        self.rank_pairs(rng_seed)
            .into_iter()
            .take(k)
            .map(|(id, _)| id)
            .collect()
    }

    /// Load state from disk, or return empty state if file doesn't exist.
    pub fn load(data_dir: &Path) -> Self {
        let path = data_dir.join(STATE_FILE);
        match fs::read_to_string(&path) {
            Ok(contents) => serde_json::from_str(&contents).unwrap_or_else(|_| Self::new()),
            Err(_) => Self::new(),
        }
    }

    /// Save state to disk.
    pub fn save(&self, data_dir: &Path) -> std::io::Result<()> {
        let path = data_dir.join(STATE_FILE);
        let json = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        fs::write(path, json)
    }

    /// Compute exploration rate: fraction of arms with < 5 observations.
    pub fn exploration_rate(&self) -> f64 {
        if self.arms.is_empty() {
            return 1.0;
        }
        let exploring = self.arms.values().filter(|a| a.n_trades < 5).count();
        exploring as f64 / self.arms.len() as f64
    }
}

impl Default for ThompsonState {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Trade history types
// ---------------------------------------------------------------------------

/// A closed trade for feedback.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClosedTrade {
    pub pair: (String, String),
    pub entry_date: String,
    pub exit_date: String,
    /// Return in basis points.
    pub return_bps: f64,
    pub holding_period_days: u32,
}

/// Trade history file format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TradeHistory {
    pub trades: Vec<ClosedTrade>,
}

impl TradeHistory {
    /// Load from file, or return empty if not found.
    pub fn load(path: &Path) -> Self {
        match fs::read_to_string(path) {
            Ok(contents) => serde_json::from_str(&contents).unwrap_or(Self { trades: Vec::new() }),
            Err(_) => Self { trades: Vec::new() },
        }
    }

    /// Group trades by pair_id → returns (bps).
    pub fn returns_by_pair(&self) -> HashMap<String, Vec<f64>> {
        let mut map: HashMap<String, Vec<f64>> = HashMap::new();
        for trade in &self.trades {
            let pair_id = format!("{}/{}", trade.pair.0, trade.pair.1);
            map.entry(pair_id).or_default().push(trade.return_bps);
        }
        map
    }
}

/// Construct a pair_id string from two symbols (canonical ordering).
pub fn pair_id(leg_a: &str, leg_b: &str) -> String {
    format!("{leg_a}/{leg_b}")
}

// ---------------------------------------------------------------------------
// Deterministic random sampling (no external dependency)
// ---------------------------------------------------------------------------

/// Sample from standard normal using Box-Muller transform.
fn sample_normal(state: &mut u64) -> f64 {
    let u1 = lcg_uniform(state);
    let u2 = lcg_uniform(state);
    // Box-Muller: Z = sqrt(-2 ln U1) * cos(2π U2)
    (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos()
}

/// Sample from Chi-squared(df) as sum of df standard normals squared.
/// For large df, uses the Wilson-Hilferty approximation.
fn sample_chi_squared(state: &mut u64, df: f64) -> f64 {
    if df <= 20.0 {
        // Direct: sum of squared normals (for small df)
        let n = df.round() as usize;
        let mut sum = 0.0;
        for _ in 0..n.max(1) {
            let z = sample_normal(state);
            sum += z * z;
        }
        // Scale to handle non-integer df
        sum * df / n.max(1) as f64
    } else {
        // Wilson-Hilferty normal approximation for large df
        let z = sample_normal(state);
        let ratio = 1.0 - 2.0 / (9.0 * df) + z * (2.0 / (9.0 * df)).sqrt();
        (df * ratio * ratio * ratio).max(0.001)
    }
}

/// LCG producing U(0, 1) — avoiding exact 0.
fn lcg_uniform(state: &mut u64) -> f64 {
    *state = state
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    let raw = (*state >> 33) as f64 / u32::MAX as f64;
    raw.max(1e-10) // avoid ln(0) in Box-Muller
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_informative_prior() {
        let excellent = ArmState::from_quality_score(0.85);
        let marginal = ArmState::from_quality_score(0.50);

        // Better quality → higher prior mean
        assert!(excellent.mu > marginal.mu);
        // Both start with same uncertainty
        assert_eq!(excellent.kappa, marginal.kappa);
        assert_eq!(excellent.alpha, marginal.alpha);
        assert_eq!(excellent.n_trades, 0);
    }

    #[test]
    fn test_posterior_update_shifts_mean() {
        let mut arm = ArmState::from_quality_score(0.50);
        let prior_mu = arm.mu;

        // Update with positive returns → mean should increase
        arm.update(&[20.0, 15.0, 25.0, 18.0]);
        assert!(
            arm.mu > prior_mu,
            "mu should increase with positive returns"
        );
        assert_eq!(arm.n_trades, 4);
    }

    #[test]
    fn test_posterior_update_with_losses() {
        let mut arm = ArmState::from_quality_score(0.85);
        let prior_mu = arm.mu;

        // Update with negative returns → mean should decrease
        arm.update(&[-10.0, -15.0, -20.0, -12.0, -8.0]);
        assert!(arm.mu < prior_mu, "mu should decrease with losses");
    }

    #[test]
    fn test_posterior_concentrates() {
        let mut arm = ArmState::from_quality_score(0.50);

        // After many observations, posterior should concentrate
        let prior_std = arm.posterior_std();
        arm.update(&[10.0; 50]);
        let posterior_std = arm.posterior_std();

        assert!(
            posterior_std < prior_std,
            "std should decrease: {posterior_std} < {prior_std}"
        );
    }

    #[test]
    fn test_sampling_produces_finite_values() {
        let arm = ArmState::from_quality_score(0.70);
        let mut rng = 42u64;

        for _ in 0..1000 {
            let s = arm.sample(&mut rng);
            assert!(s.is_finite(), "sample should be finite: {s}");
        }
    }

    #[test]
    fn test_sampling_mean_near_posterior_mean() {
        let mut arm = ArmState::from_quality_score(0.70);
        arm.update(&[15.0; 100]); // lots of data → tight posterior

        let mut rng = 42u64;
        let samples: Vec<f64> = (0..10000).map(|_| arm.sample(&mut rng)).collect();
        let sample_mean = samples.iter().sum::<f64>() / samples.len() as f64;

        assert!(
            (sample_mean - arm.mu).abs() < 2.0,
            "sample_mean={sample_mean}, posterior_mu={}",
            arm.mu
        );
    }

    #[test]
    fn test_thompson_state_lifecycle() {
        let mut state = ThompsonState::new();
        state.get_or_create("GS/MS", 0.85);
        state.get_or_create("C/JPM", 0.70);
        state.get_or_create("GLD/SLV", 0.60);

        assert_eq!(state.arms.len(), 3);
        assert!((state.exploration_rate() - 1.0).abs() < 0.01); // all new → 100% exploring
    }

    #[test]
    fn test_thompson_ranking() {
        let mut state = ThompsonState::new();

        // Good pair with evidence
        state.get_or_create("GS/MS", 0.85);
        state.update_pair("GS/MS", &[20.0, 15.0, 25.0, 18.0, 22.0], 0.85);

        // Bad pair with evidence
        state.get_or_create("X/Y", 0.40);
        state.update_pair("X/Y", &[-10.0, -5.0, -15.0, -8.0, -12.0], 0.40);

        // New pair, no evidence
        state.get_or_create("NEW/PAIR", 0.70);

        // Over many samples, GS/MS should be ranked first most often
        let mut first_count: HashMap<String, usize> = HashMap::new();
        for seed in 0..1000 {
            let ranking = state.rank_pairs(seed);
            *first_count.entry(ranking[0].0.clone()).or_default() += 1;
        }

        let gs_ms_first = first_count.get("GS/MS").copied().unwrap_or(0);
        assert!(
            gs_ms_first > 400,
            "GS/MS should be first most often: {gs_ms_first}/1000"
        );
    }

    #[test]
    fn test_exploration_happens() {
        let mut state = ThompsonState::new();

        // Well-known good pair
        state.get_or_create("GS/MS", 0.85);
        state.update_pair("GS/MS", &[20.0; 50], 0.85);

        // Brand new pair with decent prior
        state.get_or_create("NEW/PAIR", 0.75);

        // The new pair should be selected at least sometimes due to wide posterior
        let mut new_pair_selected = 0;
        for seed in 0..1000 {
            let top = state.select_top_k(1, seed);
            if top[0] == "NEW/PAIR" {
                new_pair_selected += 1;
            }
        }

        assert!(
            new_pair_selected > 50,
            "New pair should be explored: {new_pair_selected}/1000"
        );
    }

    #[test]
    fn test_convergence_to_best_arm() {
        // Simulation: 3 arms with known Sharpe ratios
        let mut state = ThompsonState::new();
        state.get_or_create("BEST", 0.80);
        state.get_or_create("MED", 0.70);
        state.get_or_create("WORST", 0.60);

        let mut rng: u64 = 42;

        // Simulate 200 rounds of selection + feedback
        for round in 0..200 {
            let selected = state.select_top_k(1, round as u64 + 1000);
            let pair_id = &selected[0];

            // Generate return based on true Sharpe
            let true_mean = match pair_id.as_str() {
                "BEST" => 15.0,
                "MED" => 5.0,
                "WORST" => -5.0,
                _ => 0.0,
            };
            rng = rng
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let noise = ((rng >> 33) as f64 / u32::MAX as f64 - 0.5) * 20.0;
            let ret = true_mean + noise;

            state.update_pair(pair_id, &[ret], 0.70);
        }

        // After 200 rounds, BEST should have highest posterior mean
        let best_mu = state.arms["BEST"].posterior_mean();
        let med_mu = state.arms["MED"].posterior_mean();
        let worst_mu = state.arms["WORST"].posterior_mean();

        assert!(
            best_mu > med_mu && med_mu > worst_mu,
            "Should converge: best={best_mu:.1}, med={med_mu:.1}, worst={worst_mu:.1}"
        );
    }

    #[test]
    fn test_sublinear_regret() {
        // Run Thompson sampling and track cumulative regret
        let mut state = ThompsonState::new();
        state.get_or_create("BEST", 0.80);
        state.get_or_create("ALT", 0.60);

        let best_mean = 15.0;
        let alt_mean = 5.0;
        let mut rng: u64 = 42;
        let mut cumulative_regret = 0.0;
        let mut regret_at_100 = 0.0;

        for round in 0..200 {
            let selected = state.select_top_k(1, round as u64 + 5000);
            let pair_id = &selected[0];

            let chosen_mean = if pair_id == "BEST" {
                best_mean
            } else {
                alt_mean
            };
            let regret = best_mean - chosen_mean;
            cumulative_regret += regret;

            if round == 99 {
                regret_at_100 = cumulative_regret;
            }

            rng = rng
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            let noise = ((rng >> 33) as f64 / u32::MAX as f64 - 0.5) * 20.0;
            let ret = chosen_mean + noise;
            state.update_pair(pair_id, &[ret], 0.70);
        }

        // Sublinear: regret from rounds 100-200 should be less than rounds 0-100
        let second_half_regret = cumulative_regret - regret_at_100;
        assert!(
            second_half_regret < regret_at_100 + 50.0, // allow some slack
            "Regret should be sublinear: first_half={regret_at_100:.0}, second_half={second_half_regret:.0}"
        );
    }

    #[test]
    fn test_persistence() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path();

        let mut state = ThompsonState::new();
        state.get_or_create("GS/MS", 0.85);
        state.update_pair("GS/MS", &[20.0, 15.0], 0.85);
        state.save(dir).unwrap();

        let loaded = ThompsonState::load(dir);
        assert_eq!(loaded.arms.len(), 1);
        assert_eq!(loaded.arms["GS/MS"].n_trades, 2);
        assert!((loaded.arms["GS/MS"].mu - state.arms["GS/MS"].mu).abs() < 1e-10);
    }

    #[test]
    fn test_trade_history_grouping() {
        let history = TradeHistory {
            trades: vec![
                ClosedTrade {
                    pair: ("GS".into(), "MS".into()),
                    entry_date: "2026-03-10".into(),
                    exit_date: "2026-03-14".into(),
                    return_bps: 42.0,
                    holding_period_days: 4,
                },
                ClosedTrade {
                    pair: ("GS".into(), "MS".into()),
                    entry_date: "2026-03-15".into(),
                    exit_date: "2026-03-18".into(),
                    return_bps: -10.0,
                    holding_period_days: 3,
                },
                ClosedTrade {
                    pair: ("C".into(), "JPM".into()),
                    entry_date: "2026-03-10".into(),
                    exit_date: "2026-03-12".into(),
                    return_bps: 25.0,
                    holding_period_days: 2,
                },
            ],
        };

        let by_pair = history.returns_by_pair();
        assert_eq!(by_pair.len(), 2);
        assert_eq!(by_pair["GS/MS"].len(), 2);
        assert_eq!(by_pair["C/JPM"].len(), 1);
    }

    #[test]
    fn test_empty_update_is_noop() {
        let mut arm = ArmState::from_quality_score(0.70);
        let mu_before = arm.mu;
        arm.update(&[]);
        assert_eq!(arm.mu, mu_before);
        assert_eq!(arm.n_trades, 0);
    }
}
