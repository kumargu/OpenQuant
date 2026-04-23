//! Alpaca real-time market data stream via WebSocket.
//!
//! Connects to `wss://stream.data.alpaca.markets/v2/iex`, authenticates,
//! subscribes to minute bars for all pair symbols, and yields bars as they arrive.
//!
//! The engine processes each bar immediately — no buffering, no polling.
//!
//! ## Observability
//!
//! The stream task emits a deliberate mix of log levels so you can tune verbosity
//! to the diagnostic question at hand:
//!
//! - **INFO**: connect / auth / subscribe-send / subscribe-ack / 60-second
//!   heartbeat (bars total + last window + symbols active) / stale-symbol
//!   warnings promoted to WARN.
//! - **DEBUG**: welcome message body, non-bar messages, ping/pong frames,
//!   per-bar receipt, subscription-ack raw payload.
//! - **WARN**: stream reconnects, 90-second read timeout, invalid bars,
//!   out-of-order bars, duplicate bars, stale subscribed symbols during RTH.
//! - **ERROR**: stream protocol errors forcing reconnect.
//!
//! Tune via `RUST_LOG`:
//!
//! ```text
//! RUST_LOG=info                                # default; ~1 line/minute per stream
//! RUST_LOG=info,openquant_runner::stream=debug # distinguish silent-death vs
//!                                              # healthy-but-quiet: ping/pong
//!                                              # frames become visible
//! ```

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, error, info, warn};

use crate::market_session;

const ALPACA_STREAM_URL: &str = "wss://stream.data.alpaca.markets/v2/iex";

/// Alpaca reports bar OPEN time in the `t` field (both REST and WebSocket).
/// Add 60s so the timestamp reflects when the bar data is finalized.
/// This matters for force_close_minute: a 15:29 bar completes at 15:30.
const MINUTE_BAR_DURATION_MS: i64 = 60_000;

/// Heartbeat interval — emits one INFO summary per interval with live counters.
const HEARTBEAT_INTERVAL_SECS: u64 = 60;

/// Per-symbol staleness threshold during RTH. A symbol that was subscribed but
/// has had no bar arrive in this long is flagged as WARN. 3 minutes accounts
/// for illiquid names that don't print every minute while still catching
/// genuinely broken subscriptions.
const SYMBOL_STALE_THRESHOLD_SECS: u64 = 180;

/// A bar received from the Alpaca stream.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct StreamBar {
    pub symbol: String,
    pub timestamp: i64, // millis since epoch
    pub close: f64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub volume: f64,
}

/// Alpaca stream message types we care about.
#[derive(Debug, Deserialize)]
struct StreamMessage {
    #[serde(rename = "T")]
    msg_type: String,
    #[serde(rename = "S", default)]
    symbol: String,
    #[serde(rename = "t", default)]
    timestamp: String,
    #[serde(rename = "c", default)]
    close: f64,
    #[serde(rename = "o", default)]
    open: f64,
    #[serde(rename = "h", default)]
    high: f64,
    #[serde(rename = "l", default)]
    low: f64,
    #[serde(rename = "v", default)]
    volume: f64,
    /// For subscription-confirmation messages (`T == "subscription"`) Alpaca
    /// echoes back the stream lists it accepted. Logging this lets us catch
    /// cases where we asked for 52 symbols but only got 48.
    #[serde(rename = "bars", default)]
    bars_confirmed: Vec<String>,
    #[serde(rename = "trades", default)]
    #[allow(dead_code)]
    trades_confirmed: Vec<String>,
    #[serde(rename = "quotes", default)]
    #[allow(dead_code)]
    quotes_confirmed: Vec<String>,
    #[serde(rename = "msg", default)]
    message_body: String,
    #[serde(rename = "code", default)]
    code: i64,
}

