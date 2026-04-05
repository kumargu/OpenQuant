//! Simple file-based bar cache for replay experiments.
//!
//! Caches Alpaca minute bars to disk so repeated replays don't hit the API.
//! Layout: `{root}/minute/{SYMBOL}/{YYYY-MM-DD}.jsonl`
//!
//! File format:
//!   Line 1: `#bars:N` (integrity header — number of bar lines that follow)
//!   Lines 2+: JSON objects `{"t":"...","o":1.0,"h":1.0,"l":1.0,"c":1.0,"v":100}`
//!
//! Bars are stored in Alpaca's raw format (bar open time). The +60s adjustment
//! to close time happens at read time, matching the non-cached path.
//!
//! Integrity:
//! - Write-once: existing files are never overwritten
//! - Atomic writes: .tmp file → rename (crash leaves .tmp, not corrupt cache)
//! - Header check: if bar count doesn't match header, file is deleted as corrupt
//! - Parse check: any unparseable JSON line → file deleted as corrupt

use crate::alpaca::AlpacaBar;
use std::collections::HashMap;
use std::path::PathBuf;
use tracing::{info, warn};

#[allow(unused_imports)]
use std::path::Path;

/// Bar cache backed by JSONL files on disk.
pub struct BarCache {
    root: PathBuf,
}

impl BarCache {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// Read cached minute bars for a single symbol on a single date.
    /// Returns None on cache miss. Deletes file if corrupt.
    pub fn read_minute(&self, symbol: &str, date: &str) -> Option<Vec<AlpacaBar>> {
        let path = self.minute_path(symbol, date);
        if !path.exists() {
            return None;
        }
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => {
                warn!(symbol, date, error = %e, "cache read failed — treating as miss");
                return None;
            }
        };

        let mut lines = content.lines();

        // First line must be the integrity header: #bars:N
        let expected_count = match lines.next() {
            Some(header) if header.starts_with("#bars:") => {
                match header[6..].parse::<usize>() {
                    Ok(n) => n,
                    Err(_) => {
                        warn!(symbol, date, "corrupt cache header — deleting");
                        let _ = std::fs::remove_file(&path);
                        return None;
                    }
                }
            }
            _ => {
                warn!(symbol, date, "missing cache header — deleting");
                let _ = std::fs::remove_file(&path);
                return None;
            }
        };

        let mut bars = Vec::with_capacity(expected_count);
        for line in lines {
            if line.is_empty() {
                continue;
            }
            match serde_json::from_str::<AlpacaBar>(line) {
                Ok(bar) => bars.push(bar),
                Err(e) => {
                    warn!(symbol, date, error = %e, "corrupt cache line — deleting file");
                    let _ = std::fs::remove_file(&path);
                    return None;
                }
            }
        }

        // Verify count matches header
        if bars.len() != expected_count {
            warn!(
                symbol, date,
                expected = expected_count,
                actual = bars.len(),
                "cache bar count mismatch (truncated?) — deleting"
            );
            let _ = std::fs::remove_file(&path);
            return None;
        }

        Some(bars)
    }

    /// Write minute bars for a single symbol on a single date.
    /// Write-once: skips silently if file already exists.
    /// Atomic: writes to .tmp first, then renames.
    pub fn write_minute(&self, symbol: &str, date: &str, bars: &[AlpacaBar]) {
        let path = self.minute_path(symbol, date);
        if path.exists() {
            return; // write-once — never overwrite
        }
        let dir = path.parent().unwrap();
        if let Err(e) = std::fs::create_dir_all(dir) {
            warn!(symbol, date, error = %e, "failed to create cache dir");
            return;
        }

        // Build content with integrity header
        let mut content = format!("#bars:{}\n", bars.len());
        for bar in bars {
            if let Ok(json) = serde_json::to_string(bar) {
                content.push_str(&json);
                content.push('\n');
            }
        }

        // Atomic write: .tmp → rename
        let tmp_path = path.with_extension("jsonl.tmp");
        if let Err(e) = std::fs::write(&tmp_path, &content) {
            warn!(symbol, date, error = %e, "failed to write cache tmp");
            return;
        }
        if let Err(e) = std::fs::rename(&tmp_path, &path) {
            warn!(symbol, date, error = %e, "failed to rename cache file");
            let _ = std::fs::remove_file(&tmp_path);
        }
    }

    /// Read cached bars for multiple symbols on a single date.
    /// Returns (cached_bars, uncached_symbols).
    /// cached_bars are already converted to (symbol, timestamp_ms, close).
    pub fn read_day(
        &self,
        symbols: &[String],
        date: &str,
    ) -> (Vec<(String, i64, f64)>, Vec<String>) {
        const MINUTE_BAR_DURATION_MS: i64 = 60_000;
        let mut cached = Vec::new();
        let mut uncached = Vec::new();

        for symbol in symbols {
            match self.read_minute(symbol, date) {
                Some(bars) => {
                    for bar in &bars {
                        if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(&bar.t) {
                            let close_ts = dt.timestamp_millis() + MINUTE_BAR_DURATION_MS;
                            cached.push((symbol.clone(), close_ts, bar.c));
                        }
                    }
                }
                None => {
                    uncached.push(symbol.clone());
                }
            }
        }

        (cached, uncached)
    }

    /// Write fetched bars to cache, split by symbol.
    /// `raw_bars` is the HashMap from Alpaca response (symbol → Vec<AlpacaBar>).
    pub fn write_day(&self, raw_bars: &HashMap<String, Vec<AlpacaBar>>, date: &str) {
        for (symbol, bars) in raw_bars {
            self.write_minute(symbol, date, bars);
        }
    }

    fn minute_path(&self, symbol: &str, date: &str) -> PathBuf {
        self.root
            .join("minute")
            .join(symbol)
            .join(format!("{date}.jsonl"))
    }
}

