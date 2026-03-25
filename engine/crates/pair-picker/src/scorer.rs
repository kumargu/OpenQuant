//! Pair scoring — combines statistical test results into a composite score.
//!
//! Score components (all normalized to [0, 1]):
//! - Cointegration strength: lower ADF p-value → higher score
//! - Half-life quality: closer to ideal range (5-15 days) → higher score
//! - Beta stability: lower CV → higher score
//! - R² of hedge ratio: higher → higher score

/// Configuration for HL-adaptive max hold time.
///
/// OU theory: after `k` half-lives, the expected reversion is `1 - 2^{-k}`.
/// Setting `max_hold = multiplier * half_life` targets a fixed fraction of
/// expected reversion before the timeout.  The cap prevents excessive hold
/// times for slow pairs.
///
/// Default: 2.5× half-life, capped at 10 days → ~82% expected reversion.
///
/// Reference: Ornstein-Uhlenbeck process theory; see also Krauss (2017),
/// "Statistical Arbitrage Pairs Trading Strategies: Review and Outlook".
#[derive(Debug, Clone)]
pub struct MaxHoldConfig {
    /// Multiplier applied to the OU half-life to derive max hold duration.
    /// Default: 2.5 (targets ~82% reversion: 1 - 2^{-2.5} ≈ 0.82).
    pub hold_multiplier: f64,
    /// Hard cap on max hold in days regardless of half-life.
    /// Default: 10 days.
    pub max_hold_cap: usize,
}

impl Default for MaxHoldConfig {
    fn default() -> Self {
        Self {
            hold_multiplier: 2.5,
            max_hold_cap: 10,
        }
    }
}

/// Compute per-pair max hold days from OU half-life.
///
/// Formula: `min(ceil(hold_multiplier * half_life), max_hold_cap)`
///
/// # Panics / edge cases
/// - `half_life` must be finite and > 0; returns `max_hold_cap` otherwise
///   (guards against NaN/Inf propagating into the trading engine).
/// - Fractional results are rounded **up** (`ceil`) so the pair always gets
///   at least the full expected reversion window.
///
/// # Examples
/// ```
/// use pair_picker::scorer::{MaxHoldConfig, compute_max_hold_days};
/// let cfg = MaxHoldConfig::default(); // multiplier=2.5, cap=10
/// assert_eq!(compute_max_hold_days(2.0, &cfg), 5);  // 2.5*2.0=5.0 → 5
/// assert_eq!(compute_max_hold_days(5.0, &cfg), 10); // 2.5*5.0=12.5 → capped at 10
/// ```
pub fn compute_max_hold_days(half_life: f64, config: &MaxHoldConfig) -> usize {
    if !half_life.is_finite() || half_life <= 0.0 {
        // Defensive: bad half-life → use the cap (safe upper bound)
        return config.max_hold_cap;
    }
    let raw = config.hold_multiplier * half_life;
    let days = raw.ceil() as usize;
    days.min(config.max_hold_cap)
}

/// Configurable scoring weights. Will be superseded by Thompson sampling (#119),
/// but kept configurable so they can be tuned via CLI or config file in the meantime.
#[derive(Debug, Clone)]
pub struct ScoringConfig {
    pub w_cointegration: f64,
    pub w_half_life: f64,
    pub w_stability: f64,
    pub w_fit: f64,
}

impl Default for ScoringConfig {
    fn default() -> Self {
        Self {
            w_cointegration: 0.35,
            w_half_life: 0.25,
            w_stability: 0.25,
            w_fit: 0.15,
        }
    }
}

/// Compute composite score from validation results using default weights.
///
/// Returns a score in [0, 1] where higher = better pair.
pub fn compute_score(
    adf_pvalue: f64,
    half_life: f64,
    beta_cv: f64,
    r_squared: f64,
    structural_break: bool,
) -> f64 {
    compute_score_with_config(
        adf_pvalue,
        half_life,
        beta_cv,
        r_squared,
        structural_break,
        &ScoringConfig::default(),
    )
}

