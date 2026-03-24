//! Parameter sensitivity analysis for the multi-day pairs strategy.
//!
//! Sweeps each parameter independently while holding the others at their defaults.
//! Uses the walk-forward framework from `walk_forward.rs` as the evaluation harness,
//! making each evaluation automatically out-of-sample.
//!
//! ## Methodology
//!
//! For each parameter P in {HL_min, HL_max, Z_lookback, entry_z, exit_z, R²_cutoff}:
//! 1. Hold all other parameters at their default values.
//! 2. Evaluate the walk-forward Sharpe at each value in P's sweep range.
//! 3. Flag **cliff edges**: values where Sharpe drops > 50% relative to the adjacent point.
//!
//! If most of the parameter space is profitable → evidence of a real edge.
//! If only a narrow band works → overfitting signal.
//!
//! ## Reference
//!
//! Gatev, Goetzmann & Rouwenhorst (2006), "Pairs Trading: Relative-Value Arbitrage",
//! Review of Financial Studies — recommends out-of-sample validation for all parameter choices.

use crate::pipeline::{InMemoryPrices, MIN_HISTORY_BARS};
use crate::types::PairCandidate;
use crate::walk_forward::{run_walk_forward, WalkForwardConfig, WalkForwardSummary};
use tracing::{info, warn};

// ── Default strategy parameter values (from issue #185 spec) ─────────────────

/// Default OU half-life minimum threshold (days).
pub const DEFAULT_HL_MIN: f64 = 2.0;
/// Default OU half-life maximum threshold (days).
pub const DEFAULT_HL_MAX: f64 = 5.0;
/// Default z-score lookback window (days) — maps to walk-forward `formation_days`.
pub const DEFAULT_Z_LOOKBACK: usize = 30;
/// Default entry z-score threshold.
pub const DEFAULT_ENTRY_Z: f64 = 2.0;
/// Default exit z-score threshold.
pub const DEFAULT_EXIT_Z: f64 = 0.5;
/// Default minimum R² cutoff for OLS hedge ratio acceptance.
pub const DEFAULT_R2_CUTOFF: f64 = 0.30;

/// Threshold for flagging cliff edges: Sharpe drop relative to the previous step.
/// A drop of > 50% (i.e., ratio < 0.5) is flagged as a cliff.
pub const CLIFF_EDGE_DROP_THRESHOLD: f64 = 0.50;

// ── Data types ────────────────────────────────────────────────────────────────

/// A single point in the parameter sweep for one parameter.
#[derive(Debug, Clone)]
pub struct SweepPoint {
    /// The parameter value evaluated.
    pub param_value: f64,
    /// Out-of-sample Sharpe ratio from walk-forward.
    pub oos_sharpe: f64,
    /// Aggregate win rate across all trades.
    pub win_rate: f64,
    /// Total P&L in USD across all windows.
    pub total_pnl_usd: f64,
    /// Total number of round-trip trades.
    pub n_trades: usize,
    /// Number of walk-forward windows evaluated (0 means insufficient data for this config).
    pub n_windows: usize,
    /// Whether this point is flagged as a cliff edge relative to the previous point.
    pub is_cliff_edge: bool,
}

/// Results for one parameter's sensitivity sweep.
#[derive(Debug, Clone)]
pub struct ParameterSweep {
    /// Human-readable parameter name.
    pub param_name: String,
    /// Default value of this parameter.
    pub default_value: f64,
    /// Sweep results ordered from lowest to highest parameter value.
    pub points: Vec<SweepPoint>,
    /// Number of cliff edges detected in this sweep.
    pub n_cliff_edges: usize,
    /// Fraction of sweep points with positive OOS Sharpe.
    pub profitable_fraction: f64,
}

/// Full sensitivity analysis result across all parameters.
#[derive(Debug, Clone)]
pub struct SensitivityResult {
    /// Per-parameter sweep results.
    pub sweeps: Vec<ParameterSweep>,
    /// Total number of walk-forward evaluations performed.
    pub total_evaluations: usize,
}

/// Configuration controlling which parameters are swept and their ranges.
///
/// All fields have defaults matching the issue #185 spec.
#[derive(Debug, Clone)]
pub struct SensitivityConfig {
    // ── Half-life min sweep ────────────────────────────────────────────────
    /// Sweep values for HL min (days). Default: 5 points from 1.0 to 3.0.
    pub hl_min_values: Vec<f64>,

    // ── Half-life max sweep ────────────────────────────────────────────────
    /// Sweep values for HL max (days). Default: 6 points from 3.0 to 8.0.
    pub hl_max_values: Vec<f64>,

    // ── Z-score lookback sweep ─────────────────────────────────────────────
    /// Sweep values for z-score lookback (days). Default: 4 points: 15, 30, 45, 60.
    pub z_lookback_values: Vec<usize>,

    // ── Entry z-score sweep ────────────────────────────────────────────────
    /// Sweep values for entry z-score threshold. Default: 5 points from 1.5 to 2.5.
    pub entry_z_values: Vec<f64>,

    // ── Exit z-score sweep ─────────────────────────────────────────────────
    /// Sweep values for exit z-score threshold. Default: 4 points from 0.25 to 1.0.
    pub exit_z_values: Vec<f64>,

    // ── R² cutoff sweep ────────────────────────────────────────────────────
    /// Sweep values for R² cutoff. Default: 4 points from 0.15 to 0.50.
    pub r2_cutoff_values: Vec<f64>,

