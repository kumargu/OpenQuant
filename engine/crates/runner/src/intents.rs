//! Order intent & trade result writers — JSON output for the Python sidecar.
//!
//! Order intents: engine decisions to be submitted to Alpaca by the sidecar.
//! Trade results: closed trade P&L for dashboard/reporting/Thompson feedback.

use openquant_core::engine::OrderIntent;
use openquant_core::pairs::PairOrderIntent;
use openquant_core::signals::Side;
use serde::Serialize;
use std::fs;
use std::path::Path;
use tracing::info;

fn side_str(side: Side) -> &'static str {
    match side {
        Side::Buy => "buy",
        Side::Sell => "sell",
    }
}

/// A unified order intent record covering both single-symbol and pairs intents.
#[derive(Debug, Serialize)]
pub struct OrderIntentRecord {
    pub symbol: String,
    pub side: String,
    pub qty: f64,
    pub reason: String,
    /// For pairs trades: canonical pair ID (e.g. "GLD/SLV"). Empty for single-symbol.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub pair_id: String,
    pub z_score: f64,
    /// Spread value (pairs only).
    #[serde(skip_serializing_if = "is_zero")]
    pub spread: f64,
    pub timestamp: i64,
}

fn is_zero(v: &f64) -> bool {
    (*v).abs() < f64::EPSILON
}

impl OrderIntentRecord {
    /// Convert a pairs engine intent to a record.
    pub fn from_pair_intent(intent: &PairOrderIntent, timestamp: i64) -> Self {
        Self {
            symbol: intent.symbol.clone(),
            side: side_str(intent.side).into(),
            qty: intent.qty,
            reason: intent.reason.describe().to_string(),
            pair_id: intent.pair_id.clone(),
            z_score: intent.z_score,
            spread: intent.spread,
            timestamp,
        }
    }

    /// Convert a single-symbol engine intent to a record.
    pub fn from_engine_intent(intent: &OrderIntent, timestamp: i64) -> Self {
        Self {
            symbol: intent.symbol.clone(),
            side: side_str(intent.side).into(),
            qty: intent.qty,
            reason: intent.reason.describe().to_string(),
            pair_id: String::new(),
            z_score: intent.z_score,
            spread: 0.0,
            timestamp,
        }
    }
}

/// A closed trade result for reporting and Thompson sampling feedback.
#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct TradeResultRecord {
    /// Canonical pair ID (e.g. "GLD/SLV") or symbol for single-symbol trades.
    pub id: String,
    pub entry_ts: i64,
    pub exit_ts: i64,
    pub return_bps: f64,
    pub exit_reason: String,
    pub holding_bars: usize,
}

/// Write order intents to JSON file (overwrites each run).
pub fn write_intents(intents: &[OrderIntentRecord], path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(intents).map_err(std::io::Error::other)?;
    fs::write(path, json)?;
    info!(count = intents.len(), path = %path.display(), "wrote order intents");
    Ok(())
}

/// Write trade results to JSON file (appends to existing file).
pub fn write_trade_results(results: &[TradeResultRecord], path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(results).map_err(std::io::Error::other)?;
    fs::write(path, json)?;

    info!(
        trades = results.len(),
        path = %path.display(),
        "wrote trade results"
    );
    Ok(())
}
