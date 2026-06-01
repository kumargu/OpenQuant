//! Zerodha Kite execution, history, auth, and streaming adapter.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[cfg(test)]
use chrono::TimeZone;
use chrono::{NaiveDate, Utc};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::sync::{mpsc, Mutex};
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, error, info, warn};

use crate::broker::{BrokerAccount, BrokerExecutionMode, BrokerOrder, SessionCloseFillContract};
use crate::refresh::RefreshBar;
use crate::stream::StreamBar;

const KITE_API_BASE: &str = "https://api.kite.trade";
const KITE_WS_URL: &str = "wss://ws.kite.trade";
const KITE_VERSION: &str = "3";
const DEFAULT_REDIRECT_URL: &str = "http://localhost:8080/kite/callback";
const MINUTE_MS: i64 = 60_000;
const STREAM_HEARTBEAT_SECS: u64 = 60;
const STREAM_READ_TIMEOUT_SECS: u64 = 90;
const INSTRUMENT_FAILURE_BACKOFF_SECS: u64 = 300;

#[derive(Debug, Clone)]
pub struct KiteOrderConfig {
    pub exchange: String,
    pub product: String,
    pub order_variety: String,
    pub order_type: String,
    pub validity: String,
    pub historical_open: String,
    pub historical_close: String,
    pub market_protection: Option<String>,
    pub autoslice: bool,
    pub include_holdings: bool,
    pub tag_prefix: Option<String>,
}

impl Default for KiteOrderConfig {
    fn default() -> Self {
        Self {
            exchange: "NSE".to_string(),
            product: "MIS".to_string(),
            order_variety: "regular".to_string(),
            order_type: "MARKET".to_string(),
            validity: "DAY".to_string(),
            historical_open: "09:15".to_string(),
            historical_close: "15:30".to_string(),
            market_protection: Some("-1".to_string()),
            autoslice: true,
            include_holdings: false,
            tag_prefix: Some("openquant".to_string()),
        }
    }
}

#[derive(Clone)]
pub struct KiteClient {
    pub api_key: String,
    pub api_secret: String,
    pub access_token: Option<String>,
    pub redirect_url: String,
    pub order: KiteOrderConfig,
    http: reqwest::Client,
    api_base_url: String,
    ws_url: String,
    instrument_cache: Arc<Mutex<KiteInstrumentCache>>,
}

#[derive(Default)]
struct KiteInstrumentCache {
    tokens: Option<HashMap<String, String>>,
    last_error: Option<(Instant, String)>,
}

#[derive(Debug, serde::Deserialize)]
struct KiteInstrumentRow {
    instrument_token: String,
    tradingsymbol: String,
    instrument_type: String,
    segment: String,
    exchange: String,
}

#[derive(Debug, Deserialize)]
struct KiteEnvelope<T> {
    #[allow(dead_code)]
    status: String,
    data: T,
}

#[derive(Debug, Deserialize)]
struct KiteSessionData {
    access_token: String,
}

#[derive(Debug, Deserialize)]
struct KiteMarginSegment {
    enabled: bool,
    net: f64,
    available: KiteMarginAvailable,
}

#[derive(Debug, Deserialize)]
struct KiteMarginAvailable {
    #[serde(default)]
    live_balance: f64,
    #[serde(default)]
    cash: f64,
}

#[derive(Debug, Deserialize)]
struct KitePositionsData {
    net: Vec<KitePositionRow>,
}

#[derive(Debug, Deserialize)]
struct KitePositionRow {
    tradingsymbol: String,
    exchange: String,
    quantity: f64,
    average_price: f64,
}

#[derive(Debug, Deserialize)]
struct KiteHoldingRow {
    tradingsymbol: String,
    exchange: String,
    quantity: f64,
    #[serde(default)]
    used_quantity: f64,
    average_price: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct KiteTick {
    instrument_token: u32,
    last_price: f64,
    day_volume: f64,
    exchange_timestamp_ms: i64,
}

#[derive(Debug, Clone)]
struct WorkingMinuteBar {
    minute_start_ms: i64,
    open: f64,
    high: f64,
    low: f64,
    close: f64,
    volume: f64,
}

impl WorkingMinuteBar {
    fn new(minute_start_ms: i64, price: f64, volume: f64) -> Self {
        Self {
            minute_start_ms,
            open: price,
            high: price,
            low: price,
            close: price,
            volume,
        }
    }

    fn update(&mut self, price: f64, volume_delta: f64) {
        self.high = self.high.max(price);
        self.low = self.low.min(price);
        self.close = price;
        self.volume += volume_delta;
    }

    fn into_stream_bar(self, symbol: String) -> StreamBar {
        StreamBar {
            symbol,
            timestamp: self.minute_start_ms + MINUTE_MS,
            close: self.close,
            open: self.open,
            high: self.high,
            low: self.low,
            volume: self.volume.max(0.0),
        }
    }
}

#[derive(Debug, Default)]
struct KiteMinuteAggregator {
    current: HashMap<String, WorkingMinuteBar>,
    last_day_volume: HashMap<String, f64>,
}

impl KiteMinuteAggregator {
    fn update(&mut self, symbol: &str, tick: KiteTick) -> Option<StreamBar> {
        if tick.last_price <= 0.0 || tick.exchange_timestamp_ms <= 0 {
            return None;
        }
        let minute_start_ms = tick.exchange_timestamp_ms / MINUTE_MS * MINUTE_MS;
        let prev_day_volume = self
            .last_day_volume
            .insert(symbol.to_string(), tick.day_volume)
            .unwrap_or(tick.day_volume);
        let volume_delta = (tick.day_volume - prev_day_volume).max(0.0);

        match self.current.get_mut(symbol) {
            Some(bar) if minute_start_ms == bar.minute_start_ms => {
                bar.update(tick.last_price, volume_delta);
                None
            }
            Some(bar) if minute_start_ms > bar.minute_start_ms => {
                let completed = bar.clone().into_stream_bar(symbol.to_string());
                *bar = WorkingMinuteBar::new(minute_start_ms, tick.last_price, volume_delta);
                Some(completed)
            }
            Some(_) => None,
            None => {
                self.current.insert(
                    symbol.to_string(),
                    WorkingMinuteBar::new(minute_start_ms, tick.last_price, volume_delta),
                );
                None
            }
        }
    }

    fn flush_due(&mut self, now_ms: i64) -> Vec<StreamBar> {
        let due: Vec<String> = self
            .current
            .iter()
            .filter(|(_, bar)| bar.minute_start_ms + MINUTE_MS <= now_ms)
            .map(|(symbol, _)| symbol.clone())
            .collect();
        let mut out = Vec::with_capacity(due.len());
        for symbol in due {
            if let Some(bar) = self.current.remove(&symbol) {
                out.push(bar.into_stream_bar(symbol));
            }
        }
        out
    }
}

impl KiteClient {
    pub fn new(api_key: String, api_secret: String, access_token: Option<String>) -> Self {
        Self {
            api_key,
            api_secret,
            access_token,
            redirect_url: DEFAULT_REDIRECT_URL.to_string(),
            order: KiteOrderConfig::default(),
            http: reqwest::Client::new(),
            api_base_url: KITE_API_BASE.to_string(),
            ws_url: KITE_WS_URL.to_string(),
            instrument_cache: Arc::new(Mutex::new(KiteInstrumentCache::default())),
        }
    }

    #[allow(dead_code)]
    pub fn from_env(path: &Path) -> Result<Self, String> {
        let env = crate::alpaca::load_env(path);
        let api_key = env_value(&env, "KITE_API_KEY").unwrap_or_default();
        let api_secret = env_value(&env, "KITE_API_SECRET").unwrap_or_default();
        if api_key.is_empty() || api_secret.is_empty() {
            return Err("KITE_API_KEY or KITE_API_SECRET missing from .env".into());
        }
        let mut client = Self::new(api_key, api_secret, env_value(&env, "KITE_ACCESS_TOKEN"));
        client.redirect_url =
            env_value(&env, "KITE_REDIRECT_URL").unwrap_or_else(|| DEFAULT_REDIRECT_URL.into());
        client.order = KiteOrderConfig::from_env(&env);
        Ok(client)
    }

