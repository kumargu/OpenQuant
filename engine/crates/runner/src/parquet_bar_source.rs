//! Replay bar source — reads per-symbol 1-min parquets and emits
//! `StreamBar`s in chronological order, driving the [`BarDrivenClock`]
//! and [`BarDrivenSessionTrigger`] as it goes.
//!
//! Wire layout produced by [`new_replay_components`]:
//!
//! ```text
//!     ┌──────────────────────┐
//!     │ ParquetBarSource     │── bar mpsc ──→ basket_live (consumer)
//!     │  (chronological      │── clock watch ─→ BarDrivenClock
//!     │   merge of N parquets│── session mpsc ─→ BarDrivenSessionTrigger
//!     │  + closes write)     │── shared_closes write ─→ SimulatedBroker
//!     └──────────────────────┘
//! ```
//!
//! Ordering invariant the consumer relies on: every bar belonging to
//! date D arrives on the bar channel before the session-close signal
//! for D arrives on the trigger channel. The implementation enforces
//! this by:
//!
//!   1. Emitting bars in strict chronological order (min-heap merge).
//!   2. After all RTH bars for date D have been emitted, waiting until
//!      the bar channel is empty (consumer has drained it) before
//!      pushing D onto the session-close channel.
//!
//! When parquets are exhausted, both senders are dropped. The session
//! trigger then yields `None`, which is `basket_live`'s replay-exit
//! condition.