    // ── Walk-forward base config ───────────────────────────────────────────
    /// Base walk-forward config — parameters not being swept use these values.
    pub base_walk_forward: WalkForwardConfig,
}

impl Default for SensitivityConfig {
    fn default() -> Self {
        Self {
            // HL min: 5 steps [1.0, 1.5, 2.0, 2.5, 3.0]
            hl_min_values: vec![1.0, 1.5, 2.0, 2.5, 3.0],
            // HL max: 6 steps [3.0, 4.0, 5.0, 6.0, 7.0, 8.0]
            hl_max_values: vec![3.0, 4.0, 5.0, 6.0, 7.0, 8.0],
            // Z lookback: 4 steps [15, 30, 45, 60]
            z_lookback_values: vec![15, 30, 45, 60],
            // Entry z: 5 steps [1.5, 1.75, 2.0, 2.25, 2.5]
            entry_z_values: vec![1.5, 1.75, 2.0, 2.25, 2.5],
            // Exit z: 4 steps [0.25, 0.5, 0.75, 1.0]
            exit_z_values: vec![0.25, 0.5, 0.75, 1.0],
            // R² cutoff: 4 steps [0.15, 0.25, 0.35, 0.50]
            r2_cutoff_values: vec![0.15, 0.25, 0.35, 0.50],
            base_walk_forward: WalkForwardConfig::default(),
        }
    }
}

// ── Sweep variant ─────────────────────────────────────────────────────────────

/// A variant of the walk-forward evaluation, parameterized by which parameter is swept.
///
/// HL min and HL max affect pair selection (filtering by half-life bounds), so they
/// require a custom evaluation wrapper that filters the results rather than changing
/// the walk-forward config directly. Entry/exit z and formation_days are walk-forward
/// config parameters and map directly.
#[derive(Debug, Clone)]
pub enum SweepVariant {
    /// Half-life lower bound filter (days).
    HlMin(f64, WalkForwardConfig),
    /// Half-life upper bound filter (days).
    HlMax(f64, WalkForwardConfig),
    /// Formation window size (z-score lookback, days).
    ZLookback(f64, WalkForwardConfig),
    /// Entry z-score threshold.
    EntryZ(f64, WalkForwardConfig),
    /// Exit z-score threshold.
    ExitZ(f64, WalkForwardConfig),
    /// R² cutoff for OLS hedge ratio acceptance.
    R2Cutoff(f64, WalkForwardConfig),
}

// ── Sweep context ─────────────────────────────────────────────────────────────

/// Shared context for a parameter sweep — avoids passing many arguments.
struct SweepContext<'a> {
    candidates: &'a [PairCandidate],
    prices: &'a InMemoryPrices,
    base_config: &'a WalkForwardConfig,
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Run the full parameter sensitivity analysis.
///
/// Sweeps each parameter independently, holding others at their defaults.
/// Each evaluation uses the walk-forward framework for out-of-sample results.
///
/// # Arguments
/// - `candidates`: list of candidate pairs to evaluate.
/// - `prices`: full price history for all symbols.
/// - `config`: sensitivity sweep configuration.
///
/// # Returns
/// `SensitivityResult` with per-parameter sweep results, or `None` if insufficient
/// data for even the base walk-forward configuration.
pub fn run_sensitivity_analysis(
    candidates: &[PairCandidate],
    prices: &InMemoryPrices,
    config: &SensitivityConfig,
) -> Option<SensitivityResult> {
    // Check minimum data requirement before any sweeps
    let min_bars = candidates
        .iter()
        .filter_map(|c| {
            let na = prices.data.get(&c.leg_a).map(|v| v.len())?;
            let nb = prices.data.get(&c.leg_b).map(|v| v.len())?;
            Some(na.min(nb))
        })
        .min()
        .unwrap_or(0);

    if min_bars < MIN_HISTORY_BARS {
        warn!(
            min_bars,
            min_needed = MIN_HISTORY_BARS,
            "Insufficient price history for sensitivity analysis"
        );
        return None;
    }

    info!(
        n_candidates = candidates.len(),
        min_bars, "Starting parameter sensitivity analysis"
    );

    let ctx = SweepContext {
        candidates,
        prices,
        base_config: &config.base_walk_forward,
    };

    let mut sweeps = Vec::new();
    let mut total_evaluations = 0;

    // ── Sweep 1: HL min ───────────────────────────────────────────────────────
    info!("Sweeping HL min ({} values)", config.hl_min_values.len());
    let hl_min_sweep = sweep_hl_min("HL min (days)", DEFAULT_HL_MIN, &config.hl_min_values, &ctx);
    total_evaluations += hl_min_sweep.points.len();
    sweeps.push(hl_min_sweep);

    // ── Sweep 2: HL max ───────────────────────────────────────────────────────
    info!("Sweeping HL max ({} values)", config.hl_max_values.len());
    let hl_max_sweep = sweep_hl_max("HL max (days)", DEFAULT_HL_MAX, &config.hl_max_values, &ctx);
    total_evaluations += hl_max_sweep.points.len();
    sweeps.push(hl_max_sweep);

    // ── Sweep 3: Z lookback (maps to formation_days) ──────────────────────────
    info!(
        "Sweeping Z lookback ({} values)",
        config.z_lookback_values.len()
    );
    let z_values: Vec<f64> = config.z_lookback_values.iter().map(|&v| v as f64).collect();
    let z_lookback_sweep = sweep_with_variant(
        "Z lookback (days)",
        DEFAULT_Z_LOOKBACK as f64,
        &z_values,
        &ctx,
        |wf_config, value| {
            let mut cfg = wf_config.clone();
            cfg.formation_days = value as usize;
            SweepVariant::ZLookback(value, cfg)
        },
    );
    total_evaluations += z_lookback_sweep.points.len();
    sweeps.push(z_lookback_sweep);

    // ── Sweep 4: Entry z-score ─────────────────────────────────────────────────
    info!(
        "Sweeping entry z-score ({} values)",
        config.entry_z_values.len()
    );
    let entry_z_sweep = sweep_with_variant(
        "Entry z-score",
        DEFAULT_ENTRY_Z,
        &config.entry_z_values,
        &ctx,
        |wf_config, value| {
            let mut cfg = wf_config.clone();
            cfg.entry_zscore = value;
            SweepVariant::EntryZ(value, cfg)
        },
    );
    total_evaluations += entry_z_sweep.points.len();
    sweeps.push(entry_z_sweep);

    // ── Sweep 5: Exit z-score ──────────────────────────────────────────────────
    info!(
        "Sweeping exit z-score ({} values)",
        config.exit_z_values.len()
    );
    let exit_z_sweep = sweep_with_variant(
        "Exit z-score",
        DEFAULT_EXIT_Z,
        &config.exit_z_values,
        &ctx,
        |wf_config, value| {
            let mut cfg = wf_config.clone();
            cfg.exit_zscore = value;
            SweepVariant::ExitZ(value, cfg)
        },
    );
    total_evaluations += exit_z_sweep.points.len();
    sweeps.push(exit_z_sweep);

    // ── Sweep 6: R² cutoff ────────────────────────────────────────────────────
    info!(
        "Sweeping R² cutoff ({} values)",
        config.r2_cutoff_values.len()
    );
    let r2_sweep = sweep_r2_cutoff(
        "R² cutoff",
        DEFAULT_R2_CUTOFF,
        &config.r2_cutoff_values,
        &ctx,
    );
    total_evaluations += r2_sweep.points.len();
    sweeps.push(r2_sweep);

    info!(
        total_evaluations,
        n_sweeps = sweeps.len(),
        "Sensitivity analysis complete"
    );

    Some(SensitivityResult {
        sweeps,
        total_evaluations,
    })
}

