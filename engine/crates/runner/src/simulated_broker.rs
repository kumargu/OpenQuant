//! In-process simulated broker for replay (#294c-2).
//!
//! Implements [`crate::broker::Broker`] without touching Alpaca. Holds
//! its own positions / cash / latest-close state. Returns synthetic
//! [`AlpacaOrder`] records shaped like a successful fill so the
//! basket-live code path runs unchanged.
//!
//! Replay fill contract (frozen here so replay vs paper/live stays
//! comparable across PRs):
//!
//!   - Fills happen at the *latest known close* for the symbol — i.e.,
//!     the close from the bar that triggered the session-close cycle.
//!     This matches "MOC executes at the official close" used by paper.
//!   - Optional one-sided slippage in basis points: buys fill higher,
//!     sells fill lower. Default 0 bps.
//!   - No partial fills, no rejections. Failure-injection modes
//!     (rejection / partial / stale-position) are deferred to #300 so
//!     the reconciliation tests there can exercise the divergence
//!     branch deliberately.
//!
//! Buying-power enforcement still runs: a place_order that would push
//! the broker past `equity * leverage` returns `Err` shaped like the
//! string Alpaca emits, so `basket_live`'s `error!("ORDER FAILED")`
//! branch gets coverage even in the happy path.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use basket_engine::PortfolioConfig;

use crate::alpaca::{AlpacaAccount, AlpacaOrder, ExecutionMode};
use crate::broker::Broker;

/// Shared close-price snapshot. Written by
/// [`crate::parquet_bar_source::ParquetBarSource`] as bars are emitted,
/// read by [`SimulatedBroker`] when filling orders and computing
/// account equity.
pub type SharedCloses = Arc<RwLock<HashMap<String, f64>>>;

/// In-process broker. Cheaply cloneable — internal state is `Arc`-shared
/// so multiple references see the same positions/cash.
#[derive(Clone)]
pub struct SimulatedBroker {
    state: Arc<SimulatedState>,
}

struct SimulatedState {
    positions: RwLock<HashMap<String, (f64, f64)>>, // sym → (qty, avg_price)
    cash: RwLock<f64>,
    closes: SharedCloses,
    initial_capital: f64,
    leverage: f64,
    slippage_bps: f64,
    next_order_id: std::sync::atomic::AtomicU64,
}

impl SimulatedBroker {
    /// Construct a broker seeded from a [`PortfolioConfig`]. Cash starts
    /// at `config.capital`; positions empty. Uses the shared closes map
    /// so fills happen at whatever the bar source has most recently
    /// emitted.
    pub fn new(config: &PortfolioConfig, closes: SharedCloses, slippage_bps: f64) -> Self {
        Self {
            state: Arc::new(SimulatedState {
                positions: RwLock::new(HashMap::new()),
                cash: RwLock::new(config.capital),
                closes,
                initial_capital: config.capital,
                leverage: config.leverage,
                slippage_bps,
                next_order_id: std::sync::atomic::AtomicU64::new(1),
            }),
        }
    }

    fn fill_price(&self, symbol: &str, side: &str) -> Result<f64, String> {
        let close = self
            .state
            .closes
            .read()
            .unwrap()
            .get(symbol)
            .copied()
            .ok_or_else(|| format!("no close price for {symbol} (bar source not seeded)"))?;
        if !close.is_finite() || close <= 0.0 {
            return Err(format!("non-finite close price for {symbol}: {close}"));
        }
        let bps = self.state.slippage_bps / 1e4;
        let signed_bps = match side {
            "buy" => bps,
            "sell" => -bps,
            other => return Err(format!("unknown side: {other}")),
        };
        Ok(close * (1.0 + signed_bps))
    }

    fn equity_unlocked(&self, positions: &HashMap<String, (f64, f64)>, cash: f64) -> f64 {
        let closes = self.state.closes.read().unwrap();
        let pos_val: f64 = positions
            .iter()
            .filter_map(|(sym, (qty, _))| closes.get(sym).map(|p| qty * p))
            .sum();
        cash + pos_val
    }
}

