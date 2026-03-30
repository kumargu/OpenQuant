//! Earnings calendar — blocks pair entries around announcement dates.
//!
//! Reads a JSON file mapping symbols to their earnings announcement dates.
//! Before each trading day, the runner calls `apply_blackouts()` to block
//! entries for pairs where either leg has earnings within the blackout window.

use openquant_core::pairs::engine::PairsEngine;
use std::collections::HashMap;
use std::path::Path;
use tracing::info;

/// Number of trading days to block entries around earnings.
/// 3 days = day before, day of, day after.
const BLACKOUT_DAYS: i64 = 3;

/// Milliseconds per trading day (for blackout window calculation).
const DAY_MS: i64 = 86_400_000;

/// Load earnings calendar from JSON file.
/// Format: {"SYMBOL": "YYYY-MM-DD", "SYMBOL2": "YYYY-MM-DD", ...}
pub fn load_earnings_calendar(path: &Path) -> HashMap<String, i64> {
    let contents = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return HashMap::new(),
    };
    let raw: HashMap<String, String> = match serde_json::from_str(&contents) {
        Ok(m) => m,
        Err(_) => return HashMap::new(),
    };

    raw.into_iter()
        .filter_map(|(symbol, date_str)| {
            chrono::NaiveDate::parse_from_str(&date_str, "%Y-%m-%d")
                .ok()
                .map(|d| {
                    let ts = d.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp_millis();
                    (symbol, ts)
                })
        })
        .collect()
}

/// Apply earnings blackouts for the current replay day.
/// Blocks entries for pairs where either leg has earnings within BLACKOUT_DAYS.
pub fn apply_blackouts(
    engine: &mut PairsEngine,
    calendar: &HashMap<String, i64>,
    current_day_ts: i64,
) {
    let window_start = current_day_ts - BLACKOUT_DAYS * DAY_MS;
    let window_end = current_day_ts + BLACKOUT_DAYS * DAY_MS;

    let mut blocked = 0;
    for (symbol, &earnings_ts) in calendar {
        if earnings_ts >= window_start && earnings_ts <= window_end {
            // Block entries until BLACKOUT_DAYS after earnings
            let until = earnings_ts + BLACKOUT_DAYS * DAY_MS;
            engine.block_symbol_entries(symbol, until);
            blocked += 1;
        }
    }

    if blocked > 0 {
        info!(blocked, "earnings blackouts applied");
    }
}
