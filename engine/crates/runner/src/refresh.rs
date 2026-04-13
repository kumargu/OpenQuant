//! Refresh quant-data parquets to a target date.
//!
//! Called at runner startup before warmup. Scans each symbol's parquet,
//! checks contiguity of RTH bars in the last 90 trading days, and fetches
//! missing bars from Alpaca 1-min IEX API.
//!
//! If a trading day has fewer than MIN_BARS_PER_DAY bars, everything from
//! that day onward is considered corrupt and re-fetched.

use arrow::array::{Array, ArrayRef, Float64Array, Int64Array, TimestampMicrosecondArray};
use arrow::datatypes::{DataType, Field, Schema, TimeUnit};
use arrow::record_batch::RecordBatch;
use chrono::{NaiveDate, Utc};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::arrow::ArrowWriter;
use std::collections::HashMap;
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

/// Minimum bars per RTH day to consider data valid.
const MIN_BARS_PER_DAY: usize = 50;

/// RTH session: 13:30–20:00 UTC (same as quant-lab and mock_alpaca.py).
const RTH_START_MINUTES: i64 = 13 * 60 + 30;
const RTH_END_MINUTES: i64 = 20 * 60;

/// Number of trading days to check for contiguity.
const LOOKBACK_DAYS: i64 = 90;

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

/// Result of checking one symbol's parquet.
#[derive(Debug)]
pub struct SymbolStatus {
    pub symbol: String,
    pub latest_ts: Option<i64>,           // unix micros
    pub needs_fetch_from: Option<String>, // date string YYYY-MM-DD
    pub corrupt_from: Option<String>,
    pub is_ok: bool,
}

/// Read a parquet file, return all timestamps as unix microseconds.
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

/// Write columns to a parquet file.
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

    // Atomic write: write to .tmp, then rename. If we crash mid-write,
    // the original parquet is untouched.
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

/// Convert unix microseconds to (hour, minute) in UTC.
fn micros_to_hm(us: i64) -> (i64, i64) {
    let secs = us / 1_000_000;
    let h = (secs % 86400) / 3600;
    let m = (secs % 3600) / 60;
    (h, m)
}

/// Convert unix microseconds to a date string "YYYY-MM-DD".
fn micros_to_date(us: i64) -> String {
    let secs = us / 1_000_000;
    let dt =
        chrono::DateTime::from_timestamp(secs, 0).unwrap_or(chrono::DateTime::UNIX_EPOCH);
    dt.format("%Y-%m-%d").to_string()
}

/// Check contiguity of RTH bars in the last LOOKBACK_DAYS days.
/// Returns (is_ok, first_corrupt_date).
fn check_contiguity(timestamps: &[i64]) -> (bool, Option<String>) {
    let cutoff_secs = Utc::now().timestamp() - LOOKBACK_DAYS * 86400;
    let cutoff_us = cutoff_secs * 1_000_000;

    // Filter to RTH bars in the lookback window
    let mut by_date: HashMap<String, usize> = HashMap::new();
    for &ts in timestamps {
        if ts < cutoff_us {
            continue;
        }
        let (h, m) = micros_to_hm(ts);
        let minutes = h * 60 + m;
        if !(RTH_START_MINUTES..RTH_END_MINUTES).contains(&minutes) {
            continue;
        }
        let date = micros_to_date(ts);
        *by_date.entry(date).or_insert(0) += 1;
    }

    // Check each trading day has enough bars
    let mut dates: Vec<_> = by_date.iter().collect();
    dates.sort_by_key(|(d, _)| d.to_string());

    for (date, &count) in &dates {
        if count < MIN_BARS_PER_DAY {
            return (false, Some(date.to_string()));
        }
    }

    (true, None)
}

/// Convert Alpaca bars to column vectors matching the parquet schema.
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
        if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(&bar.t) {
            ts.push(dt.timestamp_millis() * 1000); // millis → micros
            open.push(bar.o);
            high.push(bar.h);
            low.push(bar.l);
            close.push(bar.c);
            volume.push(bar.v as i64);
            trade_count.push(bar.n.unwrap_or(0));
            vwap.push(bar.vw.unwrap_or(0.0));
        }
    }

    (ts, open, high, low, close, volume, trade_count, vwap)
}