    pub fn from_values(
        api_key: Option<&str>,
        api_secret: Option<&str>,
        access_token: Option<&str>,
        redirect_url: Option<&str>,
        order: KiteOrderConfig,
        env_path: Option<&Path>,
    ) -> Result<Self, String> {
        let env = env_path.map(crate::alpaca::load_env).unwrap_or_default();
        let api_key = non_empty(api_key)
            .map(ToString::to_string)
            .or_else(|| env_value(&env, "KITE_API_KEY"))
            .unwrap_or_default();
        let api_secret = non_empty(api_secret)
            .map(ToString::to_string)
            .or_else(|| env_value(&env, "KITE_API_SECRET"))
            .unwrap_or_default();
        if api_key.is_empty() || api_secret.is_empty() {
            return Err("KITE_API_KEY or KITE_API_SECRET missing from runner TOML/env".into());
        }
        let access_token = non_empty(access_token)
            .map(ToString::to_string)
            .or_else(|| env_value(&env, "KITE_ACCESS_TOKEN"));
        let redirect_url = non_empty(redirect_url)
            .map(ToString::to_string)
            .or_else(|| env_value(&env, "KITE_REDIRECT_URL"))
            .unwrap_or_else(|| DEFAULT_REDIRECT_URL.into());

        let mut merged_order = KiteOrderConfig::from_env(&env);
        merged_order.merge_explicit(order);

        let mut client = Self::new(api_key, api_secret, access_token);
        client.redirect_url = redirect_url;
        client.order = merged_order;
        Ok(client)
    }

    pub fn login_url(&self) -> String {
        format!(
            "https://kite.zerodha.com/connect/login?v=3&api_key={}",
            self.api_key
        )
    }

    pub async fn exchange_request_token(&mut self, request_token: &str) -> Result<String, String> {
        let checksum = kite_checksum(&self.api_key, request_token, &self.api_secret);
        let url = format!("{}/session/token", self.api_base_url);
        let response = self
            .http
            .post(&url)
            .header("X-Kite-Version", KITE_VERSION)
            .form(&[
                ("api_key", self.api_key.as_str()),
                ("request_token", request_token),
                ("checksum", checksum.as_str()),
            ])
            .send()
            .await
            .map_err(|e| format!("kite token exchange request failed: {e}"))?;
        let payload: KiteEnvelope<KiteSessionData> =
            parse_kite_response(response, "kite token exchange").await?;
        self.access_token = Some(payload.data.access_token.clone());
        Ok(payload.data.access_token)
    }

    pub fn persist_access_token(&self, env_path: &Path, access_token: &str) -> Result<(), String> {
        persist_env_value(env_path, "KITE_ACCESS_TOKEN", access_token)
    }

    pub async fn place_order(
        &self,
        symbol: &str,
        qty: f64,
        side: &str,
        execution: BrokerExecutionMode,
    ) -> Result<BrokerOrder, String> {
        let order_variety = self.order.order_variety.clone();
        self.place_market_order_with_variety(symbol, qty, side, execution, &order_variety)
            .await
    }

    pub async fn place_session_open_reconcile_order(
        &self,
        symbol: &str,
        qty: f64,
        side: &str,
        execution: BrokerExecutionMode,
    ) -> Result<BrokerOrder, String> {
        self.place_market_order_with_variety(
            symbol,
            qty,
            side,
            execution,
            Self::session_open_reconcile_order_variety(),
        )
        .await
    }

    async fn place_market_order_with_variety(
        &self,
        symbol: &str,
        qty: f64,
        side: &str,
        execution: BrokerExecutionMode,
        order_variety: &str,
    ) -> Result<BrokerOrder, String> {
        if execution != BrokerExecutionMode::Live {
            return Err("Kite has no paper endpoint; use live or basket noop mode".into());
        }
        if !qty.is_finite() || qty <= 0.0 {
            return Err(format!("non-positive qty for {symbol}: {qty}"));
        }
        let quantity = qty.round();
        if (quantity - qty).abs() > 1e-6 {
            return Err(format!("Kite equity quantity must be whole shares: {qty}"));
        }
        let quantity = quantity as u32;
        if quantity == 0 {
            return Err(format!("zero rounded qty for {symbol}: {qty}"));
        }
        let transaction_type = match side {
            "buy" => "BUY",
            "sell" => "SELL",
            other => return Err(format!("unknown side for Kite order: {other}")),
        };
        let form = self.build_order_form(symbol, quantity, transaction_type, None, None);
        self.submit_order(symbol, side, quantity, order_variety, form)
            .await
    }

    #[allow(dead_code)]
    pub async fn place_stop_market_order(
        &self,
        symbol: &str,
        qty: f64,
        side: &str,
        trigger_price: f64,
        execution: BrokerExecutionMode,
    ) -> Result<BrokerOrder, String> {
        if !trigger_price.is_finite() || trigger_price <= 0.0 {
            return Err(format!(
                "Kite stop-market trigger must be positive for {symbol}: {trigger_price}"
            ));
        }
        if execution != BrokerExecutionMode::Live {
            return Err("Kite has no paper endpoint; use live or basket noop mode".into());
        }
        if !qty.is_finite() || qty <= 0.0 {
            return Err(format!("non-positive qty for {symbol}: {qty}"));
        }
        let quantity = qty.round();
        if (quantity - qty).abs() > 1e-6 {
            return Err(format!("Kite equity quantity must be whole shares: {qty}"));
        }
        let quantity = quantity as u32;
        if quantity == 0 {
            return Err(format!("zero rounded qty for {symbol}: {qty}"));
        }
        let transaction_type = match side {
            "buy" => "BUY",
            "sell" => "SELL",
            other => return Err(format!("unknown side for Kite order: {other}")),
        };
        let form = self.build_order_form(
            symbol,
            quantity,
            transaction_type,
            Some("SL-M"),
            Some(trigger_price),
        );
        let order_variety = self.order.order_variety.clone();
        self.submit_order(symbol, side, quantity, &order_variety, form)
            .await
    }

