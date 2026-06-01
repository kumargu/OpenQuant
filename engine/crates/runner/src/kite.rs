//! Zerodha Kite execution adapter.
//!
//! This is intentionally a thin placeholder around the clean broker traits.
//! It wires credentials and future execution/data integration points without
//! forcing the rest of the runner to depend on Alpaca-shaped types.

use std::collections::HashMap;

use crate::broker::{BrokerAccount, BrokerExecutionMode, BrokerOrder};
use crate::refresh::RefreshBar;

#[derive(Clone)]
pub struct KiteClient {
    pub api_key: String,
    pub api_secret: String,
    pub access_token: Option<String>,
    http: reqwest::Client,
}

#[derive(Debug, serde::Deserialize)]
struct KiteInstrumentRow {
    instrument_token: String,
    tradingsymbol: String,
    instrument_type: String,
    segment: String,
    exchange: String,
}

impl KiteClient {
    pub fn new(api_key: String, api_secret: String, access_token: Option<String>) -> Self {
        Self {
            api_key,
            api_secret,
            access_token,
            http: reqwest::Client::new(),
        }
    }

    pub fn from_env(path: &std::path::Path) -> Result<Self, String> {
        let env = crate::alpaca::load_env(path);
        let api_key = env.get("KITE_API_KEY").cloned().unwrap_or_default();
        let api_secret = env.get("KITE_API_SECRET").cloned().unwrap_or_default();
        let access_token = env.get("KITE_ACCESS_TOKEN").cloned();
        if api_key.is_empty() || api_secret.is_empty() {
            return Err("KITE_API_KEY or KITE_API_SECRET missing from .env".into());
        }
        Ok(Self::new(api_key, api_secret, access_token))
    }

    pub fn from_values(
        api_key: Option<&str>,
        api_secret: Option<&str>,
        access_token: Option<&str>,
        env_path: Option<&std::path::Path>,
    ) -> Result<Self, String> {
        match (api_key, api_secret) {
            (Some(key), Some(secret)) if !key.is_empty() && !secret.is_empty() => Ok(Self::new(
                key.to_string(),
                secret.to_string(),
                access_token.map(ToString::to_string),
            )),
            _ => {
                let path = env_path.unwrap_or_else(|| std::path::Path::new(".env"));
                Self::from_env(path)
            }
        }
    }

    pub async fn place_order(
        &self,
        symbol: &str,
        qty: f64,
        side: &str,
        execution: BrokerExecutionMode,
    ) -> Result<BrokerOrder, String> {
        let _ = (&self.http, symbol, qty, side, execution);
        Err("Kite order placement is not implemented yet; adapter scaffold only".into())
    }

    pub async fn get_positions(
        &self,
        execution: BrokerExecutionMode,
    ) -> Result<HashMap<String, (f64, f64)>, String> {
        let _ = (&self.http, execution);
        Err("Kite positions API is not implemented yet; adapter scaffold only".into())
    }

    pub async fn get_account(
        &self,
        execution: BrokerExecutionMode,
    ) -> Result<BrokerAccount, String> {
        let _ = (&self.http, execution);
        Err("Kite account API is not implemented yet; adapter scaffold only".into())
    }

