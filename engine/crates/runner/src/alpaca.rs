//! Alpaca market data + order execution client — async via reqwest + tokio.
//!
//! Pure Rust, no Python. Reads API keys from .env file.

use serde::Deserialize;
use std::collections::HashMap;
use tracing::{error, info};

const DATA_URL_DEFAULT: &str = "https://data.alpaca.markets/v2/stocks/bars";

/// Resolve the market-data endpoint. Override with `ALPACA_DATA_URL` (used
/// by offline replay against a local mock server that serves quant-data
/// parquets). Defaults to Alpaca's production URL.
fn data_url() -> String {
    std::env::var("ALPACA_DATA_URL").unwrap_or_else(|_| DATA_URL_DEFAULT.to_string())
}

/// Alpaca execution mode — controls the trading API endpoint.
/// Replay mode never calls place_order, so it doesn't need a variant here.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ExecutionMode {
    /// paper-api.alpaca.markets
    Paper,
    /// api.alpaca.markets (real money)
    Live,
}

impl ExecutionMode {
    fn trading_url(self) -> &'static str {
        match self {
            Self::Paper => "https://paper-api.alpaca.markets/v2",
            Self::Live => "https://api.alpaca.markets/v2",
        }
    }
}

/// Alpaca API credentials.
#[derive(Clone)]
pub struct AlpacaClient {
    pub api_key: String,
    pub api_secret: String,
    http: reqwest::Client,
}

/// A single OHLCV bar from Alpaca.
#[derive(Debug, Clone, serde::Serialize, Deserialize)]
#[allow(dead_code)]
pub struct AlpacaBar {
    pub t: String,
    pub o: f64,
    pub h: f64,
    pub l: f64,
    pub c: f64,
    pub v: f64,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct AlpacaBarsResponse {
    pub bars: HashMap<String, Vec<AlpacaBar>>,
    #[serde(default)]
    pub next_page_token: Option<String>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
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
    /// Fetch daily bars for explicit date range.
    /// Used by pair-picker in replay mode to avoid look-ahead bias.
    /// Returns (symbol, close_price) per bar — no timestamp adjustment (raw prices for stats).
    pub async fn fetch_daily_bars_range(
        &self,
        symbols: &[String],
        start: &str, // "2025-09-01"
        end: &str,   // "2026-01-02"
    ) -> Result<HashMap<String, Vec<f64>>, String> {
        let mut by_symbol: HashMap<String, Vec<(i64, f64)>> = HashMap::new();

        for chunk in symbols.chunks(50) {
            let symbols_param = chunk.join(",");
            let response = self
                .http
                .get(&data_url())
                .header("APCA-API-KEY-ID", &self.api_key)
                .header("APCA-API-SECRET-KEY", &self.api_secret)
                .query(&[
                    ("symbols", symbols_param.as_str()),
                    ("timeframe", "1Day"),
                    ("start", start),
                    ("end", end),
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
                        by_symbol
                            .entry(symbol.clone())
                            .or_default()
                            .push((dt.timestamp_millis(), bar.c));
                    }
                }
            }
        }

        // Sort each symbol's bars by timestamp, return just close prices
        let result: HashMap<String, Vec<f64>> = by_symbol
            .into_iter()
            .map(|(sym, mut bars)| {
                bars.sort_by_key(|(ts, _)| *ts);
                let prices: Vec<f64> = bars.into_iter().map(|(_, c)| c).collect();
                (sym, prices)
            })
            .collect();

        info!(
            symbols = result.len(),
            bars = result.values().map(|v| v.len()).sum::<usize>(),
            start,
            end,
            "fetched daily bars range for pair-picker"
        );
        Ok(result)
    }

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
                .get(&data_url())
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

