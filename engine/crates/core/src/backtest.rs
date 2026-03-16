//! Backtesting engine — replays historical bars through the same Engine.
//!
//! Uses identical features, strategies, and risk gates as live trading.
//! Only difference: data source (historical vs live) and fill model
//! (next-bar-open vs broker).
//!
//! A backtest is a filter, not proof. Results include full trade records,
//! equity curve, and stats (win rate, expectancy, Sharpe, drawdown).

use crate::engine::{Engine, EngineConfig, OrderIntent};
use crate::market_data::Bar;
use crate::signals::{Side, SignalReason};

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
    let sharpe_approx = if total_trades > 1 {
        let returns: Vec<f64> = trades.iter().map(|t| t.return_pct).collect();
        let mean = returns.iter().sum::<f64>() / returns.len() as f64;
        let var =
            returns.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / (returns.len() - 1) as f64;
        let std = var.sqrt();
        if std > 0.0 { mean / std } else { 0.0 }
    } else {
        0.0
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
        for _ in 0..5 {
            prices.push(100.0);
        }
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
}
