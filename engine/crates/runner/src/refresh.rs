//! Refresh quant-data parquets to a target date.
//!
//! Called at runner startup before warmup. Per symbol, the rule is dead simple:
//!
//! 1. Find the latest fulfilled date `F` from `fulfilled.json`.
//! 2. Fetch `[F+1, today+1)` from Alpaca in one call.
//! 3. Drop any existing parquet rows in `[F+1, today)` — they're partial/stale.
//! 4. Append Alpaca's bars (deduped against today's existing bars from the
//!    websocket).
//! 5. Mark every weekday in `[F+1, today)` fulfilled. Today is never marked
//!    (session may still be in progress); it'll be marked tomorrow.
//!
//! Zero-bar weekdays inside the range still get marked fulfilled — Alpaca was
//! asked, returned nothing, that's authoritative (holiday).
//!
//! State lives in `<bars_dir>/fulfilled.json`. On first run (empty file), the
//! bootstrap step trusts the existing parquet's date range as fulfilled so we
//! don't refetch the whole 90-day window.

use arrow::array::{Array, ArrayRef, Float64Array, Int64Array, TimestampMicrosecondArray};
use arrow::datatypes::{DataType, Field, Schema, TimeUnit};
use arrow::record_batch::RecordBatch;
use chrono::{Datelike, NaiveDate, Utc, Weekday};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::arrow::ArrowWriter;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tracing::{info, warn};

/// Column tuple: (timestamp_us, open, high, low, close, volume, trade_count, vwap).
type BarColumns = (
    Vec<i64>,
    Vec<f64>,
    Vec<f64>,
    Vec<f64>,
    Vec<f64>,
    Vec<i64>,
    Vec<i64>,
    Vec<f64>,
);

use crate::alpaca::{AlpacaBar, AlpacaClient};

/// RTH session: 13:30–20:00 UTC (same as quant-lab and mock_alpaca.py).
const RTH_START_MINUTES: i64 = 13 * 60 + 30;
const RTH_END_MINUTES: i64 = 20 * 60;

/// Number of trading days to check for fulfillment.
const LOOKBACK_DAYS: i64 = 90;

/// Filename for the fulfilled-dates cache at the bars_dir root.
const FULFILLED_FILE: &str = "fulfilled.json";

/// Schema matching build_foundation_dataset.py output.
fn bar_schema() -> Schema {
    Schema::new(vec![
        Field::new(
            "timestamp",
            DataType::Timestamp(TimeUnit::Microsecond, Some("UTC".into())),
            false,
        ),
        Field::new("open", DataType::Float64, false),
        Field::new("high", DataType::Float64, false),
        Field::new("low", DataType::Float64, false),
        Field::new("close", DataType::Float64, false),
        Field::new("volume", DataType::Int64, false),
        Field::new("trade_count", DataType::Int64, false),
        Field::new("vwap", DataType::Float64, false),
    ])
}

/// Per-symbol fulfilled-date tracking. Persisted at bars_dir/fulfilled.json.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Fulfilled {
    /// Map of symbol → set of ISO dates (YYYY-MM-DD) fulfilled from Alpaca.
    pub by_symbol: BTreeMap<String, HashSet<String>>,
    pub last_updated: String,
}

impl Fulfilled {
    fn get(&self, symbol: &str) -> Option<&HashSet<String>> {
        self.by_symbol.get(symbol)
    }

    fn insert(&mut self, symbol: &str, date: String) {
        self.by_symbol
            .entry(symbol.to_string())
            .or_default()
            .insert(date);
    }
}

/// Load fulfilled-date state from bars_dir/fulfilled.json.
/// Returns empty state if missing or malformed.
fn load_fulfilled(bars_dir: &Path) -> Fulfilled {
    let path = bars_dir.join(FULFILLED_FILE);
    match std::fs::read_to_string(&path) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
        Err(_) => Fulfilled::default(),
    }
}

