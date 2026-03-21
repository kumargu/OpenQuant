//! Bar reader — loads experiment_bars JSON files into openquant_core::Bar.
//!
//! Format: {"SYMBOL": [{"timestamp", "open", "high", "low", "close", "volume"}, ...], ...}

use openquant_core::config::DataConfig;
use openquant_core::market_data::Bar;
use serde::Deserialize;
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use tracing::{info, warn};

/// Check if a timestamp falls within configured trading hours.
///
/// Converts the timestamp to local time using the configured timezone offset,
/// then checks against market_open/market_close from TOML.
fn is_regular_hours(timestamp_ms: i64, data_config: &DataConfig) -> bool {
    debug_assert!(timestamp_ms > 0, "timestamp must be positive");
    // Shift to local timezone
    let local_ms = timestamp_ms + data_config.tz_offset_ms();
    let secs = local_ms / 1000;
    let time_of_day = ((secs % 86400 + 86400) % 86400) as u32; // handle negative modulo
    let hour = time_of_day / 3600;
    let min = (time_of_day % 3600) / 60;

    let (open_h, open_m) = data_config.open_hm();
    let (close_h, close_m) = data_config.close_hm();

    let open = open_h * 60 + open_m;
    let close = close_h * 60 + close_m;
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
/// Filters out bars outside configured market hours.
pub fn load_day(path: &Path, data_config: &DataConfig) -> Result<Vec<Bar>, String> {
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
            if !is_regular_hours(rb.timestamp, data_config) {
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
pub fn load_days(dir: &Path, data_config: &DataConfig) -> Result<Vec<Bar>, String> {
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
        match load_day(&entry.path(), data_config) {
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
