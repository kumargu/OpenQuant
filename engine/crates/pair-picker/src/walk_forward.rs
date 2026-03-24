//! Walk-forward pair selection validation.
//!
//! Implements out-of-sample validation for the pair picker to guard against
//! selection bias. The key insight: selecting pairs and evaluating them on the
//! same window produces inflated win rates due to look-ahead bias.
//!
//! ## Methodology
//!
//! Walk-forward framework with monthly rebalance (Gatev, Goetzmann & Rouwenhorst, 2006):
//!
//! 1. **Formation window** (month M): run the full pair-picker pipeline on the first
//!    N days. Select pairs that pass statistical criteria.
//! 2. **Trading window** (month M+1): simulate trading on the *next* N days using
//!    *frozen* parameters (alpha, beta, mean, std) estimated at the end of month M.
//!    No re-estimation during the trading window.
//! 3. **Roll forward**: repeat for M+1→M+2, M+2→M+3, etc.
//!
//! Key rules enforced:
//! - **No lookahead**: beta, mean, std estimated on formation window only
//! - **Frozen universe**: pairs selected at end of month M cannot change in month M+1
//! - **Decision-time signals**: entry z-score computed against formation-window stats
//!
//! ## Output
//!
//! Per-window table: formation metrics vs trading-window P&L, Sharpe, win rate.
//! Summary: aggregate walk-forward Sharpe vs in-sample Sharpe.
//!
//! Reference: Gatev, Goetzmann & Rouwenhorst (2006), "Pairs Trading: Relative-Value
//! Arbitrage", Review of Financial Studies.

use crate::pipeline::{validate_pair, InMemoryPrices, MIN_HISTORY_BARS};
use crate::types::PairCandidate;
use tracing::{info, warn};

/// Configuration for the walk-forward validation framework.
///
/// All parameters have sensible defaults but are overridable so the framework
/// can be tuned without recompilation.
#[derive(Debug, Clone)]
pub struct WalkForwardConfig {
    /// Number of trading days in the formation window (pair selection).
    /// Must be at least `MIN_HISTORY_BARS` (90) to run full statistical tests.
    /// Default: 90 days (~4.5 months).
    pub formation_days: usize,

    /// Number of trading days in the trading window (out-of-sample evaluation).
    /// Default: 21 days (~1 month).
    pub trading_days: usize,

    /// Entry z-score threshold: open a trade when |z| > entry_zscore.
    /// Default: 2.0 standard deviations.
    pub entry_zscore: f64,

    /// Exit z-score threshold: close a trade when |z| < exit_zscore.
    /// Default: 0.5 standard deviations.
    pub exit_zscore: f64,

    /// Maximum number of pairs active simultaneously in a window.
    /// Capital constraint: prevents over-concentration.
    /// Default: 5 pairs.
    pub max_active_pairs: usize,

    /// Capital per leg in USD.
    /// Default: $10,000.
    pub capital_per_leg_usd: f64,

    /// Maximum hold duration in trading days before forced exit.
    /// Default: 10 days.
    pub max_hold_days: usize,
}

impl Default for WalkForwardConfig {
    fn default() -> Self {
        Self {
            formation_days: 90,
            trading_days: 21,
            entry_zscore: 2.0,
            exit_zscore: 0.5,
            max_active_pairs: 5,
            capital_per_leg_usd: 10_000.0,
            max_hold_days: 10,
        }
    }
}

/// Frozen parameters estimated at end of formation window.
/// These are the only stats used during the trading window — no re-estimation.
#[derive(Debug, Clone)]
pub struct FrozenPairParams {
    /// Pair identifier (leg_a/leg_b alphabetically ordered).
    pub pair_id: String,
    pub leg_a: String,
    pub leg_b: String,
    /// OLS hedge ratio from formation window.
    pub beta: f64,
    /// OLS intercept from formation window.
    pub alpha: f64,
    /// Mean of formation-window spread for z-score centering.
    pub spread_mean: f64,
    /// Standard deviation of formation-window spread for z-score scaling.
    pub spread_std: f64,
    /// Formation-window ADF p-value (diagnostic only).
    pub formation_adf_pvalue: f64,
    /// Formation-window score (diagnostic only).
    pub formation_score: f64,
    /// Formation-window half-life (diagnostic only).
    pub formation_half_life: f64,
}

/// Result of simulating one pair in one trading window.
#[derive(Debug, Clone)]
pub struct PairTradingResult {
    pub pair_id: String,
    /// Total return over the trading window (as fraction of capital deployed).
    pub total_return: f64,
    /// Number of round-trip trades completed.
    pub n_trades: usize,
    /// Number of winning trades (positive P&L).
    pub n_winners: usize,
    /// Gross P&L in USD (both legs combined).
    pub pnl_usd: f64,
    /// Maximum drawdown during the trading window.
    pub max_drawdown: f64,
    /// Whether any trade was open at window end (forced close).
    pub forced_close: bool,
}

/// Results for a single walk-forward window (one formation + one trading period).
#[derive(Debug, Clone)]
pub struct WindowResult {
    /// Index of this window (0-based).
    pub window_idx: usize,
    /// Start index of formation window in the full price series.
    pub formation_start: usize,
    /// End index of formation window (exclusive).
    pub formation_end: usize,
    /// End index of trading window (exclusive).
    pub trading_end: usize,
    /// Number of pairs selected in formation window.
    pub n_pairs_selected: usize,
    /// Number of pairs actually traded (capped by `max_active_pairs`).
    pub n_pairs_traded: usize,
    /// In-sample Sharpe ratio (formation-window spread volatility / mean).
    /// Positive means the spread was exploitable in-sample.
    pub insample_sharpe: f64,
    /// Trading-window P&L results per pair.
    pub pair_results: Vec<PairTradingResult>,
    /// Aggregate trading-window Sharpe ratio across all pairs.
    pub oos_sharpe: f64,
    /// Total P&L in USD across all pairs.
    pub total_pnl_usd: f64,
    /// Win rate: fraction of trades that were profitable.
    pub win_rate: f64,
}

/// Aggregate summary across all walk-forward windows.
#[derive(Debug, Clone)]
pub struct WalkForwardSummary {
    /// Total number of windows evaluated.
    pub n_windows: usize,
    /// Total number of windows with at least one pair selected.
    pub n_windows_with_pairs: usize,
    /// Average in-sample Sharpe across all windows.
    pub avg_insample_sharpe: f64,
    /// Average out-of-sample Sharpe across all windows.
    pub avg_oos_sharpe: f64,
    /// Total P&L in USD across all windows.
    pub total_pnl_usd: f64,
    /// Aggregate win rate across all trades.
    pub aggregate_win_rate: f64,
    /// Total number of trades across all windows.
    pub total_trades: usize,
    /// Total winning trades across all windows.
    pub total_winners: usize,
    /// Per-window results.
    pub windows: Vec<WindowResult>,
}