// ── Per-parameter sweep functions ─────────────────────────────────────────────

/// Sweep the HL min parameter.
fn sweep_hl_min(
    param_name: &str,
    default_value: f64,
    values: &[f64],
    ctx: &SweepContext<'_>,
) -> ParameterSweep {
    let summaries: Vec<(f64, Option<WalkForwardSummary>)> = values
        .iter()
        .map(|&v| {
            let summary = run_walk_forward_with_hl_filter(ctx, Some(v), None);
            log_sweep_point(param_name, v, &summary);
            (v, summary)
        })
        .collect();
    build_parameter_sweep(param_name, default_value, summaries)
}

/// Sweep the HL max parameter.
fn sweep_hl_max(
    param_name: &str,
    default_value: f64,
    values: &[f64],
    ctx: &SweepContext<'_>,
) -> ParameterSweep {
    let summaries: Vec<(f64, Option<WalkForwardSummary>)> = values
        .iter()
        .map(|&v| {
            let summary = run_walk_forward_with_hl_filter(ctx, None, Some(v));
            log_sweep_point(param_name, v, &summary);
            (v, summary)
        })
        .collect();
    build_parameter_sweep(param_name, default_value, summaries)
}

/// Sweep the R² cutoff parameter.
fn sweep_r2_cutoff(
    param_name: &str,
    default_value: f64,
    values: &[f64],
    ctx: &SweepContext<'_>,
) -> ParameterSweep {
    let summaries: Vec<(f64, Option<WalkForwardSummary>)> = values
        .iter()
        .map(|&v| {
            let summary = run_walk_forward_with_r2_filter(ctx, v);
            log_sweep_point(param_name, v, &summary);
            (v, summary)
        })
        .collect();
    build_parameter_sweep(param_name, default_value, summaries)
}

/// Sweep a parameter that maps directly to a `SweepVariant` (entry z, exit z, z-lookback).
fn sweep_with_variant<F>(
    param_name: &str,
    default_value: f64,
    values: &[f64],
    ctx: &SweepContext<'_>,
    make_variant: F,
) -> ParameterSweep
where
    F: Fn(&WalkForwardConfig, f64) -> SweepVariant,
{
    let summaries: Vec<(f64, Option<WalkForwardSummary>)> = values
        .iter()
        .map(|&v| {
            let variant = make_variant(ctx.base_config, v);
            let summary = evaluate_variant(&variant, ctx.candidates, ctx.prices);
            log_sweep_point(param_name, v, &summary);
            (v, summary)
        })
        .collect();
    build_parameter_sweep(param_name, default_value, summaries)
}

// ── Evaluation helpers ────────────────────────────────────────────────────────