/// Counters maintained across the read loop for the INFO heartbeat.
struct StreamMetrics {
    bars_total: u64,
    bars_last_window: u64,
    invalid_bars: u64,
    out_of_order: u64,
    duplicates: u64,
    pings_received: u64,
    pongs_sent: u64,
    non_bar_messages: u64,
    window_started: Instant,
    /// Wall-clock time of the last message of ANY kind (bar, ping, text,
    /// close). Used by the silent-death watchdog in the heartbeat branch
    /// of the main select. The check has to live on a struct field rather
    /// than be implemented via `tokio::time::timeout(read.next())` because
    /// the `select!` arm that wins (the 60s heartbeat, typically) cancels
    /// the pending timeout future every iteration and prevents it from
    /// ever firing.
    last_message_at: Instant,
    last_bar_ts_by_symbol: HashMap<String, i64>,
    /// Symbols we've already warned about in the current staleness window.
    /// Prevents WARN-spam for a symbol that's persistently stale (one warn
    /// per staleness window is enough to signal the problem).
    stale_warned: HashSet<String>,
}

impl StreamMetrics {
    fn new() -> Self {
        let now = Instant::now();
        Self {
            bars_total: 0,
            bars_last_window: 0,
            invalid_bars: 0,
            out_of_order: 0,
            duplicates: 0,
            pings_received: 0,
            pongs_sent: 0,
            non_bar_messages: 0,
            window_started: now,
            last_message_at: now,
            last_bar_ts_by_symbol: HashMap::new(),
            stale_warned: HashSet::new(),
        }
    }

    fn mark_message(&mut self) {
        self.last_message_at = Instant::now();
    }

    fn record_bar(&mut self, symbol: &str, ts: i64) -> BarClassification {
        self.bars_total += 1;
        self.bars_last_window += 1;
        match self.last_bar_ts_by_symbol.get(symbol).copied() {
            Some(prev) if ts < prev => {
                // Count but DO NOT regress the stored last-seen clock.
                // Moving it backward would (1) inflate age_ms in the
                // heartbeat, triggering false stale warnings, and
                // (2) make a subsequent bar with ts < true_latest look
                // "fresh" on comparison against the now-regressed value.
                self.out_of_order += 1;
                BarClassification::OutOfOrder { prev_ts: prev }
            }
            Some(prev) if ts == prev => {
                self.duplicates += 1;
                BarClassification::Duplicate
            }
            _ => {
                self.last_bar_ts_by_symbol.insert(symbol.to_string(), ts);
                self.stale_warned.remove(symbol);
                BarClassification::Fresh
            }
        }
    }
}

enum BarClassification {
    Fresh,
    OutOfOrder { prev_ts: i64 },
    Duplicate,
}

/// Start streaming bars from Alpaca. Returns a channel receiver that yields bars.
///
/// This spawns a background task that maintains the WebSocket connection,
/// handles auth, subscribes to symbols, and sends parsed bars to the channel.
/// The task reconnects automatically on disconnect.
pub async fn start_bar_stream(
    api_key: &str,
    api_secret: &str,
    symbols: &[String],
) -> mpsc::Receiver<StreamBar> {
    let (tx, rx) = mpsc::channel(1000);
    let api_key = api_key.to_string();
    let api_secret = api_secret.to_string();
    let symbols = symbols.to_vec();

    tokio::spawn(async move {
        let mut reconnect_count: u64 = 0;
        loop {
            info!(reconnect_count, "connecting to Alpaca stream");
            match run_stream(&api_key, &api_secret, &symbols, &tx).await {
                Ok(()) => {
                    // Server closed the connection (end of day, maintenance).
                    // Always reconnect — the runner loop expects bars indefinitely.
                    warn!("stream closed by server — reconnecting in 5s");
                    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                }
                Err(e) => {
                    error!("stream error: {e} — reconnecting in 5s");
                    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                }
            }
            reconnect_count += 1;
        }
    });

    rx
}