    pub async fn fetch_minute_bars_raw(
        &self,
        symbols: &[String],
        start: &str,
        end: &str,
    ) -> Result<HashMap<String, Vec<RefreshBar>>, String> {
        let token_map = self.resolve_instrument_tokens(symbols).await?;
        let start_date = chrono::NaiveDate::parse_from_str(start, "%Y-%m-%d")
            .map_err(|e| format!("bad start date: {e}"))?;
        let end_date = chrono::NaiveDate::parse_from_str(end, "%Y-%m-%d")
            .map_err(|e| format!("bad end date: {e}"))?;
        let access_token = self
            .access_token
            .as_deref()
            .ok_or("KITE access_token missing")?;

        let mut out: HashMap<String, Vec<RefreshBar>> = HashMap::new();
        for symbol in symbols {
            let token = token_map
                .get(symbol)
                .ok_or_else(|| format!("instrument token not found for {symbol}"))?;
            let mut rows = Vec::new();
            let mut chunk_start = start_date;
            while chunk_start < end_date {
                let chunk_end = (chunk_start + chrono::Duration::days(60)).min(end_date);
                let from = format!("{} 09:15:00", chunk_start.format("%Y-%m-%d"));
                let to = format!("{} 15:30:00", chunk_end.format("%Y-%m-%d"));
                let url = format!(
                    "https://api.kite.trade/instruments/historical/{token}/minute?from={from}&to={to}&oi=0"
                );
                let response = self
                    .http
                    .get(&url)
                    .header("X-Kite-Version", "3")
                    .header(
                        "Authorization",
                        format!("token {}:{}", self.api_key, access_token),
                    )
                    .send()
                    .await
                    .map_err(|e| format!("kite historical request failed for {symbol}: {e}"))?;
                if !response.status().is_success() {
                    let status = response.status();
                    let body = response.text().await.unwrap_or_default();
                    return Err(format!(
                        "kite historical error for {symbol} {status}: {body}"
                    ));
                }
                let payload: serde_json::Value = response
                    .json()
                    .await
                    .map_err(|e| format!("kite historical parse failed for {symbol}: {e}"))?;
                let candles = payload["data"]["candles"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                for candle in candles {
                    let row = candle
                        .as_array()
                        .ok_or_else(|| format!("unexpected candle shape for {symbol}"))?;
                    let ts = row
                        .first()
                        .and_then(|v| v.as_str())
                        .ok_or_else(|| format!("missing timestamp for {symbol}"))?;
                    rows.push(RefreshBar {
                        t: ts.to_string(),
                        o: row.get(1).and_then(|v| v.as_f64()).unwrap_or(0.0),
                        h: row.get(2).and_then(|v| v.as_f64()).unwrap_or(0.0),
                        l: row.get(3).and_then(|v| v.as_f64()).unwrap_or(0.0),
                        c: row.get(4).and_then(|v| v.as_f64()).unwrap_or(0.0),
                        v: row.get(5).and_then(|v| v.as_f64()).unwrap_or(0.0),
                        n: None,
                        vw: row.get(4).and_then(|v| v.as_f64()),
                    });
                }
                chunk_start = chunk_end;
            }
            out.insert(symbol.clone(), rows);
        }
        Ok(out)
    }

    async fn resolve_instrument_tokens(
        &self,
        symbols: &[String],
    ) -> Result<HashMap<String, String>, String> {
        let access_token = self
            .access_token
            .as_deref()
            .ok_or("KITE access_token missing")?;
        let response = self
            .http
            .get("https://api.kite.trade/instruments")
            .header("X-Kite-Version", "3")
            .header(
                "Authorization",
                format!("token {}:{}", self.api_key, access_token),
            )
            .send()
            .await
            .map_err(|e| format!("kite instruments request failed: {e}"))?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(format!("kite instruments error {status}: {body}"));
        }
        let csv = response
            .text()
            .await
            .map_err(|e| format!("kite instruments body read failed: {e}"))?;
        let wanted: std::collections::HashSet<&str> = symbols.iter().map(String::as_str).collect();
        let mut map = HashMap::new();
        let mut reader = csv::ReaderBuilder::new()
            .has_headers(true)
            .from_reader(csv.as_bytes());
        for row in reader.deserialize::<KiteInstrumentRow>() {
            let row = row.map_err(|e| format!("kite instruments CSV parse failed: {e}"))?;
            if row.exchange != "NSE" || row.segment != "NSE" || row.instrument_type != "EQ" {
                continue;
            }
            if wanted.contains(row.tradingsymbol.as_str()) {
                map.insert(row.tradingsymbol, row.instrument_token);
            }
        }
        Ok(map)
    }
}