    async fn submit_order(
        &self,
        symbol: &str,
        side: &str,
        quantity: u32,
        order_variety: &str,
        form: Vec<(&'static str, String)>,
    ) -> Result<BrokerOrder, String> {
        let url = format!("{}/orders/{order_variety}", self.api_base_url);
        info!(
            symbol,
            quantity,
            side,
            variety = order_variety,
            product = self.order.product.as_str(),
            "placing Kite order"
        );
        let response = self
            .http
            .post(&url)
            .header("X-Kite-Version", KITE_VERSION)
            .header("Authorization", self.auth_header()?)
            .form(&form)
            .send()
            .await
            .map_err(|e| format!("kite order request failed for {symbol}: {e}"))?;
        let payload: serde_json::Value = parse_kite_json(response, "kite order").await?;
        let order_ids = order_ids_from_payload(&payload)?;
        Ok(BrokerOrder {
            id: order_ids.join(","),
            status: "accepted".to_string(),
            symbol: symbol.to_string(),
            side: side.to_string(),
            qty: quantity.to_string(),
        })
    }

    fn build_order_form(
        &self,
        symbol: &str,
        quantity: u32,
        transaction_type: &str,
        order_type_override: Option<&str>,
        trigger_price: Option<f64>,
    ) -> Vec<(&'static str, String)> {
        let mut form = vec![
            ("tradingsymbol", symbol.to_string()),
            ("exchange", self.order.exchange.clone()),
            ("transaction_type", transaction_type.to_string()),
            (
                "order_type",
                order_type_override
                    .unwrap_or(&self.order.order_type)
                    .to_string(),
            ),
            ("quantity", quantity.to_string()),
            ("product", self.order.product.clone()),
            ("validity", self.order.validity.clone()),
        ];
        if self.order.autoslice {
            form.push(("autoslice", "true".to_string()));
        }
        if let Some(trigger_price) = trigger_price {
            form.push(("trigger_price", format!("{trigger_price:.2}")));
        }
        if let Some(market_protection) = self.order.market_protection.as_ref() {
            form.push(("market_protection", market_protection.clone()));
        }
        if let Some(tag) = self.order_tag() {
            form.push(("tag", tag));
        }
        form
    }

    pub async fn get_positions(
        &self,
        execution: BrokerExecutionMode,
    ) -> Result<HashMap<String, (f64, f64)>, String> {
        if execution != BrokerExecutionMode::Live {
            return Err("Kite has no paper endpoint; use live or basket noop mode".into());
        }
        let mut out = HashMap::new();
        let url = format!("{}/portfolio/positions", self.api_base_url);
        let response = self
            .http
            .get(&url)
            .header("X-Kite-Version", KITE_VERSION)
            .header("Authorization", self.auth_header()?)
            .send()
            .await
            .map_err(|e| format!("kite positions request failed: {e}"))?;
        let payload: KiteEnvelope<KitePositionsData> =
            parse_kite_response(response, "kite positions").await?;
        for row in payload.data.net {
            if row.exchange == self.order.exchange && row.quantity.abs() > 0.0 {
                merge_position(&mut out, row.tradingsymbol, row.quantity, row.average_price);
            }
        }

        if self.order.include_holdings {
            let holdings = self.get_holdings().await?;
            for row in holdings {
                if row.exchange == self.order.exchange {
                    let qty = (row.quantity - row.used_quantity).max(0.0);
                    if qty > 0.0 {
                        merge_position(&mut out, row.tradingsymbol, qty, row.average_price);
                    }
                }
            }
        }
        Ok(out)
    }

    pub async fn get_account(
        &self,
        execution: BrokerExecutionMode,
    ) -> Result<BrokerAccount, String> {
        if execution != BrokerExecutionMode::Live {
            return Err("Kite has no paper endpoint; use live or basket noop mode".into());
        }
        let url = format!("{}/user/margins/equity", self.api_base_url);
        let response = self
            .http
            .get(&url)
            .header("X-Kite-Version", KITE_VERSION)
            .header("Authorization", self.auth_header()?)
            .send()
            .await
            .map_err(|e| format!("kite margins request failed: {e}"))?;
        let payload: KiteEnvelope<KiteMarginSegment> =
            parse_kite_response(response, "kite equity margins").await?;
        let available = if payload.data.available.live_balance > 0.0 {
            payload.data.available.live_balance
        } else {
            payload.data.available.cash.max(payload.data.net)
        };
        Ok(BrokerAccount {
            status: if payload.data.enabled {
                "ACTIVE".to_string()
            } else {
                "BLOCKED".to_string()
            },
            buying_power: format!("{available:.2}"),
            equity: format!("{:.2}", payload.data.net),
            trading_blocked: !payload.data.enabled,
            account_blocked: !payload.data.enabled,
        })
    }

    pub async fn fetch_minute_bars_raw(
        &self,
        symbols: &[String],
        start: &str,
        end: &str,
    ) -> Result<HashMap<String, Vec<RefreshBar>>, String> {
        let token_map = self.resolve_instrument_tokens(symbols).await?;
        let start_date = NaiveDate::parse_from_str(start, "%Y-%m-%d")
            .map_err(|e| format!("bad start date: {e}"))?;
        let end_date =
            NaiveDate::parse_from_str(end, "%Y-%m-%d").map_err(|e| format!("bad end date: {e}"))?;

        let mut out: HashMap<String, Vec<RefreshBar>> = HashMap::new();
        for symbol in symbols {
            let token = token_map
                .get(symbol)
                .ok_or_else(|| format!("instrument token not found for {symbol}"))?;
            let mut rows_by_ts: BTreeMap<String, RefreshBar> = BTreeMap::new();
            for (chunk_start, chunk_end_exclusive) in historical_chunks(start_date, end_date) {
                let (from, to) = historical_request_window(
                    chunk_start,
                    chunk_end_exclusive,
                    &self.order.historical_open,
                    &self.order.historical_close,
                );
                let url = format!(
                    "{}/instruments/historical/{token}/minute?from={from}&to={to}&oi=0",
                    self.api_base_url
                );
                let response = self
                    .http
                    .get(&url)
                    .header("X-Kite-Version", KITE_VERSION)
                    .header("Authorization", self.auth_header()?)
                    .send()
                    .await
                    .map_err(|e| format!("kite historical request failed for {symbol}: {e}"))?;
                let payload: serde_json::Value =
                    parse_kite_json(response, "kite historical").await?;
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
                    rows_by_ts.insert(
                        ts.to_string(),
                        RefreshBar {
                            t: ts.to_string(),
                            o: row.get(1).and_then(|v| v.as_f64()).unwrap_or(0.0),
                            h: row.get(2).and_then(|v| v.as_f64()).unwrap_or(0.0),
                            l: row.get(3).and_then(|v| v.as_f64()).unwrap_or(0.0),
                            c: row.get(4).and_then(|v| v.as_f64()).unwrap_or(0.0),
                            v: row.get(5).and_then(|v| v.as_f64()).unwrap_or(0.0),
                            n: None,
                            vw: row.get(4).and_then(|v| v.as_f64()),
                        },
                    );
                }
            }
            out.insert(symbol.clone(), rows_by_ts.into_values().collect());
        }
        Ok(out)
    }

    pub async fn start_bar_stream(&self, symbols: &[String]) -> mpsc::Receiver<StreamBar> {
        let (tx, rx) = mpsc::channel(1000);
        let client = self.clone();
        let symbols = symbols.to_vec();
        tokio::spawn(async move {
            let mut reconnect_count: u64 = 0;
            let (token_to_symbol, tokens) = loop {
                match client.resolve_stream_tokens(&symbols).await {
                    Ok(v) => break v,
                    Err(e) => {
                        metrics::counter!("kite.stream.token_resolve", "result" => "error")
                            .increment(1);
                        error!(
                            "Kite stream token resolution failed: {e}; retrying in {}s",
                            INSTRUMENT_FAILURE_BACKOFF_SECS
                        );
                        tokio::time::sleep(tokio::time::Duration::from_secs(
                            INSTRUMENT_FAILURE_BACKOFF_SECS,
                        ))
                        .await;
                    }
                }
            };
            loop {
                info!(reconnect_count, "connecting to Kite stream");
                match client
                    .run_stream_once_with_tokens(token_to_symbol.clone(), tokens.clone(), &tx)
                    .await
                {
                    Ok(()) => {
                        metrics::counter!("kite.stream.reconnect", "reason" => "closed")
                            .increment(1);
                        warn!("Kite stream closed; reconnecting after backoff");
                    }
                    Err(e) => {
                        metrics::counter!("kite.stream.reconnect", "reason" => "error")
                            .increment(1);
                        error!("Kite stream error: {e}; reconnecting after backoff");
                    }
                }
                reconnect_count += 1;
                tokio::time::sleep(tokio::time::Duration::from_secs(
                    kite_stream_reconnect_delay_secs(reconnect_count),
                ))
                .await;
            }
        });
        rx
    }

    pub fn session_close_fill_contract(&self) -> SessionCloseFillContract {
        if self.order.order_variety.eq_ignore_ascii_case("amo") {
            SessionCloseFillContract::NextSessionOpen
        } else {
            SessionCloseFillContract::Immediate
        }
    }

    pub fn supports_persisted_pending_open_reconcile(&self) -> bool {
        self.session_close_fill_contract() == SessionCloseFillContract::NextSessionOpen
    }

    fn session_open_reconcile_order_variety() -> &'static str {
        "regular"
    }

