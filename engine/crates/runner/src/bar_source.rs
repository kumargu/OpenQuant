//! Bar-source abstraction for the basket live loop.
//!
//! [`AlpacaBarSource`] drives the WebSocket `stream` module in production.
//! A `ParquetBarSource` (follow-up PR) will feed historical bars from
//! per-symbol parquet files for replay, letting the live code path run
//! without hitting the network.

use tokio::sync::mpsc;

use crate::stream::{self, StreamBar};

/// Abstraction over the bar feed.
///
/// The returned channel is the same one the existing WebSocket stream
/// emits; consumers don't need to know whether bars came from Alpaca or
/// a replay source.
pub trait BarSource: Send + Sync {
    /// Start streaming bars for `symbols`. Returns a receiver that yields
    /// bars as they arrive. The source is responsible for maintaining the
    /// upstream connection / iterator.
    async fn start(&self, symbols: &[String]) -> mpsc::Receiver<StreamBar>;
}

/// Production bar source — Alpaca WebSocket feed.
pub struct AlpacaBarSource {
    api_key: String,
    api_secret: String,
}

impl AlpacaBarSource {
    pub fn new(api_key: String, api_secret: String) -> Self {
        Self {
            api_key,
            api_secret,
        }
    }
}

impl BarSource for AlpacaBarSource {
    async fn start(&self, symbols: &[String]) -> mpsc::Receiver<StreamBar> {
        stream::start_bar_stream(&self.api_key, &self.api_secret, symbols).await
    }
}
