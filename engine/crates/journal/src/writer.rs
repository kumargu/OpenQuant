//! Async journal writer — receives records via channel, writes to SQLite.
//!
//! Runs on the data runtime (Tokio). Never blocks the trading hot path.
//! Uses an mpsc channel: trading thread sends records, writer task batches
//! and flushes them to SQLite.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use rusqlite::Connection;
use tokio::sync::mpsc;

use openquant_core::features::FeatureValues;
use openquant_core::signals::SignalReason;

use crate::schema;

/// A complete record of what happened at one bar.
#[derive(Debug, Clone)]
pub struct BarRecord {
    pub symbol: String,
    pub timestamp: i64,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: f64,
    pub features: FeatureValues,
    pub signal_fired: bool,
    pub signal_side: Option<String>,
    pub signal_score: Option<f64>,
    pub signal_reason: Option<SignalReason>,
    pub risk_passed: Option<bool>,
    pub risk_rejection: Option<String>,
    pub qty_approved: Option<f64>,
    pub engine_version: String,
}

/// A fill event to journal.
#[derive(Debug, Clone)]
pub struct FillRecord {
    pub symbol: String,
    pub side: String,
    pub qty: f64,
    pub fill_price: f64,
    pub slippage: f64,
    pub engine_version: String,
}

/// Messages sent to the journal writer task.
#[derive(Debug, Clone)]
pub enum JournalMessage {
    Bar(BarRecord),
    Fill(FillRecord),
    Flush,
    Shutdown,
}

/// Per-symbol drop counters for journal backpressure isolation.
#[derive(Clone, Default)]
pub struct DropCounters {
    total: Arc<AtomicU64>,
    per_symbol: Arc<std::sync::Mutex<HashMap<String, u64>>>,
}

impl DropCounters {
    fn record_drop(&self, symbol: &str) {
        self.total.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut map) = self.per_symbol.lock() {
            *map.entry(symbol.to_string()).or_insert(0) += 1;
        }
    }

    /// Total drops across all symbols.
    pub fn total(&self) -> u64 {
        self.total.load(Ordering::Relaxed)
    }

    /// Per-symbol drop counts (for diagnosing which symbol is causing backpressure).
    pub fn per_symbol(&self) -> HashMap<String, u64> {
        self.per_symbol
            .lock()
            .map(|m| m.clone())
            .unwrap_or_default()
    }
}

/// Cached metric handles for journal channel health.
#[derive(Clone)]
struct JournalMetrics {
    drops_bar: metrics::Counter,
    drops_fill: metrics::Counter,
    channel_pending: metrics::Gauge,
    buffer_size: usize,
}

impl JournalMetrics {
    fn new(buffer_size: usize) -> Self {
        let m = Self {
            drops_bar: metrics::counter!("journal.channel.drops.bar"),
            drops_fill: metrics::counter!("journal.channel.drops.fill"),
            channel_pending: metrics::gauge!("journal.channel.pending"),
            buffer_size,
        };
        // Record static capacity for dashboards
        metrics::gauge!("journal.channel.capacity").set(buffer_size as f64);
        m
    }

    /// Update pending count from sender's remaining capacity.
    fn record_pending(&self, sender_capacity: usize) {
        let pending = self.buffer_size.saturating_sub(sender_capacity);
        self.channel_pending.set(pending as f64);
    }
}

/// Handle for sending records to the journal writer.
#[derive(Clone)]
pub struct JournalHandle {
    tx: mpsc::Sender<JournalMessage>,
    drops: DropCounters,
    metrics: JournalMetrics,
}

impl JournalHandle {
    /// Send a bar record to the journal (non-blocking).
    /// Tracks drops per-symbol so one symbol's backpressure is visible.
    pub fn log_bar(&self, record: BarRecord) {
        let symbol = record.symbol.clone();
        self.metrics.record_pending(self.tx.capacity());
        if let Err(mpsc::error::TrySendError::Full(_)) =
            self.tx.try_send(JournalMessage::Bar(record))
        {
            self.drops.record_drop(&symbol);
            self.metrics.drops_bar.increment(1);
            let count = self.drops.total();
            if count % 100 == 1 {
                eprintln!(
                    "[journal] WARNING: channel full, dropped {count} records total (symbol: {symbol})"
                );
            }
        }
    }

    /// Send a fill record to the journal (non-blocking).
    /// Fills are critical — warns on every drop.
    pub fn log_fill(&self, record: FillRecord) {
        let symbol = record.symbol.clone();
        self.metrics.record_pending(self.tx.capacity());
        if let Err(mpsc::error::TrySendError::Full(_)) =
            self.tx.try_send(JournalMessage::Fill(record))
        {
            self.drops.record_drop(&symbol);
            self.metrics.drops_fill.increment(1);
            let count = self.drops.total();
            eprintln!(
                "[journal] WARNING: fill record dropped! {count} total drops (symbol: {symbol})"
            );
        }
    }