/// Persist fulfilled-date state to bars_dir/fulfilled.json. Atomic via tmp+rename.
fn save_fulfilled(bars_dir: &Path, fulfilled: &Fulfilled) -> Result<(), String> {
    let path = bars_dir.join(FULFILLED_FILE);
    let tmp = bars_dir.join(format!("{FULFILLED_FILE}.tmp"));
    let s = serde_json::to_string_pretty(fulfilled).map_err(|e| format!("serialize: {e}"))?;
    std::fs::write(&tmp, s).map_err(|e| format!("write tmp: {e}"))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("rename: {e}"))
}

/// True if the date is Mon–Fri.
fn is_weekday(date: NaiveDate) -> bool {
    !matches!(date.weekday(), Weekday::Sat | Weekday::Sun)
}

/// Read a parquet file, return timestamps only.
#[cfg(test)]
fn read_timestamps(path: &Path) -> Result<Vec<i64>, String> {
    let (ts, _, _, _, _, _, _, _) = read_full_parquet(path)?;
    Ok(ts)
}

/// Read full parquet into columns for manipulation.
fn read_full_parquet(path: &Path) -> Result<BarColumns, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("open: {e}"))?;
    let builder =
        ParquetRecordBatchReaderBuilder::try_new(file).map_err(|e| format!("reader: {e}"))?;
    let reader = builder.build().map_err(|e| format!("build: {e}"))?;

    let mut ts = Vec::new();
    let mut open = Vec::new();
    let mut high = Vec::new();
    let mut low = Vec::new();
    let mut close = Vec::new();
    let mut volume = Vec::new();
    let mut trade_count = Vec::new();
    let mut vwap = Vec::new();

    for batch in reader {
        let batch = batch.map_err(|e| format!("batch: {e}"))?;
        let ts_arr = batch
            .column(0)
            .as_any()
            .downcast_ref::<TimestampMicrosecondArray>()
            .unwrap();
        let o_arr = batch
            .column(1)
            .as_any()
            .downcast_ref::<Float64Array>()
            .unwrap();
        let h_arr = batch
            .column(2)
            .as_any()
            .downcast_ref::<Float64Array>()
            .unwrap();
        let l_arr = batch
            .column(3)
            .as_any()
            .downcast_ref::<Float64Array>()
            .unwrap();
        let c_arr = batch
            .column(4)
            .as_any()
            .downcast_ref::<Float64Array>()
            .unwrap();
        let v_arr = batch
            .column(5)
            .as_any()
            .downcast_ref::<Int64Array>()
            .unwrap();
        let tc_arr = batch
            .column(6)
            .as_any()
            .downcast_ref::<Int64Array>()
            .unwrap();
        let vw_arr = batch
            .column(7)
            .as_any()
            .downcast_ref::<Float64Array>()
            .unwrap();

        for i in 0..batch.num_rows() {
            ts.push(ts_arr.value(i));
            open.push(o_arr.value(i));
            high.push(h_arr.value(i));
            low.push(l_arr.value(i));
            close.push(c_arr.value(i));
            volume.push(v_arr.value(i));
            trade_count.push(tc_arr.value(i));
            vwap.push(vw_arr.value(i));
        }
    }

    Ok((ts, open, high, low, close, volume, trade_count, vwap))
}