/// Stats for cache performance logging.
pub struct CacheStats {
    pub hits: usize,
    pub misses: usize,
}

impl CacheStats {
    pub fn new() -> Self {
        Self { hits: 0, misses: 0 }
    }

    pub fn log_summary(&self) {
        let total = self.hits + self.misses;
        if total > 0 {
            let rate = self.hits as f64 / total as f64 * 100.0;
            info!(
                hits = self.hits,
                misses = self.misses,
                rate = format!("{:.0}%", rate).as_str(),
                "bar cache summary"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn sample_bar() -> AlpacaBar {
        AlpacaBar {
            t: "2026-03-10T14:30:00Z".to_string(),
            o: 100.0,
            h: 101.0,
            l: 99.5,
            c: 100.5,
            v: 1234.0,
        }
    }

    #[test]
    fn round_trip() {
        let dir = TempDir::new().unwrap();
        let cache = BarCache::new(dir.path().to_path_buf());
        let bars = vec![sample_bar(), sample_bar()];

        // Write
        cache.write_minute("AAPL", "2026-03-10", &bars);

        // Read back
        let read = cache.read_minute("AAPL", "2026-03-10").unwrap();
        assert_eq!(read.len(), 2);
        assert_eq!(read[0].c, 100.5);
    }

    #[test]
    fn write_once() {
        let dir = TempDir::new().unwrap();
        let cache = BarCache::new(dir.path().to_path_buf());

        let bars1 = vec![sample_bar()];
        cache.write_minute("AAPL", "2026-03-10", &bars1);

        // Second write with different data should be ignored
        let bar2 = AlpacaBar { c: 999.0, ..sample_bar() };
        cache.write_minute("AAPL", "2026-03-10", &vec![bar2]);

        let read = cache.read_minute("AAPL", "2026-03-10").unwrap();
        assert_eq!(read[0].c, 100.5); // original, not 999.0
    }

    #[test]
    fn cache_miss() {
        let dir = TempDir::new().unwrap();
        let cache = BarCache::new(dir.path().to_path_buf());
        assert!(cache.read_minute("AAPL", "2026-03-10").is_none());
    }

    #[test]
    fn corrupt_file_deleted() {
        let dir = TempDir::new().unwrap();
        let cache = BarCache::new(dir.path().to_path_buf());
        let path = cache.minute_path("AAPL", "2026-03-10");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();

        // Write corrupt content (wrong header count)
        std::fs::write(&path, "#bars:5\n{\"t\":\"x\",\"o\":1,\"h\":1,\"l\":1,\"c\":1,\"v\":1}\n").unwrap();

        // Read should detect mismatch and delete
        assert!(cache.read_minute("AAPL", "2026-03-10").is_none());
        assert!(!path.exists()); // file was deleted
    }

    #[test]
    fn truncated_file_deleted() {
        let dir = TempDir::new().unwrap();
        let cache = BarCache::new(dir.path().to_path_buf());
        let path = cache.minute_path("AAPL", "2026-03-10");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();

        // Write truncated content (header says 2, only 1 bar)
        std::fs::write(&path, "#bars:2\n{\"t\":\"2026-03-10T14:30:00Z\",\"o\":1,\"h\":1,\"l\":1,\"c\":1,\"v\":1}\n").unwrap();

        assert!(cache.read_minute("AAPL", "2026-03-10").is_none());
        assert!(!path.exists());
    }
}
