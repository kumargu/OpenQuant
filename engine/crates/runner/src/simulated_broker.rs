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
        let notional = price * qty;

        let mut positions = self.state.positions.write().unwrap();
        let mut cash = self.state.cash.write().unwrap();

        // Buying-power check shaped like Alpaca's. Computed against
        // CURRENT equity (before this fill) so we don't bootstrap the
        // first order off projected post-fill state.
        let current_equity = self.equity_unlocked(&positions, *cash);
        let buying_power = current_equity * self.state.leverage;
        let projected_gross: f64 =
            positions.iter().map(|(_, (q, _))| q.abs()).sum::<f64>() * price.max(1.0) + notional; // rough upper bound; real Alpaca check is per-order notional
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