async fn run_stream(
    api_key: &str,
    api_secret: &str,
    symbols: &[String],
    tx: &mpsc::Sender<StreamBar>,
) -> Result<(), String> {
    let (ws_stream, _) = tokio_tungstenite::connect_async(ALPACA_STREAM_URL)
        .await
        .map_err(|e| format!("WebSocket connect failed: {e}"))?;

    let (mut write, mut read) = ws_stream.split();
    info!("WebSocket connected");

    // Read the welcome message
    if let Some(msg) = read.next().await {
        let msg = msg.map_err(|e| format!("read error: {e}"))?;
        debug!(welcome = msg.to_text().unwrap_or(""), "welcome message");
    }

    // Authenticate
    let auth = serde_json::json!({
        "action": "auth",
        "key": api_key,
        "secret": api_secret,
    });
    write
        .send(Message::Text(auth.to_string()))
        .await
        .map_err(|e| format!("auth send failed: {e}"))?;

    // Read auth response
    if let Some(msg) = read.next().await {
        let msg = msg.map_err(|e| format!("read error: {e}"))?;
        let text = msg.to_text().unwrap_or("");
        if text.contains("\"error\"") {
            return Err(format!("auth failed: {text}"));
        }
        info!("authenticated with Alpaca stream");
    }

    // Subscribe to minute bars for all symbols
    let subscribe = serde_json::json!({
        "action": "subscribe",
        "bars": symbols,
    });
    write
        .send(Message::Text(subscribe.to_string()))
        .await
        .map_err(|e| format!("subscribe send failed: {e}"))?;

    info!(
        symbols_requested = symbols.len(),
        "subscribed to minute bars (sent)"
    );

    // Expected symbol set — used by the watchdog to enumerate what we're
    // waiting for and to validate server-side subscription acknowledgements.
    let expected: HashSet<String> = symbols.iter().cloned().collect();
    let mut metrics = StreamMetrics::new();

    // Silent-death watchdog: if no message (bar, ping, or otherwise) has
    // arrived in this long, the socket is a zombie and we force a
    // reconnect. Enforced by a `sleep_until(deadline)` future in its own
    // select arm — NOT the heartbeat arm — so detection latency is bounded
    // by the timeout itself rather than by the 60s heartbeat cadence. A
    // heartbeat-branch check could add up to ~60s of extra latency if the
    // socket went silent right after a tick.
    let read_timeout = tokio::time::Duration::from_secs(90);
    let mut heartbeat =
        tokio::time::interval(tokio::time::Duration::from_secs(HEARTBEAT_INTERVAL_SECS));
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // First tick fires immediately — consume it so we don't emit a
    // heartbeat with zero data before any bars have had a chance to arrive.
    heartbeat.tick().await;

    // Silent-death deadline. Reset to `now + read_timeout` on every
    // message arrival. When this future fires, we treat the stream as
    // dead and return Err to force reconnect.
    let idle_deadline = tokio::time::sleep(read_timeout);
    tokio::pin!(idle_deadline);

    loop {
        tokio::select! {
            // Keep read arm first so that if a message and the deadline
            // fire at the same tick (unlikely but possible), the message
            // wins.
            biased;

            msg = read.next() => {
                let msg = match msg {
                    Some(m) => m.map_err(|e| format!("read error: {e}"))?,
                    None => {
                        // Stream ended normally
                        info!(bars_total = metrics.bars_total, "stream ended (no more messages)");
                        return Ok(());
                    }
                };
                // Any message — bar, text, ping, close — resets the
                // silent-death deadline. Keep `last_message_at` in sync
                // for pretty-printing in the heartbeat log.
                metrics.mark_message();
                idle_deadline
                    .as_mut()
                    .reset(tokio::time::Instant::now() + read_timeout);

                match msg {
                    Message::Ping(data) => {
                        metrics.pings_received += 1;
                        if write.send(Message::Pong(data)).await.is_ok() {
                            metrics.pongs_sent += 1;
                        }
                        debug!(pings_received = metrics.pings_received, "ping/pong");
                        continue;
                    }
                    Message::Close(_) => {
                        info!(bars_total = metrics.bars_total, "stream closed by server");
                        return Ok(());
                    }
                    Message::Text(t) => {
                        handle_text(&t, &mut metrics, &expected, tx).await?;
                    }
                    _ => {}
                }
            }
            _ = heartbeat.tick() => {
                emit_heartbeat(&mut metrics, &expected);
            }
            _ = &mut idle_deadline => {
                let silent_for = metrics.last_message_at.elapsed();
                warn!(
                    bars_total = metrics.bars_total,
                    pings_received = metrics.pings_received,
                    seconds_silent = silent_for.as_secs(),
                    threshold_secs = read_timeout.as_secs(),
                    "no message from stream — assuming dead connection, reconnecting"
                );
                return Err(format!("read timeout ({}s)", silent_for.as_secs()));
            }
        }
    }
}