    async fn get_holdings(&self) -> Result<Vec<KiteHoldingRow>, String> {
        let url = format!("{}/portfolio/holdings", self.api_base_url);
        let response = self
            .http
            .get(&url)
            .header("X-Kite-Version", KITE_VERSION)
            .header("Authorization", self.auth_header()?)
            .send()
            .await
            .map_err(|e| format!("kite holdings request failed: {e}"))?;
        let payload: KiteEnvelope<Vec<KiteHoldingRow>> =
            parse_kite_response(response, "kite holdings").await?;
        Ok(payload.data)
    }

    pub(crate) async fn resolve_instrument_tokens(
        &self,
        symbols: &[String],
    ) -> Result<HashMap<String, String>, String> {
        let mut cache = self.instrument_cache.lock().await;
        if let Some(tokens) = cache.tokens.as_ref() {
            return filter_instrument_tokens(symbols, tokens);
        }
        if let Some(tokens) = load_cached_instrument_tokens(&self.order.exchange) {
            match filter_instrument_tokens(symbols, &tokens) {
                Ok(filtered) => {
                    info!(
                        exchange = self.order.exchange.as_str(),
                        symbols = filtered.len(),
                        "loaded Kite instrument tokens from disk cache"
                    );
                    cache.tokens = Some(tokens);
                    cache.last_error = None;
                    return Ok(filtered);
                }
                Err(e) => {
                    warn!(
                        error = e.as_str(),
                        exchange = self.order.exchange.as_str(),
                        "Kite instrument disk cache incomplete; refetching instruments"
                    );
                }
            }
        }
        if let Some((at, _error)) = cache.last_error.as_ref() {
            let elapsed = at.elapsed();
            if elapsed < Duration::from_secs(INSTRUMENT_FAILURE_BACKOFF_SECS) {
                return Err(format!(
                    "Kite instruments temporarily unavailable after recent failure; suppressing refetch for another {}s",
                    INSTRUMENT_FAILURE_BACKOFF_SECS.saturating_sub(elapsed.as_secs())
                ));
            }
        }
        let response = match self
            .http
            .get(format!("{}/instruments", self.api_base_url))
            .header("X-Kite-Version", KITE_VERSION)
            .header("Authorization", self.auth_header()?)
            .send()
            .await
        {
            Ok(response) => response,
            Err(e) => {
                let error = format!("kite instruments request failed: {e}");
                cache.last_error = Some((Instant::now(), error.clone()));
                return Err(error);
            }
        };
        let csv = match parse_kite_text(response, "kite instruments").await {
            Ok(csv) => csv,
            Err(e) => {
                cache.last_error = Some((Instant::now(), e.clone()));
                return Err(e);
            }
        };
        let tokens = match resolve_tokens_from_csv(&csv, &self.order.exchange) {
            Ok(tokens) => tokens,
            Err(e) => {
                cache.last_error = Some((Instant::now(), e.clone()));
                return Err(e);
            }
        };
        persist_instrument_cache(&self.order.exchange, &csv);
        cache.tokens = Some(tokens);
        cache.last_error = None;
        filter_instrument_tokens(
            symbols,
            cache.tokens.as_ref().expect("instrument cache set"),
        )
    }

    async fn resolve_stream_tokens(
        &self,
        symbols: &[String],
    ) -> Result<(HashMap<u32, String>, Vec<u32>), String> {
        let token_map = self.resolve_instrument_tokens(symbols).await?;
        stream_tokens_from_map(symbols, &token_map)
    }

    async fn run_stream_once_with_tokens(
        &self,
        token_to_symbol: HashMap<u32, String>,
        tokens: Vec<u32>,
        tx: &mpsc::Sender<StreamBar>,
    ) -> Result<(), String> {
        let url = self.websocket_url()?;
        let (ws_stream, _) = tokio_tungstenite::connect_async(url)
            .await
            .map_err(|e| format!("Kite WebSocket connect failed: {e}"))?;
        let (mut write, mut read) = ws_stream.split();
        write
            .send(Message::Text(
                serde_json::json!({"a":"subscribe","v":tokens}).to_string(),
            ))
            .await
            .map_err(|e| format!("Kite subscribe send failed: {e}"))?;
        write
            .send(Message::Text(
                serde_json::json!({"a":"mode","v":["full",tokens]}).to_string(),
            ))
            .await
            .map_err(|e| format!("Kite mode send failed: {e}"))?;
        info!(
            symbols = token_to_symbol.len(),
            "subscribed to Kite full quote stream"
        );

        let mut aggregator = KiteMinuteAggregator::default();
        let mut bars_total: u64 = 0;
        let mut ticks_total: u64 = 0;
        let mut heartbeat =
            tokio::time::interval(tokio::time::Duration::from_secs(STREAM_HEARTBEAT_SECS));
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        heartbeat.tick().await;
        let mut flush = tokio::time::interval(tokio::time::Duration::from_secs(1));
        flush.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let read_timeout = tokio::time::Duration::from_secs(STREAM_READ_TIMEOUT_SECS);
        let idle_deadline = tokio::time::sleep(read_timeout);
        tokio::pin!(idle_deadline);
        let mut last_message_at = Instant::now();

        loop {
            tokio::select! {
                biased;
                msg = read.next() => {
                    let msg = match msg {
                        Some(m) => m.map_err(|e| format!("Kite stream read error: {e}"))?,
                        None => return Ok(()),
                    };
                    last_message_at = Instant::now();
                    idle_deadline
                        .as_mut()
                        .reset(tokio::time::Instant::now() + read_timeout);
                    match msg {
                        Message::Binary(bytes) => {
                            let ticks = match parse_kite_binary_frame(&bytes) {
                                Ok(ticks) => ticks,
                                Err(e) => {
                                    metrics::counter!("bug", "component" => "kite_stream", "kind" => "bad_binary_frame").increment(1);
                                    warn!(error = %e, bytes = bytes.len(), "dropping malformed Kite binary frame");
                                    continue;
                                }
                            };
                            for tick in ticks {
                                ticks_total += 1;
                                let Some(symbol) = token_to_symbol.get(&tick.instrument_token) else {
                                    continue;
                                };
                                if let Some(bar) = aggregator.update(symbol, tick) {
                                    bars_total += 1;
                                    tx.send(bar).await.map_err(|_| "receiver dropped".to_string())?;
                                }
                            }
                        }
                        Message::Text(text) => debug!(payload = text.as_str(), "Kite stream text message"),
                        Message::Ping(data) => {
                            let _ = write.send(Message::Pong(data)).await;
                        }
                        Message::Close(_) => {
                            metrics::counter!("kite.stream.disconnect", "reason" => "broker_close").increment(1);
                            warn!("Kite stream sent close frame");
                            return Ok(());
                        }
                        _ => {}
                    }
                }
                _ = flush.tick() => {
                    for bar in aggregator.flush_due(Utc::now().timestamp_millis()) {
                        bars_total += 1;
                        tx.send(bar).await.map_err(|_| "receiver dropped".to_string())?;
                    }
                }
                _ = heartbeat.tick() => {
                    info!(
                        ticks_total,
                        bars_total,
                        active_minutes = aggregator.current.len(),
                        seconds_silent = last_message_at.elapsed().as_secs(),
                        "Kite stream heartbeat"
                    );
                }
                _ = &mut idle_deadline => {
                    metrics::counter!("kite.stream.disconnect", "reason" => "read_timeout").increment(1);
                    return Err(format!("Kite stream read timeout ({}s)", last_message_at.elapsed().as_secs()));
                }
            }
        }
    }

    fn auth_header(&self) -> Result<String, String> {
        Ok(format!("token {}:{}", self.api_key, self.access_token()?))
    }

    fn access_token(&self) -> Result<&str, String> {
        self.access_token
            .as_deref()
            .filter(|token| !token.trim().is_empty())
            .ok_or_else(|| "KITE_ACCESS_TOKEN missing; run kite-login or update .env.india".into())
    }

