//! Order intent types — used for structured logging and future replay output.

use openquant_core::pairs::PairOrderIntent;
use openquant_core::signals::Side;
use serde::Serialize;

fn side_str(side: Side) -> &'static str {
    match side {
        Side::Buy => "buy",
        Side::Sell => "sell",
    }
}

/// A unified order intent record covering both single-symbol and pairs intents.
#[derive(Debug, Serialize)]
#[allow(dead_code)]
pub struct OrderIntentRecord {
    pub symbol: String,
    pub side: String,
    pub qty: f64,
    pub reason: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub pair_id: String,
    pub z_score: f64,
    #[serde(skip_serializing_if = "is_zero")]
    pub spread: f64,
    pub timestamp: i64,
}

fn is_zero(v: &f64) -> bool {
    (*v).abs() < f64::EPSILON
}

impl OrderIntentRecord {
    /// Convert a pairs engine intent to a record.
    #[allow(dead_code)]
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
}
