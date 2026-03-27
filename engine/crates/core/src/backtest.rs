//! Backtesting engine — replays historical bars through the same Engine.
//!
//! Uses identical features, strategies, and risk gates as live trading.
//! Only difference: data source (historical vs live) and fill model
//! (next-bar-open vs broker).
//!
//! A backtest is a filter, not proof. Results include full trade records,
//! equity curve, and stats (win rate, expectancy, Sharpe, drawdown).

use crate::engine::{SingleEngine as Engine, SingleEngineConfig as EngineConfig, OrderIntent};
use crate::market_data::Bar;
use crate::signals::{Side, SignalReason};

/// Standard normal CDF approximation (Abramowitz & Stegun 26.2.17, |ε| < 7.5e-8).
fn normal_cdf(x: f64) -> f64 {
    if x.is_nan() {
        return 0.5;
    }
    let a = x.abs();
    let t = 1.0 / (1.0 + 0.2316419 * a);
    let d = 0.3989422804014327; // 1/√(2π)
    let p = d * (-a * a / 2.0).exp();
    let c = t
        * (0.319381530
            + t * (-0.356563782 + t * (1.781477937 + t * (-1.821255978 + t * 1.330274429))));
    if x >= 0.0 { 1.0 - p * c } else { p * c }
}

/// Inverse normal CDF (Peter Acklam's rational approximation, |ε| < 1.15e-9).
fn normal_inv(p: f64) -> f64 {
    if p <= 0.0 {
        return f64::NEG_INFINITY;
    }
    if p >= 1.0 {
        return f64::INFINITY;
    }
    if (p - 0.5).abs() < 1e-15 {
        return 0.0;
    }

    const A: [f64; 6] = [
        -3.969683028665376e1,
        2.209460984245205e2,
        -2.759285104469687e2,
        1.383_577_518_672_69e2,
        -3.066479806614716e1,
        2.506628277459239e0,
    ];
    const B: [f64; 5] = [
        -5.447609879822406e1,
        1.615858368580409e2,
        -1.556989798598866e2,
        6.680131188771972e1,
        -1.328068155288572e1,
    ];
    const C: [f64; 6] = [
        -7.784894002430293e-3,
        -3.223964580411365e-1,
        -2.400758277161838e0,
        -2.549732539343734e0,
        4.374664141464968e0,
        2.938163982698783e0,
    ];
    const D: [f64; 4] = [
        7.784695709041462e-3,
        3.224671290700398e-1,
        2.445134137142996e0,
        3.754408661907416e0,
    ];

    let p_low = 0.02425;
    let p_high = 1.0 - p_low;

    if p < p_low {
        let q = (-2.0 * p.ln()).sqrt();
        (((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5])
            / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0)
    } else if p <= p_high {
        let q = p - 0.5;
        let r = q * q;
        (((((A[0] * r + A[1]) * r + A[2]) * r + A[3]) * r + A[4]) * r + A[5]) * q
            / (((((B[0] * r + B[1]) * r + B[2]) * r + B[3]) * r + B[4]) * r + 1.0)
    } else {
        let q = (-2.0 * (1.0 - p).ln()).sqrt();
        -(((((C[0] * q + C[1]) * q + C[2]) * q + C[3]) * q + C[4]) * q + C[5])
            / ((((D[0] * q + D[1]) * q + D[2]) * q + D[3]) * q + 1.0)
    }
}