    fn websocket_url(&self) -> Result<String, String> {
        let url = reqwest::Url::parse_with_params(
            &self.ws_url,
            [
                ("api_key", self.api_key.as_str()),
                ("access_token", self.access_token()?),
            ],
        )
        .map_err(|e| format!("Kite WebSocket URL build failed: {e}"))?;
        Ok(url.to_string())
    }

    fn order_tag(&self) -> Option<String> {
        self.order.tag_prefix.as_ref().map(|prefix| {
            let clean: String = prefix
                .chars()
                .filter(|c| c.is_ascii_alphanumeric())
                .take(12)
                .collect();
            format!("{clean}{:08}", Utc::now().timestamp() % 100_000_000)
                .chars()
                .take(20)
                .collect()
        })
    }
}

impl KiteOrderConfig {
    fn from_env(env: &HashMap<String, String>) -> Self {
        let mut cfg = Self::default();
        if let Some(value) = env_value(env, "KITE_EXCHANGE") {
            cfg.exchange = value;
        }
        if let Some(value) = env_value(env, "KITE_PRODUCT") {
            cfg.product = value;
        }
        if let Some(value) = env_value(env, "KITE_ORDER_VARIETY") {
            cfg.order_variety = value;
        }
        if let Some(value) = env_value(env, "KITE_ORDER_TYPE") {
            cfg.order_type = value;
        }
        if let Some(value) = env_value(env, "KITE_VALIDITY") {
            cfg.validity = value;
        }
        if let Some(value) = env_value(env, "KITE_HISTORICAL_OPEN") {
            cfg.historical_open = value;
        }
        if let Some(value) = env_value(env, "KITE_HISTORICAL_CLOSE") {
            cfg.historical_close = value;
        }
        if let Some(value) = env_value(env, "KITE_MARKET_PROTECTION") {
            cfg.market_protection = Some(value);
        }
        if let Some(value) = env_value(env, "KITE_AUTOSLICE") {
            cfg.autoslice = parse_bool(&value).unwrap_or(cfg.autoslice);
        }
        if let Some(value) = env_value(env, "KITE_INCLUDE_HOLDINGS") {
            cfg.include_holdings = parse_bool(&value).unwrap_or(cfg.include_holdings);
        }
        if let Some(value) = env_value(env, "KITE_TAG_PREFIX") {
            cfg.tag_prefix = Some(value);
        }
        cfg
    }

    fn merge_explicit(&mut self, explicit: KiteOrderConfig) {
        self.exchange = explicit.exchange;
        self.product = explicit.product;
        self.order_variety = explicit.order_variety;
        self.order_type = explicit.order_type;
        self.validity = explicit.validity;
        self.historical_open = explicit.historical_open;
        self.historical_close = explicit.historical_close;
        self.market_protection = explicit.market_protection;
        self.autoslice = explicit.autoslice;
        self.include_holdings = explicit.include_holdings;
        self.tag_prefix = explicit.tag_prefix;
    }
}

pub fn wait_for_local_request_token(
    redirect_url: &str,
    timeout: std::time::Duration,
) -> Result<String, String> {
    let (host, port, path) = parse_local_redirect_url(redirect_url)?;
    let listener = std::net::TcpListener::bind((host.as_str(), port))
        .map_err(|e| format!("failed to bind Kite callback listener on {host}:{port}: {e}"))?;
    listener
        .set_nonblocking(true)
        .map_err(|e| format!("failed to configure callback listener: {e}"))?;
    let deadline = Instant::now() + timeout;
    info!(redirect_url, "waiting for Kite request_token callback");
    while Instant::now() < deadline {
        match listener.accept() {
            Ok((mut stream, _)) => {
                let mut buf = [0_u8; 4096];
                let n = stream
                    .read(&mut buf)
                    .map_err(|e| format!("failed to read Kite callback: {e}"))?;
                let request = String::from_utf8_lossy(&buf[..n]);
                let token = extract_request_token(&request, &path);
                let body = if token.is_some() {
                    "Kite token captured. You can close this tab.\n"
                } else {
                    "Kite callback received but request_token was missing.\n"
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes());
                return token.ok_or_else(|| "request_token missing from Kite callback".into());
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            Err(e) => return Err(format!("Kite callback listener failed: {e}")),
        }
    }
    Err("timed out waiting for Kite request_token callback".into())
}

fn parse_kite_binary_frame(bytes: &[u8]) -> Result<Vec<KiteTick>, String> {
    if bytes.len() == 1 {
        return Ok(Vec::new());
    }
    if bytes.len() < 2 {
        return Err("Kite binary frame too short".into());
    }
    let packet_count = u16::from_be_bytes([bytes[0], bytes[1]]) as usize;
    let mut offset = 2;
    let mut ticks = Vec::with_capacity(packet_count);
    for _ in 0..packet_count {
        if offset + 2 > bytes.len() {
            return Err("Kite packet length missing".into());
        }
        let packet_len = u16::from_be_bytes([bytes[offset], bytes[offset + 1]]) as usize;
        offset += 2;
        if offset + packet_len > bytes.len() {
            return Err("Kite packet truncated".into());
        }
        let packet = &bytes[offset..offset + packet_len];
        offset += packet_len;
        if let Some(tick) = parse_kite_quote_packet(packet)? {
            ticks.push(tick);
        }
    }
    Ok(ticks)
}

fn parse_kite_quote_packet(packet: &[u8]) -> Result<Option<KiteTick>, String> {
    if packet.len() < 8 {
        return Ok(None);
    }
    let instrument_token = read_i32_be(packet, 0)? as u32;
    let last_price = read_i32_be(packet, 4)? as f64 / 100.0;
    let day_volume = if packet.len() >= 20 {
        read_i32_be(packet, 16)? as f64
    } else {
        0.0
    };
    let exchange_timestamp_ms = if packet.len() >= 64 {
        read_i32_be(packet, 60)? as i64 * 1000
    } else {
        Utc::now().timestamp_millis()
    };
    Ok(Some(KiteTick {
        instrument_token,
        last_price,
        day_volume,
        exchange_timestamp_ms,
    }))
}

fn read_i32_be(bytes: &[u8], offset: usize) -> Result<i32, String> {
    let slice = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| format!("missing int32 at offset {offset}"))?;
    Ok(i32::from_be_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

fn historical_chunks(start: NaiveDate, end_exclusive: NaiveDate) -> Vec<(NaiveDate, NaiveDate)> {
    let mut out = Vec::new();
    let mut chunk_start = start;
    while chunk_start < end_exclusive {
        let chunk_end = (chunk_start + chrono::Duration::days(60)).min(end_exclusive);
        out.push((chunk_start, chunk_end));
        chunk_start = chunk_end;
    }
    out
}

fn historical_request_window(
    chunk_start: NaiveDate,
    chunk_end_exclusive: NaiveDate,
    market_open: &str,
    market_close: &str,
) -> (String, String) {
    let chunk_to = chunk_end_exclusive - chrono::Duration::days(1);
    (
        format!("{} {market_open}:00", chunk_start.format("%Y-%m-%d")),
        format!("{} {market_close}:00", chunk_to.format("%Y-%m-%d")),
    )
}

fn resolve_tokens_from_csv(csv: &str, exchange: &str) -> Result<HashMap<String, String>, String> {
    let mut map = HashMap::new();
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(true)
        .from_reader(csv.as_bytes());
    for row in reader.deserialize::<KiteInstrumentRow>() {
        let row = row.map_err(|e| format!("kite instruments CSV parse failed: {e}"))?;
        // Kite's instruments dump includes BSE rows and index pseudo-instruments
        // with the same tradingsymbols. Keep only cash-equity rows for the
        // configured exchange so live/replay symbols resolve deterministically.
        if row.exchange != exchange || row.segment != exchange || row.instrument_type != "EQ" {
            continue;
        }
        map.insert(row.tradingsymbol, row.instrument_token);
    }
    Ok(map)
}

fn kite_instrument_cache_path_in(base_dir: &Path, exchange: &str) -> PathBuf {
    let exchange = exchange
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .collect::<String>()
        .to_ascii_lowercase();
    base_dir
        .join("data/cache/kite")
        .join(format!("instruments_{exchange}.csv"))
}

fn persist_instrument_cache(exchange: &str, csv: &str) {
    persist_instrument_cache_at(Path::new("."), exchange, csv);
}

fn persist_instrument_cache_at(base_dir: &Path, exchange: &str, csv: &str) {
    let path = kite_instrument_cache_path_in(base_dir, exchange);
    if let Some(parent) = path.parent() {
        if let Err(e) = fs::create_dir_all(parent) {
            warn!(
                path = %parent.display(),
                error = %e,
                "failed to create Kite instrument cache directory"
            );
            return;
        }
    }
    if let Err(e) = fs::write(&path, csv) {
        warn!(
            path = %path.display(),
            error = %e,
            "failed to persist Kite instrument cache"
        );
    }
}

fn load_cached_instrument_tokens(exchange: &str) -> Option<HashMap<String, String>> {
    load_cached_instrument_tokens_at(Path::new("."), exchange)
}

fn load_cached_instrument_tokens_at(
    base_dir: &Path,
    exchange: &str,
) -> Option<HashMap<String, String>> {
    let path = kite_instrument_cache_path_in(base_dir, exchange);
    let csv = match fs::read_to_string(&path) {
        Ok(csv) => csv,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            warn!(
                path = %path.display(),
                error = %e,
                "failed to read Kite instrument cache"
            );
            return None;
        }
    };
    match resolve_tokens_from_csv(&csv, exchange) {
        Ok(tokens) if !tokens.is_empty() => Some(tokens),
        Ok(_) => {
            warn!(
                path = %path.display(),
                exchange,
                "Kite instrument cache did not contain equity tokens"
            );
            None
        }
        Err(e) => {
            warn!(
                path = %path.display(),
                error = %e,
                "failed to parse Kite instrument cache"
            );
            None
        }
    }
}

fn filter_instrument_tokens(
    symbols: &[String],
    token_map: &HashMap<String, String>,
) -> Result<HashMap<String, String>, String> {
    let mut out = HashMap::new();
    for symbol in symbols {
        let token = token_map
            .get(symbol)
            .ok_or_else(|| format!("instrument token not found for {symbol}"))?;
        out.insert(symbol.clone(), token.clone());
    }
    Ok(out)
}

fn stream_tokens_from_map(
    symbols: &[String],
    token_map: &HashMap<String, String>,
) -> Result<(HashMap<u32, String>, Vec<u32>), String> {
    let mut token_to_symbol = HashMap::new();
    let mut tokens = Vec::new();
    for symbol in symbols {
        let token = token_map
            .get(symbol)
            .ok_or_else(|| format!("instrument token not found for {symbol}"))?
            .parse::<u32>()
            .map_err(|e| format!("bad instrument token for {symbol}: {e}"))?;
        token_to_symbol.insert(token, symbol.clone());
        tokens.push(token);
    }
    Ok((token_to_symbol, tokens))
}

fn kite_stream_reconnect_delay_secs(reconnect_count: u64) -> u64 {
    match reconnect_count {
        0 | 1 => 5,
        2 => 10,
        3 => 20,
        _ => 60,
    }
}

fn merge_position(map: &mut HashMap<String, (f64, f64)>, symbol: String, qty: f64, avg: f64) {
    map.entry(symbol)
        .and_modify(|(existing_qty, existing_avg)| {
            let new_qty = *existing_qty + qty;
            if new_qty.abs() > 1e-9 {
                *existing_avg =
                    ((*existing_avg * existing_qty.abs()) + (avg * qty.abs())) / new_qty.abs();
            }
            *existing_qty = new_qty;
        })
        .or_insert((qty, avg));
}

async fn parse_kite_response<T: for<'de> Deserialize<'de>>(
    response: reqwest::Response,
    context: &str,
) -> Result<T, String> {
    let value = parse_kite_json(response, context).await?;
    serde_json::from_value(value).map_err(|e| format!("{context} parse failed: {e}"))
}

async fn parse_kite_json(
    response: reqwest::Response,
    context: &str,
) -> Result<serde_json::Value, String> {
    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|e| format!("{context} body read failed: {e}"))?;
    if !status.is_success() {
        return Err(format!("{context} error {status}: {text}"));
    }
    serde_json::from_str(&text).map_err(|e| format!("{context} JSON parse failed: {e}: {text}"))
}

