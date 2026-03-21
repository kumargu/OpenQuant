//! Bar reader — loads experiment_bars JSON files into openquant_core::Bar.
//!
//! Format: {"SYMBOL": [{"timestamp", "open", "high", "low", "close", "volume"}, ...], ...}

use openquant_core::market_data::Bar;
use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use tracing::{info, warn};

/// US equity regular trading hours in UTC.
/// 9:30 ET = 14:30 UTC (EST) or 13:30 UTC (EDT).
/// We use 13:30 UTC (EDT) as the earliest valid bar — conservative.
/// 16:00 ET = 21:00 UTC (EST) or 20:00 UTC (EDT).
/// We use 20:00 UTC (EDT) as the latest valid bar.
const MARKET_OPEN_UTC_HOUR: u32 = 13;
const MARKET_OPEN_UTC_MIN: u32 = 30;
const MARKET_CLOSE_UTC_HOUR: u32 = 20;
const MARKET_CLOSE_UTC_MIN: u32 = 0;

/// Check if a timestamp falls within US equity regular trading hours.
///
/// Uses EDT boundaries (13:30-20:00 UTC). During EST (Nov-Mar), this
/// admits ~30 min of pre-market bars (8:30-9:00 ET). Acceptable since
/// pre-market bars have low volume and won't trigger entries.
fn is_regular_hours(timestamp_ms: i64) -> bool {
    debug_assert!(timestamp_ms > 0, "timestamp must be positive");
    let secs = timestamp_ms / 1000;
    let time_of_day = (secs % 86400) as u32; // seconds since midnight UTC
    let hour = time_of_day / 3600;
    let min = (time_of_day % 3600) / 60;

    let open = MARKET_OPEN_UTC_HOUR * 60 + MARKET_OPEN_UTC_MIN;
    let close = MARKET_CLOSE_UTC_HOUR * 60 + MARKET_CLOSE_UTC_MIN;
    let current = hour * 60 + min;

    current >= open && current < close
}

#[derive(Debug, Deserialize)]
struct RawBar {
    timestamp: i64,
    open: f64,
    high: f64,
    low: f64,
    close: f64,
    volume: f64,
}

/// Load bars from a single JSON file.
/// Returns bars sorted by timestamp, interleaved across symbols.
pub fn load_day(path: &Path) -> Result<Vec<Bar>, String> {
    let contents =
        fs::read_to_string(path).map_err(|e| format!("Failed to read {}: {e}", path.display()))?;

    let raw: HashMap<String, Vec<RawBar>> = serde_json::from_str(&contents)
        .map_err(|e| format!("Failed to parse {}: {e}", path.display()))?;

    let mut bars = Vec::new();
    let mut filtered_count = 0usize;
    for (symbol, raw_bars) in &raw {
        for rb in raw_bars {
            if !rb.open.is_finite() || !rb.close.is_finite() || rb.close <= 0.0 {
                continue; // skip corrupt bars
            }
            if !is_regular_hours(rb.timestamp) {
                filtered_count += 1;
                continue; // skip pre-market / after-hours bars
            }
            bars.push(Bar {
                symbol: symbol.clone(),
                timestamp: rb.timestamp,
                open: rb.open,
                high: rb.high,
                low: rb.low,
                close: rb.close,
                volume: rb.volume,
            });
        }
    }

    if filtered_count > 0 {
        info!(
            filtered = filtered_count,
            "Filtered pre-market/after-hours bars"
        );
    }

    // Sort by timestamp for correct interleaving
    bars.sort_by_key(|b| b.timestamp);

    info!(
        path = %path.display(),
        symbols = raw.len(),
        bars = bars.len(),
        "Loaded bar file"
    );

    Ok(bars)
}

/// Load bars from all matching files in a directory.
/// Glob pattern: experiment_bars_*.json (excluding 5min/15min variants).
pub fn load_days(dir: &Path) -> Result<Vec<Bar>, String> {
    let mut all_bars = Vec::new();
    let mut files: Vec<_> = fs::read_dir(dir)
        .map_err(|e| format!("Failed to read dir {}: {e}", dir.display()))?
        .filter_map(|e| e.ok())
        .filter(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            name.starts_with("experiment_bars_")
                && name.ends_with(".json")
                && !name.contains("5min")
                && !name.contains("15min")
        })
        .collect();

    files.sort_by_key(|e| e.file_name());

    if files.is_empty() {
        warn!(dir = %dir.display(), "No experiment_bars files found");
        return Ok(all_bars);
    }

    for entry in &files {
        match load_day(&entry.path()) {
            Ok(bars) => all_bars.extend(bars),
            Err(e) => warn!("Skipping {}: {e}", entry.path().display()),
        }
    }

    // Final sort across all days
    all_bars.sort_by_key(|b| b.timestamp);

    info!(
        files = files.len(),
        total_bars = all_bars.len(),
        "Loaded all bar files"
    );

    Ok(all_bars)
}
