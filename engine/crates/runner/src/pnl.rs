//! Pair trade P&L tracker — matches entries with exits, computes return_bps.
//!
//! Single source of truth for pair trade P&L. Eliminates conflicting
//! Python scripts that produced different numbers for the same trades.

use crate::intents::TradeResultRecord;
use openquant_core::pairs::PairOrderIntent;
use openquant_core::signals::SignalReason;
use serde::Serialize;
use std::collections::HashMap;
use tracing::info;

/// Cost per leg in basis points (half-spread + commission).
/// 6 bps per leg = 24 bps round-trip (2 legs × entry + exit).
const DEFAULT_COST_BPS_PER_LEG: f64 = 6.0;

/// An open pair position being tracked.
#[derive(Debug, Clone)]
struct OpenPairTrade {
    entry_ts: i64,
    entry_price_a: f64,
    entry_price_b: f64,
    is_long_spread: bool,
    bars_held: usize,
}

/// Tracks pair trades and produces TradeResultRecords on close.
pub struct PairPnlTracker {
    open_trades: HashMap<String, OpenPairTrade>,
    closed_trades: Vec<TradeResultRecord>,
    last_prices: HashMap<String, f64>,
}

impl PairPnlTracker {
    pub fn new() -> Self {
        Self {
            open_trades: HashMap::new(),
            closed_trades: Vec::new(),
            last_prices: HashMap::new(),
        }
    }

    pub fn update_price(&mut self, symbol: &str, price: f64) {
        self.last_prices.insert(symbol.to_string(), price);
    }

    pub fn on_intents(&mut self, intents: &[PairOrderIntent], timestamp: i64) {
        if intents.is_empty() {
            return;
        }

        for chunk in intents.chunks(2) {
            if chunk.len() != 2 {
                continue;
            }

            let intent = &chunk[0];
            let pair_id = &intent.pair_id;

            match intent.reason {
                SignalReason::PairsEntry => {
                    let price_a = self
                        .last_prices
                        .get(&chunk[0].symbol)
                        .copied()
                        .unwrap_or(0.0);
                    let price_b = self
                        .last_prices
                        .get(&chunk[1].symbol)
                        .copied()
                        .unwrap_or(0.0);

                    if price_a <= 0.0 || price_b <= 0.0 {
                        continue;
                    }

                    let is_long_spread = chunk[0].side == openquant_core::signals::Side::Buy;

                    self.open_trades.insert(
                        pair_id.clone(),
                        OpenPairTrade {
                            entry_ts: timestamp,
                            entry_price_a: price_a,
                            entry_price_b: price_b,
                            is_long_spread,
                            bars_held: 0,
                        },
                    );
                }
                SignalReason::PairsExit | SignalReason::StopLoss | SignalReason::MaxHoldTime => {
                    if let Some(open) = self.open_trades.remove(pair_id) {
                        let exit_price_a = self
                            .last_prices
                            .get(&chunk[0].symbol)
                            .copied()
                            .unwrap_or(0.0);
                        let exit_price_b = self
                            .last_prices
                            .get(&chunk[1].symbol)
                            .copied()
                            .unwrap_or(0.0);

                        if exit_price_a <= 0.0 || exit_price_b <= 0.0 {
                            continue;
                        }

                        let return_a = (exit_price_a - open.entry_price_a) / open.entry_price_a;
                        let return_b = (exit_price_b - open.entry_price_b) / open.entry_price_b;

                        let gross_bps = if open.is_long_spread {
                            (return_a - return_b) * 10_000.0 / 2.0
                        } else {
                            (return_b - return_a) * 10_000.0 / 2.0
                        };

                        let cost_bps = DEFAULT_COST_BPS_PER_LEG * 4.0;
                        let net_bps = gross_bps - cost_bps;

                        let exit_reason = match intent.reason {
                            SignalReason::StopLoss => "stop_loss",
                            SignalReason::MaxHoldTime => "max_hold",
                            _ => "reversion",
                        };

                        info!(
                            pair = pair_id.as_str(),
                            gross_bps = format!("{gross_bps:.1}").as_str(),
                            net_bps = format!("{net_bps:.1}").as_str(),
                            bars = open.bars_held,
                            exit = exit_reason,
                            "Pair trade closed"
                        );

                        self.closed_trades.push(TradeResultRecord {
                            id: pair_id.clone(),
                            entry_ts: open.entry_ts,
                            exit_ts: timestamp,
                            return_bps: net_bps,
                            exit_reason: exit_reason.to_string(),
                            holding_bars: open.bars_held,
                        });
                    }
                }
                _ => {}
            }
        }
    }

    pub fn tick_bars(&mut self) {
        for trade in self.open_trades.values_mut() {
            trade.bars_held += 1;
        }
    }

    pub fn closed_trades(&self) -> &[TradeResultRecord] {
        &self.closed_trades
    }

    pub fn summary(&self) -> PnlSummary {
        let trades = &self.closed_trades;
        if trades.is_empty() {
            return PnlSummary::default();
        }

        let total_pnl: f64 = trades.iter().map(|t| t.return_bps).sum();
        let wins: Vec<f64> = trades
            .iter()
            .filter(|t| t.return_bps > 0.0)
            .map(|t| t.return_bps)
            .collect();
        let losses: Vec<f64> = trades
            .iter()
            .filter(|t| t.return_bps <= 0.0)
            .map(|t| t.return_bps)
            .collect();

        let win_rate = wins.len() as f64 / trades.len() as f64;
        let avg_win = if wins.is_empty() {
            0.0
        } else {
            wins.iter().sum::<f64>() / wins.len() as f64
        };
        let avg_loss = if losses.is_empty() {
            0.0
        } else {
            losses.iter().sum::<f64>() / losses.len() as f64
        };

        PnlSummary {
            total_trades: trades.len(),
            total_pnl_bps: total_pnl,
            win_rate,
            avg_win_bps: avg_win,
            avg_loss_bps: avg_loss,
            winning_trades: wins.len(),
            losing_trades: losses.len(),
        }
    }
}

#[derive(Debug, Default, Serialize)]
pub struct PnlSummary {
    pub total_trades: usize,
    pub total_pnl_bps: f64,
    pub win_rate: f64,
    pub avg_win_bps: f64,
    pub avg_loss_bps: f64,
    pub winning_trades: usize,
    pub losing_trades: usize,
}