/// Refresh one symbol's parquet to target_date.
/// Returns number of new bars fetched.
pub async fn refresh_symbol(
    bars_dir: &Path,
    symbol: &str,
    target_date: &str,
    client: &AlpacaClient,
) -> Result<usize, String> {
    let path = bars_dir.join(format!("{symbol}.parquet"));
    if !path.exists() {
        return Err(format!("{symbol}.parquet not found"));
    }

    let timestamps = read_timestamps(&path)?;
    if timestamps.is_empty() {
        return Err(format!("{symbol}: empty parquet"));
    }

    let latest_us = *timestamps.last().unwrap();
    let latest_date = micros_to_date(latest_us);

    // Already up to date?
    if latest_date.as_str() >= target_date {
        return Ok(0);
    }

    // Check contiguity
    let (ok, corrupt_from) = check_contiguity(&timestamps);

    let fetch_from = if !ok {
        let corrupt_date = corrupt_from.unwrap();
        warn!(
            symbol,
            corrupt_date = corrupt_date.as_str(),
            "corrupt data detected — re-fetching from corrupt point"
        );

        // Read full parquet, drop everything from corrupt date onward
        let (mut ts, mut o, mut h, mut l, mut c, mut v, mut tc, mut vw) = read_full_parquet(&path)?;

        let corrupt_date_parsed =
            NaiveDate::parse_from_str(&corrupt_date, "%Y-%m-%d").map_err(|e| format!("{e}"))?;
        let corrupt_us = corrupt_date_parsed
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp()
            * 1_000_000;

        // Keep only bars before the corrupt date
        let keep: Vec<bool> = ts.iter().map(|&t| t < corrupt_us).collect();
        let mut new_ts = Vec::new();
        let mut new_o = Vec::new();
        let mut new_h = Vec::new();
        let mut new_l = Vec::new();
        let mut new_c = Vec::new();
        let mut new_v = Vec::new();
        let mut new_tc = Vec::new();
        let mut new_vw = Vec::new();

        for (i, &k) in keep.iter().enumerate() {
            if k {
                new_ts.push(ts[i]);
                new_o.push(o[i]);
                new_h.push(h[i]);
                new_l.push(l[i]);
                new_c.push(c[i]);
                new_v.push(v[i]);
                new_tc.push(tc[i]);
                new_vw.push(vw[i]);
            }
        }

        ts = new_ts;
        o = new_o;
        h = new_h;
        l = new_l;
        c = new_c;
        v = new_v;
        tc = new_tc;
        vw = new_vw;

        // Write clean data back
        write_parquet(&path, &(ts, o, h, l, c, v, tc, vw))?;

        corrupt_date
    } else {
        // Data is clean — fetch from day after latest
        let latest_parsed =
            NaiveDate::parse_from_str(&latest_date, "%Y-%m-%d").map_err(|e| format!("{e}"))?;
        let next_day = latest_parsed + chrono::Duration::days(1);
        next_day.format("%Y-%m-%d").to_string()
    };

    // Fetch missing bars from Alpaca
    let symbols = vec![symbol.to_string()];
    let raw = client
        .fetch_minute_bars_raw(&symbols, &fetch_from, target_date)
        .await?;

    let bars = raw.get(symbol).cloned().unwrap_or_default();
    if bars.is_empty() {
        info!(symbol, "no new bars from Alpaca");
        return Ok(0);
    }

    let (new_ts, new_o, new_h, new_l, new_c, new_v, new_tc, new_vw) = alpaca_bars_to_columns(&bars);

    // Read existing, append new, deduplicate, write
    let (mut ts, mut o, mut h, mut l, mut c, mut v, mut tc, mut vw) = read_full_parquet(&path)?;

    let existing_set: std::collections::HashSet<i64> = ts.iter().cloned().collect();
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

    // Sort by timestamp
    let mut indices: Vec<usize> = (0..ts.len()).collect();
    indices.sort_by_key(|&i| ts[i]);

    let ts_sorted: Vec<_> = indices.iter().map(|&i| ts[i]).collect();
    let o_sorted: Vec<_> = indices.iter().map(|&i| o[i]).collect();
    let h_sorted: Vec<_> = indices.iter().map(|&i| h[i]).collect();
    let l_sorted: Vec<_> = indices.iter().map(|&i| l[i]).collect();
    let c_sorted: Vec<_> = indices.iter().map(|&i| c[i]).collect();
    let v_sorted: Vec<_> = indices.iter().map(|&i| v[i]).collect();
    let tc_sorted: Vec<_> = indices.iter().map(|&i| tc[i]).collect();
    let vw_sorted: Vec<_> = indices.iter().map(|&i| vw[i]).collect();

    write_parquet(
        &path,
        &(
            ts_sorted, o_sorted, h_sorted, l_sorted, c_sorted, v_sorted, tc_sorted, vw_sorted,
        ),
    )?;

    info!(symbol, added, "refreshed parquet");
    Ok(added)
}