/// Evaluate one sweep variant, returning the walk-forward summary (or None).
fn evaluate_variant(
    variant: &SweepVariant,
    candidates: &[PairCandidate],
    prices: &InMemoryPrices,
) -> Option<WalkForwardSummary> {
    let ctx = SweepContext {
        candidates,
        prices,
        base_config: match variant {
            SweepVariant::HlMin(_, c)
            | SweepVariant::HlMax(_, c)
            | SweepVariant::ZLookback(_, c)
            | SweepVariant::EntryZ(_, c)
            | SweepVariant::ExitZ(_, c)
            | SweepVariant::R2Cutoff(_, c) => c,
        },
    };
    match variant {
        SweepVariant::HlMin(v, _) => run_walk_forward_with_hl_filter(&ctx, Some(*v), None),
        SweepVariant::HlMax(v, _) => run_walk_forward_with_hl_filter(&ctx, None, Some(*v)),
        SweepVariant::ZLookback(_, c) | SweepVariant::EntryZ(_, c) | SweepVariant::ExitZ(_, c) => {
            run_walk_forward(candidates, prices, c)
        }
        SweepVariant::R2Cutoff(v, _) => run_walk_forward_with_r2_filter(&ctx, *v),
    }
}

/// Run walk-forward with a custom half-life filter applied during pair selection.
///
/// The standard pipeline uses `is_half_life_valid()` which checks [MIN_HALF_LIFE, MAX_HALF_LIFE].
/// This wrapper replaces the validity check by pre-screening candidates whose full-history
/// half-life falls within [hl_min, hl_max]. The effect mirrors tightening/relaxing the
/// HL validity range in the pipeline.
fn run_walk_forward_with_hl_filter(
    ctx: &SweepContext<'_>,
    hl_min_override: Option<f64>,
    hl_max_override: Option<f64>,
) -> Option<WalkForwardSummary> {
    use crate::stats::halflife::estimate_half_life;
    use crate::stats::ols::ols_simple;

    let hl_min = hl_min_override.unwrap_or(crate::stats::halflife::MIN_HALF_LIFE);
    let hl_max = hl_max_override.unwrap_or(crate::stats::halflife::MAX_HALF_LIFE);

    // Guard: hl_min must be < hl_max and both must be positive
    if !hl_min.is_finite() || !hl_max.is_finite() || hl_min <= 0.0 || hl_max <= hl_min {
        warn!(
            hl_min,
            hl_max, "Invalid HL filter range — skipping this sweep point"
        );
        return None;
    }

    // Pre-screen candidates: estimate HL on the full price history. Candidates whose
    // estimated HL falls outside [hl_min, hl_max] are filtered out for this sweep point.
    let filtered_candidates: Vec<PairCandidate> = ctx
        .candidates
        .iter()
        .filter(|c| {
            let prices_a = match ctx.prices.data.get(&c.leg_a) {
                Some(p) if p.len() >= crate::pipeline::MIN_HISTORY_BARS => p,
                _ => return false,
            };
            let prices_b = match ctx.prices.data.get(&c.leg_b) {
                Some(p) if p.len() >= crate::pipeline::MIN_HISTORY_BARS => p,
                _ => return false,
            };

            let n = prices_a.len().min(prices_b.len());
            let pa = &prices_a[prices_a.len() - n..];
            let pb = &prices_b[prices_b.len() - n..];

            if pa.iter().any(|&p| !p.is_finite() || p <= 0.0)
                || pb.iter().any(|&p| !p.is_finite() || p <= 0.0)
            {
                return false;
            }

            let log_a: Vec<f64> = pa.iter().map(|p| p.ln()).collect();
            let log_b: Vec<f64> = pb.iter().map(|p| p.ln()).collect();

            let beta = match ols_simple(&log_b, &log_a) {
                Some(r) => r.beta,
                None => return false,
            };

            let spread: Vec<f64> = log_a
                .iter()
                .zip(log_b.iter())
                .map(|(a, b)| a - beta * b)
                .collect();

            match estimate_half_life(&spread) {
                Some(hl) => {
                    let hl_val = hl.half_life;
                    hl_val.is_finite() && hl_val >= hl_min && hl_val <= hl_max
                }
                None => false,
            }
        })
        .cloned()
        .collect();

    if filtered_candidates.is_empty() {
        warn!(
            hl_min,
            hl_max, "No candidates pass HL filter at this sweep point"
        );
        return Some(zero_summary());
    }

    run_walk_forward(&filtered_candidates, ctx.prices, ctx.base_config)
}

/// Run walk-forward with a custom R² cutoff instead of the pipeline's `MIN_R_SQUARED`.
///
/// Filters candidates pre-screened to only include those whose OLS R² meets the
/// specified cutoff. The effect is equivalent to raising/lowering `MIN_R_SQUARED`.
fn run_walk_forward_with_r2_filter(
    ctx: &SweepContext<'_>,
    r2_cutoff: f64,
) -> Option<WalkForwardSummary> {
    use crate::stats::ols::ols_simple;

    if !r2_cutoff.is_finite() || !(0.0..=1.0).contains(&r2_cutoff) {
        warn!(r2_cutoff, "Invalid R² cutoff — skipping sweep point");
        return None;
    }

    let filtered_candidates: Vec<PairCandidate> = ctx
        .candidates
        .iter()
        .filter(|c| {
            let prices_a = match ctx.prices.data.get(&c.leg_a) {
                Some(p) if p.len() >= crate::pipeline::MIN_HISTORY_BARS => p,
                _ => return false,
            };
            let prices_b = match ctx.prices.data.get(&c.leg_b) {
                Some(p) if p.len() >= crate::pipeline::MIN_HISTORY_BARS => p,
                _ => return false,
            };

            let n = prices_a.len().min(prices_b.len());
            let pa = &prices_a[prices_a.len() - n..];
            let pb = &prices_b[prices_b.len() - n..];

            if pa.iter().any(|&p| !p.is_finite() || p <= 0.0)
                || pb.iter().any(|&p| !p.is_finite() || p <= 0.0)
            {
                return false;
            }

            let log_a: Vec<f64> = pa.iter().map(|p| p.ln()).collect();
            let log_b: Vec<f64> = pb.iter().map(|p| p.ln()).collect();

            match ols_simple(&log_b, &log_a) {
                Some(r) => r.r_squared >= r2_cutoff,
                None => false,
            }
        })
        .cloned()
        .collect();

    if filtered_candidates.is_empty() {
        warn!(
            r2_cutoff,
            "No candidates pass R² filter at this sweep point"
        );
        return Some(zero_summary());
    }

    run_walk_forward(&filtered_candidates, ctx.prices, ctx.base_config)
}

