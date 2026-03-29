//! Alpaca market data + order execution client — async via reqwest + tokio.
//!
//! Pure Rust, no Python. Reads API keys from .env file.

use serde::Deserialize;
use std::collections::HashMap;
use tracing::{error, info, warn};

const DATA_URL: &str = "https://data.alpaca.markets/v2/stocks/bars";
const TRADING_URL: &str = "https://paper-api.alpaca.markets/v2";

/// Alpaca API credentials.
#[derive(Clone)]
pub struct AlpacaClient {
    pub api_key: String,
    pub api_secret: String,
    http: reqwest::Client,
}

/// A single OHLCV bar from Alpaca.
#[derive(Debug, Deserialize)]
pub struct AlpacaBar {
    pub t: String,
    pub o: f64,
    pub h: f64,
    pub l: f64,
    pub c: f64,
    pub v: f64,
}

#[derive(Debug, Deserialize)]
pub struct AlpacaBarsResponse {
    pub bars: HashMap<String, Vec<AlpacaBar>>,
    #[serde(default)]
    pub next_page_token: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct AlpacaPosition {
    pub symbol: String,
    pub qty: String,
    pub current_price: String,
    pub unrealized_pl: String,
    pub side: String,
}

#[derive(Debug, Deserialize)]
pub struct AlpacaOrder {
    pub id: String,
    pub status: String,
    pub symbol: String,
    pub side: String,
    pub qty: String,
}

impl AlpacaClient {
    pub fn new(api_key: String, api_secret: String) -> Self {
        Self {
            api_key,
            api_secret,
            http: reqwest::Client::new(),
        }
    }

    pub fn from_env(path: &std::path::Path) -> Result<Self, String> {
        let env = load_env(path);
        let api_key = env.get("ALPACA_API_KEY").cloned().unwrap_or_default();
        let api_secret = env.get("ALPACA_SECRET_KEY").cloned().unwrap_or_default();
        if api_key.is_empty() || api_secret.is_empty() {
            return Err("ALPACA_API_KEY or ALPACA_SECRET_KEY missing from .env".into());
        }
        Ok(Self::new(api_key, api_secret))
    }

    /// Fetch daily bars for a list of symbols, last N days.
    pub async fn fetch_daily_bars(
        &self,
        symbols: &[String],
        lookback_days: usize,
    ) -> Result<Vec<(String, i64, f64)>, String> {
        let end = chrono::Utc::now();
        let start = end - chrono::Duration::days(lookback_days as i64);
        let mut all_bars = Vec::new();

        for chunk in symbols.chunks(50) {
            let symbols_param = chunk.join(",");
            let response = self
                .http
                .get(DATA_URL)
                .header("APCA-API-KEY-ID", &self.api_key)
                .header("APCA-API-SECRET-KEY", &self.api_secret)
                .query(&[
                    ("symbols", symbols_param.as_str()),
                    ("timeframe", "1Day"),
                    ("start", &start.format("%Y-%m-%d").to_string()),
                    ("end", &end.format("%Y-%m-%d").to_string()),
                    ("limit", "10000"),
                    ("feed", "iex"),
                ])
                .send()
                .await
                .map_err(|e| format!("HTTP request failed: {e}"))?;

            if !response.status().is_success() {
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                return Err(format!("Alpaca data API error {status}: {body}"));
            }

            let data: AlpacaBarsResponse = response
                .json()
                .await
                .map_err(|e| format!("JSON parse failed: {e}"))?;

            for (symbol, bars) in &data.bars {
                for bar in bars {
                    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(&bar.t) {
                        all_bars.push((symbol.clone(), dt.timestamp_millis(), bar.c));
                    }
                }
            }
        }

        all_bars.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(&b.0)));
        info!(
            symbols = symbols.len(),
            bars = all_bars.len(),
            "fetched daily bars"
        );
        Ok(all_bars)
    }

    /// Place a market order. Returns order ID.
    pub async fn place_order(
        &self,
        symbol: &str,
        qty: f64,
        side: &str, // "buy" or "sell"
    ) -> Result<AlpacaOrder, String> {
        let body = serde_json::json!({
            "symbol": symbol,
            "qty": qty.to_string(),
            "side": side,
            "type": "market",
            "time_in_force": "day",
        });

        info!(symbol, qty, side, "placing order");

        let response = self
            .http
            .post(format!("{TRADING_URL}/orders"))
            .header("APCA-API-KEY-ID", &self.api_key)
            .header("APCA-API-SECRET-KEY", &self.api_secret)
            .json(&body)
            .send()
            .await
            .map_err(|e| format!("order request failed: {e}"))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            error!(symbol, side, %status, "order rejected");
            return Err(format!("order rejected {status}: {body}"));
        }

        let order: AlpacaOrder = response
            .json()
            .await
            .map_err(|e| format!("order response parse failed: {e}"))?;

        info!(
            symbol,
            side,
            id = order.id.as_str(),
            status = order.status.as_str(),
            "order placed"
        );
        Ok(order)
    }

    /// Get all open positions.
    pub async fn get_positions(&self) -> Result<Vec<AlpacaPosition>, String> {
        let response = self
            .http
            .get(format!("{TRADING_URL}/positions"))
            .header("APCA-API-KEY-ID", &self.api_key)
            .header("APCA-API-SECRET-KEY", &self.api_secret)
            .send()
            .await
            .map_err(|e| format!("positions request failed: {e}"))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(format!("positions API error {status}: {body}"));
        }

        response
            .json()
            .await
            .map_err(|e| format!("positions parse failed: {e}"))
    }

    /// Close a position by symbol.
    pub async fn close_position(&self, symbol: &str) -> Result<(), String> {
        info!(symbol, "closing position");
        let response = self
            .http
            .delete(format!("{TRADING_URL}/positions/{symbol}"))
            .header("APCA-API-KEY-ID", &self.api_key)
            .header("APCA-API-SECRET-KEY", &self.api_secret)
            .send()
            .await
            .map_err(|e| format!("close request failed: {e}"))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            warn!(symbol, %status, "close position failed");
            return Err(format!("close failed {status}: {body}"));
        }
        Ok(())
    }
}

/// Load key=value pairs from a .env file.
pub fn load_env(path: &std::path::Path) -> HashMap<String, String> {
    let mut env = HashMap::new();
    if let Ok(content) = std::fs::read_to_string(path) {
        for line in content.lines() {
            let line = line.trim();
            if line.starts_with('#') || !line.contains('=') {
                continue;
            }
            if let Some((key, value)) = line.split_once('=') {
                env.insert(key.trim().to_string(), value.trim().to_string());
            }
        }
    }
    env
}