/// Refresh all symbols in bars_dir to target_date.
/// Returns total number of new bars fetched.
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

    let mut total = 0;
    let mut refreshed = 0;
    let mut errors = 0;

    for (i, sym) in symbols.iter().enumerate() {
        match refresh_symbol(bars_dir, sym, target_date, client).await {
            Ok(0) => {} // up to date
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
        }
    }

    info!(refreshed, total_bars = total, errors, "refresh complete");

    Ok(total)
}

/// Resolve the quant-data bars directory.
/// Override with `QUANT_DATA_BARS_DIR` env var.
pub fn default_bars_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("QUANT_DATA_BARS_DIR") {
        return PathBuf::from(dir);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join("quant-data/bars/v2_sp500_2025-2026_1min_adjusted")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    /// Create a test parquet with known bars.
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

    /// Generate RTH timestamps for N consecutive minutes starting at 13:30 UTC on a given date.
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

        let bars: Vec<(i64, f64)> = rth_timestamps("2026-04-07", 390);
        write_test_parquet(&path, &bars);

        let timestamps = read_timestamps(&path).unwrap();
        assert_eq!(timestamps.len(), 390);
        assert_eq!(timestamps[0], bars[0].0);
        assert_eq!(timestamps[389], bars[389].0);
    }

    #[test]
    fn test_contiguity_good() {
        // 5 full trading days (390 bars each) — all contiguous
        let mut bars = Vec::new();
        for date in &[
            "2026-04-07",
            "2026-04-08",
            "2026-04-09",
            "2026-04-10",
            "2026-04-11",
        ] {
            bars.extend(rth_timestamps(date, 390));
        }
        let timestamps: Vec<i64> = bars.iter().map(|(t, _)| *t).collect();
        let (ok, corrupt) = check_contiguity(&timestamps);
        assert!(ok, "should be contiguous");
        assert!(corrupt.is_none());
    }

    #[test]
    fn test_contiguity_corrupt_day() {
        // 4 good days + 1 day with only 20 bars (corrupt)
        let mut bars = Vec::new();
        for date in &["2026-04-07", "2026-04-08", "2026-04-09"] {
            bars.extend(rth_timestamps(date, 390));
        }
        bars.extend(rth_timestamps("2026-04-10", 20)); // corrupt
        bars.extend(rth_timestamps("2026-04-11", 390));

        let timestamps: Vec<i64> = bars.iter().map(|(t, _)| *t).collect();
        let (ok, corrupt) = check_contiguity(&timestamps);
        assert!(!ok, "should detect corrupt day");
        assert_eq!(corrupt.unwrap(), "2026-04-10");
    }

    #[test]
    fn test_full_read_write() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("TEST.parquet");

        let bars = rth_timestamps("2026-04-07", 100);
        write_test_parquet(&path, &bars);

        let (ts, _o, _h, _l, c, _v, _tc, _vw) = read_full_parquet(&path).unwrap();
        assert_eq!(ts.len(), 100);
        assert_eq!(c[0], 100.0);
        assert_eq!(c[99], 100.99);
    }

    #[test]
    fn test_micros_to_date() {
        // 2026-04-07 13:30:00 UTC in micros
        let d = NaiveDate::parse_from_str("2026-04-07", "%Y-%m-%d").unwrap();
        let ts = d.and_hms_opt(13, 30, 0).unwrap().and_utc().timestamp() * 1_000_000;
        assert_eq!(micros_to_date(ts), "2026-04-07");
    }

    #[test]
    fn test_micros_to_hm() {
        let d = NaiveDate::parse_from_str("2026-04-07", "%Y-%m-%d").unwrap();
        let ts = d.and_hms_opt(14, 45, 0).unwrap().and_utc().timestamp() * 1_000_000;
        let (h, m) = micros_to_hm(ts);
        assert_eq!(h, 14);
        assert_eq!(m, 45);
    }

    #[test]
    fn test_alpaca_bars_to_columns() {
        let bars = vec![
            AlpacaBar {
                t: "2026-04-07T13:30:00Z".to_string(),
                o: 100.0,
                h: 101.0,
                l: 99.0,
                c: 100.5,
                v: 1000.0,
                n: Some(50),
                vw: Some(100.3),
            },
            AlpacaBar {
                t: "2026-04-07T13:31:00Z".to_string(),
                o: 100.5,
                h: 101.5,
                l: 99.5,
                c: 101.0,
                v: 2000.0,
                n: Some(75),
                vw: Some(100.8),
            },
        ];
        let (ts, _o, _h, _l, c, v, _tc, _vw) = alpaca_bars_to_columns(&bars);
        assert_eq!(ts.len(), 2);
        assert_eq!(c[0], 100.5);
        assert_eq!(c[1], 101.0);
        assert_eq!(v[0], 1000);
    }
}