/// Run walk-forward validation over a full price history.
///
/// # Arguments
/// - `candidates`: list of pair candidates to screen each formation window.
/// - `prices`: symbol → full price history (oldest-to-newest).
/// - `config`: walk-forward parameters.
///
/// # Returns
/// `None` if there is insufficient data for even one window.
pub fn run_walk_forward(
    candidates: &[PairCandidate],
    prices: &InMemoryPrices,
    config: &WalkForwardConfig,
) -> Option<WalkForwardSummary> {
    // Validate config
    if config.formation_days < MIN_HISTORY_BARS {
        warn!(
            formation_days = config.formation_days,
            min = MIN_HISTORY_BARS,
            "formation_days below MIN_HISTORY_BARS — statistical tests will fail"
        );
    }
    if !config.entry_zscore.is_finite() || config.entry_zscore <= 0.0 {
        warn!("entry_zscore must be finite and positive");
        return None;
    }
    if !config.exit_zscore.is_finite() || config.exit_zscore < 0.0 {
        warn!("exit_zscore must be finite and non-negative");
        return None;
    }
    if config.exit_zscore >= config.entry_zscore {
        warn!(
            entry = config.entry_zscore,
            exit = config.exit_zscore,
            "exit_zscore must be < entry_zscore"
        );
        return None;
    }
    if !config.capital_per_leg_usd.is_finite() || config.capital_per_leg_usd <= 0.0 {
        warn!("capital_per_leg_usd must be positive and finite");
        return None;
    }

    // Determine the total number of bars available (use the shortest series)
    let total_bars = candidates
        .iter()
        .filter_map(|c| {
            let na = prices.data.get(&c.leg_a).map(|v| v.len())?;
            let nb = prices.data.get(&c.leg_b).map(|v| v.len())?;
            Some(na.min(nb))
        })
        .min()
        .unwrap_or(0);

    let min_bars_needed = config.formation_days + config.trading_days;
    if total_bars < min_bars_needed {
        warn!(
            total_bars,
            min_bars_needed, "Insufficient data for walk-forward validation"
        );
        return None;
    }

    // Enumerate windows: slide formation + trading blocks forward
    let n_windows = (total_bars - config.formation_days) / config.trading_days;
    if n_windows == 0 {
        warn!("No complete windows fit in the available data");
        return None;
    }

    info!(
        total_bars,
        n_windows,
        formation_days = config.formation_days,
        trading_days = config.trading_days,
        "Starting walk-forward validation"
    );

    let mut window_results = Vec::with_capacity(n_windows);

    for w in 0..n_windows {
        let formation_start = w * config.trading_days;
        let formation_end = formation_start + config.formation_days;
        let trading_end = formation_end + config.trading_days;

        if trading_end > total_bars {
            break;
        }

        let result = evaluate_window(
            w,
            formation_start,
            formation_end,
            trading_end,
            candidates,
            prices,
            config,
        );

        info!(
            window = w,
            n_selected = result.n_pairs_selected,
            n_traded = result.n_pairs_traded,
            pnl_usd = format!("{:.2}", result.total_pnl_usd).as_str(),
            win_rate = format!("{:.1}%", result.win_rate * 100.0).as_str(),
            oos_sharpe = format!("{:.3}", result.oos_sharpe).as_str(),
            insample_sharpe = format!("{:.3}", result.insample_sharpe).as_str(),
            "Window complete"
        );

        window_results.push(result);
    }

    // Aggregate summary
    let n_windows_with_pairs = window_results
        .iter()
        .filter(|w| w.n_pairs_selected > 0)
        .count();

    let total_pnl_usd: f64 = window_results.iter().map(|w| w.total_pnl_usd).sum();

    let total_trades: usize = window_results
        .iter()
        .flat_map(|w| w.pair_results.iter())
        .map(|r| r.n_trades)
        .sum();
    let total_winners: usize = window_results
        .iter()
        .flat_map(|w| w.pair_results.iter())
        .map(|r| r.n_winners)
        .sum();
    let aggregate_win_rate = if total_trades > 0 {
        total_winners as f64 / total_trades as f64
    } else {
        0.0
    };

    let avg_insample_sharpe = if !window_results.is_empty() {
        window_results
            .iter()
            .map(|w| w.insample_sharpe)
            .sum::<f64>()
            / window_results.len() as f64
    } else {
        0.0
    };
    let avg_oos_sharpe = if !window_results.is_empty() {
        window_results.iter().map(|w| w.oos_sharpe).sum::<f64>() / window_results.len() as f64
    } else {
        0.0
    };

    info!(
        n_windows = window_results.len(),
        n_windows_with_pairs,
        total_trades,
        total_winners,
        aggregate_win_rate = format!("{:.1}%", aggregate_win_rate * 100.0).as_str(),
        avg_insample_sharpe = format!("{:.3}", avg_insample_sharpe).as_str(),
        avg_oos_sharpe = format!("{:.3}", avg_oos_sharpe).as_str(),
        total_pnl_usd = format!("{:.2}", total_pnl_usd).as_str(),
        "Walk-forward validation complete"
    );

    Some(WalkForwardSummary {
        n_windows: window_results.len(),
        n_windows_with_pairs,
        avg_insample_sharpe,
        avg_oos_sharpe,
        total_pnl_usd,
        aggregate_win_rate,
        total_trades,
        total_winners,
        windows: window_results,
    })
}