            // Alpaca 1Day bars use midnight ET as timestamp (e.g., 05:00 UTC for EST).
            // Adjust to market close (16:00 ET = +16h) so the engine's
            // is_daily_close check (et_minutes >= 950) recognizes them.
            const MIDNIGHT_TO_CLOSE_MS: i64 = 16 * 3600 * 1000; // +16h
            for (symbol, bars) in &data.bars {
                for bar in bars {
                    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(&bar.t) {
                        let close_ts = dt.timestamp_millis() + MIDNIGHT_TO_CLOSE_MS;
                        all_bars.push((symbol.clone(), close_ts, bar.c));
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

    /// Fetch minute bars for a date range. Paginates automatically.
    /// Returns (symbol, timestamp_ms, close) sorted by (timestamp, symbol).
    /// Timestamp is adjusted to bar CLOSE time (Alpaca REST returns bar open time).
    pub async fn fetch_minute_bars(
        &self,
        symbols: &[String],
        start: &str, // "2026-03-01"
        end: &str,   // "2026-03-28"
    ) -> Result<Vec<(String, i64, f64)>, String> {
        // Alpaca reports bar OPEN time. Add 60s so timestamp = bar completion time.
        // Matches the same adjustment in stream.rs for WebSocket bars.
        const MINUTE_BAR_DURATION_MS: i64 = 60_000;
        let mut all_bars = Vec::new();

        for chunk in symbols.chunks(50) {
            let symbols_param = chunk.join(",");
            let mut page_token: Option<String> = None;

            loop {
                let mut query = vec![
                    ("symbols".to_string(), symbols_param.clone()),
                    ("timeframe".to_string(), "1Min".to_string()),
                    ("start".to_string(), start.to_string()),
                    ("end".to_string(), end.to_string()),
                    ("limit".to_string(), "10000".to_string()),
                    ("feed".to_string(), "iex".to_string()),
                ];
                if let Some(ref token) = page_token {
                    query.push(("page_token".to_string(), token.clone()));
                }

                let response = self
                    .http
                    .get(&data_url())
                    .header("APCA-API-KEY-ID", &self.api_key)
                    .header("APCA-API-SECRET-KEY", &self.api_secret)
                    .query(&query)
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
                            // Add timeframe duration: REST returns bar OPEN time,
                            // but live WebSocket emits after bar CLOSE. The engine
                            // expects close-time semantics.
                            let close_ts = dt.timestamp_millis() + MINUTE_BAR_DURATION_MS;
                            all_bars.push((symbol.clone(), close_ts, bar.c));
                        }
                    }
                }

                match data.next_page_token {
                    Some(token) if !token.is_empty() => {
                        page_token = Some(token);
                    }
                    _ => break,
                }
            }
        }

        // No sort — bars are in Alpaca's natural return order.
        // The caller groups by timestamp to feed one minute at a time.
        info!(
            symbols = symbols.len(),
            bars = all_bars.len(),
            start,
            end,
            "fetched minute bars"
        );
        Ok(all_bars)
    }

    /// Fetch minute bars returning raw AlpacaBar structs grouped by symbol.
    /// Used by the bar cache to store raw data before timestamp conversion.
    pub async fn fetch_minute_bars_raw(
        &self,
        symbols: &[String],
        start: &str,
        end: &str,
    ) -> Result<HashMap<String, Vec<AlpacaBar>>, String> {
        let mut all: HashMap<String, Vec<AlpacaBar>> = HashMap::new();

        for chunk in symbols.chunks(50) {
            let symbols_param = chunk.join(",");
            let mut page_token: Option<String> = None;

            loop {
                let mut query = vec![
                    ("symbols".to_string(), symbols_param.clone()),
                    ("timeframe".to_string(), "1Min".to_string()),
                    ("start".to_string(), start.to_string()),
                    ("end".to_string(), end.to_string()),
                    ("limit".to_string(), "10000".to_string()),
                    ("feed".to_string(), "iex".to_string()),
                ];
                if let Some(ref token) = page_token {
                    query.push(("page_token".to_string(), token.clone()));
                }

                let response = self
                    .http
                    .get(&data_url())
                    .header("APCA-API-KEY-ID", &self.api_key)
                    .header("APCA-API-SECRET-KEY", &self.api_secret)
                    .query(&query)
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

                for (symbol, bars) in data.bars {
                    all.entry(symbol).or_default().extend(bars);
                }

                match data.next_page_token {
                    Some(token) if !token.is_empty() => {
                        page_token = Some(token);
                    }
                    _ => break,
                }
            }
        }

        let total: usize = all.values().map(|v| v.len()).sum();
        info!(
            symbols = symbols.len(),
            bars = total,
            start,
            end,
            "fetched minute bars (raw)"
        );
        Ok(all)
    }

    /// Place a market order. URL determined by execution mode.
    pub async fn place_order(
        &self,
        symbol: &str,
        qty: f64,
        side: &str,
        execution: ExecutionMode,
    ) -> Result<AlpacaOrder, String> {
        let url = format!("{}/orders", execution.trading_url());
        let body = serde_json::json!({
            "symbol": symbol,
            "qty": qty.to_string(),
            "side": side,
            "type": "market",
            "time_in_force": "day",
        });

        info!(symbol, qty, side, ?execution, "placing order");

        let response = self
            .http
            .post(&url)
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

    /// Get all open positions from Alpaca.
    /// Returns map of symbol → (qty, avg_entry_price). Positive qty = long.
    pub async fn get_positions(
        &self,
        execution: ExecutionMode,
    ) -> Result<HashMap<String, (f64, f64)>, String> {
        let url = format!("{}/positions", execution.trading_url());
        let response = self
            .http
            .get(&url)
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

        #[derive(serde::Deserialize)]
        struct AlpacaPosition {
            symbol: String,
            qty: String,
            avg_entry_price: String,
        }

        let positions: Vec<AlpacaPosition> = response
            .json()
            .await
            .map_err(|e| format!("positions parse failed: {e}"))?;

        let map: HashMap<String, (f64, f64)> = positions
            .into_iter()
            .filter_map(|p| {
                let qty: f64 = p.qty.parse().ok()?;
                let price: f64 = p.avg_entry_price.parse().ok()?;
                Some((p.symbol, (qty, price)))
            })
            .collect();

        info!(positions = map.len(), "fetched Alpaca positions");
        Ok(map)
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