/// Write columns to a parquet file (atomic: tmp + rename).
fn write_parquet(path: &Path, cols: &BarColumns) -> Result<(), String> {
    let (ts, open, high, low, close, volume, trade_count, vwap) = cols;
    let schema = Arc::new(bar_schema());

    let ts_arr: ArrayRef =
        Arc::new(TimestampMicrosecondArray::from(ts.to_vec()).with_timezone("UTC"));
    let o_arr: ArrayRef = Arc::new(Float64Array::from(open.to_vec()));
    let h_arr: ArrayRef = Arc::new(Float64Array::from(high.to_vec()));
    let l_arr: ArrayRef = Arc::new(Float64Array::from(low.to_vec()));
    let c_arr: ArrayRef = Arc::new(Float64Array::from(close.to_vec()));
    let v_arr: ArrayRef = Arc::new(Int64Array::from(volume.to_vec()));
    let tc_arr: ArrayRef = Arc::new(Int64Array::from(trade_count.to_vec()));
    let vw_arr: ArrayRef = Arc::new(Float64Array::from(vwap.to_vec()));

    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![ts_arr, o_arr, h_arr, l_arr, c_arr, v_arr, tc_arr, vw_arr],
    )
    .map_err(|e| format!("batch: {e}"))?;

    let tmp_path = path.with_extension("parquet.tmp");
    let file = std::fs::File::create(&tmp_path).map_err(|e| format!("create tmp: {e}"))?;

    let props = parquet::file::properties::WriterProperties::builder()
        .set_compression(parquet::basic::Compression::SNAPPY)
        .build();
    let mut writer =
        ArrowWriter::try_new(file, schema, Some(props)).map_err(|e| format!("writer: {e}"))?;
    writer.write(&batch).map_err(|e| format!("write: {e}"))?;
    writer.close().map_err(|e| format!("close: {e}"))?;

    std::fs::rename(&tmp_path, path).map_err(|e| format!("rename: {e}"))?;
    Ok(())
}

fn micros_to_hm(us: i64) -> (i64, i64) {
    let secs = us / 1_000_000;
    let h = (secs % 86400) / 3600;
    let m = (secs % 3600) / 60;
    (h, m)
}

fn micros_to_date(us: i64) -> String {
    let secs = us / 1_000_000;
    let dt = chrono::DateTime::from_timestamp(secs, 0).unwrap_or(chrono::DateTime::UNIX_EPOCH);
    dt.format("%Y-%m-%d").to_string()
}

/// Count RTH bars per date from a timestamp slice.
fn rth_bars_by_date(timestamps: &[i64]) -> HashMap<String, usize> {
    let mut by_date: HashMap<String, usize> = HashMap::new();
    for &ts in timestamps {
        let (h, m) = micros_to_hm(ts);
        let minutes = h * 60 + m;
        if !(RTH_START_MINUTES..RTH_END_MINUTES).contains(&minutes) {
            continue;
        }
        *by_date.entry(micros_to_date(ts)).or_insert(0) += 1;
    }
    by_date
}

/// Bootstrap: if a symbol has no fulfilled entries yet, trust the existing
/// parquet as a prior-best-effort sync. Every weekday in [earliest_parquet_date,
/// latest_parquet_date] within the lookback window is marked fulfilled — even
/// zero-bar days, since they're presumed to be holidays already handled when
/// the parquet was produced.
///
/// Weekdays AFTER the parquet's latest_date stay unfulfilled so the next run
/// fetches them.
fn bootstrap_fulfilled(
    symbol: &str,
    timestamps: &[i64],
    fulfilled: &mut Fulfilled,
    today: NaiveDate,
) {
    if fulfilled.get(symbol).map(|s| !s.is_empty()).unwrap_or(false) {
        return;
    }
    if timestamps.is_empty() {
        return;
    }
    let by_date = rth_bars_by_date(timestamps);
    let dates: Vec<&String> = by_date.keys().collect();
    let earliest = dates.iter().min().map(|s| s.as_str()).unwrap_or("");
    let latest = dates.iter().max().map(|s| s.as_str()).unwrap_or("");
    let earliest_d = match NaiveDate::parse_from_str(earliest, "%Y-%m-%d") {
        Ok(d) => d,
        Err(_) => return,
    };
    let latest_d = match NaiveDate::parse_from_str(latest, "%Y-%m-%d") {
        Ok(d) => d,
        Err(_) => return,
    };
    let cutoff = today - chrono::Duration::days(LOOKBACK_DAYS);
    let start = earliest_d.max(cutoff);
    let end = latest_d.min(today - chrono::Duration::days(1));

    let mut d = start;
    let mut added = 0;
    while d <= end {
        if is_weekday(d) {
            fulfilled.insert(symbol, d.format("%Y-%m-%d").to_string());
            added += 1;
        }
        d += chrono::Duration::days(1);
    }
    if added > 0 {
        info!(
            symbol,
            bootstrapped = added,
            from = %start,
            to = %end,
            "bootstrapped fulfilled dates from existing parquet"
        );
    }
}