/// Handle a text message from Alpaca. Parses the JSON array, routes each
/// element to the appropriate handler (bar, subscription-ack, error).
async fn handle_text(
    text: &str,
    metrics: &mut StreamMetrics,
    expected: &HashSet<String>,
    tx: &mpsc::Sender<StreamBar>,
) -> Result<(), String> {
    let messages: Vec<StreamMessage> = match serde_json::from_str(text) {
        Ok(m) => m,
        Err(e) => {
            debug!(err = %e, payload = text, "failed to parse stream message");
            return Ok(());
        }
    };

    for msg in messages {
        match msg.msg_type.as_str() {
            "b" => process_bar(msg, metrics, tx).await?,
            "subscription" => log_subscription_ack(&msg, expected)?,
            "success" => info!(msg = msg.message_body.as_str(), "stream success"),
            "error" => {
                error!(
                    code = msg.code,
                    msg = msg.message_body.as_str(),
                    "stream error message"
                );
            }
            other => {
                metrics.non_bar_messages += 1;
                debug!(msg_type = other, "non-bar message");
            }
        }
    }
    Ok(())
}

fn log_subscription_ack(msg: &StreamMessage, expected: &HashSet<String>) -> Result<(), String> {
    let confirmed: HashSet<String> = msg.bars_confirmed.iter().cloned().collect();
    let missing: Vec<String> = expected.difference(&confirmed).cloned().collect();
    let extra: Vec<String> = confirmed.difference(expected).cloned().collect();
    if missing.is_empty() && extra.is_empty() {
        info!(
            confirmed = confirmed.len(),
            "subscription ack — server confirmed all requested symbols"
        );
        Ok(())
    } else {
        warn!(
            requested = expected.len(),
            confirmed = confirmed.len(),
            missing_count = missing.len(),
            extra_count = extra.len(),
            missing_sample = ?missing.iter().take(10).collect::<Vec<_>>(),
            extra_sample = ?extra.iter().take(10).collect::<Vec<_>>(),
            "subscription ack — mismatch between requested and confirmed symbols"
        );
        Err(format!(
            "subscription ack mismatch: confirmed={}, missing={}, extra={}",
            confirmed.len(),
            missing.len(),
            extra.len()
        ))
    }
}

async fn process_bar(
    msg: StreamMessage,
    metrics: &mut StreamMetrics,
    tx: &mpsc::Sender<StreamBar>,
) -> Result<(), String> {
    let ts = chrono::DateTime::parse_from_rfc3339(&msg.timestamp)
        .map(|dt| dt.timestamp_millis() + MINUTE_BAR_DURATION_MS)
        .unwrap_or(0);

    if ts == 0 || msg.close <= 0.0 {
        metrics.invalid_bars += 1;
        warn!(
            symbol = msg.symbol.as_str(),
            close = msg.close,
            raw_ts = msg.timestamp.as_str(),
            "invalid bar from stream"
        );
        return Ok(());
    }

    let classification = metrics.record_bar(&msg.symbol, ts);
    match classification {
        BarClassification::Fresh => {}
        BarClassification::Duplicate => {
            warn!(
                symbol = msg.symbol.as_str(),
                ts,
                duplicates_total = metrics.duplicates,
                "duplicate bar (same symbol, same ts)"
            );
            // Continue processing — the downstream engine dedups via
            // HashMap insert, so a dup is harmless. We log + count so
            // we can spot a server-side replay storm.
        }
        BarClassification::OutOfOrder { prev_ts } => {
            warn!(
                symbol = msg.symbol.as_str(),
                ts,
                prev_ts,
                out_of_order_total = metrics.out_of_order,
                "out-of-order bar (arrival ts older than previous for this symbol)"
            );
        }
    }

    let bar = StreamBar {
        symbol: msg.symbol,
        timestamp: ts,
        close: msg.close,
        open: msg.open,
        high: msg.high,
        low: msg.low,
        volume: msg.volume,
    };

    debug!(
        symbol = bar.symbol.as_str(),
        close = %format_args!("{:.2}", bar.close),
        ts = bar.timestamp,
        "bar received"
    );

    if tx.send(bar).await.is_err() {
        // Receiver dropped — shutdown
        return Err("receiver dropped".into());
    }
    Ok(())
}

