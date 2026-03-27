//! Alpaca market data client — fetches daily bars via REST API.
//!
//! Pure Rust, no Python dependency. Reads API keys from .env file.
//! Only fetches data — does not place orders (order execution is a separate concern).

use serde::Deserialize;
use std::collections::HashMap;
use tracing::{error, info, warn};

const ALPACA_DATA_URL: &str = "https://data.alpaca.markets/v2/stocks/bars";

/// A single OHLCV bar from Alpaca.
#[derive(Debug, Deserialize)]
pub struct AlpacaBar {
    /// RFC3339 timestamp
    pub t: String,
    /// Open price
    pub o: f64,
    /// High price
    pub h: f64,
    /// Low price
    pub l: f64,
    /// Close price
    pub c: f64,
    /// Volume
    pub v: f64,
}

/// Response from Alpaca bars endpoint.
#[derive(Debug, Deserialize)]
pub struct AlpacaBarsResponse {
    pub bars: HashMap<String, Vec<AlpacaBar>>,
    #[serde(default)]
    pub next_page_token: Option<String>,
}

/// Load API keys from .env file.
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

/// Fetch the latest daily bars for a list of symbols.
///
/// Returns a map of symbol → (timestamp_ms, close_price).
/// Fetches the last `lookback_days` of daily bars.
pub fn fetch_daily_bars(
    symbols: &[String],
    api_key: &str,
    api_secret: &str,
    lookback_days: usize,
) -> Result<Vec<(String, i64, f64)>, String> {
    let client = reqwest::blocking::Client::new();

    let end = chrono::Utc::now();
    let start = end - chrono::Duration::days(lookback_days as i64);

    // Alpaca allows up to ~200 symbols per request
    let mut all_bars = Vec::new();

    for chunk in symbols.chunks(50) {
        let symbols_param = chunk.join(",");

        let response = client
            .get(ALPACA_DATA_URL)
            .header("APCA-API-KEY-ID", api_key)
            .header("APCA-API-SECRET-KEY", api_secret)
            .query(&[
                ("symbols", symbols_param.as_str()),
                ("timeframe", "1Day"),
                ("start", &start.format("%Y-%m-%d").to_string()),
                ("end", &end.format("%Y-%m-%d").to_string()),
                ("limit", "10000"),
                ("feed", "iex"),
            ])
            .send()
            .map_err(|e| format!("HTTP request failed: {e}"))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().unwrap_or_default();
            return Err(format!("Alpaca API error {status}: {body}"));
        }

        let data: AlpacaBarsResponse = response
            .json()
            .map_err(|e| format!("JSON parse failed: {e}"))?;

        for (symbol, bars) in &data.bars {
            for bar in bars {
                // Parse timestamp to millis
                if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(&bar.t) {
                    all_bars.push((symbol.clone(), dt.timestamp_millis(), bar.c));
                }
            }
        }
    }

    // Sort by (timestamp, symbol) for deterministic ordering
    all_bars.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(&b.0)));

    info!(
        symbols = symbols.len(),
        bars = all_bars.len(),
        "fetched daily bars from Alpaca"
    );

    Ok(all_bars)
}

/// Fetch only the most recent daily close for each symbol.
pub fn fetch_latest_closes(
    symbols: &[String],
    api_key: &str,
    api_secret: &str,
) -> Result<Vec<(String, i64, f64)>, String> {
    let all_bars = fetch_daily_bars(symbols, api_key, api_secret, 5)?;

    // Keep only the latest bar per symbol
    let mut latest: HashMap<String, (i64, f64)> = HashMap::new();
    for (sym, ts, close) in &all_bars {
        let entry = latest.entry(sym.clone()).or_insert((*ts, *close));
        if *ts > entry.0 {
            *entry = (*ts, *close);
        }
    }

    let result: Vec<(String, i64, f64)> = latest
        .into_iter()
        .map(|(sym, (ts, close))| (sym, ts, close))
        .collect();

    info!(symbols = result.len(), "latest closes resolved");
    Ok(result)
}