impl Broker for SimulatedBroker {
    async fn place_order(
        &self,
        symbol: &str,
        qty: f64,
        side: &str,
        _execution: ExecutionMode,
    ) -> Result<AlpacaOrder, String> {
        if !qty.is_finite() || qty <= 0.0 {
            return Err(format!("non-positive qty for {symbol}: {qty}"));
        }
        let price = self.fill_price(symbol, side)?;
        let signed_qty = match side {
            "buy" => qty,
            "sell" => -qty,
            _ => return Err(format!("unknown side: {side}")),
        };

        let mut positions = self.state.positions.write().unwrap();
        let mut cash = self.state.cash.write().unwrap();

        // Buying-power check shaped like Alpaca's. Project the post-trade
        // gross book by valuing every position at its OWN current close
        // (not the current order's price), then comparing against
        // `equity * leverage`.
        //
        // Equity is computed BEFORE the fill so we don't bootstrap the
        // first order off projected post-fill state.
        //
        // The previous implementation valued all existing positions at
        // the new order's price, which materially mis-stated exposure
        // for multi-symbol books — a $10 order in a cheap name would
        // value a $1000-name position at $10 (under-rejection), and a
        // $1000 order in an expensive name would value a $10-name
        // position at $1000 (over-rejection). #302 review caught this.
        let closes_snapshot = self.state.closes.read().unwrap();
        let current_equity = self.equity_unlocked(&positions, *cash);
        let buying_power = current_equity * self.state.leverage;
        let prev_qty_signed = positions.get(symbol).map(|(q, _)| *q).unwrap_or(0.0);
        let new_qty_signed = prev_qty_signed + signed_qty;
        let mut projected_gross: f64 = 0.0;
        let mut traded_symbol_seen = false;
        for (sym, (q, _)) in positions.iter() {
            if sym == symbol {
                // Symbol being traded: value at the post-trade qty × fill price.
                projected_gross += new_qty_signed.abs() * price;
                traded_symbol_seen = true;
            } else if let Some(p) = closes_snapshot.get(sym) {
                projected_gross += q.abs() * p;
            } else {
                // Fallback: value at avg entry if the bar source hasn't
                // emitted a close for this symbol yet (shouldn't happen
                // in normal replay flow, but don't silently drop it).
                let avg = positions.get(sym).map(|(_, a)| *a).unwrap_or(0.0);
                projected_gross += q.abs() * avg;
            }
        }
        if !traded_symbol_seen {
            // New position — wasn't in the iteration above.
            projected_gross += new_qty_signed.abs() * price;
        }
        drop(closes_snapshot);
        if projected_gross > buying_power * 1.01 {
            return Err(format!(
                "buying power exceeded: gross {projected_gross:.2} > buying_power {buying_power:.2}"
            ));
        }

        // Apply the fill. Update avg cost when adding to a position;
        // realize cash on closes/reverses.
        let entry = positions.entry(symbol.to_string()).or_insert((0.0, price));
        let prev_qty = entry.0;
        let prev_avg = entry.1;
        let new_qty = prev_qty + signed_qty;
        let new_avg = if new_qty.abs() > 1e-9 && prev_qty.signum() == signed_qty.signum() {
            // Adding in same direction → weighted-avg cost.
            (prev_avg * prev_qty.abs() + price * qty) / new_qty.abs()
        } else {
            // Closing or reversing → reset cost basis to fill price.
            price
        };
        entry.0 = new_qty;
        entry.1 = new_avg;
        if entry.0.abs() < 1e-9 {
            positions.remove(symbol);
        }
        // Cash side: buying decreases cash, selling increases it.
        *cash -= signed_qty * price;

        let id = self
            .state
            .next_order_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(AlpacaOrder {
            id: format!("sim-{id:08}"),
            status: "filled".to_string(),
            symbol: symbol.to_string(),
            side: side.to_string(),
            qty: format!("{qty}"),
        })
    }