/// Compute composite score with custom weights.
pub fn compute_score_with_config(
    adf_pvalue: f64,
    half_life: f64,
    beta_cv: f64,
    r_squared: f64,
    structural_break: bool,
    config: &ScoringConfig,
) -> f64 {
    let coint_score = cointegration_score(adf_pvalue);
    let hl_score = half_life_score(half_life);
    let stability_score = beta_stability_score(beta_cv, structural_break);
    let fit_score = if r_squared.is_finite() {
        r_squared.clamp(0.0, 1.0)
    } else {
        0.0
    };

    let raw = config.w_cointegration * coint_score
        + config.w_half_life * hl_score
        + config.w_stability * stability_score
        + config.w_fit * fit_score;

    raw.clamp(0.0, 1.0)
}

/// ADF p-value → cointegration score.
/// p < 0.01 → 1.0, p = 0.05 → 0.5, p > 0.10 → 0.0
fn cointegration_score(p_value: f64) -> f64 {
    if p_value <= 0.01 {
        1.0
    } else if p_value >= 0.10 {
        0.0
    } else {
        // Linear interpolation: [0.01, 0.10] → [1.0, 0.0]
        1.0 - (p_value - 0.01) / (0.10 - 0.01)
    }
}

/// Half-life → quality score.
/// Ideal range: 2-15 days (fast enough to trade intraday, slow enough to execute).
/// Valid range: 1-40 days (lowered from 3 to admit fast mean-reverting pairs).
fn half_life_score(hl: f64) -> f64 {
    if !(1.0..=40.0).contains(&hl) {
        return 0.0;
    }
    if (2.0..=15.0).contains(&hl) {
        return 1.0; // ideal
    }
    if hl < 2.0 {
        // 1-2: ramp up (very fast reversion — may be noise)
        hl - 1.0
    } else {
        // 15-40: ramp down
        1.0 - (hl - 15.0) / 25.0
    }
}

/// Beta stability → score.
/// CV < 0.05 → 1.0, CV = 0.20 → 0.0 (at threshold)
fn beta_stability_score(cv: f64, structural_break: bool) -> f64 {
    if structural_break {
        return 0.0;
    }
    if cv <= 0.05 {
        1.0
    } else if cv >= 0.20 {
        0.0
    } else {
        // Linear: [0.05, 0.20] → [1.0, 0.0]
        1.0 - (cv - 0.05) / (0.20 - 0.05)
    }
}

// ---------------------------------------------------------------------------
// Priority scoring for the signal queue
// ---------------------------------------------------------------------------
//
// When multiple pairs signal simultaneously, we must prioritise which to enter
// first.  Sorting by |z| alone ignores reversion speed and risk: a pair with
// z=3, fast reversion, and tight spread is better than one with z=3.5, slow
// reversion, and wide, noisy spread.
//
// Two complementary metrics are provided:
//
// 1. `compute_priority_score` — fast approximation, used for real-time ranking.
// 2. `expected_return_per_dollar_per_day` — common-unit metric for A/B comparison.
//
// References:
//   Avellaneda & Lee (2010), "Statistical Arbitrage in the US Equities Market",
//   Quantitative Finance 10(7): 761-782.
//   Lee, Leung & Ning (2023), "Optimal Mean Reversion Trading with Transaction Costs".

/// Configuration for the signal-queue priority scorer.
///
/// All parameters have `Default` impls; they can be overridden from TOML
/// without recompilation.
#[derive(Debug, Clone)]
pub struct PriorityConfig {
    /// Minimum sigma_spread to guard against division by near-zero.
    ///
    /// If the spread has negligibly small volatility the pair is essentially
    /// always flat — no meaningful P&L potential. Score is clamped to 0 below
    /// this threshold.
    ///
    /// Default: 1e-6 (in log-spread units; corresponds to ~0.0001% price move)
    pub min_sigma: f64,
    /// Minimum kappa (mean-reversion rate, per day) to guard against division
    /// by near-zero / negative kappa.
    ///
    /// Negative or zero kappa means the spread is not mean-reverting, so we
    /// assign a score of 0.
    ///
    /// Default: 1e-6
    pub min_kappa: f64,
}