use std::collections::{BTreeMap, BinaryHeap, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::RwLock as StdRwLock;

use arrow::array::{Array, Float64Array, TimestampMicrosecondArray};
use basket_engine::PortfolioConfig;
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::bar_source::BarSource;
use crate::market_session;
use crate::replay_clock::{
    make_replay_clock_and_trigger, BarDrivenClock, BarDrivenSessionTrigger, ReplayChannels,
};
use crate::simulated_broker::{SharedCloses, SimulatedBroker};
use crate::stream::StreamBar;

/// Bundle of replay-side components, all sharing the right state.
/// Constructed by [`new_replay_components`] and passed into
/// `run_basket_live`.
pub struct ReplayComponents {
    pub bar_source: ParquetBarSource,
    pub broker: SimulatedBroker,
    pub clock: BarDrivenClock,
    pub session_trigger: BarDrivenSessionTrigger,
}

/// Bar source that reads per-symbol parquet files in `bars_dir` and
/// emits their RTH bars in chronological order.
pub struct ParquetBarSource {
    bars_dir: PathBuf,
    start: NaiveDate,
    end: NaiveDate,
    closes: SharedCloses,
    channels: StdRwLock<Option<ReplayChannels>>,
}

impl BarSource for ParquetBarSource {
    async fn start(&self, symbols: &[String]) -> mpsc::Receiver<StreamBar> {
        // Channel buffer of 1 is intentional: it guarantees that by the
        // time we've emitted all bars for a session AND advanced past
        // the next bar-send, basket_live has consumed at least up to
        // the prior bar. Combined with `select! { biased; ... }` on
        // the consumer side, this preserves the
        // "all-bars-of-D-before-session-close-of-D" invariant.
        let (tx, rx) = mpsc::channel::<StreamBar>(1);
        let channels = self
            .channels
            .write()
            .unwrap()
            .take()
            .expect("ParquetBarSource::start called twice");
        let bars_dir = self.bars_dir.clone();
        let start = self.start;
        let end = self.end;
        let closes = self.closes.clone();
        let symbols: Vec<String> = symbols.to_vec();

        tokio::spawn(async move {
            if let Err(e) = emit_loop(&bars_dir, start, end, &symbols, tx, channels, closes).await {
                warn!(error = %e, "parquet replay emitter terminated with error");
            }
        });

        rx
    }
}

/// One-shot constructor that builds every replay-side component. The
/// returned `ReplayComponents` are designed to be unpacked at the
/// `replay --engine basket` call site.
pub fn new_replay_components(
    bars_dir: PathBuf,
    start: NaiveDate,
    end: NaiveDate,
    portfolio_config: &PortfolioConfig,
    broker_config: crate::simulated_broker::SimulatedBrokerConfig,
) -> ReplayComponents {
    // Initial clock = start of the first session in the window. Updated
    // by the emitter task as bars flow.
    let initial_dt = Utc.from_utc_datetime(&start.and_hms_opt(13, 30, 0).expect("valid hms"));
    let (clock, session_trigger, channels) = make_replay_clock_and_trigger(initial_dt);

    let closes: SharedCloses = Arc::new(StdRwLock::new(HashMap::new()));
    let broker = SimulatedBroker::with_config(portfolio_config, closes.clone(), broker_config);

    let bar_source = ParquetBarSource {
        bars_dir,
        start,
        end,
        closes,
        channels: StdRwLock::new(Some(channels)),
    };

    ReplayComponents {
        bar_source,
        broker,
        clock,
        session_trigger,
    }
}

/// One row from a parquet, normalized for emission. Symbol is tracked
/// separately by the heap entry that owns the bar.
#[derive(Debug, Clone)]
struct ParquetBar {
    /// Bar-OPEN timestamp in microseconds since epoch.
    ts_us: i64,
    open: f64,
    high: f64,
    low: f64,
    close: f64,
    volume: f64,
}

impl ParquetBar {
    fn dt_open(&self) -> Option<DateTime<Utc>> {
        DateTime::from_timestamp(self.ts_us / 1_000_000, 0)
    }
}

/// Min-heap entry: orders by ts_us ASC, then by symbol for stable
/// tiebreaks. `BinaryHeap` is max-heap, so we negate ts.
///
/// Only `neg_ts_us` and `symbol` participate in ordering / equality —
/// the bar payload itself contains `f64`s and isn't `Eq`.
#[derive(Debug, Clone)]
struct HeapEntry {
    neg_ts_us: i64,
    symbol: String,
    bar: ParquetBar,
}

impl PartialEq for HeapEntry {
    fn eq(&self, other: &Self) -> bool {
        self.neg_ts_us == other.neg_ts_us && self.symbol == other.symbol
    }
}
impl Eq for HeapEntry {}

impl Ord for HeapEntry {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.neg_ts_us
            .cmp(&other.neg_ts_us)
            .then_with(|| other.symbol.cmp(&self.symbol)) // alpha ASC for ties
    }
}
impl PartialOrd for HeapEntry {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

#[allow(clippy::too_many_arguments)]
async fn emit_loop(
    bars_dir: &std::path::Path,
    start: NaiveDate,
    end: NaiveDate,
    symbols: &[String],
    bar_tx: mpsc::Sender<StreamBar>,
    mut channels: ReplayChannels,
    closes: SharedCloses,
) -> Result<(), String> {
    info!(
        bars_dir = %bars_dir.display(),
        start = %start,
        end = %end,
        symbol_count = symbols.len(),
        "replay emit loop starting"
    );

    // Load every symbol's RTH bars in [start, end] into per-symbol
    // sorted vecs. Memory cost ≈ symbols × days × 390 rows × ~64B; for
    // 50 symbols × 250 trading days that's ~1GB worst case but in
    // practice we replay ~3 months at a time so well under that.
    //
    // For very long replay windows we'd want streaming reads; that's
    // a follow-up.
    let mut per_symbol: BTreeMap<String, Vec<ParquetBar>> = BTreeMap::new();
    for symbol in symbols {
        let path = bars_dir.join(format!("{symbol}.parquet"));
        match read_symbol_bars(&path, symbol, start, end) {
            Ok(bars) if !bars.is_empty() => {
                debug!(symbol = %symbol, count = bars.len(), "loaded parquet bars");
                per_symbol.insert(symbol.clone(), bars);
            }
            Ok(_) => {
                warn!(symbol = %symbol, path = %path.display(), "no bars in window for symbol");
            }
            Err(e) => {
                warn!(symbol = %symbol, path = %path.display(), error = %e, "skipping symbol — parquet read failed");
            }
        }
    }
    if per_symbol.is_empty() {
        return Err(format!(
            "no parquet bars loaded for any of {} symbols in window {start}..={end}",
            symbols.len()
        ));
    }

    // Init heap with the first bar from each symbol. Use cursors for
    // O(N log N) merge, where N = total bars across all symbols.
    let mut cursors: HashMap<String, usize> = per_symbol.keys().map(|s| (s.clone(), 0)).collect();
    let mut heap = BinaryHeap::new();
    for (sym, bars) in &per_symbol {
        if let Some(first) = bars.first() {
            heap.push(HeapEntry {
                neg_ts_us: -first.ts_us,
                symbol: sym.clone(),
                bar: first.clone(),
            });
        }
    }

    let mut current_date: Option<NaiveDate> = None;
    let mut bars_emitted: u64 = 0;

    while let Some(entry) = heap.pop() {
        let bar = entry.bar;
        let sym = entry.symbol;

        // Advance the cursor for this symbol; push next if any.
        let cursor = cursors.get_mut(&sym).expect("cursor present");
        *cursor += 1;
        if let Some(next_bar) = per_symbol.get(&sym).and_then(|v| v.get(*cursor)) {
            heap.push(HeapEntry {
                neg_ts_us: -next_bar.ts_us,
                symbol: sym.clone(),
                bar: next_bar.clone(),
            });
        }

        let dt_open = match bar.dt_open() {
            Some(d) => d,
            None => continue,
        };
        let bar_date = market_session::trading_day_utc(dt_open);

        // If we've crossed a date boundary, signal the previous date's
        // session close, then BLOCK on the consumer's ack before
        // resuming. The ack is the gate that keeps `SharedCloses`
        // frozen at prev_date's last close while the consumer's
        // `process_session_close` fills + record_eod read prices.
        // Without this gate, every emitter advance below would
        // overwrite SharedCloses with the next day's prices and
        // races would land fills at prices the engine never saw.
        if let Some(prev_date) = current_date {
            if bar_date != prev_date {
                drain_signal_and_wait_ack(
                    &bar_tx,
                    &channels.session_tx,
                    &mut channels.done_rx,
                    prev_date,
                )
                .await;
            }
        }
        current_date = Some(bar_date);

        // Update shared closes BEFORE we send the bar, so SimulatedBroker
        // sees this price if a place_order races with bar-emit. (In
        // practice place_order happens during process_session_close,
        // long after the bar is consumed, but ordering this way costs
        // nothing.)
        if bar.close.is_finite() && bar.close > 0.0 {
            closes.write().unwrap().insert(sym.clone(), bar.close);
        }

        // Update the clock to bar-OPEN time.
        let _ = channels.clock_tx.send(dt_open);

        // Emit the bar with the same OPEN→CLOSE +60s shift the live
        // stream applies, so basket_live's `bar.timestamp - 60_000`
        // correctly recovers bar-OPEN time.
        const MINUTE_BAR_DURATION_MS: i64 = 60_000;
        let stream_bar = StreamBar {
            symbol: sym.clone(),
            timestamp: (bar.ts_us / 1_000) + MINUTE_BAR_DURATION_MS,
            close: bar.close,
            open: bar.open,
            high: bar.high,
            low: bar.low,
            volume: bar.volume,
        };
        if bar_tx.send(stream_bar).await.is_err() {
            // Consumer dropped; no point continuing.
            return Ok(());
        }
        bars_emitted += 1;
    }

    // Final session: signal close for the last date AND wait for the
    // consumer's ack so the final session's fills + record_eod
    // happen against the last day's snapshot, not a half-overwritten
    // SharedCloses.
    if let Some(last_date) = current_date {
        drain_signal_and_wait_ack(
            &bar_tx,
            &channels.session_tx,
            &mut channels.done_rx,
            last_date,
        )
        .await;
    }

    info!(bars_emitted, "replay emit loop drained");
    // Senders dropped here → channels close → basket_live exits via
    // session_trigger.next() returning None.
    Ok(())
}

/// Wait for the bar channel to drain, signal the session-close trigger
/// for `date`, then BLOCK until the consumer acks "session-close fully
/// processed."
///
/// The ack closes a race that produced ~$15 cash drift across
/// otherwise-identical replay runs (#321 investigation). Pre-fix:
/// after `session_tx.send(D)` the emitter immediately advanced and
/// overwrote `SharedCloses` with bars from D+1; the consumer's
/// `process_session_close` for D then read those D+1 prices on
/// fills. Different tokio scheduling produced different drift
/// per run.
///
/// Post-fix: the emitter blocks on `done_rx.recv()` until the
/// consumer signals (via `BarDrivenSessionTrigger::ack_session_processed`)
/// that fills + EOD valuation are complete against the day's
/// snapshot. Until then `SharedCloses` is guaranteed to hold
/// exactly D's last-RTH-bar closes.
async fn drain_signal_and_wait_ack(
    bar_tx: &mpsc::Sender<StreamBar>,
    session_tx: &mpsc::Sender<NaiveDate>,
    done_rx: &mut mpsc::Receiver<()>,
    date: NaiveDate,
) {
    while bar_tx.capacity() < bar_tx.max_capacity() {
        // Some bars are still queued. Yield to let the consumer drain.
        tokio::task::yield_now().await;
    }
    if let Err(e) = session_tx.send(date).await {
        debug!(error = %e, "session-close signal dropped (consumer gone)");
        return;
    }
    // Block on the ack. `None` means the consumer dropped its sender —
    // either Ctrl+C / panic / replay exit. Either way there's no point
    // continuing to emit bars; let the emit loop unwind.
    if done_rx.recv().await.is_none() {
        debug!("session-done ack channel closed; consumer has exited");
    }
}

fn read_symbol_bars(
    path: &std::path::Path,
    _symbol: &str,
    start: NaiveDate,
    end: NaiveDate,
) -> Result<Vec<ParquetBar>, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("open: {e}"))?;
    let builder =
        ParquetRecordBatchReaderBuilder::try_new(file).map_err(|e| format!("reader: {e}"))?;
    let reader = builder.build().map_err(|e| format!("build: {e}"))?;

    let start_us = Utc
        .from_utc_datetime(&start.and_hms_opt(0, 0, 0).expect("hms"))
        .timestamp_micros();
    let end_us = Utc
        .from_utc_datetime(&end.and_hms_opt(23, 59, 59).expect("hms"))
        .timestamp_micros();

    let mut out = Vec::new();
    for batch in reader {
        let batch = batch.map_err(|e| format!("batch: {e}"))?;
        let ts = batch
            .column(0)
            .as_any()
            .downcast_ref::<TimestampMicrosecondArray>()
            .ok_or("ts column cast")?;
        let open = batch
            .column(1)
            .as_any()
            .downcast_ref::<Float64Array>()
            .ok_or("open column cast")?;
        let high = batch
            .column(2)
            .as_any()
            .downcast_ref::<Float64Array>()
            .ok_or("high column cast")?;
        let low = batch
            .column(3)
            .as_any()
            .downcast_ref::<Float64Array>()
            .ok_or("low column cast")?;
        let close = batch
            .column(4)
            .as_any()
            .downcast_ref::<Float64Array>()
            .ok_or("close column cast")?;

        for i in 0..batch.num_rows() {
            let ts_us = ts.value(i);
            if ts_us < start_us || ts_us > end_us {
                continue;
            }
            let dt = match DateTime::from_timestamp(ts_us / 1_000_000, 0) {
                Some(d) => d,
                None => continue,
            };
            // Filter to RTH on bar-OPEN time, matching the live stream
            // gate in basket_live.
            if !market_session::is_rth_utc(dt) {
                continue;
            }
            let close_v = close.value(i);
            if !close_v.is_finite() || close_v <= 0.0 {
                continue;
            }
            out.push(ParquetBar {
                ts_us,
                open: open.value(i),
                high: high.value(i),
                low: low.value(i),
                close: close_v,
                volume: 0.0, // volume isn't used by the engine; skip Int64 cast
            });
        }
    }
    out.sort_by_key(|b| b.ts_us);
    Ok(out)
}
