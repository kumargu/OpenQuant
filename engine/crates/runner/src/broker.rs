//! Broker abstraction for order placement and account queries.
//!
//! [`AlpacaClient`] is the production implementation. The replay-side
//! `SimulatedBroker` in `simulated_broker.rs` is the other.

use std::collections::HashMap;

use chrono::NaiveDate;

use crate::alpaca::{AlpacaAccount, AlpacaClient, AlpacaOrder, ExecutionMode};

/// Abstraction over the brokerage backend.
///
/// Only broker actions (place orders, query positions / account) go through
/// this trait. Historical bar fetches stay on [`AlpacaClient`] directly —
/// they are data-plane calls, not execution.
pub trait Broker: Send + Sync {
    /// Place a market order. Returns the broker-side order record.
    async fn place_order(
        &self,
        symbol: &str,
        qty: f64,
        side: &str,
        execution: ExecutionMode,
    ) -> Result<AlpacaOrder, String>;

    /// Fetch open positions as `symbol → (qty, avg_entry_price)`.
    async fn get_positions(
        &self,
        execution: ExecutionMode,
    ) -> Result<HashMap<String, (f64, f64)>, String>;

    /// Fetch the account snapshot (status, buying power, equity, gate flags).
    async fn get_account(&self, execution: ExecutionMode) -> Result<AlpacaAccount, String>;

    /// Optional end-of-day equity snapshot hook. Called by `basket_live`
    /// after `process_session_close` finishes for a trading day. The
    /// production `AlpacaClient` ignores it (Alpaca already exposes
    /// historical equity); the replay `SimulatedBroker` records into
    /// its own time series so the parity TSV writer can read it back.
    async fn record_eod(&self, _date: NaiveDate) {}
}

impl Broker for AlpacaClient {
    async fn place_order(
        &self,
        symbol: &str,
        qty: f64,
        side: &str,
        execution: ExecutionMode,
    ) -> Result<AlpacaOrder, String> {
        AlpacaClient::place_order(self, symbol, qty, side, execution).await
    }

    async fn get_positions(
        &self,
        execution: ExecutionMode,
    ) -> Result<HashMap<String, (f64, f64)>, String> {
        AlpacaClient::get_positions(self, execution).await
    }

    async fn get_account(&self, execution: ExecutionMode) -> Result<AlpacaAccount, String> {
        AlpacaClient::get_account(self, execution).await
    }
}