impl Default for PriorityConfig {
    fn default() -> Self {
        Self {
            min_sigma: 1e-6,
            min_kappa: 1e-6,
        }
    }
}

/// Convert OU half-life (days) to mean-reversion rate κ (per day).
///
/// OU process: dS_t = -κ(S_t - μ) dt + σ dW_t
/// Half-life τ relates to κ by: τ = ln(2) / κ, so κ = ln(2) / τ.
///
/// Reference: Ornstein-Uhlenbeck process; see also Avellaneda & Lee (2010).
///
/// Returns `None` if `half_life_days` is not finite or ≤ 0.
///
/// # Examples
/// ```
/// use pair_picker::scorer::half_life_to_kappa;
/// // HL=10 days → κ = ln(2)/10 ≈ 0.0693
/// let k = half_life_to_kappa(10.0).unwrap();
/// assert!((k - f64::ln(2.0) / 10.0).abs() < 1e-12);
/// ```
pub fn half_life_to_kappa(half_life_days: f64) -> Option<f64> {
    if !half_life_days.is_finite() || half_life_days <= 0.0 {
        return None;
    }
    Some(f64::ln(2.0) / half_life_days)
}

/// Compute the real-time priority score for signal-queue ranking.
///
/// Formula (Avellaneda-Lee signal strength × OU speed × risk normalisation):
/// ```text
///   priority = |z| × sqrt(κ) / σ_spread
/// ```
///
/// - |z|: signal strength — the further the spread from equilibrium, the
///   larger the expected P&L.
/// - sqrt(κ): reversion speed weight — faster-reverting pairs earn the capital
///   back quicker.  sqrt instead of κ prevents over-weighting extremely fast
///   but noisy pairs.
/// - 1/σ_spread: risk normalisation — tighter-spread pairs require less
///   capital to achieve the same basis-point return.
///
/// Returns 0.0 if any input is not finite, z is NaN/±∞, σ_spread < min_sigma,
/// or κ < min_kappa (no mean-reversion detected).
///
/// Reference: Avellaneda & Lee (2010) §3; Lee, Leung & Ning (2023).
///
/// # Examples
/// ```
/// use pair_picker::scorer::{PriorityConfig, compute_priority_score};
/// let cfg = PriorityConfig::default();
/// let score = compute_priority_score(2.5, 0.069, 0.02, &cfg); // |z|=2.5, κ≈ln2/10, σ=2%
/// assert!(score > 0.0);
/// // NaN input → 0.0 (safe, never NaN-propagates)
/// assert_eq!(compute_priority_score(f64::NAN, 0.069, 0.02, &cfg), 0.0);
/// ```
pub fn compute_priority_score(
    z: f64,
    kappa: f64,
    sigma_spread: f64,
    config: &PriorityConfig,
) -> f64 {
    // Guard all boundaries — this path runs every bar per pair
    if !z.is_finite() || !kappa.is_finite() || !sigma_spread.is_finite() {
        return 0.0;
    }
    if kappa < config.min_kappa || sigma_spread < config.min_sigma {
        return 0.0;
    }
    z.abs() * kappa.sqrt() / sigma_spread
}