/// Find the latest fulfilled date for a symbol, or None if it has none.
fn latest_fulfilled(symbol: &str, fulfilled: &Fulfilled) -> Option<NaiveDate> {
    fulfilled
        .get(symbol)?
        .iter()
        .filter_map(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
        .max()
}

/// Convert Alpaca bars to column vectors. Silently skips bars with
/// unparseable timestamps (logged as warning).
fn alpaca_bars_to_columns(bars: &[AlpacaBar]) -> BarColumns {
    let mut ts = Vec::with_capacity(bars.len());
    let mut open = Vec::with_capacity(bars.len());
    let mut high = Vec::with_capacity(bars.len());
    let mut low = Vec::with_capacity(bars.len());
    let mut close = Vec::with_capacity(bars.len());
    let mut volume = Vec::with_capacity(bars.len());
    let mut trade_count = Vec::with_capacity(bars.len());
    let mut vwap = Vec::with_capacity(bars.len());

    for bar in bars {
        match chrono::DateTime::parse_from_rfc3339(&bar.t) {
            Ok(dt) => {
                ts.push(dt.timestamp_millis() * 1000);
                open.push(bar.o);
                high.push(bar.h);
                low.push(bar.l);
                close.push(bar.c);
                volume.push(bar.v as i64);
                trade_count.push(bar.n.unwrap_or(0));
                vwap.push(bar.vw.unwrap_or(0.0));
            }
            Err(e) => {
                warn!(timestamp = bar.t.as_str(), error = %e, "skipping bar with invalid timestamp");
            }
        }
    }

    (ts, open, high, low, close, volume, trade_count, vwap)
}

/// Refresh one symbol's parquet to today using the simple latest-fulfilled rule:
/// fetch `[F+1, today+1)` from Alpaca, replace any existing rows in `[F+1, today)`
/// with Alpaca's copy, dedupe-merge today's bars (websocket may have written
/// some), then mark every weekday in `[F+1, today)` fulfilled.
pub async fn refresh_symbol(
    bars_dir: &Path,
    symbol: &str,
    target_date: &str,
    client: &AlpacaClient,
    fulfilled: &mut Fulfilled,
) -> Result<usize, String> {
    let path = bars_dir.join(format!("{symbol}.parquet"));
    if !path.exists() {
        return Err(format!("{symbol}.parquet not found"));
    }
    let today = NaiveDate::parse_from_str(target_date, "%Y-%m-%d")
        .map_err(|e| format!("target_date: {e}"))?;

    let (mut ts, mut o, mut h, mut l, mut c, mut v, mut tc, mut vw) = read_full_parquet(&path)?;
    if ts.is_empty() {
        return Err(format!("{symbol}: empty parquet"));
    }

    // Bootstrap on first run: trust existing parquet data.
    bootstrap_fulfilled(symbol, &ts, fulfilled, today);

    // F = latest fulfilled date. Fall back to a 90-day-ago floor if none exists
    // (fresh symbol with no parquet history we trust).
    let f = latest_fulfilled(symbol, fulfilled)
        .unwrap_or(today - chrono::Duration::days(LOOKBACK_DAYS));
    let fetch_from = f + chrono::Duration::days(1);

    if fetch_from > today {
        info!(symbol, latest_fulfilled = %f, "already up to date");
        return Ok(0);
    }

    let fetch_from_str = fetch_from.format("%Y-%m-%d").to_string();
    let fetch_to_str = (today + chrono::Duration::days(1))
        .format("%Y-%m-%d")
        .to_string();

    // Drop any existing rows in [fetch_from, today). Today's bars survive — they
    // may have come from the websocket and will be deduped against Alpaca below.
    let drop_from_us = fetch_from
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_utc()
        .timestamp_micros();
    let today_us = today
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_utc()
        .timestamp_micros();

    let keep_idx: Vec<usize> = (0..ts.len())
        .filter(|&i| ts[i] < drop_from_us || ts[i] >= today_us)
        .collect();
    if keep_idx.len() < ts.len() {
        ts = keep_idx.iter().map(|&i| ts[i]).collect();
        o = keep_idx.iter().map(|&i| o[i]).collect();
        h = keep_idx.iter().map(|&i| h[i]).collect();
        l = keep_idx.iter().map(|&i| l[i]).collect();
        c = keep_idx.iter().map(|&i| c[i]).collect();
        v = keep_idx.iter().map(|&i| v[i]).collect();
        tc = keep_idx.iter().map(|&i| tc[i]).collect();
        vw = keep_idx.iter().map(|&i| vw[i]).collect();
    }

    // Fetch [fetch_from, today+1) from Alpaca in one call.
    let raw = client
        .fetch_minute_bars_raw(&[symbol.to_string()], &fetch_from_str, &fetch_to_str)
        .await?;
    let bars = raw.get(symbol).cloned().unwrap_or_default();
    let (new_ts, new_o, new_h, new_l, new_c, new_v, new_tc, new_vw) = alpaca_bars_to_columns(&bars);

    // Dedupe-merge: only today's existing bars can collide.
    let existing_set: HashSet<i64> = ts.iter().cloned().collect();
    let mut added = 0;
    for i in 0..new_ts.len() {
        if !existing_set.contains(&new_ts[i]) {
            ts.push(new_ts[i]);
            o.push(new_o[i]);
            h.push(new_h[i]);
            l.push(new_l[i]);
            c.push(new_c[i]);
            v.push(new_v[i]);
            tc.push(new_tc[i]);
            vw.push(new_vw[i]);
            added += 1;
        }
    }

    if ts.is_empty() {
        return Err(format!("{symbol}: empty after refresh"));
    }

    // Sort by timestamp.
    let mut indices: Vec<usize> = (0..ts.len()).collect();
    indices.sort_by_key(|&i| ts[i]);
    let ts_s: Vec<_> = indices.iter().map(|&i| ts[i]).collect();
    let o_s: Vec<_> = indices.iter().map(|&i| o[i]).collect();
    let h_s: Vec<_> = indices.iter().map(|&i| h[i]).collect();
    let l_s: Vec<_> = indices.iter().map(|&i| l[i]).collect();
    let c_s: Vec<_> = indices.iter().map(|&i| c[i]).collect();
    let v_s: Vec<_> = indices.iter().map(|&i| v[i]).collect();
    let tc_s: Vec<_> = indices.iter().map(|&i| tc[i]).collect();
    let vw_s: Vec<_> = indices.iter().map(|&i| vw[i]).collect();

    let new_latest = micros_to_date(*ts_s.last().unwrap());
    write_parquet(&path, &(ts_s, o_s, h_s, l_s, c_s, v_s, tc_s, vw_s))?;

    // Mark every weekday in [fetch_from, today) as fulfilled. Today stays
    // unfulfilled — it'll be marked by tomorrow's run.
    let mut d = fetch_from;
    let mut marked = 0;
    while d < today {
        if is_weekday(d) {
            fulfilled.insert(symbol, d.format("%Y-%m-%d").to_string());
            marked += 1;
        }
        d += chrono::Duration::days(1);
    }

    if added == 0 && marked == 0 {
        info!(symbol, latest = new_latest.as_str(), "no new data");
    } else {
        info!(
            symbol,
            added,
            marked_fulfilled = marked,
            from = fetch_from_str.as_str(),
            latest = new_latest.as_str(),
            "refreshed parquet"
        );
    }
    Ok(added)
}

/// Refresh all symbols in bars_dir to target_date.
pub async fn refresh_all(
    bars_dir: &Path,
    target_date: &str,
    client: &AlpacaClient,
) -> Result<usize, String> {
    let mut symbols: Vec<String> = Vec::new();
    for entry in std::fs::read_dir(bars_dir).map_err(|e| format!("read dir: {e}"))? {
        let entry = entry.map_err(|e| format!("entry: {e}"))?;
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "parquet") {
            if let Some(stem) = path.file_stem() {
                symbols.push(stem.to_string_lossy().to_string());
            }
        }
    }
    symbols.sort();

    info!(
        symbols = symbols.len(),
        target = target_date,
        "refreshing quant-data parquets"
    );

    let mut fulfilled = load_fulfilled(bars_dir);
    let mut total = 0;
    let mut refreshed = 0;
    let mut errors = 0;

    for (i, sym) in symbols.iter().enumerate() {
        match refresh_symbol(bars_dir, sym, target_date, client, &mut fulfilled).await {
            Ok(0) => {}
            Ok(n) => {
                total += n;
                refreshed += 1;
            }
            Err(e) => {
                warn!(symbol = sym.as_str(), error = e.as_str(), "refresh failed");
                errors += 1;
            }
        }
        if (i + 1) % 50 == 0 {
            info!(
                progress = format!("{}/{}", i + 1, symbols.len()).as_str(),
                "refresh progress"
            );
            // Periodically persist fulfilled state in case of crash
            fulfilled.last_updated = Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
            if let Err(e) = save_fulfilled(bars_dir, &fulfilled) {
                warn!(error = e.as_str(), "failed to persist fulfilled.json mid-run");
            }
        }
    }

    fulfilled.last_updated = Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    save_fulfilled(bars_dir, &fulfilled)?;

    info!(refreshed, total_bars = total, errors, "refresh complete");
    Ok(total)
}