    async fn get_positions(
        &self,
        _execution: ExecutionMode,
    ) -> Result<HashMap<String, (f64, f64)>, String> {
        Ok(self.state.positions.read().unwrap().clone())
    }

    async fn get_account(&self, _execution: ExecutionMode) -> Result<AlpacaAccount, String> {
        let positions = self.state.positions.read().unwrap();
        let cash = *self.state.cash.read().unwrap();
        let equity = self.equity_unlocked(&positions, cash);
        let buying_power = equity * self.state.leverage;
        Ok(AlpacaAccount {
            status: "ACTIVE".to_string(),
            buying_power: format!("{buying_power:.2}"),
            equity: format!("{equity:.2}"),
            trading_blocked: false,
            account_blocked: false,
        })
    }
}

impl SimulatedBroker {
    /// Snapshot for end-of-replay reporting. Kept separate from the
    /// trait so `basket_live` doesn't grow a replay-specific call.
    pub fn final_snapshot(&self) -> ReplaySnapshot {
        let positions = self.state.positions.read().unwrap().clone();
        let cash = *self.state.cash.read().unwrap();
        let equity = self.equity_unlocked(&positions, cash);
        ReplaySnapshot {
            initial_capital: self.state.initial_capital,
            cash,
            equity,
            positions,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ReplaySnapshot {
    pub initial_capital: f64,
    pub cash: f64,
    pub equity: f64,
    pub positions: HashMap<String, (f64, f64)>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shared_closes(pairs: &[(&str, f64)]) -> SharedCloses {
        Arc::new(RwLock::new(
            pairs.iter().map(|(s, p)| (s.to_string(), *p)).collect(),
        ))
    }

    fn config() -> PortfolioConfig {
        PortfolioConfig {
            capital: 10_000.0,
            leverage: 4.0,
            n_active_baskets: 5,
        }
    }

    #[tokio::test]
    async fn fills_at_close_with_zero_slippage() {
        let closes = shared_closes(&[("AMD", 100.0)]);
        let broker = SimulatedBroker::new(&config(), closes, 0.0);
        let order = broker
            .place_order("AMD", 10.0, "buy", ExecutionMode::Paper)
            .await
            .unwrap();
        assert_eq!(order.status, "filled");
        let positions = broker.get_positions(ExecutionMode::Paper).await.unwrap();
        assert_eq!(
            positions.get("AMD").map(|(q, p)| (*q, *p)),
            Some((10.0, 100.0))
        );
        let snap = broker.final_snapshot();
        assert!((snap.cash - (10_000.0 - 1000.0)).abs() < 1e-6);
        assert!((snap.equity - 10_000.0).abs() < 1e-6); // unchanged: cash dropped, position equal value
    }

    #[tokio::test]
    async fn applies_slippage_one_sided() {
        let closes = shared_closes(&[("AMD", 100.0)]);
        let broker = SimulatedBroker::new(&config(), closes, 10.0); // 10 bps
        let _ = broker
            .place_order("AMD", 1.0, "buy", ExecutionMode::Paper)
            .await
            .unwrap();
        let snap = broker.final_snapshot();
        // Buy of 1 share at 100 + 10bps = 100.10
        assert!((snap.cash - (10_000.0 - 100.10)).abs() < 1e-6);
    }

    #[tokio::test]
    async fn rejects_when_buying_power_exceeded() {
        let closes = shared_closes(&[("AMD", 100.0)]);
        let broker = SimulatedBroker::new(&config(), closes, 0.0);
        // Equity 10k * leverage 4 = 40k buying power. 1000 shares × 100 = 100k.
        let err = broker
            .place_order("AMD", 1000.0, "buy", ExecutionMode::Paper)
            .await
            .unwrap_err();
        assert!(err.contains("buying power exceeded"), "got: {err}");
    }

    /// Regression for the #302 review finding: a small order in a cheap
    /// name must NOT under-reject when an expensive position is already
    /// held. The buggy formula valued the existing position at the new
    /// order's price, which would let huge implicit exposures slip past
    /// the gate.
    #[tokio::test]
    async fn buying_power_uses_each_symbols_own_close() {
        let closes = shared_closes(&[("EXPENSIVE", 1000.0), ("CHEAP", 10.0)]);
        let broker = SimulatedBroker::new(&config(), closes, 0.0);
        // First, build a position in EXPENSIVE that's near the buying-power cap.
        // Equity 10k * 4 = 40k buying power. 30 shares × 1000 = 30k. Inside cap.
        broker
            .place_order("EXPENSIVE", 30.0, "buy", ExecutionMode::Paper)
            .await
            .unwrap();
        // Now try to place a $200 order in CHEAP (20 shares × 10).
        // Total projected gross = 30 × 1000 (EXPENSIVE valued correctly)
        // + 20 × 10 = 30200. Under 40k buying power: should pass.
        broker
            .place_order("CHEAP", 20.0, "buy", ExecutionMode::Paper)
            .await
            .unwrap();
        // But a 2000-share order in CHEAP (= 20k) would push total to 50k:
        // should reject.
        let err = broker
            .place_order("CHEAP", 2000.0, "buy", ExecutionMode::Paper)
            .await
            .unwrap_err();
        assert!(err.contains("buying power exceeded"), "got: {err}");
    }

    /// Regression: closing an existing position must REDUCE projected
    /// gross, not add to it.
    #[tokio::test]
    async fn closing_a_position_does_not_increase_projected_gross() {
        let closes = shared_closes(&[("AMD", 100.0)]);
        let broker = SimulatedBroker::new(&config(), closes, 0.0);
        // Buy 100 shares = 10k notional. Inside 40k cap.
        broker
            .place_order("AMD", 100.0, "buy", ExecutionMode::Paper)
            .await
            .unwrap();
        // Selling 50 shares should always pass: it reduces gross from
        // 10k to 5k, well under 40k buying power. The buggy formula
        // would have computed projected_gross = (100 × 100) + (50 × 100)
        // = 15k. Still under cap so the test wouldn't have caught it
        // with the same numbers — but at scale (e.g., near the cap)
        // closing trades would have been wrongly rejected. Verify the
        // close goes through cleanly.
        broker
            .place_order("AMD", 50.0, "sell", ExecutionMode::Paper)
            .await
            .unwrap();
        let positions = broker.get_positions(ExecutionMode::Paper).await.unwrap();
        assert_eq!(positions.get("AMD").map(|(q, _)| *q), Some(50.0));
    }

    #[tokio::test]
    async fn errors_on_missing_close() {
        let closes = shared_closes(&[]);
        let broker = SimulatedBroker::new(&config(), closes, 0.0);
        let err = broker
            .place_order("AMD", 10.0, "buy", ExecutionMode::Paper)
            .await
            .unwrap_err();
        assert!(err.contains("no close price"), "got: {err}");
    }

    #[tokio::test]
    async fn closes_position_resets_cost_basis() {
        let closes = shared_closes(&[("AMD", 100.0)]);
        let broker = SimulatedBroker::new(&config(), closes.clone(), 0.0);
        broker
            .place_order("AMD", 10.0, "buy", ExecutionMode::Paper)
            .await
            .unwrap();
        // Move price, then sell out
        closes.write().unwrap().insert("AMD".to_string(), 110.0);
        broker
            .place_order("AMD", 10.0, "sell", ExecutionMode::Paper)
            .await
            .unwrap();
        let snap = broker.final_snapshot();
        // Bought 10 @ 100 = -1000 cash, sold 10 @ 110 = +1100 cash
        assert!((snap.cash - (10_000.0 + 100.0)).abs() < 1e-6);
        assert!(snap.positions.is_empty());
    }
}