/// Emit a heartbeat summarizing the last window, and fire stale-symbol
/// warnings if any subscribed symbol has been quiet during RTH.
fn emit_heartbeat(metrics: &mut StreamMetrics, expected: &HashSet<String>) {
    let window_secs = metrics.window_started.elapsed().as_secs().max(1);
    let now_ms = chrono::Utc::now().timestamp_millis();
    let now_utc = chrono::Utc::now();
    let in_rth = market_session::is_rth_utc(now_utc);

    // Age of the oldest last-seen bar — flags whether ANY symbols are getting
    // bars at all.
    let oldest_symbol_age_ms = metrics
        .last_bar_ts_by_symbol
        .iter()
        .map(|(sym, &ts)| (sym.clone(), now_ms - ts))
        .max_by_key(|(_, age)| *age);

    let active_symbols = metrics.last_bar_ts_by_symbol.len();

    info!(
        bars_total = metrics.bars_total,
        bars_last_window = metrics.bars_last_window,
        window_secs,
        symbols_active = active_symbols,
        symbols_subscribed = expected.len(),
        pings = metrics.pings_received,
        invalid = metrics.invalid_bars,
        out_of_order = metrics.out_of_order,
        duplicates = metrics.duplicates,
        oldest_symbol_age_s = oldest_symbol_age_ms
            .as_ref()
            .map(|(_, age)| age / 1000)
            .unwrap_or(0),
        oldest_symbol = oldest_symbol_age_ms
            .as_ref()
            .map(|(s, _)| s.as_str())
            .unwrap_or("-"),
        in_rth,
        "stream heartbeat"
    );

    // Stale-symbol watchdog — only active during RTH.
    if in_rth {
        let stale_threshold_ms = (SYMBOL_STALE_THRESHOLD_SECS * 1000) as i64;
        // Symbols we expected but have never seen at all:
        let never_seen: Vec<&str> = expected
            .iter()
            .filter(|s| !metrics.last_bar_ts_by_symbol.contains_key(s.as_str()))
            .map(|s| s.as_str())
            .collect();
        if !never_seen.is_empty() && metrics.bars_total > 0 {
            // Bars are arriving for *some* symbols but not these. One WARN per
            // heartbeat is enough to surface the gap without flooding.
            warn!(
                count = never_seen.len(),
                sample = ?never_seen.iter().take(10).collect::<Vec<_>>(),
                "symbols subscribed but never received any bar during RTH"
            );
        }

        // Symbols that were fresh at some point but have since gone quiet:
        let mut newly_stale = Vec::new();
        for (sym, &last_ts) in &metrics.last_bar_ts_by_symbol {
            let age_ms = now_ms - last_ts;
            if age_ms > stale_threshold_ms && !metrics.stale_warned.contains(sym) {
                newly_stale.push((sym.clone(), age_ms / 1000));
            }
        }
        for (sym, age_s) in newly_stale {
            warn!(
                symbol = sym.as_str(),
                age_s,
                threshold_s = SYMBOL_STALE_THRESHOLD_SECS,
                "subscribed symbol has gone quiet during RTH"
            );
            metrics.stale_warned.insert(sym);
        }
    }

    // Reset window counters but keep the per-symbol map alive across windows.
    metrics.bars_last_window = 0;
    metrics.window_started = Instant::now();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_subscription_ack_mismatch_fails_closed() {
        let expected: HashSet<String> = ["AAPL".to_string(), "MSFT".to_string()]
            .into_iter()
            .collect();
        let msg = StreamMessage {
            msg_type: "subscription".to_string(),
            symbol: String::new(),
            timestamp: String::new(),
            close: 0.0,
            open: 0.0,
            high: 0.0,
            low: 0.0,
            volume: 0.0,
            bars_confirmed: vec!["AAPL".to_string()],
            trades_confirmed: vec![],
            quotes_confirmed: vec![],
            message_body: String::new(),
            code: 0,
        };

        assert!(log_subscription_ack(&msg, &expected).is_err());
    }
}