async fn parse_kite_text(response: reqwest::Response, context: &str) -> Result<String, String> {
    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|e| format!("{context} body read failed: {e}"))?;
    if !status.is_success() {
        return Err(format!("{context} error {status}: {text}"));
    }
    Ok(text)
}

fn order_ids_from_payload(payload: &serde_json::Value) -> Result<Vec<String>, String> {
    let data = payload
        .get("data")
        .ok_or_else(|| "kite order response missing data".to_string())?;
    if let Some(id) = data.get("order_id").and_then(|v| v.as_str()) {
        return Ok(vec![id.to_string()]);
    }
    if let Some(items) = data.as_array() {
        let mut ids = Vec::new();
        let mut errors = Vec::new();
        for item in items {
            if let Some(id) = item.get("order_id").and_then(|v| v.as_str()) {
                ids.push(id.to_string());
            } else if let Some(error) = item.get("error") {
                errors.push(error.to_string());
            }
        }
        if !errors.is_empty() {
            return Err(format!("kite autoslice placement had errors: {errors:?}"));
        }
        if !ids.is_empty() {
            return Ok(ids);
        }
    }
    Err(format!("kite order response missing order_id: {payload}"))
}

fn kite_checksum(api_key: &str, request_token: &str, api_secret: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(api_key.as_bytes());
    hasher.update(request_token.as_bytes());
    hasher.update(api_secret.as_bytes());
    let digest = hasher.finalize();
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn persist_env_value(path: &Path, key: &str, value: &str) -> Result<(), String> {
    let original = std::fs::read_to_string(path).unwrap_or_default();
    let mut found = false;
    let mut lines = Vec::new();
    for line in original.lines() {
        if line.trim_start().starts_with(&format!("{key}=")) {
            lines.push(format!("{key}={value}"));
            found = true;
        } else {
            lines.push(line.to_string());
        }
    }
    if !found {
        if !lines.is_empty() {
            lines.push(String::new());
        }
        lines.push(format!("{key}={value}"));
    }
    std::fs::write(path, format!("{}\n", lines.join("\n")))
        .map_err(|e| format!("failed to persist {key} in {}: {e}", path.display()))
}

fn parse_local_redirect_url(url: &str) -> Result<(String, u16, String), String> {
    let rest = url
        .strip_prefix("http://")
        .ok_or_else(|| "Kite local callback must use http://localhost".to_string())?;
    let (host_port, path) = rest.split_once('/').unwrap_or((rest, ""));
    let (host, port) = host_port.split_once(':').unwrap_or((host_port, "80"));
    if host != "localhost" && host != "127.0.0.1" {
        return Err("Kite local callback host must be localhost or 127.0.0.1".into());
    }
    let port = port
        .parse::<u16>()
        .map_err(|e| format!("bad Kite callback port: {e}"))?;
    Ok((host.to_string(), port, format!("/{path}")))
}

fn extract_request_token(request: &str, expected_path: &str) -> Option<String> {
    let request_line = request.lines().next()?;
    let target = request_line.split_whitespace().nth(1)?;
    let (path, query) = target.split_once('?')?;
    if path != expected_path && path != "/" {
        return None;
    }
    let mut token = None;
    let mut success = false;
    for pair in query.split('&') {
        let (key, value) = pair.split_once('=')?;
        match key {
            "request_token" => token = Some(value.to_string()),
            "status" if value == "success" => success = true,
            _ => {}
        }
    }
    if path == "/" && !success {
        return None;
    }
    token
}

fn env_value(env: &HashMap<String, String>, key: &str) -> Option<String> {
    env.get(key)
        .map(|v| v.trim())
        .filter(|v| !v.is_empty() && *v != "replace-me")
        .map(ToString::to_string)
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|v| !v.is_empty())
}