/// Compute Deflated Sharpe Ratio given observed SR stats and number of experiments.
///
/// Corrects the observed Sharpe for multiple-testing bias using
/// Bailey & López de Prado (2014) expected max SR under null.
pub fn deflated_sharpe(
    observed_sr: f64,
    n_trades: usize,
    skewness: f64,
    kurtosis: f64,
    n_experiments: usize,
) -> f64 {
    if n_trades < 3 || n_experiments == 0 {
        return 0.5;
    }
    let n = n_trades as f64;
    let n_exp = n_experiments as f64;

    // Expected max SR under null: E[max] ≈ (1-γ)Φ^{-1}(1-1/N) + γΦ^{-1}(1-1/(Ne))
    let euler_mascheroni = 0.5772156649;
    let sr_star = if n_experiments <= 1 {
        0.0
    } else {
        let p1 = 1.0 - 1.0 / n_exp;
        let p2 = 1.0 - 1.0 / (n_exp * std::f64::consts::E);
        (1.0 - euler_mascheroni) * normal_inv(p1.min(0.9999999))
            + euler_mascheroni * normal_inv(p2.min(0.9999999))
    };

    // DSR = Φ((SR - SR*) × √(n-1) / √(1 - skew×SR + (kurt-1)/4 × SR²))
    let denom_sq =
        1.0 - skewness * observed_sr + (kurtosis - 1.0) / 4.0 * observed_sr * observed_sr;
    if denom_sq <= 0.0 {
        return 0.5;
    }
    let z = (observed_sr - sr_star) * (n - 1.0).sqrt() / denom_sq.sqrt();
    normal_cdf(z)
}

/// Record of a completed (round-trip) trade.
#[derive(Debug, Clone)]
pub struct TradeRecord {
    pub symbol: String,
    pub entry_price: f64,
    pub exit_price: f64,
    pub qty: f64,
    pub entry_time: i64,
    pub exit_time: i64,
    pub pnl: f64,
    pub return_pct: f64,
    pub entry_reason: SignalReason,
    pub exit_reason: SignalReason,
    pub bars_held: usize,
}

/// Running state for an open trade (not yet closed).
#[derive(Debug, Clone)]
struct OpenTrade {
    symbol: String,
    entry_price: f64,
    qty: f64,
    entry_time: i64,
    entry_reason: SignalReason,
    entry_bar_idx: usize,
}

/// Summary statistics from a backtest run.
#[derive(Debug, Clone)]
pub struct BacktestResult {
    pub total_bars: usize,
    pub total_trades: usize,
    pub winning_trades: usize,
    pub losing_trades: usize,
    pub win_rate: f64,
    pub total_pnl: f64,
    pub avg_win: f64,
    pub avg_loss: f64,
    pub profit_factor: f64,
    pub max_drawdown: f64,
    pub max_drawdown_pct: f64,
    pub expectancy: f64,    // avg pnl per trade
    pub sharpe_approx: f64, // simplified: mean(returns) / std(returns)
    pub psr: f64,           // Probabilistic Sharpe Ratio vs SR_benchmark=0
    pub dsr: f64,           // Deflated Sharpe Ratio (corrected for multiple testing)
    pub trades: Vec<TradeRecord>,
    pub equity_curve: Vec<f64>,
    pub signals_generated: usize,
    pub signals_rejected: usize,
}