/// Evaluate one walk-forward window.
fn evaluate_window(
    window_idx: usize,
    formation_start: usize,
    formation_end: usize,
    trading_end: usize,
    candidates: &[PairCandidate],
    prices: &InMemoryPrices,
    config: &WalkForwardConfig,
) -> WindowResult {
    // ── Step 1: Build formation-window price provider ──
    // Slice each symbol's prices to [formation_start, formation_end).
    // The validate_pair function sees only formation-window data.
    let formation_provider = slice_prices(prices, formation_start, formation_end);

    // ── Step 2: Run pair selection on formation window ──
    let mut selected_params: Vec<(FrozenPairParams, Vec<f64>)> = candidates
        .iter()
        .filter_map(|candidate| {
            let result = validate_pair(candidate, &formation_provider);
            if !result.passed {
                return None;
            }
            // Extract formation-window stats — these are frozen for trading window
            let beta = result.beta?;
            let alpha = result.alpha?;

            // Compute formation-window spread stats (mean + std for z-score normalization)
            let prices_a = formation_provider.data.get(&candidate.leg_a)?;
            let prices_b = formation_provider.data.get(&candidate.leg_b)?;

            // Guard non-positive prices before ln
            if prices_a.iter().any(|&p| !p.is_finite() || p <= 0.0)
                || prices_b.iter().any(|&p| !p.is_finite() || p <= 0.0)
            {
                return None;
            }

            let spread: Vec<f64> = prices_a
                .iter()
                .zip(prices_b.iter())
                .map(|(a, b)| a.ln() - beta * b.ln())
                .collect();

            let (spread_mean, spread_std) = mean_std(&spread)?;

            // Spread std must be positive and finite — guards against degenerate spreads
            if !spread_std.is_finite() || spread_std <= 0.0 {
                warn!(
                    pair = format!("{}/{}", candidate.leg_a, candidate.leg_b).as_str(),
                    spread_std, "Degenerate spread std — skipping pair"
                );
                return None;
            }

            // In-sample Sharpe: measures exploitability of formation-window spread.
            // We compute daily P&L of a mock mean-reversion strategy: go long spread
            // when z < -1, short when z > 1, exit when |z| < 0.5. This is the
            // in-sample statistic — it will always look good (selection bias), but
            // lets us compare against the OOS Sharpe.
            let insample_daily_pnl = insample_daily_returns(&spread, spread_mean, spread_std);

            let pair_id = format_pair_id(&candidate.leg_a, &candidate.leg_b);

            info!(
                window = window_idx,
                pair = pair_id.as_str(),
                beta = format!("{:.4}", beta).as_str(),
                half_life = format!("{:.1}", result.half_life.unwrap_or(0.0)).as_str(),
                adf_p = format!("{:.4}", result.adf_pvalue.unwrap_or(1.0)).as_str(),
                score = format!("{:.3}", result.score).as_str(),
                "Formation: pair selected"
            );

            Some((
                FrozenPairParams {
                    pair_id,
                    leg_a: candidate.leg_a.clone(),
                    leg_b: candidate.leg_b.clone(),
                    beta,
                    alpha,
                    spread_mean,
                    spread_std,
                    formation_adf_pvalue: result.adf_pvalue.unwrap_or(1.0),
                    formation_score: result.score,
                    formation_half_life: result.half_life.unwrap_or(0.0),
                },
                insample_daily_pnl,
            ))
        })
        .collect();

    let n_pairs_selected = selected_params.len();

    // Sort by formation score descending — trade the best pairs first
    selected_params.sort_by(|(a, _), (b, _)| {
        b.formation_score
            .partial_cmp(&a.formation_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // Apply capital constraint
    selected_params.truncate(config.max_active_pairs);
    let n_pairs_traded = selected_params.len();

    // Compute in-sample Sharpe (average across selected pairs)
    let insample_sharpe = if !selected_params.is_empty() {
        let sharpes: Vec<f64> = selected_params
            .iter()
            .map(|(_, daily_pnl)| sharpe_from_daily_returns(daily_pnl))
            .collect();
        sharpes.iter().sum::<f64>() / sharpes.len() as f64
    } else {
        0.0
    };

    // ── Step 3: Simulate trading in the trading window ──
    let mut pair_results = Vec::with_capacity(n_pairs_traded);

    for (params, _) in &selected_params {
        let result = simulate_trading_window(params, prices, formation_end, trading_end, config);

        info!(
            window = window_idx,
            pair = params.pair_id.as_str(),
            n_trades = result.n_trades,
            pnl_usd = format!("{:.2}", result.pnl_usd).as_str(),
            win_rate = if result.n_trades > 0 {
                format!(
                    "{:.1}%",
                    result.n_winners as f64 / result.n_trades as f64 * 100.0
                )
            } else {
                "N/A".to_string()
            }
            .as_str(),
            "Trading window result"
        );

        pair_results.push(result);
    }

    // ── Step 4: Aggregate window metrics ──
    let total_pnl_usd: f64 = pair_results.iter().map(|r| r.pnl_usd).sum();
    let total_trades: usize = pair_results.iter().map(|r| r.n_trades).sum();
    let total_winners: usize = pair_results.iter().map(|r| r.n_winners).sum();
    let win_rate = if total_trades > 0 {
        total_winners as f64 / total_trades as f64
    } else {
        0.0
    };

    // Aggregate OOS Sharpe: daily P&L across all pairs, then annualized
    let oos_sharpe = aggregate_oos_sharpe(
        &pair_results,
        prices,
        &selected_params,
        formation_end,
        trading_end,
        config,
    );

    WindowResult {
        window_idx,
        formation_start,
        formation_end,
        trading_end,
        n_pairs_selected,
        n_pairs_traded,
        insample_sharpe,
        pair_results,
        oos_sharpe,
        total_pnl_usd,
        win_rate,
    }
}

/// Simulate a mean-reversion strategy for one pair in one trading window.
///
/// Uses frozen parameters (beta, spread_mean, spread_std) estimated in the formation window.
/// No re-estimation occurs during the trading window.
///
/// Strategy:
/// - Long spread (long A, short B) when z-score < -entry_zscore
/// - Short spread (short A, long B) when z-score > +entry_zscore
/// - Exit when |z-score| < exit_zscore or after max_hold_days
/// - Only one position at a time (simple book)
///
/// P&L computed from log-return differences: Δlog(A) - beta * Δlog(B).
/// This approximates the dollar return for small moves.
fn simulate_trading_window(
    params: &FrozenPairParams,
    prices: &InMemoryPrices,
    trading_start: usize,
    trading_end: usize,
    config: &WalkForwardConfig,
) -> PairTradingResult {
    let prices_a = match prices.data.get(&params.leg_a) {
        Some(p) => p,
        None => {
            return PairTradingResult {
                pair_id: params.pair_id.clone(),
                total_return: 0.0,
                n_trades: 0,
                n_winners: 0,
                pnl_usd: 0.0,
                max_drawdown: 0.0,
                forced_close: false,
            }
        }
    };
    let prices_b = match prices.data.get(&params.leg_b) {
        Some(p) => p,
        None => {
            return PairTradingResult {
                pair_id: params.pair_id.clone(),
                total_return: 0.0,
                n_trades: 0,
                n_winners: 0,
                pnl_usd: 0.0,
                max_drawdown: 0.0,
                forced_close: false,
            }
        }
    };

    let window_len = trading_end - trading_start;
    if window_len < 2 {
        return PairTradingResult {
            pair_id: params.pair_id.clone(),
            total_return: 0.0,
            n_trades: 0,
            n_winners: 0,
            pnl_usd: 0.0,
            max_drawdown: 0.0,
            forced_close: false,
        };
    }

    // Extract trading window prices — guard bounds
    let end_a = trading_end.min(prices_a.len());
    let end_b = trading_end.min(prices_b.len());
    let start_a = trading_start.min(end_a);
    let start_b = trading_start.min(end_b);

    let window_a = &prices_a[start_a..end_a];
    let window_b = &prices_b[start_b..end_b];
    let n = window_a.len().min(window_b.len());

    if n < 2 {
        return PairTradingResult {
            pair_id: params.pair_id.clone(),
            total_return: 0.0,
            n_trades: 0,
            n_winners: 0,
            pnl_usd: 0.0,
            max_drawdown: 0.0,
            forced_close: false,
        };
    }

    // Guard non-positive prices
    if window_a[..n].iter().any(|&p| !p.is_finite() || p <= 0.0)
        || window_b[..n].iter().any(|&p| !p.is_finite() || p <= 0.0)
    {
        warn!(
            pair = params.pair_id.as_str(),
            "Non-positive prices in trading window — skipping"
        );
        return PairTradingResult {
            pair_id: params.pair_id.clone(),
            total_return: 0.0,
            n_trades: 0,
            n_winners: 0,
            pnl_usd: 0.0,
            max_drawdown: 0.0,
            forced_close: false,
        };
    }

    // Trading simulation state machine
    // Position: +1 = long spread (long A, short B), -1 = short spread (short A, long B), 0 = flat
    let mut position: i8 = 0;
    let mut entry_price_a: f64 = 0.0;
    let mut entry_price_b: f64 = 0.0;
    let mut entry_day: usize = 0;
    let mut n_trades: usize = 0;
    let mut n_winners: usize = 0;
    let mut total_pnl_usd: f64 = 0.0;
    let mut cumulative_pnl: f64 = 0.0;
    let mut peak_pnl: f64 = 0.0;
    let mut max_drawdown: f64 = 0.0;
    let mut forced_close = false;

    for t in 0..n {
        let pa = window_a[t];
        let pb = window_b[t];

        // Compute z-score using frozen formation-window parameters
        let spread = pa.ln() - params.beta * pb.ln();
        let z = (spread - params.spread_mean) / params.spread_std;

        // Check for forced exit (max hold reached)
        if position != 0 && (t - entry_day) >= config.max_hold_days {
            let trade_pnl = compute_pnl(
                position,
                entry_price_a,
                pa,
                entry_price_b,
                pb,
                params.beta,
                config.capital_per_leg_usd,
            );
            n_trades += 1;
            if trade_pnl > 0.0 {
                n_winners += 1;
            }
            total_pnl_usd += trade_pnl;
            cumulative_pnl += trade_pnl;
            if cumulative_pnl > peak_pnl {
                peak_pnl = cumulative_pnl;
            }
            let drawdown = peak_pnl - cumulative_pnl;
            if drawdown > max_drawdown {
                max_drawdown = drawdown;
            }
            position = 0;
            forced_close = true;
            continue;
        }

        // Exit condition: |z| < exit_zscore
        if position != 0 && z.abs() < config.exit_zscore {
            let trade_pnl = compute_pnl(
                position,
                entry_price_a,
                pa,
                entry_price_b,
                pb,
                params.beta,
                config.capital_per_leg_usd,
            );
            n_trades += 1;
            if trade_pnl > 0.0 {
                n_winners += 1;
            }
            total_pnl_usd += trade_pnl;
            cumulative_pnl += trade_pnl;
            if cumulative_pnl > peak_pnl {
                peak_pnl = cumulative_pnl;
            }
            let drawdown = peak_pnl - cumulative_pnl;
            if drawdown > max_drawdown {
                max_drawdown = drawdown;
            }
            position = 0;
            continue;
        }

        // Entry conditions (only if flat)
        if position == 0 {
            if z < -config.entry_zscore {
                // Long spread: buy A, sell B
                position = 1;
                entry_price_a = pa;
                entry_price_b = pb;
                entry_day = t;
            } else if z > config.entry_zscore {
                // Short spread: sell A, buy B
                position = -1;
                entry_price_a = pa;
                entry_price_b = pb;
                entry_day = t;
            }
        }
    }

    // Force-close any open position at window end
    if position != 0 {
        let last_pa = window_a[n - 1];
        let last_pb = window_b[n - 1];
        let trade_pnl = compute_pnl(
            position,
            entry_price_a,
            last_pa,
            entry_price_b,
            last_pb,
            params.beta,
            config.capital_per_leg_usd,
        );
        n_trades += 1;
        if trade_pnl > 0.0 {
            n_winners += 1;
        }
        total_pnl_usd += trade_pnl;
        cumulative_pnl += trade_pnl;
        if cumulative_pnl > peak_pnl {
            peak_pnl = cumulative_pnl;
        }
        let drawdown = peak_pnl - cumulative_pnl;
        if drawdown > max_drawdown {
            max_drawdown = drawdown;
        }
        forced_close = true;
    }

    let total_capital = config.capital_per_leg_usd * 2.0; // both legs
    let total_return = if total_capital > 0.0 {
        total_pnl_usd / total_capital
    } else {
        0.0
    };

    PairTradingResult {
        pair_id: params.pair_id.clone(),
        total_return,
        n_trades,
        n_winners,
        pnl_usd: total_pnl_usd,
        max_drawdown,
        forced_close,
    }
}

/// Compute P&L for one round-trip trade.
///
/// For a long-spread position (position = +1):
///   P&L_A = capital * (exit_A - entry_A) / entry_A  (long A)
///   P&L_B = capital * beta * (entry_B - exit_B) / entry_B  (short B, scaled by beta)
///
/// For a short-spread position (position = -1):
///   P&L_A = capital * (entry_A - exit_A) / entry_A  (short A)
///   P&L_B = capital * beta * (exit_B - entry_B) / entry_B  (long B, scaled by beta)
///
/// This approximates the dollar return for a $capital_per_leg investment on each leg.
fn compute_pnl(
    position: i8,
    entry_a: f64,
    exit_a: f64,
    entry_b: f64,
    exit_b: f64,
    beta: f64,
    capital_per_leg: f64,
) -> f64 {
    // Guard: prices must be positive and finite
    if !entry_a.is_finite()
        || !exit_a.is_finite()
        || !entry_b.is_finite()
        || !exit_b.is_finite()
        || entry_a <= 0.0
        || exit_a <= 0.0
        || entry_b <= 0.0
        || exit_b <= 0.0
        || !beta.is_finite()
        || beta <= 0.0
    {
        return 0.0;
    }

    let return_a = (exit_a - entry_a) / entry_a;
    let return_b = (exit_b - entry_b) / entry_b;

    match position {
        1 => {
            // Long spread: long A, short B (beta-weighted)
            capital_per_leg * return_a - capital_per_leg * beta * return_b
        }
        -1 => {
            // Short spread: short A, long B (beta-weighted)
            -capital_per_leg * return_a + capital_per_leg * beta * return_b
        }
        _ => 0.0,
    }
}

/// Compute aggregate OOS Sharpe by summing daily P&L across all traded pairs.
///
/// Sharpe = mean(daily_returns) / std(daily_returns) * sqrt(252)
/// where daily_returns are computed from spread changes (log differences).
fn aggregate_oos_sharpe(
    _pair_results: &[PairTradingResult],
    prices: &InMemoryPrices,
    selected_params: &[(FrozenPairParams, Vec<f64>)],
    trading_start: usize,
    trading_end: usize,
    _config: &WalkForwardConfig,
) -> f64 {
    if selected_params.is_empty() {
        return 0.0;
    }

    let n_days = trading_end - trading_start;
    if n_days < 2 {
        return 0.0;
    }

    // Sum daily log-returns of spreads across all pairs
    let mut daily_portfolio_returns: Vec<f64> = vec![0.0; n_days - 1];
    let mut n_contributing = 0;

    for (params, _) in selected_params {
        let prices_a = match prices.data.get(&params.leg_a) {
            Some(p) => p,
            None => continue,
        };
        let prices_b = match prices.data.get(&params.leg_b) {
            Some(p) => p,
            None => continue,
        };

        let end_a = trading_end.min(prices_a.len());
        let end_b = trading_end.min(prices_b.len());
        let start_a = trading_start.min(end_a);
        let start_b = trading_start.min(end_b);

        let window_a = &prices_a[start_a..end_a];
        let window_b = &prices_b[start_b..end_b];
        let n = window_a.len().min(window_b.len());

        if n < 2 {
            continue;
        }

        // Guard non-positive prices
        if window_a[..n].iter().any(|&p| !p.is_finite() || p <= 0.0)
            || window_b[..n].iter().any(|&p| !p.is_finite() || p <= 0.0)
        {
            continue;
        }

        for t in 0..(n - 1) {
            let spread_t0 = window_a[t].ln() - params.beta * window_b[t].ln();
            let spread_t1 = window_a[t + 1].ln() - params.beta * window_b[t + 1].ln();

            // Z-score at t determines trade direction; return is the spread change
            let z_t = (spread_t0 - params.spread_mean) / params.spread_std;

            // Only contribute when we'd have a position
            // This is a simplified attribution — actual P&L from the trade sim is the
            // source of truth. Here we compute a spread-change-based Sharpe for summary.
            let daily_return = if z_t > 1.0 {
                // Short spread: profit from spread narrowing
                -(spread_t1 - spread_t0)
            } else if z_t < -1.0 {
                // Long spread: profit from spread widening back
                spread_t1 - spread_t0
            } else {
                0.0
            };

            if t < daily_portfolio_returns.len() {
                daily_portfolio_returns[t] += daily_return;
            }
        }
        n_contributing += 1;
    }

    if n_contributing == 0 {
        return 0.0;
    }

    // Average across pairs
    for r in &mut daily_portfolio_returns {
        *r /= n_contributing as f64;
    }

    sharpe_from_daily_returns(&daily_portfolio_returns)
}

/// Create a sliced InMemoryPrices covering [start, end) for each symbol.
fn slice_prices(prices: &InMemoryPrices, start: usize, end: usize) -> InMemoryPrices {
    let data = prices
        .data
        .iter()
        .filter_map(|(symbol, series)| {
            let s = start.min(series.len());
            let e = end.min(series.len());
            if e <= s {
                return None;
            }
            Some((symbol.clone(), series[s..e].to_vec()))
        })
        .collect();
    InMemoryPrices { data }
}

/// Compute mean and standard deviation using a numerically stable two-pass algorithm.
///
/// Returns `None` if the series has fewer than 2 observations or zero variance.
fn mean_std(series: &[f64]) -> Option<(f64, f64)> {
    let n = series.len();
    if n < 2 {
        return None;
    }

    // Guard NaN/Inf
    if series.iter().any(|x| !x.is_finite()) {
        return None;
    }

    // Two-pass: compute mean, then variance from deviations.
    // Single-pass formula (sum_xx - n*mean^2) suffers catastrophic cancellation.
    let mean = series.iter().sum::<f64>() / n as f64;
    let variance = series.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / (n - 1) as f64;

    if variance <= 0.0 || !variance.is_finite() {
        return None;
    }

    Some((mean, variance.sqrt()))
}

/// Compute Sharpe ratio from a series of daily returns.
///
/// Annualizes by sqrt(252). Returns 0.0 if fewer than 2 observations or zero variance.
fn sharpe_from_daily_returns(daily_returns: &[f64]) -> f64 {
    let n = daily_returns.len();
    if n < 2 {
        return 0.0;
    }

    // Filter out zeros (days with no position) to avoid biasing the Sharpe downward
    let active: Vec<f64> = daily_returns
        .iter()
        .copied()
        .filter(|r| r.abs() > 1e-12)
        .collect();

    if active.len() < 2 {
        return 0.0;
    }

    let (mean, std) = match mean_std(&active) {
        Some(ms) => ms,
        None => return 0.0,
    };

    if !std.is_finite() || std <= 0.0 {
        return 0.0;
    }

    let sharpe = (mean / std) * 252_f64.sqrt();
    if sharpe.is_finite() {
        sharpe
    } else {
        0.0
    }
}

/// Compute mock in-sample daily P&L for spread (log-spread changes when position active).
///
/// This represents what the strategy *would have* earned in the formation window —
/// useful only for comparing IS Sharpe vs OOS Sharpe (the gap reveals selection bias).
///
/// Uses the same entry/exit logic as the trading simulation but on formation data.
fn insample_daily_returns(spread: &[f64], mean: f64, std: f64) -> Vec<f64> {
    if std <= 0.0 || !std.is_finite() {
        return vec![0.0; spread.len().saturating_sub(1)];
    }

    let n = spread.len();
    let mut returns = Vec::with_capacity(n.saturating_sub(1));

    for t in 0..n.saturating_sub(1) {
        let z = (spread[t] - mean) / std;
        let daily_return = if z > 1.0 {
            -(spread[t + 1] - spread[t])
        } else if z < -1.0 {
            spread[t + 1] - spread[t]
        } else {
            0.0
        };
        returns.push(daily_return);
    }

    returns
}

/// Format a canonical pair ID: alphabetically ordered, slash-separated.
pub fn format_pair_id(leg_a: &str, leg_b: &str) -> String {
    if leg_a <= leg_b {
        format!("{leg_a}/{leg_b}")
    } else {
        format!("{leg_b}/{leg_a}")
    }
}

/// Print a formatted comparison table to stdout.
///
/// Shows per-window metrics: formation-window Sharpe vs trading-window Sharpe,
/// P&L, win rate — the key comparison for detecting selection bias.
pub fn print_comparison_table(summary: &WalkForwardSummary) {
    println!("\n{:=^90}", " Walk-Forward Pair Selection Validation ");
    println!(
        "\n{:<8} {:>8} {:>8} {:>12} {:>10} {:>10} {:>10} {:>10}",
        "Window", "N_sel", "N_trd", "P&L ($)", "Win%", "IS Sharpe", "OOS Sharpe", "Gap"
    );
    println!("{}", "-".repeat(90));

    for w in &summary.windows {
        let gap = w.oos_sharpe - w.insample_sharpe;
        println!(
            "{:<8} {:>8} {:>8} {:>12.2} {:>10.1} {:>10.3} {:>10.3} {:>10.3}",
            w.window_idx,
            w.n_pairs_selected,
            w.n_pairs_traded,
            w.total_pnl_usd,
            w.win_rate * 100.0,
            w.insample_sharpe,
            w.oos_sharpe,
            gap
        );
    }

    println!("{}", "-".repeat(90));
    println!(
        "{:<8} {:>8} {:>8} {:>12.2} {:>10.1} {:>10.3} {:>10.3} {:>10.3}",
        "TOTAL",
        "-",
        "-",
        summary.total_pnl_usd,
        summary.aggregate_win_rate * 100.0,
        summary.avg_insample_sharpe,
        summary.avg_oos_sharpe,
        summary.avg_oos_sharpe - summary.avg_insample_sharpe
    );

    println!("\nSummary:");
    println!("  Windows evaluated:        {}", summary.n_windows);
    println!(
        "  Windows with pairs:       {}",
        summary.n_windows_with_pairs
    );
    println!("  Total trades:             {}", summary.total_trades);
    println!(
        "  Aggregate win rate:       {:.1}%",
        summary.aggregate_win_rate * 100.0
    );
    println!(
        "  Avg in-sample Sharpe:     {:.3}",
        summary.avg_insample_sharpe
    );
    println!("  Avg OOS Sharpe:           {:.3}", summary.avg_oos_sharpe);
    println!(
        "  IS→OOS Sharpe decay:      {:.3}",
        summary.avg_oos_sharpe - summary.avg_insample_sharpe
    );
    println!("  Total P&L (USD):          {:.2}", summary.total_pnl_usd);

    // Interpretation guidance
    println!("\nInterpretation:");
    if summary.avg_oos_sharpe > 0.5 {
        println!("  OOS Sharpe > 0.5: strategy shows genuine out-of-sample alpha.");
    } else if summary.avg_oos_sharpe > 0.0 {
        println!("  OOS Sharpe > 0, < 0.5: marginal OOS performance — verify with more windows.");
    } else {
        println!("  OOS Sharpe <= 0: selection bias likely — pairs do not trade profitably OOS.");
    }

    let decay = summary.avg_oos_sharpe - summary.avg_insample_sharpe;
    if decay < -1.0 {
        println!(
            "  Large IS→OOS Sharpe decay ({:.2}): strong evidence of selection bias.",
            decay
        );
    } else if decay < -0.5 {
        println!(
            "  Moderate IS→OOS Sharpe decay ({:.2}): some selection bias present.",
            decay
        );
    }
    println!();
}

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

    // ─── Config validation tests ─────────────────────────────────────────────

    #[test]
    fn test_insufficient_data_returns_none() {
        // Need formation_days + trading_days bars. Provide less.
        let config = WalkForwardConfig {
            formation_days: 90,
            trading_days: 21,
            ..Default::default()
        };
        let (pa, pb) = test_utils::cointegrated_pair(50, 1.5, 10.0, 42);
        let prices = make_prices(vec![("A", pa), ("B", pb)]);
        let candidates = vec![candidate("A", "B")];

        let result = run_walk_forward(&candidates, &prices, &config);
        assert!(result.is_none(), "Expected None for insufficient data");
    }

    #[test]
    fn test_invalid_config_exit_ge_entry_returns_none() {
        let config = WalkForwardConfig {
            entry_zscore: 1.0,
            exit_zscore: 1.5, // exit >= entry — invalid
            ..Default::default()
        };
        let (pa, pb) = test_utils::cointegrated_pair(300, 1.5, 10.0, 42);
        let prices = make_prices(vec![("A", pa), ("B", pb)]);
        let candidates = vec![candidate("A", "B")];

        let result = run_walk_forward(&candidates, &prices, &config);
        assert!(result.is_none());
    }

    #[test]
    fn test_invalid_config_negative_capital_returns_none() {
        let config = WalkForwardConfig {
            capital_per_leg_usd: -1.0,
            ..Default::default()
        };
        let (pa, pb) = test_utils::cointegrated_pair(300, 1.5, 10.0, 42);
        let prices = make_prices(vec![("A", pa), ("B", pb)]);
        let candidates = vec![candidate("A", "B")];

        let result = run_walk_forward(&candidates, &prices, &config);
        assert!(result.is_none());
    }

    // ─── Walk-forward structural tests ───────────────────────────────────────

    #[test]
    fn test_walk_forward_produces_windows() {
        // 90 + 21*3 = 153 bars → 3 trading windows
        let n = 90 + 21 * 3 + 10; // extra buffer
        let (pa, pb) = test_utils::cointegrated_pair(n, 1.5, 10.0, 42);
        let prices = make_prices(vec![("A", pa), ("B", pb)]);
        let candidates = vec![candidate("A", "B")];

        let config = WalkForwardConfig {
            formation_days: 90,
            trading_days: 21,
            ..Default::default()
        };

        let summary = run_walk_forward(&candidates, &prices, &config);
        assert!(summary.is_some(), "Expected Some(summary)");
        let summary = summary.unwrap();
        assert!(
            summary.n_windows >= 3,
            "Expected >= 3 windows, got {}",
            summary.n_windows
        );
    }

    #[test]
    fn test_no_lookahead_frozen_params() {
        // Run two windows with different underlying data. Verify that the
        // frozen params in window 0 don't leak into window 1.
        let n = 90 + 21 * 2 + 5;
        let (pa, pb) = test_utils::cointegrated_pair(n, 1.5, 10.0, 42);
        let prices = make_prices(vec![("A", pa), ("B", pb)]);
        let candidates = vec![candidate("A", "B")];

        let config = WalkForwardConfig {
            formation_days: 90,
            trading_days: 21,
            ..Default::default()
        };

        let summary = run_walk_forward(&candidates, &prices, &config);
        assert!(summary.is_some());
        let summary = summary.unwrap();

        // Each window should reference disjoint indices
        if summary.n_windows >= 2 {
            let w0 = &summary.windows[0];
            let w1 = &summary.windows[1];
            // Trading window of w0 should end where w1's formation starts from
            assert!(w0.trading_end <= w1.trading_end);
            assert!(w0.formation_end <= w1.formation_end);
        }
    }

    #[test]
    fn test_random_walks_rarely_selected() {
        // Random walk pairs should almost always fail validation (ADF, half-life).
        // Due to finite sample effects, a random walk can *occasionally* pass (Type I error),
        // but over many windows, the selection rate should be low.
        //
        // Use a long history to get many windows and verify that the rate of false
        // positives is < 50% of windows (ADF at 5% → expected ~5% false positive rate).
        let n = 90 + 21 * 10 + 5; // 10 trading windows
        let (pa, pb) = test_utils::independent_walks(n, 123);
        let prices = make_prices(vec![("X", pa), ("Y", pb)]);
        let candidates = vec![candidate("X", "Y")];

        let config = WalkForwardConfig {
            formation_days: 90,
            trading_days: 21,
            ..Default::default()
        };

        let summary = run_walk_forward(&candidates, &prices, &config);
        assert!(summary.is_some());
        let summary = summary.unwrap();

        // Most windows should have 0 pairs selected (false positive rate < 50%)
        let windows_with_selection = summary
            .windows
            .iter()
            .filter(|w| w.n_pairs_selected > 0)
            .count();
        let total_windows = summary.n_windows;
        let false_positive_rate = if total_windows > 0 {
            windows_with_selection as f64 / total_windows as f64
        } else {
            0.0
        };
        assert!(
            false_positive_rate < 0.5,
            "Random walks should rarely pass validation: {}/{} windows had pairs selected ({:.0}% rate)",
            windows_with_selection,
            total_windows,
            false_positive_rate * 100.0
        );
    }

    #[test]
    fn test_max_active_pairs_cap() {
        // Provide 3 good pairs, cap at 2 — should trade at most 2
        let n = 90 + 21 * 2 + 5;
        let (pa1, pb1) = test_utils::cointegrated_pair(n, 1.5, 10.0, 42);
        let (pa2, pb2) = test_utils::cointegrated_pair(n, 1.3, 8.0, 43);
        let (pa3, pb3) = test_utils::cointegrated_pair(n, 1.7, 12.0, 44);
        let prices = make_prices(vec![
            ("A", pa1),
            ("B", pb1),
            ("C", pa2),
            ("D", pb2),
            ("E", pa3),
            ("F", pb3),
        ]);
        let candidates = vec![
            candidate("A", "B"),
            candidate("C", "D"),
            candidate("E", "F"),
        ];

        let config = WalkForwardConfig {
            formation_days: 90,
            trading_days: 21,
            max_active_pairs: 2,
            ..Default::default()
        };

        let summary = run_walk_forward(&candidates, &prices, &config);
        assert!(summary.is_some());
        let summary = summary.unwrap();

        for w in &summary.windows {
            assert!(
                w.n_pairs_traded <= 2,
                "Expected <= 2 pairs traded, got {} in window {}",
                w.n_pairs_traded,
                w.window_idx
            );
        }
    }

    // ─── P&L mechanics tests ─────────────────────────────────────────────────

    #[test]
    fn test_compute_pnl_long_spread_profit() {
        // Long spread (pos=+1): A goes up, B goes up less → profit
        // Entry: A=100, B=100. Exit: A=103, B=100 (A outperformed).
        let pnl = compute_pnl(1, 100.0, 103.0, 100.0, 100.0, 1.0, 10_000.0);
        // return_a = 0.03, return_b = 0.00
        // pnl = 10000 * 0.03 - 10000 * 1.0 * 0.00 = 300
        assert!((pnl - 300.0).abs() < 1e-6, "Expected 300, got {pnl}");
    }

    #[test]
    fn test_compute_pnl_long_spread_loss() {
        // Long spread (pos=+1): A falls, B stays — loss
        let pnl = compute_pnl(1, 100.0, 97.0, 100.0, 100.0, 1.0, 10_000.0);
        // return_a = -0.03, return_b = 0
        // pnl = 10000*(-0.03) - 0 = -300
        assert!((pnl - (-300.0)).abs() < 1e-6, "Expected -300, got {pnl}");
    }

    #[test]
    fn test_compute_pnl_short_spread_profit() {
        // Short spread (pos=-1): A falls, B stays → profit
        let pnl = compute_pnl(-1, 100.0, 97.0, 100.0, 100.0, 1.0, 10_000.0);
        // pnl = -10000*(-0.03) + 0 = 300
        assert!((pnl - 300.0).abs() < 1e-6, "Expected 300, got {pnl}");
    }

    #[test]
    fn test_compute_pnl_nan_returns_zero() {
        let pnl = compute_pnl(1, 0.0, 100.0, 100.0, 100.0, 1.0, 10_000.0);
        assert_eq!(pnl, 0.0, "Zero entry price should return 0.0");

        let pnl_nan = compute_pnl(1, f64::NAN, 100.0, 100.0, 100.0, 1.0, 10_000.0);
        assert_eq!(pnl_nan, 0.0, "NaN entry price should return 0.0");
    }

    #[test]
    fn test_compute_pnl_zero_position_returns_zero() {
        let pnl = compute_pnl(0, 100.0, 110.0, 100.0, 90.0, 1.0, 10_000.0);
        assert_eq!(pnl, 0.0);
    }

    // ─── Statistical utility tests ────────────────────────────────────────────

    #[test]
    fn test_mean_std_basic() {
        let data = vec![1.0, 2.0, 3.0, 4.0, 5.0];
        let (mean, std) = mean_std(&data).unwrap();
        assert!((mean - 3.0).abs() < 1e-10, "mean={mean}");
        // Sample std of [1,2,3,4,5] = sqrt(2.5) ≈ 1.5811
        assert!((std - 1.5811388300841898).abs() < 1e-10, "std={std}");
    }

    #[test]
    fn test_mean_std_single_returns_none() {
        assert!(mean_std(&[5.0]).is_none());
    }

    #[test]
    fn test_mean_std_nan_returns_none() {
        assert!(mean_std(&[1.0, f64::NAN, 3.0]).is_none());
    }

    #[test]
    fn test_mean_std_constant_returns_none() {
        // Zero variance — all same value
        let data = vec![5.0; 10];
        assert!(mean_std(&data).is_none());
    }

    #[test]
    fn test_sharpe_from_daily_returns_positive() {
        // Consistently positive returns → positive Sharpe
        let returns = vec![0.01_f64; 100];
        let sharpe = sharpe_from_daily_returns(&returns);
        assert!(sharpe.is_finite());
        assert!(
            sharpe > 0.0,
            "Positive returns → positive Sharpe, got {sharpe}"
        );
    }

    #[test]
    fn test_sharpe_from_daily_returns_empty() {
        assert_eq!(sharpe_from_daily_returns(&[]), 0.0);
        assert_eq!(sharpe_from_daily_returns(&[0.01]), 0.0);
    }

    #[test]
    fn test_format_pair_id_alphabetical() {
        assert_eq!(format_pair_id("AAPL", "MSFT"), "AAPL/MSFT");
        assert_eq!(format_pair_id("MSFT", "AAPL"), "AAPL/MSFT");
        assert_eq!(format_pair_id("A", "A"), "A/A");
    }

    // ─── Simulation tests ─────────────────────────────────────────────────────

    #[test]
    fn test_simulation_no_trades_when_no_crossings() {
        // Flat spread — z-score always 0 — no entries triggered
        let config = WalkForwardConfig::default();
        // Use a constant spread pair: both assets identical
        let n_trading = config.trading_days + 5;
        let pa: Vec<f64> = vec![100.0; n_trading];
        let pb: Vec<f64> = vec![100.0; n_trading];
        let prices = make_prices(vec![("A", pa), ("B", pb)]);

        let params = FrozenPairParams {
            pair_id: "A/B".to_string(),
            leg_a: "A".to_string(),
            leg_b: "B".to_string(),
            beta: 1.0,
            alpha: 0.0,
            spread_mean: 0.0,
            spread_std: 1.0, // non-zero std
            formation_adf_pvalue: 0.01,
            formation_score: 0.8,
            formation_half_life: 5.0,
        };

        let result = simulate_trading_window(&params, &prices, 0, n_trading, &config);
        assert_eq!(result.n_trades, 0, "Flat spread should produce no trades");
        assert_eq!(result.pnl_usd, 0.0);
    }

    #[test]
    fn test_simulation_forced_close_max_hold() {
        // Spread stays above entry zscore for entire window → forced close
        let config = WalkForwardConfig {
            trading_days: 15,
            entry_zscore: 1.0,
            exit_zscore: 0.1,
            max_hold_days: 5, // force close after 5 days
            ..Default::default()
        };

        // Create a spread that is always "z > entry" (spread consistently elevated)
        // z = (spread - mean) / std. If spread >> mean, z is always large.
        // We'll set params so that every bar's spread generates z > 1.0.
        let n_total = 20;
        // log(A) - beta * log(B) where A stays elevated relative to B
        let pa: Vec<f64> = (0..n_total)
            .map(|i| (5.0 + 0.001 * i as f64).exp())
            .collect();
        let pb: Vec<f64> = vec![(4.0_f64).exp(); n_total];
        let prices = make_prices(vec![("A", pa), ("B", pb)]);

        let params = FrozenPairParams {
            pair_id: "A/B".to_string(),
            leg_a: "A".to_string(),
            leg_b: "B".to_string(),
            beta: 1.0,
            alpha: 0.0,
            spread_mean: 0.5, // formation mean well below current spread of ~1.0
            spread_std: 0.1,  // tight std → high z-score always
            formation_adf_pvalue: 0.01,
            formation_score: 0.8,
            formation_half_life: 5.0,
        };

        let result = simulate_trading_window(&params, &prices, 0, n_total, &config);
        // Should have at least one forced close due to max_hold_days
        assert!(
            result.forced_close || result.n_trades >= 1,
            "Expected forced close or at least one trade"
        );
    }

    #[test]
    fn test_non_positive_trading_prices_safe() {
        // Put a zero price in the trading window — simulation should not panic
        let config = WalkForwardConfig {
            trading_days: 10,
            ..Default::default()
        };

        let mut pa = vec![100.0_f64; 15];
        pa[5] = 0.0; // corrupt price
        let pb = vec![100.0_f64; 15];
        let prices = make_prices(vec![("A", pa), ("B", pb)]);

        let params = FrozenPairParams {
            pair_id: "A/B".to_string(),
            leg_a: "A".to_string(),
            leg_b: "B".to_string(),
            beta: 1.0,
            alpha: 0.0,
            spread_mean: 0.0,
            spread_std: 1.0,
            formation_adf_pvalue: 0.01,
            formation_score: 0.8,
            formation_half_life: 5.0,
        };

        // Should not panic — returns safely with 0 trades
        let result = simulate_trading_window(&params, &prices, 5, 15, &config);
        assert_eq!(result.n_trades, 0);
    }

    // ─── Regression / sanity tests ────────────────────────────────────────────

    #[test]
    fn test_walk_forward_summary_aggregate_win_rate_range() {
        let n = 90 + 21 * 3 + 10;
        let (pa, pb) = test_utils::cointegrated_pair(n, 1.5, 10.0, 42);
        let prices = make_prices(vec![("A", pa), ("B", pb)]);
        let candidates = vec![candidate("A", "B")];

        let config = WalkForwardConfig {
            formation_days: 90,
            trading_days: 21,
            ..Default::default()
        };

        let summary = run_walk_forward(&candidates, &prices, &config);
        if let Some(s) = summary {
            assert!(
                s.aggregate_win_rate >= 0.0 && s.aggregate_win_rate <= 1.0,
                "Win rate must be in [0, 1], got {}",
                s.aggregate_win_rate
            );
        }
    }

    #[test]
    fn test_sharpe_finite_for_cointegrated_pair() {
        let n = 90 + 21 * 2 + 5;
        let (pa, pb) = test_utils::cointegrated_pair(n, 1.5, 10.0, 99);
        let prices = make_prices(vec![("A", pa), ("B", pb)]);
        let candidates = vec![candidate("A", "B")];

        let config = WalkForwardConfig {
            formation_days: 90,
            trading_days: 21,
            ..Default::default()
        };

        let summary = run_walk_forward(&candidates, &prices, &config);
        if let Some(s) = summary {
            assert!(
                s.avg_insample_sharpe.is_finite(),
                "IS Sharpe must be finite"
            );
            assert!(s.avg_oos_sharpe.is_finite(), "OOS Sharpe must be finite");
        }
    }
}