    /// Request a flush of pending writes.
    pub fn flush(&self) {
        let _ = self.tx.try_send(JournalMessage::Flush);
    }

    /// Number of records dropped due to channel backpressure.
    pub fn dropped_count(&self) -> u64 {
        self.drops.total()
    }

    /// Per-symbol drop counts for diagnosing backpressure.
    pub fn dropped_per_symbol(&self) -> HashMap<String, u64> {
        self.drops.per_symbol()
    }

    /// Shut down the journal writer.
    pub async fn shutdown(self) {
        let _ = self.tx.send(JournalMessage::Shutdown).await;
    }
}

/// Start the journal writer task. Returns a handle for sending records
/// and a `JoinHandle` to await writer completion during shutdown.
///
/// The writer batches incoming records and writes them in transactions
/// for efficiency. It runs on the Tokio data runtime.
pub fn start(db_path: &Path, buffer_size: usize) -> (JournalHandle, tokio::task::JoinHandle<()>) {
    let (tx, rx) = mpsc::channel(buffer_size);
    let db_path = db_path.to_path_buf();

    let join_handle = tokio::spawn(async move {
        writer_loop(rx, &db_path).await;
    });

    let handle = JournalHandle {
        tx,
        drops: DropCounters::default(),
        metrics: JournalMetrics::new(buffer_size),
    };

    (handle, join_handle)
}

async fn writer_loop(mut rx: mpsc::Receiver<JournalMessage>, db_path: &Path) {
    let conn = Connection::open(db_path).expect("failed to open journal database");
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")
        .expect("failed to set journal pragmas");
    schema::init(&conn).expect("failed to init journal schema");

    // Writer-side metrics (separate from the channel-side handles)
    let flush_duration = metrics::histogram!("journal.flush.duration_ns");
    let flush_batch_size = metrics::histogram!("journal.flush.batch_size");
    let flush_count = metrics::counter!("journal.flush.count");

    let mut batch: Vec<JournalMessage> = Vec::with_capacity(64);

    loop {
        // Wait for first message
        match rx.recv().await {
            Some(JournalMessage::Shutdown) | None => break,
            Some(msg) => batch.push(msg),
        }

        // Drain any additional pending messages (non-blocking)
        while let Ok(msg) = rx.try_recv() {
            match msg {
                JournalMessage::Shutdown => {
                    flush_batch(&conn, &batch);
                    return;
                }
                other => batch.push(other),
            }
        }

        let start = std::time::Instant::now();
        flush_batch(&conn, &batch);
        flush_duration.record(start.elapsed().as_nanos() as f64);
        flush_batch_size.record(batch.len() as f64);
        flush_count.increment(1);
        batch.clear();
    }
}

fn flush_batch(conn: &Connection, batch: &[JournalMessage]) {
    if batch.is_empty() {
        return;
    }

    let tx = conn
        .unchecked_transaction()
        .expect("failed to start transaction");

    for msg in batch {
        match msg {
            JournalMessage::Bar(rec) => write_bar_record(conn, rec),
            JournalMessage::Fill(rec) => write_fill_record(conn, rec),
            JournalMessage::Flush => {} // just triggers the batch write
            JournalMessage::Shutdown => {} // handled above
        }
    }

    tx.commit().expect("failed to commit journal batch");
}

fn write_bar_record(conn: &Connection, rec: &BarRecord) {
    // Insert bar
    conn.execute(
        "INSERT INTO bars (symbol, timestamp, open, high, low, close, volume)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        rusqlite::params![
            rec.symbol,
            rec.timestamp,
            rec.open,
            rec.high,
            rec.low,
            rec.close,
            rec.volume
        ],
    )
    .expect("failed to insert bar");

    let bar_id = conn.last_insert_rowid();

    // Insert features (V1 + V2)
    conn.execute(
        "INSERT INTO features (bar_id, return_1, return_5, return_20, sma_20, sma_50, atr,
         return_std_20, return_z_score, relative_volume, bar_range, close_location, trend_up, warmed_up,
         ema_fast, ema_slow, ema_fast_above_slow, adx, plus_di, minus_di,
         bollinger_upper, bollinger_lower, bollinger_pct_b, bollinger_bandwidth)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                 ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24)",
        rusqlite::params![
            bar_id,
            rec.features.return_1,
            rec.features.return_5,
            rec.features.return_20,
            rec.features.sma_20,
            rec.features.sma_50,
            rec.features.atr,
            rec.features.return_std_20,
            rec.features.return_z_score,
            rec.features.relative_volume,
            rec.features.bar_range,
            rec.features.close_location,
            rec.features.trend_up as i32,
            rec.features.warmed_up as i32,
            rec.features.ema_fast,
            rec.features.ema_slow,
            rec.features.ema_fast_above_slow as i32,
            rec.features.adx,
            rec.features.plus_di,
            rec.features.minus_di,
            rec.features.bollinger_upper,
            rec.features.bollinger_lower,
            rec.features.bollinger_pct_b,
            rec.features.bollinger_bandwidth,
        ],
    )
    .expect("failed to insert features");

    // Insert decision
    conn.execute(
        "INSERT INTO decisions (bar_id, signal_fired, signal_side, signal_score,
         signal_reason, risk_passed, risk_rejection, qty_approved, engine_version)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        rusqlite::params![
            bar_id,
            rec.signal_fired as i32,
            rec.signal_side,
            rec.signal_score,
            rec.signal_reason.as_ref().map(|r| r.describe()),
            rec.risk_passed.map(|b| b as i32),
            rec.risk_rejection,
            rec.qty_approved,
            rec.engine_version,
        ],
    )
    .expect("failed to insert decision");
}