/// Compute expected return per dollar deployed per day.
///
/// This is the **common-unit metric** that lets us compare active trades with
/// queued signals on the same scale, enabling capital rotation (evict a slower
/// trade to free room for a faster/stronger signal).
///
/// Formula (Lee-Leung-Ning 2023, adapted for daily-bar scaling):
/// ```text
///   E[R / $ / day] = |z| × σ_spread × κ / (1 + κ × E[hold_days])
/// ```
///
/// Units breakdown:
/// - |z| × σ_spread → expected spread move in log-spread units (≈ P&L potential)
/// - κ / (1 + κ × T) → effective reversion rate given expected holding period T
///   (faster reversion = more trades per unit time = more P&L per day)
///
/// Returns 0.0 if any input is not finite, κ ≤ 0, σ ≤ 0, or expected_hold ≤ 0.
///
/// Reference: Lee, Leung & Ning (2023), "Optimal Mean Reversion Trading with
/// Transaction Costs", §2.2.
///
/// # Examples
/// ```
/// use pair_picker::scorer::expected_return_per_dollar_per_day;
/// // |z|=2.5, σ=0.02, κ=ln(2)/10≈0.0693, hold=10 days
/// let r = expected_return_per_dollar_per_day(2.5, 0.02, f64::ln(2.0)/10.0, 10.0);
/// assert!(r > 0.0);
/// assert!(r.is_finite());
/// // Zero sigma → 0.0
/// assert_eq!(expected_return_per_dollar_per_day(2.5, 0.0, 0.069, 10.0), 0.0);
/// ```
pub fn expected_return_per_dollar_per_day(
    z: f64,
    sigma_spread: f64,
    kappa: f64,
    expected_hold_days: f64,
) -> f64 {
    if !z.is_finite()
        || !sigma_spread.is_finite()
        || !kappa.is_finite()
        || !expected_hold_days.is_finite()
    {
        return 0.0;
    }
    if kappa <= 0.0 || sigma_spread <= 0.0 || expected_hold_days <= 0.0 {
        return 0.0;
    }
    let numerator = z.abs() * sigma_spread * kappa;
    let denominator = 1.0 + kappa * expected_hold_days;
    numerator / denominator
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_excellent_pair() {
        let score = compute_score(0.001, 8.0, 0.03, 0.95, false);
        assert!(score > 0.85, "score={score}");
    }

    #[test]
    fn test_mediocre_pair() {
        let score = compute_score(0.04, 25.0, 0.15, 0.70, false);
        assert!(score > 0.2 && score < 0.6, "score={score}");
    }

    #[test]
    fn test_structural_break_penalizes() {
        let no_break = compute_score(0.01, 10.0, 0.10, 0.90, false);
        let with_break = compute_score(0.01, 10.0, 0.10, 0.90, true);
        assert!(
            with_break < no_break,
            "no_break={no_break}, with_break={with_break}"
        );
    }

    #[test]
    fn test_cointegration_score() {
        assert!((cointegration_score(0.001) - 1.0).abs() < 0.01);
        assert!((cointegration_score(0.05) - 0.556).abs() < 0.01);
        assert!((cointegration_score(0.15) - 0.0).abs() < 0.01);
    }

    #[test]
    fn test_half_life_score() {
        assert_eq!(half_life_score(0.5), 0.0); // below MIN_HALF_LIFE
        assert!(half_life_score(1.5) > 0.0 && half_life_score(1.5) < 1.0); // ramp-up
        assert_eq!(half_life_score(2.0), 1.0); // ideal range
        assert_eq!(half_life_score(10.0), 1.0); // ideal range
        assert!(half_life_score(30.0) > 0.0 && half_life_score(30.0) < 1.0); // ramp-down
        assert_eq!(half_life_score(50.0), 0.0); // above MAX_HALF_LIFE
    }

    #[test]
    fn test_score_range() {
        // Score should always be in [0, 1]
        for p in [0.001, 0.01, 0.05, 0.10, 0.50] {
            for hl in [3.0, 5.0, 10.0, 20.0, 40.0] {
                for cv in [0.01, 0.10, 0.20, 0.50] {
                    for r2 in [0.5, 0.8, 0.95] {
                        let s = compute_score(p, hl, cv, r2, false);
                        assert!(
                            s >= 0.0 && s <= 1.0,
                            "p={p}, hl={hl}, cv={cv}, r2={r2}, s={s}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn test_custom_config() {
        let config = ScoringConfig {
            w_cointegration: 1.0,
            w_half_life: 0.0,
            w_stability: 0.0,
            w_fit: 0.0,
        };
        let score = compute_score_with_config(0.001, 50.0, 1.0, 0.0, false, &config);
        // Only cointegration matters, and p=0.001 → 1.0
        assert!((score - 1.0).abs() < 0.01, "score={score}");
    }

    #[test]
    fn test_nan_r_squared() {
        let score = compute_score(0.01, 10.0, 0.05, f64::NAN, false);
        assert!(score.is_finite(), "NaN R² should not produce NaN score");
    }

    // -----------------------------------------------------------------------
    // MaxHoldConfig / compute_max_hold_days tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_max_hold_hl2_gives_5() {
        // HL=2d → 2.5*2=5.0 → ceil(5.0)=5 (no cap)
        let cfg = MaxHoldConfig::default();
        assert_eq!(compute_max_hold_days(2.0, &cfg), 5);
    }

    #[test]
    fn test_max_hold_hl5_capped_at_10() {
        // HL=5d → 2.5*5=12.5 → ceil=13, capped at 10
        let cfg = MaxHoldConfig::default();
        assert_eq!(compute_max_hold_days(5.0, &cfg), 10);
    }

    #[test]
    fn test_max_hold_hl3_rounds_up() {
        // HL=3d → 2.5*3=7.5 → ceil(7.5)=8
        let cfg = MaxHoldConfig::default();
        assert_eq!(compute_max_hold_days(3.0, &cfg), 8);
    }

    #[test]
    fn test_max_hold_exactly_at_cap() {
        // HL=4d → 2.5*4=10.0 → ceil=10, cap=10 → exactly 10
        let cfg = MaxHoldConfig::default();
        assert_eq!(compute_max_hold_days(4.0, &cfg), 10);
    }

    #[test]
    fn test_max_hold_nan_halflife_uses_cap() {
        let cfg = MaxHoldConfig::default();
        assert_eq!(compute_max_hold_days(f64::NAN, &cfg), 10);
    }

    #[test]
    fn test_max_hold_inf_halflife_uses_cap() {
        let cfg = MaxHoldConfig::default();
        assert_eq!(compute_max_hold_days(f64::INFINITY, &cfg), 10);
    }

    #[test]
    fn test_max_hold_zero_halflife_uses_cap() {
        let cfg = MaxHoldConfig::default();
        assert_eq!(compute_max_hold_days(0.0, &cfg), 10);
    }

    #[test]
    fn test_max_hold_negative_halflife_uses_cap() {
        let cfg = MaxHoldConfig::default();
        assert_eq!(compute_max_hold_days(-1.0, &cfg), 10);
    }

    #[test]
    fn test_max_hold_custom_config() {
        // multiplier=3.0, cap=15
        let cfg = MaxHoldConfig {
            hold_multiplier: 3.0,
            max_hold_cap: 15,
        };
        // HL=4d → 3*4=12 → ceil=12 (under cap)
        assert_eq!(compute_max_hold_days(4.0, &cfg), 12);
        // HL=6d → 3*6=18 → capped at 15
        assert_eq!(compute_max_hold_days(6.0, &cfg), 15);
    }

    // -----------------------------------------------------------------------
    // half_life_to_kappa tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_kappa_from_halflife_10() {
        // HL=10d → κ = ln(2)/10 ≈ 0.06931
        let k = half_life_to_kappa(10.0).unwrap();
        let expected = f64::ln(2.0) / 10.0;
        assert!(
            (k - expected).abs() < 1e-12,
            "kappa={k} expected={expected}"
        );
    }

    #[test]
    fn test_kappa_from_halflife_5() {
        let k = half_life_to_kappa(5.0).unwrap();
        let expected = f64::ln(2.0) / 5.0;
        assert!((k - expected).abs() < 1e-12);
    }

    #[test]
    fn test_kappa_zero_halflife_none() {
        assert!(half_life_to_kappa(0.0).is_none());
    }

    #[test]
    fn test_kappa_negative_halflife_none() {
        assert!(half_life_to_kappa(-1.0).is_none());
    }

    #[test]
    fn test_kappa_nan_halflife_none() {
        assert!(half_life_to_kappa(f64::NAN).is_none());
    }

    #[test]
    fn test_kappa_inf_halflife_none() {
        assert!(half_life_to_kappa(f64::INFINITY).is_none());
    }

    // -----------------------------------------------------------------------
    // compute_priority_score tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_priority_score_typical() {
        // |z|=2.5, κ=ln2/10≈0.0693, σ=0.02
        // score = 2.5 * sqrt(0.0693) / 0.02 ≈ 2.5 * 0.2632 / 0.02 ≈ 32.9
        let cfg = PriorityConfig::default();
        let kappa = f64::ln(2.0) / 10.0;
        let score = compute_priority_score(2.5, kappa, 0.02, &cfg);
        let expected = 2.5 * kappa.sqrt() / 0.02;
        assert!(
            (score - expected).abs() < 1e-10,
            "score={score} expected={expected}"
        );
        assert!(score > 0.0);
    }

    #[test]
    fn test_priority_score_higher_z_wins() {
        // Larger |z| → higher priority (all else equal)
        let cfg = PriorityConfig::default();
        let kappa = f64::ln(2.0) / 10.0;
        let s1 = compute_priority_score(2.0, kappa, 0.02, &cfg);
        let s2 = compute_priority_score(3.0, kappa, 0.02, &cfg);
        assert!(s2 > s1, "s1={s1} s2={s2}");
    }

    #[test]
    fn test_priority_score_faster_reversion_wins() {
        // Larger κ (faster reversion) → higher priority (all else equal)
        let cfg = PriorityConfig::default();
        let kappa_slow = f64::ln(2.0) / 15.0; // 15-day HL
        let kappa_fast = f64::ln(2.0) / 5.0; // 5-day HL
        let s_slow = compute_priority_score(2.5, kappa_slow, 0.02, &cfg);
        let s_fast = compute_priority_score(2.5, kappa_fast, 0.02, &cfg);
        assert!(s_fast > s_slow, "s_slow={s_slow} s_fast={s_fast}");
    }

    #[test]
    fn test_priority_score_tighter_spread_wins() {
        // Smaller σ_spread → higher priority (same P&L potential, less risk)
        let cfg = PriorityConfig::default();
        let kappa = f64::ln(2.0) / 10.0;
        let s_wide = compute_priority_score(2.5, kappa, 0.05, &cfg);
        let s_tight = compute_priority_score(2.5, kappa, 0.02, &cfg);
        assert!(s_tight > s_wide, "s_wide={s_wide} s_tight={s_tight}");
    }

    #[test]
    fn test_priority_score_nan_z_returns_zero() {
        let cfg = PriorityConfig::default();
        assert_eq!(compute_priority_score(f64::NAN, 0.069, 0.02, &cfg), 0.0);
    }

    #[test]
    fn test_priority_score_inf_z_returns_zero() {
        let cfg = PriorityConfig::default();
        assert_eq!(
            compute_priority_score(f64::INFINITY, 0.069, 0.02, &cfg),
            0.0
        );
    }

    #[test]
    fn test_priority_score_nan_kappa_returns_zero() {
        let cfg = PriorityConfig::default();
        assert_eq!(compute_priority_score(2.5, f64::NAN, 0.02, &cfg), 0.0);
    }

    #[test]
    fn test_priority_score_zero_sigma_returns_zero() {
        let cfg = PriorityConfig::default();
        assert_eq!(compute_priority_score(2.5, 0.069, 0.0, &cfg), 0.0);
    }

    #[test]
    fn test_priority_score_negative_kappa_returns_zero() {
        // κ < 0 means explosive process — not mean-reverting
        let cfg = PriorityConfig::default();
        assert_eq!(compute_priority_score(2.5, -0.069, 0.02, &cfg), 0.0);
    }

    #[test]
    fn test_priority_score_symmetric_in_z() {
        // Score for z=+2.5 should equal score for z=-2.5 (|z| used)
        let cfg = PriorityConfig::default();
        let kappa = f64::ln(2.0) / 10.0;
        let pos = compute_priority_score(2.5, kappa, 0.02, &cfg);
        let neg = compute_priority_score(-2.5, kappa, 0.02, &cfg);
        assert!((pos - neg).abs() < 1e-12, "pos={pos} neg={neg}");
    }

    // -----------------------------------------------------------------------
    // expected_return_per_dollar_per_day tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_erpdd_typical() {
        // |z|=2.5, σ=0.02, κ=ln2/10≈0.0693, hold=10d
        // numerator = 2.5 * 0.02 * 0.0693 = 0.003465
        // denominator = 1 + 0.0693 * 10 = 1.693
        // result ≈ 0.002046
        let kappa = f64::ln(2.0) / 10.0;
        let r = expected_return_per_dollar_per_day(2.5, 0.02, kappa, 10.0);
        let expected = 2.5 * 0.02 * kappa / (1.0 + kappa * 10.0);
        assert!((r - expected).abs() < 1e-12, "r={r} expected={expected}");
        assert!(r > 0.0);
    }

    #[test]
    fn test_erpdd_faster_reversion_higher_return() {
        let kappa_slow = f64::ln(2.0) / 15.0;
        let kappa_fast = f64::ln(2.0) / 5.0;
        let r_slow = expected_return_per_dollar_per_day(2.5, 0.02, kappa_slow, 10.0);
        let r_fast = expected_return_per_dollar_per_day(2.5, 0.02, kappa_fast, 10.0);
        assert!(r_fast > r_slow, "r_slow={r_slow} r_fast={r_fast}");
    }

    #[test]
    fn test_erpdd_zero_sigma_returns_zero() {
        assert_eq!(
            expected_return_per_dollar_per_day(2.5, 0.0, 0.069, 10.0),
            0.0
        );
    }

    #[test]
    fn test_erpdd_zero_kappa_returns_zero() {
        assert_eq!(
            expected_return_per_dollar_per_day(2.5, 0.02, 0.0, 10.0),
            0.0
        );
    }

    #[test]
    fn test_erpdd_negative_kappa_returns_zero() {
        assert_eq!(
            expected_return_per_dollar_per_day(2.5, 0.02, -0.069, 10.0),
            0.0
        );
    }

    #[test]
    fn test_erpdd_nan_returns_zero() {
        assert_eq!(
            expected_return_per_dollar_per_day(f64::NAN, 0.02, 0.069, 10.0),
            0.0
        );
        assert_eq!(
            expected_return_per_dollar_per_day(2.5, f64::NAN, 0.069, 10.0),
            0.0
        );
        assert_eq!(
            expected_return_per_dollar_per_day(2.5, 0.02, f64::NAN, 10.0),
            0.0
        );
        assert_eq!(
            expected_return_per_dollar_per_day(2.5, 0.02, 0.069, f64::NAN),
            0.0
        );
    }

    #[test]
    fn test_erpdd_inf_returns_zero() {
        assert_eq!(
            expected_return_per_dollar_per_day(f64::INFINITY, 0.02, 0.069, 10.0),
            0.0
        );
    }

    #[test]
    fn test_erpdd_symmetric_in_z() {
        let kappa = f64::ln(2.0) / 10.0;
        let pos = expected_return_per_dollar_per_day(2.5, 0.02, kappa, 10.0);
        let neg = expected_return_per_dollar_per_day(-2.5, 0.02, kappa, 10.0);
        assert!((pos - neg).abs() < 1e-12);
    }

    #[test]
    fn test_erpdd_zero_hold_days_returns_zero() {
        assert_eq!(
            expected_return_per_dollar_per_day(2.5, 0.02, 0.069, 0.0),
            0.0
        );
    }
}