/// Resolve the quant-data bars directory. Override with `QUANT_DATA_BARS_DIR`.
pub fn default_bars_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("QUANT_DATA_BARS_DIR") {
        return PathBuf::from(dir);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join("quant-data/bars/v3_sp500_2024-2026_1min_adjusted")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write_test_parquet(path: &Path, bars: &[(i64, f64)]) {
        let ts: Vec<i64> = bars.iter().map(|(t, _)| *t).collect();
        let close: Vec<f64> = bars.iter().map(|(_, c)| *c).collect();
        let open = close.clone();
        let high = close.clone();
        let low = close.clone();
        let volume: Vec<i64> = vec![100; bars.len()];
        let trade_count: Vec<i64> = vec![10; bars.len()];
        let vwap = close.clone();

        write_parquet(
            path,
            &(ts, open, high, low, close, volume, trade_count, vwap),
        )
        .unwrap();
    }

    fn rth_timestamps(date: &str, n_minutes: usize) -> Vec<(i64, f64)> {
        let d = NaiveDate::parse_from_str(date, "%Y-%m-%d").unwrap();
        let start = d.and_hms_opt(13, 30, 0).unwrap().and_utc();
        (0..n_minutes)
            .map(|i| {
                let ts = start + chrono::Duration::minutes(i as i64);
                (ts.timestamp() * 1_000_000, 100.0 + i as f64 * 0.01)
            })
            .collect()
    }

    #[test]
    fn test_read_write_roundtrip() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("TEST.parquet");
        let bars = rth_timestamps("2026-04-07", 390);
        write_test_parquet(&path, &bars);
        let timestamps = read_timestamps(&path).unwrap();
        assert_eq!(timestamps.len(), 390);
    }

    #[test]
    fn test_bootstrap_marks_existing_data() {
        // Existing parquet has Apr 8-10 data. Bootstrap should mark those 3
        // weekdays (and only those) as fulfilled.
        let today = NaiveDate::parse_from_str("2026-04-13", "%Y-%m-%d").unwrap();
        let mut bars = Vec::new();
        for date in &["2026-04-08", "2026-04-09", "2026-04-10"] {
            bars.extend(rth_timestamps(date, 50));
        }
        let ts: Vec<i64> = bars.iter().map(|(t, _)| *t).collect();

        let mut fulfilled = Fulfilled::default();
        bootstrap_fulfilled("TEST", &ts, &mut fulfilled, today);

        let set = fulfilled.get("TEST").unwrap();
        assert!(set.contains("2026-04-08"));
        assert!(set.contains("2026-04-09"));
        assert!(set.contains("2026-04-10"));
    }

    #[test]
    fn test_bootstrap_skips_zero_bar_days() {
        // Apr 8 has data; Apr 9 is zero bars (holiday candidate). Bootstrap
        // marks only Apr 8. Apr 9 stays unfulfilled.
        let today = NaiveDate::parse_from_str("2026-04-13", "%Y-%m-%d").unwrap();
        let bars = rth_timestamps("2026-04-08", 50);
        let ts: Vec<i64> = bars.iter().map(|(t, _)| *t).collect();

        let mut fulfilled = Fulfilled::default();
        bootstrap_fulfilled("TEST", &ts, &mut fulfilled, today);

        let set = fulfilled.get("TEST").unwrap();
        assert!(set.contains("2026-04-08"));
        assert!(!set.contains("2026-04-09"));
    }

    #[test]
    fn test_bootstrap_is_idempotent() {
        // Second bootstrap call does nothing when already populated.
        let today = NaiveDate::parse_from_str("2026-04-13", "%Y-%m-%d").unwrap();
        let bars = rth_timestamps("2026-04-08", 50);
        let ts: Vec<i64> = bars.iter().map(|(t, _)| *t).collect();

        let mut fulfilled = Fulfilled::default();
        bootstrap_fulfilled("TEST", &ts, &mut fulfilled, today);
        let before = fulfilled.get("TEST").unwrap().clone();

        bootstrap_fulfilled("TEST", &[], &mut fulfilled, today);
        let after = fulfilled.get("TEST").unwrap();
        assert_eq!(&before, after);
    }

    #[test]
    fn test_latest_fulfilled_returns_max() {
        let mut f = Fulfilled::default();
        f.insert("TEST", "2026-04-08".to_string());
        f.insert("TEST", "2026-04-10".to_string());
        f.insert("TEST", "2026-04-09".to_string());
        let got = latest_fulfilled("TEST", &f).unwrap();
        assert_eq!(got, NaiveDate::parse_from_str("2026-04-10", "%Y-%m-%d").unwrap());
    }

    #[test]
    fn test_latest_fulfilled_none_for_unknown_symbol() {
        let f = Fulfilled::default();
        assert!(latest_fulfilled("NOPE", &f).is_none());
    }

    #[test]
    fn test_fulfilled_load_save_roundtrip() {
        let dir = TempDir::new().unwrap();
        let mut f = Fulfilled::default();
        f.insert("AAPL", "2026-04-08".to_string());
        f.insert("AAPL", "2026-04-09".to_string());
        f.insert("MSFT", "2026-04-08".to_string());
        f.last_updated = "now".to_string();

        save_fulfilled(dir.path(), &f).unwrap();
        let loaded = load_fulfilled(dir.path());
        assert_eq!(loaded.get("AAPL").unwrap().len(), 2);
        assert_eq!(loaded.get("MSFT").unwrap().len(), 1);
    }

    #[test]
    fn test_fulfilled_load_missing_file() {
        let dir = TempDir::new().unwrap();
        let loaded = load_fulfilled(dir.path());
        assert!(loaded.by_symbol.is_empty());
    }

    #[test]
    fn test_alpaca_bars_to_columns() {
        let bars = vec![AlpacaBar {
            t: "2026-04-07T13:30:00Z".to_string(),
            o: 100.0,
            h: 101.0,
            l: 99.0,
            c: 100.5,
            v: 1000.0,
            n: Some(50),
            vw: Some(100.3),
        }];
        let (ts, _, _, _, c, v, _, _) = alpaca_bars_to_columns(&bars);
        assert_eq!(ts.len(), 1);
        assert_eq!(c[0], 100.5);
        assert_eq!(v[0], 1000);
    }

}