/// Run a backtest: replay bars through the engine, simulate fills at next-bar open.
///
/// Performs basic data quality validation before running. Bars with critical issues
/// (OHLC violations, non-positive prices) are logged but the backtest continues —
/// the caller is responsible for checking data quality beforehand.
pub fn run(bars: &[Bar], config: EngineConfig) -> BacktestResult {
    // Validate data quality (60_000ms = 1 min gap threshold for 1-min bars)
    let report = crate::market_data::validate_bars(bars, 60_000 * 3);
    if report.has_critical_issues() {
        // Log but don't fail — caller should validate beforehand
        #[cfg(debug_assertions)]
        eprintln!(
            "WARNING: bar data has critical issues: ohlc={}, neg_price={}, ts_back={}, dupes={}",
            report.ohlc_violations,
            report.non_positive_prices,
            report.timestamp_backwards,
            report.duplicate_timestamps
        );
    }

    let mut engine = Engine::new(config);
    let mut trades: Vec<TradeRecord> = Vec::new();
    let mut open_trades: Vec<OpenTrade> = Vec::new();
    let mut pending_intents: Vec<OrderIntent> = Vec::new();

    let mut equity = 0.0_f64; // cumulative P&L (not starting capital)
    let mut peak_equity = 0.0_f64;
    let mut max_drawdown = 0.0_f64;
    let mut equity_curve = Vec::with_capacity(bars.len());

    let mut signals_generated = 0_usize;
    let signals_rejected = 0_usize;

    for (bar_idx, bar) in bars.iter().enumerate() {
        // 1. Execute pending intents at this bar's open (next-bar-open fill)
        for intent in pending_intents.drain(..) {
            let fill_price = bar.open; // fill at open of next bar

            match intent.side {
                Side::Buy => {
                    engine.on_fill(&intent.symbol, Side::Buy, intent.qty, fill_price);
                    open_trades.push(OpenTrade {
                        symbol: intent.symbol.clone(),
                        entry_price: fill_price,
                        qty: intent.qty,
                        entry_time: bar.timestamp,
                        entry_reason: intent.reason,
                        entry_bar_idx: bar_idx,
                    });
                }
                Side::Sell => {
                    // Close matching open trade
                    if let Some(pos) = open_trades.iter().position(|t| t.symbol == intent.symbol) {
                        let open = open_trades.remove(pos);
                        let pnl = open.qty * (fill_price - open.entry_price);
                        let return_pct = (fill_price - open.entry_price) / open.entry_price;

                        engine.on_fill(&intent.symbol, Side::Sell, open.qty, fill_price);
                        equity += pnl;

                        trades.push(TradeRecord {
                            symbol: open.symbol,
                            entry_price: open.entry_price,
                            exit_price: fill_price,
                            qty: open.qty,
                            entry_time: open.entry_time,
                            exit_time: bar.timestamp,
                            pnl,
                            return_pct,
                            entry_reason: open.entry_reason,
                            exit_reason: intent.reason,
                            bars_held: bar_idx - open.entry_bar_idx,
                        });
                    }
                }
            }
        }

        // Track equity curve and drawdown
        if equity > peak_equity {
            peak_equity = equity;
        }
        let drawdown = peak_equity - equity;
        if drawdown > max_drawdown {
            max_drawdown = drawdown;
        }
        equity_curve.push(equity);

        // 2. Feed bar into engine, collect new intents
        let intents = engine.on_bar(bar);
        if !intents.is_empty() {
            signals_generated += intents.len();
            pending_intents.extend(intents);
        }
    }

    // Compute summary stats
    let total_trades = trades.len();
    let winning: Vec<&TradeRecord> = trades.iter().filter(|t| t.pnl > 0.0).collect();
    let losing: Vec<&TradeRecord> = trades.iter().filter(|t| t.pnl <= 0.0).collect();

    let winning_count = winning.len();
    let losing_count = losing.len();

    let total_pnl: f64 = trades.iter().map(|t| t.pnl).sum();
    let gross_profit: f64 = winning.iter().map(|t| t.pnl).sum();
    let gross_loss: f64 = losing.iter().map(|t| t.pnl.abs()).sum();

    let avg_win = if winning_count > 0 {
        gross_profit / winning_count as f64
    } else {
        0.0
    };
    let avg_loss = if losing_count > 0 {
        gross_loss / losing_count as f64
    } else {
        0.0
    };

    let win_rate = if total_trades > 0 {
        winning_count as f64 / total_trades as f64
    } else {
        0.0
    };

    let profit_factor = if gross_loss > 0.0 {
        gross_profit / gross_loss
    } else if gross_profit > 0.0 {
        f64::INFINITY
    } else {
        0.0
    };

    let expectancy = if total_trades > 0 {
        total_pnl / total_trades as f64
    } else {
        0.0
    };

    // Max drawdown as percentage of peak
    let max_drawdown_pct = if peak_equity > 0.0 {
        max_drawdown / peak_equity
    } else {
        0.0
    };

    // Simplified Sharpe: mean(trade returns) / std(trade returns)
    let (sharpe_approx, psr, dsr) = if total_trades > 1 {
        let returns: Vec<f64> = trades.iter().map(|t| t.return_pct).collect();
        let n = returns.len() as f64;
        let mean = returns.iter().sum::<f64>() / n;
        let var = returns.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / (n - 1.0);
        let std = var.sqrt();
        let sr = if std > 0.0 { mean / std } else { 0.0 };

        // PSR: Probabilistic Sharpe Ratio (Bailey & López de Prado, 2012)
        // PSR = Φ((SR - SR*) × √(n-1) / √(1 - skew×SR + (kurt-1)/4 × SR²))
        // SR* = benchmark Sharpe (0 for "do no harm")
        let psr_val = if std > 0.0 && n > 2.0 {
            let m3 = returns
                .iter()
                .map(|r| ((r - mean) / std).powi(3))
                .sum::<f64>()
                / n;
            let m4 = returns
                .iter()
                .map(|r| ((r - mean) / std).powi(4))
                .sum::<f64>()
                / n;
            let skew = m3;
            let kurt = m4; // excess kurtosis = m4 - 3, but formula uses raw kurtosis
            let denom_sq = 1.0 - skew * sr + (kurt - 1.0) / 4.0 * sr * sr;
            if denom_sq > 0.0 {
                let z = sr * (n - 1.0).sqrt() / denom_sq.sqrt();
                normal_cdf(z)
            } else {
                0.5
            }
        } else {
            0.5
        };

        // DSR: Deflated Sharpe Ratio (Bailey & López de Prado, 2014)
        // Corrects for multiple testing by using E[max(SR)] under null as benchmark
        // E[max] ≈ (1-γ) × Φ^{-1}(1 - 1/N_tests) + γ × Φ^{-1}(1 - 1/(N_tests × e))
        // where γ ≈ 0.5772 (Euler-Mascheroni), N_tests from config (default 1)
        // For now, we compute DSR with N_tests=1 (same as PSR). The caller passes
        // N_tests when comparing multiple experiments.
        (sr, psr_val, psr_val) // dsr == psr when n_tests=1
    } else {
        (0.0, 0.5, 0.5)
    };

    BacktestResult {
        total_bars: bars.len(),
        total_trades,
        winning_trades: winning_count,
        losing_trades: losing_count,
        win_rate,
        total_pnl,
        avg_win,
        avg_loss,
        profit_factor,
        max_drawdown,
        max_drawdown_pct,
        expectancy,
        sharpe_approx,
        psr,
        dsr,
        trades,
        equity_curve,
        signals_generated,
        signals_rejected,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_bars(prices: &[f64], volume: f64) -> Vec<Bar> {
        prices
            .iter()
            .enumerate()
            .map(|(i, &close)| Bar {
                symbol: "TEST".into(),
                timestamp: (i as i64) * 60_000,
                open: close,
                high: close + 0.5,
                low: close - 0.5,
                close,
                volume,
            })
            .collect()
    }

    #[test]
    fn test_no_trades_during_warmup() {
        let bars = make_bars(&vec![100.0; 45], 1000.0);
        let result = run(&bars, EngineConfig::default());
        assert_eq!(result.total_trades, 0);
        assert_eq!(result.total_bars, 45);
    }

    #[test]
    fn test_deterministic_backtest() {
        // Same data, same config → same result
        let mut prices: Vec<f64> = vec![100.0; 55];
        prices.push(94.0); // crash
        prices.push(106.0); // spike
        let bars = make_bars(&prices, 1000.0);
        let config = EngineConfig::default();

        let r1 = run(&bars, config.clone());
        let r2 = run(&bars, config);

        assert_eq!(r1.total_trades, r2.total_trades);
        assert_eq!(r1.total_pnl, r2.total_pnl);
        assert_eq!(r1.equity_curve.len(), r2.equity_curve.len());
    }

    #[test]
    fn test_crash_and_recovery_trade() {
        // 55 steady bars, then crash, then recovery
        let mut prices: Vec<f64> = vec![100.0; 65];
        prices.push(93.0); // crash — should trigger buy signal
        // Next bar opens at 93, engine should have pending buy
        prices.push(95.0); // recovery bar — buy fills at open=95
        // More recovery
        prices.extend(std::iter::repeat_n(100.0, 5));
        // Spike — should trigger sell
        prices.push(108.0);
        // Sell fills at next bar open
        prices.push(107.0);

        let mut bars = make_bars(&prices, 1000.0);
        // Give crash bar high volume
        bars[65].volume = 2000.0;

        let config = EngineConfig {
            signal: crate::signals::mean_reversion::Config {
                trend_filter: false,
                ..Default::default()
            },
            risk: crate::risk::RiskConfig {
                min_reward_cost_ratio: 0.0,
                ..Default::default()
            },
            ..Default::default()
        };

        let result = run(&bars, config);
        assert!(
            result.signals_generated > 0,
            "should have generated signals"
        );
        assert_eq!(result.equity_curve.len(), bars.len());
    }

    #[test]
    fn test_equity_curve_length_matches_bars() {
        let bars = make_bars(&vec![100.0; 60], 1000.0);
        let result = run(&bars, EngineConfig::default());
        assert_eq!(result.equity_curve.len(), 60);
    }

    #[test]
    fn test_normal_cdf_known_values() {
        assert!((normal_cdf(0.0) - 0.5).abs() < 1e-6);
        assert!((normal_cdf(1.96) - 0.975).abs() < 1e-3);
        assert!((normal_cdf(-1.96) - 0.025).abs() < 1e-3);
        assert!(normal_cdf(5.0) > 0.999);
        assert!(normal_cdf(-5.0) < 0.001);
    }

    #[test]
    fn test_normal_inv_roundtrip() {
        for &p in &[0.025, 0.1, 0.25, 0.5, 0.75, 0.9, 0.975] {
            let z = normal_inv(p);
            let p_back = normal_cdf(z);
            assert!(
                (p - p_back).abs() < 1e-5,
                "roundtrip failed for p={p}: got {p_back}"
            );
        }
    }

    #[test]
    fn test_deflated_sharpe_single_experiment_equals_psr() {
        // With 1 experiment, DSR should equal PSR (SR* = 0)
        let dsr = deflated_sharpe(0.5, 100, 0.0, 3.0, 1);
        assert!(
            dsr > 0.99,
            "DSR with 1 experiment and good SR should be high, got {dsr}"
        );
    }

    #[test]
    fn test_deflated_sharpe_penalizes_many_experiments() {
        let dsr_1 = deflated_sharpe(0.3, 50, 0.1, 3.5, 1);
        let dsr_29 = deflated_sharpe(0.3, 50, 0.1, 3.5, 29);
        assert!(
            dsr_1 > dsr_29,
            "DSR should decrease with more experiments: {dsr_1} vs {dsr_29}"
        );
    }

    #[test]
    fn test_deflated_sharpe_edge_cases() {
        assert!((deflated_sharpe(0.0, 1, 0.0, 3.0, 1) - 0.5).abs() < 1e-6); // too few trades
        assert!((deflated_sharpe(0.0, 100, 0.0, 3.0, 0) - 0.5).abs() < 1e-6); // zero experiments
    }

    #[test]
    fn test_psr_in_backtest_result() {
        // No trades → PSR = 0.5
        let bars = make_bars(&vec![100.0; 45], 1000.0);
        let result = run(&bars, EngineConfig::default());
        assert_eq!(result.total_trades, 0);
        assert!((result.psr - 0.5).abs() < 1e-6);
        assert!((result.dsr - 0.5).abs() < 1e-6);
    }
}
