//! Broker abstraction for order placement and account queries.
//!
//! [`AlpacaClient`] is the production implementation. The replay-side
//! `SimulatedBroker` in `simulated_broker.rs` is the other.

use std::collections::HashMap;

use chrono::NaiveDate;

use crate::alpaca::{AlpacaAccount, AlpacaClient, AlpacaOrder, ExecutionMode};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionCloseFillContract {
    Immediate,
    NextSessionOpen,
}

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

    /// Seconds to wait before post-submit reconciliation. Real broker
    /// fills need time to settle; simulated replay fills are synchronous.
    fn reconciliation_delay_secs(&self) -> u64 {
        30
    }

    /// Fill contract for orders submitted by the basket runner after the
    /// session-close decision point.
    fn session_close_fill_contract(&self) -> SessionCloseFillContract {
        SessionCloseFillContract::Immediate
    }
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

    fn reconciliation_delay_secs(&self) -> u64 {
        // The basket runner submits after the close, so Alpaca can keep
        // those market/day orders queued until the next regular session.
        120
    }

    fn session_close_fill_contract(&self) -> SessionCloseFillContract {
        SessionCloseFillContract::NextSessionOpen
    }
}