/// Construct a zero-trade summary when no candidates pass a filter.
///
/// This lets us record a sweep point with zero metrics rather than returning None,
/// which would lose information about the filter being too tight.
fn zero_summary() -> WalkForwardSummary {
    WalkForwardSummary {
        n_windows: 0,
        n_windows_with_pairs: 0,
        avg_insample_sharpe: 0.0,
        avg_oos_sharpe: 0.0,
        total_pnl_usd: 0.0,
        aggregate_win_rate: 0.0,
        total_trades: 0,
        total_winners: 0,
        windows: vec![],
    }
}

// ── Cliff edge detection ──────────────────────────────────────────────────────

/// Returns true if the Sharpe drop from `prev` to `curr` constitutes a cliff edge.
///
/// A cliff edge is detected when:
/// - The previous Sharpe was positive (strategy was working at the prior value), AND
/// - The current Sharpe is below 50% of the previous Sharpe.
///
/// This handles both gradual and step-function drops, and ignores noise around zero.
pub fn is_cliff_drop(prev_sharpe: f64, curr_sharpe: f64) -> bool {
    // Only flag if the previous point was meaningfully profitable
    if prev_sharpe <= 0.0 {
        return false;
    }
    // Current is less than 50% of previous positive Sharpe
    curr_sharpe < prev_sharpe * CLIFF_EDGE_DROP_THRESHOLD
}

// ── Build and log helpers ─────────────────────────────────────────────────────

/// Log a single sweep point result.
fn log_sweep_point(param_name: &str, value: f64, summary: &Option<WalkForwardSummary>) {
    info!(
        param = param_name,
        value,
        oos_sharpe = summary
            .as_ref()
            .map(|s| format!("{:.3}", s.avg_oos_sharpe))
            .unwrap_or_else(|| "N/A".to_string())
            .as_str(),
        n_trades = summary
            .as_ref()
            .map(|s| s.total_trades.to_string())
            .unwrap_or_else(|| "N/A".to_string())
            .as_str(),
        "Sweep point evaluated"
    );
}

/// Convert a list of (value, summary) pairs into a `ParameterSweep`, detecting cliff edges.
fn build_parameter_sweep(
    param_name: &str,
    default_value: f64,
    raw_points: Vec<(f64, Option<WalkForwardSummary>)>,
) -> ParameterSweep {
    let mut points: Vec<SweepPoint> = Vec::with_capacity(raw_points.len());
    let mut n_cliff_edges = 0;
    let mut n_profitable = 0;

    for (i, (value, summary)) in raw_points.into_iter().enumerate() {
        let (oos_sharpe, win_rate, total_pnl_usd, n_trades, n_windows) =
            if let Some(ref s) = summary {
                (
                    s.avg_oos_sharpe,
                    s.aggregate_win_rate,
                    s.total_pnl_usd,
                    s.total_trades,
                    s.n_windows,
                )
            } else {
                (0.0, 0.0, 0.0, 0, 0)
            };

        let is_cliff_edge = if i > 0 {
            let prev_sharpe = points[i - 1].oos_sharpe;
            is_cliff_drop(prev_sharpe, oos_sharpe)
        } else {
            false
        };

        if is_cliff_edge {
            n_cliff_edges += 1;
            warn!(
                param = param_name,
                value,
                oos_sharpe,
                prev_sharpe = points[i - 1].oos_sharpe,
                "Cliff edge detected: Sharpe drops > 50%"
            );
        }

        if oos_sharpe > 0.0 {
            n_profitable += 1;
        }

        points.push(SweepPoint {
            param_value: value,
            oos_sharpe,
            win_rate,
            total_pnl_usd,
            n_trades,
            n_windows,
            is_cliff_edge,
        });
    }

    let profitable_fraction = if !points.is_empty() {
        n_profitable as f64 / points.len() as f64
    } else {
        0.0
    };

    info!(
        param = param_name,
        n_points = points.len(),
        n_cliff_edges,
        profitable_fraction = format!("{:.0}%", profitable_fraction * 100.0).as_str(),
        "Parameter sweep complete"
    );

    ParameterSweep {
        param_name: param_name.to_string(),
        default_value,
        points,
        n_cliff_edges,
        profitable_fraction,
    }
}

// ── Output formatting ─────────────────────────────────────────────────────────

