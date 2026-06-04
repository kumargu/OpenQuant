//! Broker abstraction for order placement and account queries.

use std::collections::HashMap;

use chrono::NaiveDate;

use crate::alpaca::AlpacaClient;
use crate::kite::KiteClient;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrokerExecutionMode {
    Paper,
    Live,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct BrokerOrder {
    pub id: String,
    pub status: String,
    pub symbol: String,
    pub side: String,
    pub qty: String,
    #[serde(default)]
    pub submitted_at: Option<String>,
    #[serde(default)]
    pub filled_at: Option<String>,
    #[serde(default)]
    pub filled_qty: Option<String>,
    #[serde(default)]
    pub filled_avg_price: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize, PartialEq)]
pub struct BrokerOpenOrder {
    pub id: String,
    pub status: String,
    pub symbol: String,
    pub side: String,
    pub qty: String,
    #[serde(default)]
    pub filled_qty: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct BrokerAccount {
    pub status: String,
    pub buying_power: String,
    pub equity: String,
    pub trading_blocked: bool,
    pub account_blocked: bool,
}

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
        execution: BrokerExecutionMode,
    ) -> Result<BrokerOrder, String>;

    /// Place a corrective order for prior session-close orders that settle at
    /// the next session open. Brokers can override this when the close-order
    /// venue differs from the regular-session venue.
    async fn place_session_open_reconcile_order(
        &self,
        symbol: &str,
        qty: f64,
        side: &str,
        execution: BrokerExecutionMode,
    ) -> Result<BrokerOrder, String> {
        self.place_order(symbol, qty, side, execution).await
    }

    /// Fetch open positions as `symbol → (qty, avg_entry_price)`.
    async fn get_positions(
        &self,
        execution: BrokerExecutionMode,
    ) -> Result<HashMap<String, (f64, f64)>, String>;

    /// Fetch currently open broker orders. Used to avoid duplicate corrective
    /// orders when broker inventory has not yet reflected an accepted order.
    async fn get_open_orders(
        &self,
        _execution: BrokerExecutionMode,
    ) -> Result<Vec<BrokerOpenOrder>, String> {
        Ok(Vec::new())
    }

    /// Fetch a broker order by id. Brokers that do not expose durable order
    /// lookup can return `Ok(None)`.
    async fn get_order(
        &self,
        _id: &str,
        _execution: BrokerExecutionMode,
    ) -> Result<Option<BrokerOrder>, String> {
        Ok(None)
    }

    /// Fetch the account snapshot (status, buying power, equity, gate flags).
    async fn get_account(&self, execution: BrokerExecutionMode) -> Result<BrokerAccount, String>;

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

    /// Earliest minute-from-open when a prior session-close order set is
    /// eligible for next-session open reconciliation.
    fn next_session_open_fill_ready_minute(&self) -> u32 {
        1
    }

    /// Whether accepted-but-unfilled close orders can survive a process
    /// restart and therefore need persisted broker-inventory catch-up state.
    fn supports_persisted_pending_open_reconcile(&self) -> bool {
        false
    }
}

impl Broker for AlpacaClient {
    async fn place_order(
        &self,
        symbol: &str,
        qty: f64,
        side: &str,
        execution: BrokerExecutionMode,
    ) -> Result<BrokerOrder, String> {
        AlpacaClient::place_order(self, symbol, qty, side, execution).await
    }

    async fn get_positions(
        &self,
        execution: BrokerExecutionMode,
    ) -> Result<HashMap<String, (f64, f64)>, String> {
        AlpacaClient::get_positions(self, execution).await
    }

    async fn get_open_orders(
        &self,
        execution: BrokerExecutionMode,
    ) -> Result<Vec<BrokerOpenOrder>, String> {
        AlpacaClient::get_open_orders(self, execution).await
    }

    async fn get_order(
        &self,
        id: &str,
        execution: BrokerExecutionMode,
    ) -> Result<Option<BrokerOrder>, String> {
        AlpacaClient::get_order(self, id, execution).await
    }

    async fn get_account(&self, execution: BrokerExecutionMode) -> Result<BrokerAccount, String> {
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

    fn next_session_open_fill_ready_minute(&self) -> u32 {
        1
    }

    fn supports_persisted_pending_open_reconcile(&self) -> bool {
        true
    }
}

impl Broker for KiteClient {
    async fn place_order(
        &self,
        symbol: &str,
        qty: f64,
        side: &str,
        execution: BrokerExecutionMode,
    ) -> Result<BrokerOrder, String> {
        KiteClient::place_order(self, symbol, qty, side, execution).await
    }

    async fn place_session_open_reconcile_order(
        &self,
        symbol: &str,
        qty: f64,
        side: &str,
        execution: BrokerExecutionMode,
    ) -> Result<BrokerOrder, String> {
        KiteClient::place_session_open_reconcile_order(self, symbol, qty, side, execution).await
    }

    async fn get_positions(
        &self,
        execution: BrokerExecutionMode,
    ) -> Result<HashMap<String, (f64, f64)>, String> {
        KiteClient::get_positions(self, execution).await
    }

    async fn get_account(&self, execution: BrokerExecutionMode) -> Result<BrokerAccount, String> {
        KiteClient::get_account(self, execution).await
    }

    fn session_close_fill_contract(&self) -> SessionCloseFillContract {
        KiteClient::session_close_fill_contract(self)
    }

    fn supports_persisted_pending_open_reconcile(&self) -> bool {
        KiteClient::supports_persisted_pending_open_reconcile(self)
    }
}
