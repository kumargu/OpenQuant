//! Alpaca real-time market data stream via WebSocket.
//!
//! Connects to `wss://stream.data.alpaca.markets/v2/iex`, authenticates,
//! subscribes to minute bars for all pair symbols, and yields bars as they arrive.
//!
//! The engine processes each bar immediately — no buffering, no polling.

use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, error, info, warn};

const ALPACA_STREAM_URL: &str = "wss://stream.data.alpaca.markets/v2/iex";

/// A bar received from the Alpaca stream.
#[derive(Debug, Clone)]
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
        loop {
            info!("connecting to Alpaca stream");
            match run_stream(&api_key, &api_secret, &symbols, &tx).await {
                Ok(()) => {
                    info!("stream ended cleanly");
                    break;
                }
                Err(e) => {
                    error!("stream error: {e} — reconnecting in 5s");
                    tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
                }
            }
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
        debug!("welcome: {}", msg.to_text().unwrap_or(""));
    }

    // Authenticate
    let auth = serde_json::json!({
        "action": "auth",
        "key": api_key,
        "secret": api_secret,
    });
    write
        .send(Message::Text(auth.to_string().into()))
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
        .send(Message::Text(subscribe.to_string().into()))
        .await
        .map_err(|e| format!("subscribe send failed: {e}"))?;

    info!(symbols = symbols.len(), "subscribed to minute bars");

    // Read messages forever — each bar is fed to the channel
    while let Some(msg) = read.next().await {
        let msg = msg.map_err(|e| format!("read error: {e}"))?;

        let text = match msg {
            Message::Text(t) => t.to_string(),
            Message::Ping(data) => {
                let _ = write.send(Message::Pong(data)).await;
                continue;
            }
            Message::Close(_) => {
                info!("stream closed by server");
                return Ok(());
            }
            _ => continue,
        };

        // Alpaca sends arrays of messages
        let messages: Vec<StreamMessage> = match serde_json::from_str(&text) {
            Ok(m) => m,
            Err(_) => continue,
        };

        for msg in messages {
            if msg.msg_type != "b" {
                // "b" = bar, skip others (subscription confirmations, errors, etc.)
                debug!(msg_type = msg.msg_type.as_str(), "non-bar message");
                continue;
            }

            let ts = chrono::DateTime::parse_from_rfc3339(&msg.timestamp)
                .map(|dt| dt.timestamp_millis())
                .unwrap_or(0);

            if ts == 0 || msg.close <= 0.0 {
                warn!(symbol = msg.symbol.as_str(), "invalid bar from stream");
                continue;
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
                "bar received"
            );

            if tx.send(bar).await.is_err() {
                // Receiver dropped — shutdown
                return Ok(());
            }
        }
    }

    Ok(())
}