/// Print a formatted Markdown sensitivity table for all parameter sweeps.
///
/// The output is suitable for inclusion in a PR description.
pub fn print_sensitivity_table(result: &SensitivityResult) {
    println!("\n# Parameter Sensitivity Analysis");
    println!("\nEach parameter swept independently while others are held at defaults.");
    println!("All evaluations use walk-forward (out-of-sample) Sharpe ratios.\n");

    for sweep in &result.sweeps {
        println!("## {}", sweep.param_name);
        println!(
            "\nDefault: `{:.2}` | Profitable range: {:.0}% of sweep points | Cliff edges: {}\n",
            sweep.default_value,
            sweep.profitable_fraction * 100.0,
            sweep.n_cliff_edges
        );
        println!("| Value | OOS Sharpe | Win% | P&L ($) | Trades | Cliff? |");
        println!("|------:|----------:|-----:|--------:|-------:|:------:|");
        for pt in &sweep.points {
            let cliff_marker = if pt.is_cliff_edge { "CLIFF" } else { "" };
            let default_marker = if (pt.param_value - sweep.default_value).abs() < 1e-9 {
                " *"
            } else {
                ""
            };
            println!(
                "| {:.2}{} | {:.3} | {:.1}% | {:.0} | {} | {} |",
                pt.param_value,
                default_marker,
                pt.oos_sharpe,
                pt.win_rate * 100.0,
                pt.total_pnl_usd,
                pt.n_trades,
                cliff_marker
            );
        }
        println!();

        print_sweep_interpretation(sweep);
    }

    // Overall summary
    println!("\n## Overall Assessment\n");
    let total_cliff_edges: usize = result.sweeps.iter().map(|s| s.n_cliff_edges).sum();
    let avg_profitable_fraction: f64 = if !result.sweeps.is_empty() {
        result
            .sweeps
            .iter()
            .map(|s| s.profitable_fraction)
            .sum::<f64>()
            / result.sweeps.len() as f64
    } else {
        0.0
    };

    println!("- Total cliff edges detected: **{}**", total_cliff_edges);
    println!(
        "- Average profitable fraction across sweeps: **{:.0}%**",
        avg_profitable_fraction * 100.0
    );
    println!(
        "- Total walk-forward evaluations: **{}**",
        result.total_evaluations
    );

    if avg_profitable_fraction >= 0.7 && total_cliff_edges == 0 {
        println!(
            "\n**Assessment: Robust edge.** Most of the parameter space is profitable with no cliff edges."
        );
    } else if avg_profitable_fraction >= 0.5 && total_cliff_edges <= 2 {
        println!(
            "\n**Assessment: Moderate robustness.** Strategy performs over a reasonable range but shows some sensitivity."
        );
    } else if total_cliff_edges > 3 || avg_profitable_fraction < 0.3 {
        println!(
            "\n**Assessment: Fragile — possible overfitting.** Only a narrow parameter band works or multiple cliff edges detected."
        );
    } else {
        println!(
            "\n**Assessment: Mixed.** Review individual parameter sweeps for sensitivity patterns."
        );
    }
    println!();
}