fn write_fill_record(conn: &Connection, rec: &FillRecord) {
    conn.execute(
        "INSERT INTO fills (symbol, side, qty, fill_price, slippage, engine_version)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        rusqlite::params![
            rec.symbol,
            rec.side,
            rec.qty,
            rec.fill_price,
            rec.slippage,
            rec.engine_version
        ],
    )
    .expect("failed to insert fill");
}

#[cfg(test)]
mod tests {
    use super::*;
    use openquant_core::features::FeatureValues;

    fn test_bar_record() -> BarRecord {
        BarRecord {
            symbol: "BTCUSD".to_string(),
            timestamp: 1700000000000,
            open: 100.0,
            high: 101.0,
            low: 99.0,
            close: 100.5,
            volume: 1000.0,
            features: FeatureValues {
                return_1: 0.005,
                return_5: 0.02,
                return_20: 0.05,
                sma_20: 100.0,
                sma_50: 99.5,
                atr: 1.5,
                return_std_20: 0.01,
                return_z_score: 0.5,
                relative_volume: 1.2,
                bar_range: 2.0,
                close_location: 0.75,
                trend_up: true,
                warmed_up: true,
                ..Default::default()
            },
            signal_fired: false,
            signal_side: None,
            signal_score: None,
            signal_reason: None,
            risk_passed: None,
            risk_rejection: None,
            qty_approved: None,
            engine_version: "test".to_string(),
        }
    }

    #[test]
    fn write_and_read_bar_record() {
        let conn = Connection::open_in_memory().unwrap();
        schema::init(&conn).unwrap();

        let rec = test_bar_record();
        write_bar_record(&conn, &rec);

        // Verify bar was written
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM bars", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);

        // Verify features were written
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM features", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);

        // Verify decision was written
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM decisions", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);

        // Verify feature values
        let z_score: f64 = conn
            .query_row(
                "SELECT return_z_score FROM features WHERE bar_id = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!((z_score - 0.5).abs() < 1e-10);
    }

    #[test]
    fn write_bar_with_signal() {
        let conn = Connection::open_in_memory().unwrap();
        schema::init(&conn).unwrap();

        let mut rec = test_bar_record();
        rec.signal_fired = true;
        rec.signal_side = Some("buy".to_string());
        rec.signal_score = Some(1.5);
        rec.signal_reason = Some(SignalReason::MeanReversionBuy);
        rec.risk_passed = Some(true);
        rec.qty_approved = Some(10.0);

        write_bar_record(&conn, &rec);

        let fired: i32 = conn
            .query_row(
                "SELECT signal_fired FROM decisions WHERE bar_id = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(fired, 1);

        let reason: String = conn
            .query_row(
                "SELECT signal_reason FROM decisions WHERE bar_id = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(reason, "mean-reversion buy: oversold + volume confirmation");
    }

    #[test]
    fn write_fill_record_test() {
        let conn = Connection::open_in_memory().unwrap();
        schema::init(&conn).unwrap();

        let rec = FillRecord {
            symbol: "BTCUSD".to_string(),
            side: "buy".to_string(),
            qty: 0.5,
            fill_price: 42000.0,
            slippage: 5.0,
            engine_version: "test".to_string(),
        };

        write_fill_record(&conn, &rec);

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM fills", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn journal_handle_roundtrip() {
        let tmp = std::env::temp_dir().join("test_journal.db");
        let _ = std::fs::remove_file(&tmp);

        let (handle, writer_task) = start(&tmp, 64);

        handle.log_bar(test_bar_record());
        handle.log_fill(FillRecord {
            symbol: "BTCUSD".to_string(),
            side: "buy".to_string(),
            qty: 1.0,
            fill_price: 100.0,
            slippage: 0.0,
            engine_version: "test".to_string(),
        });

        handle.shutdown().await;
        // Wait for writer to finish flushing all queued records
        let _ = writer_task.await;

        // Verify data was written
        let conn = Connection::open(&tmp).unwrap();
        let bar_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM bars", [], |row| row.get(0))
            .unwrap();
        assert_eq!(bar_count, 1);

        let fill_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM fills", [], |row| row.get(0))
            .unwrap();
        assert_eq!(fill_count, 1);

        let _ = std::fs::remove_file(&tmp);
    }
}