fn parse_bool(raw: &str) -> Option<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_values_merges_access_token_from_env() {
        let dir = tempfile::tempdir().unwrap();
        let env_path = dir.path().join(".env.india");
        std::fs::write(
            &env_path,
            "KITE_ACCESS_TOKEN=env-token\nKITE_PRODUCT=CNC\nKITE_AUTOSLICE=false\n",
        )
        .unwrap();
        let client = KiteClient::from_values(
            Some("key"),
            Some("secret"),
            None,
            None,
            KiteOrderConfig::default(),
            Some(&env_path),
        )
        .unwrap();
        assert_eq!(client.access_token.as_deref(), Some("env-token"));
        assert_eq!(client.order.product, "MIS");
        assert!(client.order.autoslice);
    }

    #[test]
    fn historical_chunks_do_not_overlap_boundary_days() {
        let start = NaiveDate::from_ymd_opt(2025, 1, 1).unwrap();
        let end = NaiveDate::from_ymd_opt(2025, 5, 1).unwrap();
        let chunks = historical_chunks(start, end);
        assert!(chunks.len() > 1);
        for pair in chunks.windows(2) {
            assert_eq!(pair[0].1, pair[1].0);
            assert!(pair[0].1 - chrono::Duration::days(1) < pair[1].0);
        }
        assert_eq!(chunks.first().unwrap().0, start);
        assert_eq!(chunks.last().unwrap().1, end);
    }

    #[test]
    fn historical_request_window_uses_market_session_config() {
        let start = NaiveDate::from_ymd_opt(2025, 1, 1).unwrap();
        let end = NaiveDate::from_ymd_opt(2025, 3, 2).unwrap();
        let (from, to) = historical_request_window(start, end, "09:15", "15:30");
        assert_eq!(from, "2025-01-01 09:15:00");
        assert_eq!(to, "2025-03-01 15:30:00");
    }

    #[test]
    fn parses_kite_full_binary_packet() {
        let ts = Utc
            .with_ymd_and_hms(2026, 1, 2, 3, 45, 0)
            .unwrap()
            .timestamp() as i32;
        let mut packet = vec![0_u8; 184];
        write_i32(&mut packet, 0, 123);
        write_i32(&mut packet, 4, 10025);
        write_i32(&mut packet, 16, 900);
        write_i32(&mut packet, 60, ts);
        let mut frame = Vec::new();
        frame.extend_from_slice(&1_u16.to_be_bytes());
        frame.extend_from_slice(&(packet.len() as u16).to_be_bytes());
        frame.extend_from_slice(&packet);

        let ticks = parse_kite_binary_frame(&frame).unwrap();
        assert_eq!(ticks.len(), 1);
        assert_eq!(ticks[0].instrument_token, 123);
        assert_eq!(ticks[0].last_price, 100.25);
        assert_eq!(ticks[0].day_volume, 900.0);
        assert_eq!(ticks[0].exchange_timestamp_ms, ts as i64 * 1000);
    }

    #[test]
    fn binary_parser_accepts_heartbeat_and_rejects_empty_frame() {
        assert!(parse_kite_binary_frame(&[0]).unwrap().is_empty());
        assert!(parse_kite_binary_frame(&[]).is_err());
    }

    #[test]
    fn minute_aggregator_emits_completed_bar_on_next_minute() {
        let mut agg = KiteMinuteAggregator::default();
        let t1 = Utc
            .with_ymd_and_hms(2026, 1, 2, 3, 45, 10)
            .unwrap()
            .timestamp_millis();
        let t2 = Utc
            .with_ymd_and_hms(2026, 1, 2, 3, 46, 2)
            .unwrap()
            .timestamp_millis();
        assert!(agg
            .update(
                "TESTEQ",
                KiteTick {
                    instrument_token: 1,
                    last_price: 100.0,
                    day_volume: 10.0,
                    exchange_timestamp_ms: t1,
                },
            )
            .is_none());
        let bar = agg
            .update(
                "TESTEQ",
                KiteTick {
                    instrument_token: 1,
                    last_price: 101.0,
                    day_volume: 15.0,
                    exchange_timestamp_ms: t2,
                },
            )
            .unwrap();
        assert_eq!(bar.symbol, "TESTEQ");
        assert_eq!(bar.open, 100.0);
        assert_eq!(bar.close, 100.0);
        assert_eq!(bar.volume, 0.0);
        assert_eq!(bar.timestamp, (t1 / MINUTE_MS * MINUTE_MS) + MINUTE_MS);
    }

    #[test]
    fn parses_regular_and_autoslice_order_ids() {
        let regular = serde_json::json!({"status":"success","data":{"order_id":"1"}});
        assert_eq!(order_ids_from_payload(&regular).unwrap(), vec!["1"]);
        let sliced =
            serde_json::json!({"status":"success","data":[{"order_id":"1"},{"order_id":"2"}]});
        assert_eq!(order_ids_from_payload(&sliced).unwrap(), vec!["1", "2"]);
        let partial_error = serde_json::json!({"status":"success","data":[{"order_id":"1"},{"error":{"message":"bad"}}]});
        assert!(order_ids_from_payload(&partial_error).is_err());
    }

    #[test]
    fn stop_market_order_form_includes_trigger_without_changing_default_order_type() {
        let client = KiteClient::new("key".into(), "secret".into(), Some("token".into()));
        let form = client.build_order_form("TESTEQ", 3, "SELL", Some("SL-M"), Some(98.25));
        let values: HashMap<&str, String> = form.into_iter().collect();
        assert_eq!(values.get("order_type").map(String::as_str), Some("SL-M"));
        assert_eq!(
            values.get("trigger_price").map(String::as_str),
            Some("98.25")
        );
        assert_eq!(client.order.order_type, "MARKET");
    }

    #[test]
    fn order_form_omits_autoslice_unless_enabled() {
        let mut client = KiteClient::new("key".into(), "secret".into(), Some("token".into()));
        client.order.autoslice = false;
        let form = client.build_order_form("TESTEQ", 1, "BUY", None, None);
        assert!(!form.iter().any(|(key, _)| *key == "autoslice"));

        client.order.autoslice = true;
        let form = client.build_order_form("TESTEQ", 1, "BUY", None, None);
        assert!(form
            .iter()
            .any(|(key, value)| *key == "autoslice" && value == "true"));
    }

    #[test]
    fn amo_orders_reconcile_at_next_open() {
        let mut client = KiteClient::new("key".into(), "secret".into(), Some("token".into()));
        client.order.order_variety = "amo".into();
        assert_eq!(
            client.session_close_fill_contract(),
            SessionCloseFillContract::NextSessionOpen
        );
        assert!(client.supports_persisted_pending_open_reconcile());
    }

    #[test]
    fn amo_session_open_reconcile_orders_use_regular_variety() {
        let mut client = KiteClient::new("key".into(), "secret".into(), Some("token".into()));
        client.order.order_variety = "amo".into();
        assert_eq!(client.order.order_variety, "amo");
        assert_eq!(
            KiteClient::session_open_reconcile_order_variety(),
            "regular"
        );
    }

    #[tokio::test]
    async fn kite_ordering_fails_closed_for_paper_mode() {
        let client = KiteClient::new("key".into(), "secret".into(), Some("token".into()));
        let err = client
            .place_order("TESTEQ", 1.0, "buy", BrokerExecutionMode::Paper)
            .await
            .unwrap_err();
        assert!(err.contains("no paper endpoint"));
    }

    #[test]
    fn persists_access_token_in_env_file() {
        let dir = tempfile::tempdir().unwrap();
        let env_path = dir.path().join(".env.india");
        std::fs::write(&env_path, "KITE_API_KEY=k\nKITE_ACCESS_TOKEN=old\n").unwrap();
        persist_env_value(&env_path, "KITE_ACCESS_TOKEN", "new").unwrap();
        let text = std::fs::read_to_string(&env_path).unwrap();
        assert!(text.contains("KITE_API_KEY=k"));
        assert!(text.contains("KITE_ACCESS_TOKEN=new"));
        assert!(!text.contains("KITE_ACCESS_TOKEN=old"));
    }

    #[test]
    fn extracts_request_token_from_local_callback() {
        let request = "GET /kite/callback?request_token=abc123&status=success HTTP/1.1\r\n\r\n";
        assert_eq!(
            extract_request_token(request, "/kite/callback").as_deref(),
            Some("abc123")
        );
    }

    #[test]
    fn extracts_request_token_from_kite_root_redirect() {
        let request =
            "GET /?type=login&status=success&request_token=abc123&action=login HTTP/1.1\r\n\r\n";
        assert_eq!(
            extract_request_token(request, "/kite/callback").as_deref(),
            Some("abc123")
        );
    }

    #[test]
    fn rejects_root_redirect_without_success_status() {
        let request = "GET /?type=login&status=error&request_token=abc123 HTTP/1.1\r\n\r\n";
        assert_eq!(extract_request_token(request, "/kite/callback"), None);
    }

    #[test]
    fn filters_nse_equity_tokens_from_csv() {
        let csv = "instrument_token,exchange_token,tradingsymbol,name,last_price,expiry,strike,tick_size,lot_size,instrument_type,segment,exchange\n1,1,TESTEQ,TESTEQ,0,,0,0.05,1,EQ,NSE,NSE\n2,2,TESTEQ,TESTEQ,0,,0,0.05,1,EQ,BSE,BSE\n3,3,INDEXEQ,INDEXEQ,0,,0,0.05,1,EQ,INDICES,NSE\n";
        let map = resolve_tokens_from_csv(csv, "NSE").unwrap();
        assert_eq!(map.get("TESTEQ").map(String::as_str), Some("1"));
        assert_eq!(map.len(), 1);
    }

    #[test]
    fn instrument_cache_path_is_exchange_scoped_and_sanitized() {
        let base = Path::new("/tmp/openquant-test");
        assert_eq!(
            kite_instrument_cache_path_in(base, "NSE")
                .display()
                .to_string(),
            "/tmp/openquant-test/data/cache/kite/instruments_nse.csv"
        );
        assert_eq!(
            kite_instrument_cache_path_in(base, "NSE/../BSE")
                .display()
                .to_string(),
            "/tmp/openquant-test/data/cache/kite/instruments_nsebse.csv"
        );
    }

    #[test]
    fn instrument_cache_round_trips_without_kite_request() {
        let dir = tempfile::tempdir().unwrap();
        let csv = "instrument_token,exchange_token,tradingsymbol,name,last_price,expiry,strike,tick_size,lot_size,instrument_type,segment,exchange\n123,1,TESTEQ,TESTEQ,0,,0,0.05,1,EQ,NSE,NSE\n456,2,OTHEREQ,OTHEREQ,0,,0,0.05,1,EQ,NSE,NSE\n";
        persist_instrument_cache_at(dir.path(), "NSE", csv);

        let map = load_cached_instrument_tokens_at(dir.path(), "NSE").unwrap();
        assert_eq!(map.get("TESTEQ").map(String::as_str), Some("123"));
        assert_eq!(map.get("OTHEREQ").map(String::as_str), Some("456"));
    }

    #[test]
    fn filters_cached_instrument_tokens_to_requested_symbols() {
        let mut map = HashMap::new();
        map.insert("TESTEQ".to_string(), "123".to_string());
        map.insert("OTHEREQ".to_string(), "456".to_string());
        let filtered = filter_instrument_tokens(&["TESTEQ".to_string()], &map).unwrap();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered.get("TESTEQ").map(String::as_str), Some("123"));
    }

    #[test]
    fn stream_tokens_are_reused_without_refetching_instruments() {
        let mut map = HashMap::new();
        map.insert("TESTEQ".to_string(), "123".to_string());
        let (token_to_symbol, tokens) =
            stream_tokens_from_map(&["TESTEQ".to_string()], &map).expect("stream tokens");
        assert_eq!(tokens, vec![123]);
        assert_eq!(
            token_to_symbol.get(&123).map(String::as_str),
            Some("TESTEQ")
        );
    }

    #[test]
    fn websocket_url_uses_encoded_query_credentials_without_custom_headers() {
        let client = KiteClient::new(
            "key with space".into(),
            "secret".into(),
            Some("token/with+symbols".into()),
        );
        let url = client.websocket_url().unwrap();
        assert!(url.starts_with("wss://ws.kite.trade/?"));
        assert!(url.contains("api_key=key+with+space"));
        assert!(url.contains("access_token=token%2Fwith%2Bsymbols"));
        assert!(!url.contains("uid="));
    }

    #[tokio::test]
    #[ignore = "live Kite read-only integration; requires .env.india and only opens/closes WebSocket"]
    async fn live_kite_websocket_handshake_smoke() {
        if std::env::var("KITE_RUN_LIVE_INTEGRATION").ok().as_deref() != Some("1") {
            eprintln!("set KITE_RUN_LIVE_INTEGRATION=1 to run live Kite WebSocket smoke test");
            return;
        }
        let repo_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(3)
            .expect("repo root");
        let client = KiteClient::from_env(&repo_root.join(".env.india")).unwrap();
        let url = client.websocket_url().unwrap();
        let connection = tokio::time::timeout(
            tokio::time::Duration::from_secs(8),
            tokio_tungstenite::connect_async(url),
        )
        .await
        .expect("Kite WebSocket connect timed out")
        .expect("Kite WebSocket handshake failed");
        let (mut ws, _) = connection;
        ws.close(None).await.expect("close Kite WebSocket");
    }

    #[test]
    fn kite_stream_reconnect_backoff_caps_at_sixty_seconds() {
        assert_eq!(kite_stream_reconnect_delay_secs(1), 5);
        assert_eq!(kite_stream_reconnect_delay_secs(2), 10);
        assert_eq!(kite_stream_reconnect_delay_secs(3), 20);
        assert_eq!(kite_stream_reconnect_delay_secs(4), 60);
        assert_eq!(kite_stream_reconnect_delay_secs(99), 60);
    }

    #[test]
    fn instrument_failure_backoff_is_long_enough_for_kite_throttle_cooldown() {
        assert!(INSTRUMENT_FAILURE_BACKOFF_SECS >= 300);
    }

    #[tokio::test]
    async fn instrument_resolution_suppresses_recent_failure_without_refetch() {
        let client = KiteClient::new(
            "key".to_string(),
            "secret".to_string(),
            Some("token".into()),
        );
        {
            let mut cache = client.instrument_cache.lock().await;
            cache.last_error = Some((Instant::now(), "previous failure".to_string()));
        }

        let error = client
            .resolve_instrument_tokens(&["TESTEQ".to_string()])
            .await
            .unwrap_err();
        assert!(error.contains("suppressing refetch"));
    }

    #[test]
    fn merge_position_weighted_average_handles_holdings_and_net_positions() {
        let mut map = HashMap::new();
        merge_position(&mut map, "TESTEQ".to_string(), 10.0, 100.0);
        merge_position(&mut map, "TESTEQ".to_string(), 5.0, 112.0);
        assert_eq!(map.get("TESTEQ").map(|(qty, _)| *qty), Some(15.0));
        assert_eq!(map.get("TESTEQ").map(|(_, avg)| *avg), Some(104.0));
    }

    fn write_i32(packet: &mut [u8], offset: usize, value: i32) {
        packet[offset..offset + 4].copy_from_slice(&value.to_be_bytes());
    }
}