fn print_sweep_interpretation(sweep: &ParameterSweep) {
    if sweep.profitable_fraction >= 0.7 {
        println!(
            "> Robust: {:.0}% of tested values produce positive OOS Sharpe.",
            sweep.profitable_fraction * 100.0
        );
    } else if sweep.profitable_fraction >= 0.4 {
        println!(
            "> Mixed: {:.0}% of tested values produce positive OOS Sharpe. Monitor this parameter.",
            sweep.profitable_fraction * 100.0
        );
    } else {
        println!(
            "> Fragile: only {:.0}% of tested values are profitable. Possible overfit to default.",
            sweep.profitable_fraction * 100.0
        );
    }
    if sweep.n_cliff_edges > 0 {
        println!(
            "> WARNING: {} cliff edge(s) detected — sharp performance drop at parameter boundaries.",
            sweep.n_cliff_edges
        );
    }
    println!();
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils;

    fn make_prices(pairs: Vec<(&str, Vec<f64>)>) -> InMemoryPrices {
        InMemoryPrices {
            data: pairs.into_iter().map(|(s, p)| (s.to_string(), p)).collect(),
        }
    }

    fn candidate(a: &str, b: &str) -> PairCandidate {
        PairCandidate {
            leg_a: a.to_string(),
            leg_b: b.to_string(),
            economic_rationale: "test".to_string(),
        }
    }

    // ── is_cliff_drop tests ──────────────────────────────────────────────────

    #[test]
    fn test_cliff_drop_detects_large_drop() {
        // Previous: 1.0, current: 0.4 → drops to 40% of previous → cliff
        assert!(is_cliff_drop(1.0, 0.4));
    }

    #[test]
    fn test_cliff_drop_no_cliff_for_gradual_decline() {
        // Previous: 1.0, current: 0.6 → drops to 60% of previous → not a cliff
        assert!(!is_cliff_drop(1.0, 0.6));
    }

    #[test]
    fn test_cliff_drop_no_cliff_when_prev_not_positive() {
        // Previous Sharpe was negative — no cliff flagged
        assert!(!is_cliff_drop(-0.5, -2.0));
        assert!(!is_cliff_drop(0.0, -1.0));
    }

    #[test]
    fn test_cliff_drop_positive_to_negative() {
        // Sharp drop from positive to negative — this is a cliff
        assert!(is_cliff_drop(1.0, -0.5));
    }

    #[test]
    fn test_cliff_drop_exactly_50_pct() {
        // Exactly 50% drop — at the threshold, should not be a cliff (strict <)
        assert!(!is_cliff_drop(1.0, 0.5));
    }

    #[test]
    fn test_cliff_drop_just_below_50_pct() {
        // Just below 50% — should be a cliff
        assert!(is_cliff_drop(1.0, 0.499));
    }

    // ── SensitivityConfig defaults tests ────────────────────────────────────

    #[test]
    fn test_sensitivity_config_defaults_have_correct_counts() {
        let config = SensitivityConfig::default();
        assert_eq!(config.hl_min_values.len(), 5, "HL min: 5 steps");
        assert_eq!(config.hl_max_values.len(), 6, "HL max: 6 steps");
        assert_eq!(config.z_lookback_values.len(), 4, "Z lookback: 4 steps");
        assert_eq!(config.entry_z_values.len(), 5, "Entry z: 5 steps");
        assert_eq!(config.exit_z_values.len(), 4, "Exit z: 4 steps");
        assert_eq!(config.r2_cutoff_values.len(), 4, "R² cutoff: 4 steps");
    }

    #[test]
    fn test_sensitivity_config_defaults_match_spec() {
        let config = SensitivityConfig::default();
        // HL min: 1.0 - 3.0
        assert!((config.hl_min_values[0] - 1.0).abs() < 1e-9);
        assert!((config.hl_min_values[4] - 3.0).abs() < 1e-9);
        // HL max: 3.0 - 8.0
        assert!((config.hl_max_values[0] - 3.0).abs() < 1e-9);
        assert!((config.hl_max_values[5] - 8.0).abs() < 1e-9);
        // Entry z: 1.5 - 2.5
        assert!((config.entry_z_values[0] - 1.5).abs() < 1e-9);
        assert!((config.entry_z_values[4] - 2.5).abs() < 1e-9);
        // Exit z: 0.25 - 1.0
        assert!((config.exit_z_values[0] - 0.25).abs() < 1e-9);
        assert!((config.exit_z_values[3] - 1.0).abs() < 1e-9);
    }

    #[test]
    fn test_sensitivity_config_entry_z_always_above_exit_z() {
        // All entry_z values should be > default exit_z.
        // All exit_z values should be < default entry_z.
        // When sweeping one, the other stays at its default — this guards that
        // no individual sweep value creates entry_z <= exit_z with the base config.
        let config = SensitivityConfig::default();
        for &ez in &config.entry_z_values {
            assert!(
                ez > DEFAULT_EXIT_Z,
                "entry_z={ez} must be > DEFAULT_EXIT_Z={DEFAULT_EXIT_Z}"
            );
        }
        for &xz in &config.exit_z_values {
            assert!(
                xz < DEFAULT_ENTRY_Z,
                "exit_z={xz} must be < DEFAULT_ENTRY_Z={DEFAULT_ENTRY_Z}"
            );
        }
    }

    // ── Insufficient data test ────────────────────────────────────────────────

    #[test]
    fn test_sensitivity_returns_none_for_insufficient_data() {
        let (pa, pb) = test_utils::cointegrated_pair(50, 1.5, 10.0, 42);
        let prices = make_prices(vec![("A", pa), ("B", pb)]);
        let candidates = vec![candidate("A", "B")];
        let config = SensitivityConfig::default();

        let result = run_sensitivity_analysis(&candidates, &prices, &config);
        assert!(
            result.is_none(),
            "Expected None for insufficient data (50 bars < {MIN_HISTORY_BARS})"
        );
    }

    // ── Full sensitivity run with adequate data ───────────────────────────────

    #[test]
    fn test_sensitivity_produces_all_sweeps() {
        // Need enough bars for walk-forward with the smallest formation window (15 days).
        // Smallest formation: 15, trading: 21 → need 15+21*3 = 78 bars minimum.
        // Use 300 bars to be safe with the larger formation windows (up to 60 days).
        let n = 300;
        let (pa, pb) = test_utils::cointegrated_pair(n, 1.5, 10.0, 42);
        let prices = make_prices(vec![("A", pa), ("B", pb)]);
        let candidates = vec![candidate("A", "B")];

        // Use a minimal sweep config to keep the test fast
        let config = SensitivityConfig {
            hl_min_values: vec![1.0, 2.0],
            hl_max_values: vec![4.0, 6.0],
            z_lookback_values: vec![15, 30],
            entry_z_values: vec![1.5, 2.0],
            exit_z_values: vec![0.25, 0.5],
            r2_cutoff_values: vec![0.20, 0.40],
            base_walk_forward: WalkForwardConfig {
                formation_days: 60,
                trading_days: 21,
                ..Default::default()
            },
        };

        let result = run_sensitivity_analysis(&candidates, &prices, &config);
        assert!(result.is_some(), "Expected Some(result)");
        let result = result.unwrap();

        // Should produce exactly 6 sweeps (one per parameter)
        assert_eq!(result.sweeps.len(), 6, "Expected 6 parameter sweeps");

        // Each sweep should have the right number of points
        assert_eq!(result.sweeps[0].points.len(), 2, "HL min sweep");
        assert_eq!(result.sweeps[1].points.len(), 2, "HL max sweep");
        assert_eq!(result.sweeps[2].points.len(), 2, "Z lookback sweep");
        assert_eq!(result.sweeps[3].points.len(), 2, "Entry z sweep");
        assert_eq!(result.sweeps[4].points.len(), 2, "Exit z sweep");
        assert_eq!(result.sweeps[5].points.len(), 2, "R² cutoff sweep");

        // Total evaluations should match
        assert_eq!(result.total_evaluations, 12, "2 values × 6 params");
    }

    #[test]
    fn test_sweep_points_have_valid_metrics() {
        let n = 300;
        let (pa, pb) = test_utils::cointegrated_pair(n, 1.5, 10.0, 42);
        let prices = make_prices(vec![("A", pa), ("B", pb)]);
        let candidates = vec![candidate("A", "B")];

        let config = SensitivityConfig {
            hl_min_values: vec![1.0, 2.0, 3.0],
            hl_max_values: vec![4.0],
            z_lookback_values: vec![30],
            entry_z_values: vec![1.5, 2.0, 2.5],
            exit_z_values: vec![0.5],
            r2_cutoff_values: vec![0.3],
            base_walk_forward: WalkForwardConfig {
                formation_days: 60,
                trading_days: 21,
                ..Default::default()
            },
        };

        let result = run_sensitivity_analysis(&candidates, &prices, &config).unwrap();

        for sweep in &result.sweeps {
            for pt in &sweep.points {
                assert!(
                    pt.oos_sharpe.is_finite(),
                    "OOS Sharpe must be finite: param={} value={}",
                    sweep.param_name,
                    pt.param_value
                );
                assert!(
                    (0.0..=1.0).contains(&pt.win_rate),
                    "Win rate must be in [0,1]: param={} value={} win_rate={}",
                    sweep.param_name,
                    pt.param_value,
                    pt.win_rate
                );
                assert!(
                    pt.total_pnl_usd.is_finite(),
                    "P&L must be finite: param={} value={}",
                    sweep.param_name,
                    pt.param_value
                );
                assert!(
                    pt.param_value > 0.0,
                    "param_value must be positive: param={} value={}",
                    sweep.param_name,
                    pt.param_value
                );
            }
            assert!(
                (0.0..=1.0).contains(&sweep.profitable_fraction),
                "profitable_fraction out of range: param={} frac={}",
                sweep.param_name,
                sweep.profitable_fraction
            );
        }
    }

    #[test]
    fn test_cliff_edge_count_matches_detected_cliffs() {
        let n = 300;
        let (pa, pb) = test_utils::cointegrated_pair(n, 1.5, 10.0, 42);
        let prices = make_prices(vec![("A", pa), ("B", pb)]);
        let candidates = vec![candidate("A", "B")];

        let config = SensitivityConfig {
            entry_z_values: vec![1.5, 1.75, 2.0, 2.25, 2.5],
            hl_min_values: vec![2.0],
            hl_max_values: vec![5.0],
            z_lookback_values: vec![30],
            exit_z_values: vec![0.5],
            r2_cutoff_values: vec![0.3],
            base_walk_forward: WalkForwardConfig {
                formation_days: 60,
                trading_days: 21,
                ..Default::default()
            },
        };

        let result = run_sensitivity_analysis(&candidates, &prices, &config).unwrap();

        // Find entry_z sweep
        let entry_z_sweep = result
            .sweeps
            .iter()
            .find(|s| s.param_name.contains("Entry"))
            .expect("Entry z sweep should exist");

        let counted_cliffs = entry_z_sweep
            .points
            .iter()
            .filter(|p| p.is_cliff_edge)
            .count();
        assert_eq!(
            entry_z_sweep.n_cliff_edges, counted_cliffs,
            "Cliff edge count mismatch"
        );
    }

    #[test]
    fn test_invalid_hl_filter_range_returns_none() {
        // HL min > HL max → invalid, should return None
        let n = 300;
        let (pa, pb) = test_utils::cointegrated_pair(n, 1.5, 10.0, 42);
        let prices = make_prices(vec![("A", pa), ("B", pb)]);
        let candidates = vec![candidate("A", "B")];
        let base_config = WalkForwardConfig {
            formation_days: 60,
            trading_days: 21,
            ..Default::default()
        };
        let ctx = SweepContext {
            candidates: &candidates,
            prices: &prices,
            base_config: &base_config,
        };

        let result = run_walk_forward_with_hl_filter(&ctx, Some(10.0), Some(2.0));
        assert!(
            result.is_none(),
            "Invalid HL range (min > max) should return None"
        );
    }

    #[test]
    fn test_r2_cutoff_zero_allows_all_candidates() {
        // R² cutoff of 0.0 — all candidates pass → walk-forward runs
        let n = 300;
        let (pa, pb) = test_utils::cointegrated_pair(n, 1.5, 10.0, 42);
        let prices = make_prices(vec![("A", pa), ("B", pb)]);
        let candidates = vec![candidate("A", "B")];
        let base_config = WalkForwardConfig {
            formation_days: 60,
            trading_days: 21,
            ..Default::default()
        };
        let ctx = SweepContext {
            candidates: &candidates,
            prices: &prices,
            base_config: &base_config,
        };

        let result = run_walk_forward_with_r2_filter(&ctx, 0.0);
        assert!(
            result.is_some(),
            "R² cutoff of 0.0 should allow all candidates through"
        );
    }

    #[test]
    fn test_r2_cutoff_one_yields_no_trades() {
        // R² cutoff of 1.0 — no candidate can achieve perfect R² → zero trades
        let n = 300;
        let (pa, pb) = test_utils::cointegrated_pair(n, 1.5, 10.0, 42);
        let prices = make_prices(vec![("A", pa), ("B", pb)]);
        let candidates = vec![candidate("A", "B")];
        let base_config = WalkForwardConfig {
            formation_days: 60,
            trading_days: 21,
            ..Default::default()
        };
        let ctx = SweepContext {
            candidates: &candidates,
            prices: &prices,
            base_config: &base_config,
        };

        let result = run_walk_forward_with_r2_filter(&ctx, 1.0);
        if let Some(s) = result {
            assert_eq!(s.total_trades, 0, "R² cutoff of 1.0 should yield no trades");
        }
        // None is also acceptable if no candidates pass
    }
}
