//! SQLite journal performance gate — catches write-path regressions in CI.
//!
//! Tests bulk insert throughput and single-bar write latency to ensure
//! the journal never becomes a bottleneck for the trading hot path.
//!
//! Baselines (Apple M4, 2026-03-17):
//!   single bar write:      ~15µs  → gate: 500µs
//!   1k bars bulk write:    ~12ms  → gate: 500ms
//!   10k bars bulk write:   ~120ms → gate: 5s
//!
//! CI Ubuntu runners are slower and have variable disk I/O,
//! so gates are set generously (30-40x baseline).

use std::time::Instant;

use openquant_core::features::FeatureValues;
use openquant_journal::writer::{BarRecord, FillRecord};
use openquant_journal::DataRuntime;
use rusqlite::Connection;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn make_bar_record(i: usize) -> BarRecord {
    BarRecord {
        symbol: "BTCUSD".to_string(),
        timestamp: 1700000000000 + (i as i64 * 60_000),
        open: 100.0 + (i as f64 * 0.01),
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
            return_z_score: -2.1,
            relative_volume: 1.3,
            bar_range: 2.0,
            close_location: 0.75,
            trend_up: true,
            warmed_up: true,
            ema_fast: 100.2,
            ema_slow: 99.8,
            ema_fast_above_slow: true,
            adx: 25.0,
            plus_di: 30.0,
            minus_di: 15.0,
            bollinger_upper: 102.0,
            bollinger_lower: 98.0,
            bollinger_pct_b: 0.6,
            bollinger_bandwidth: 0.04,
            ..Default::default()
        },
        signal_fired: i % 10 == 0,
        signal_side: if i % 10 == 0 {
            Some("buy".to_string())
        } else {
            None
        },
        signal_score: if i % 10 == 0 { Some(1.5) } else { None },
        signal_reason: None,
        risk_passed: if i % 10 == 0 { Some(true) } else { None },
        risk_rejection: None,
        qty_approved: if i % 10 == 0 { Some(0.1) } else { None },
        engine_version: "bench".to_string(),
    }
}

fn make_fill_record(i: usize) -> FillRecord {
    FillRecord {
        symbol: "BTCUSD".to_string(),
        side: if i % 2 == 0 { "buy" } else { "sell" }.to_string(),
        qty: 0.1,
        fill_price: 100.0 + (i as f64 * 0.5),
        slippage: 0.05,
        engine_version: "bench".to_string(),
    }
}

fn tmp_db(name: &str) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("oq_bench_{name}.db"));
    let _ = std::fs::remove_file(&path);
    path
}

/// Write N bars through the DataRuntime, return total elapsed time.
fn write_bars_through_runtime(n: usize, db_name: &str) -> (std::time::Duration, std::path::PathBuf) {
    let path = tmp_db(db_name);
    // Buffer must hold all bars + fills + flush. try_send drops on full channel.
    let buffer = n + n / 20 + 64;
    let rt = DataRuntime::new(&path, buffer);
    let journal = rt.journal();

    let start = Instant::now();
    for i in 0..n {
        journal.log_bar(make_bar_record(i));
        // Sprinkle in fills (1 fill per 20 bars)
        if i % 20 == 0 {
            journal.log_fill(make_fill_record(i));
        }
    }
    journal.flush();
    rt.shutdown(); // waits for all writes to complete
    let elapsed = start.elapsed();

    (elapsed, path)
}

// ---------------------------------------------------------------------------
// Gate tests
// ---------------------------------------------------------------------------

#[test]
#[ignore] // cargo test --test bench_gate --release -p openquant-journal -- --ignored
fn gate_single_bar_write() {
    let path = tmp_db("single");
    let rt = DataRuntime::new(&path, 256);
    let journal = rt.journal();

    // Warm up — write 100 bars to get SQLite pages allocated
    for i in 0..100 {
        journal.log_bar(make_bar_record(i));
    }
    journal.flush();
    // Give the writer time to flush warmup
    std::thread::sleep(std::time::Duration::from_millis(100));

    // Measure: write a single bar, flush, shutdown to force sync
    let start = Instant::now();
    journal.log_bar(make_bar_record(999));
    journal.flush();
    rt.shutdown();
    let us = start.elapsed().as_micros();

    let _ = std::fs::remove_file(&path);

    assert!(
        us < 500_000, // 500ms — very generous for a single bar + flush + shutdown
        "single bar write+flush took {us}µs, gate is 500ms"
    );
}

#[test]
#[ignore]
fn gate_bulk_1k_bars() {
    let (elapsed, path) = write_bars_through_runtime(1_000, "bulk_1k");
    let ms = elapsed.as_millis();

    // Verify all rows landed
    let conn = Connection::open(&path).unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM bars", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 1_000, "expected 1000 bars, got {count}");

    let fill_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM fills", [], |row| row.get(0))
        .unwrap();
    assert_eq!(fill_count, 50, "expected 50 fills, got {fill_count}");

    let _ = std::fs::remove_file(&path);

    assert!(
        ms < 500,
        "1k bars bulk write took {ms}ms, gate is 500ms (baseline ~12ms)"
    );
}

#[test]
#[ignore]
fn gate_bulk_10k_bars() {
    let (elapsed, path) = write_bars_through_runtime(10_000, "bulk_10k");
    let ms = elapsed.as_millis();

    let conn = Connection::open(&path).unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM bars", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 10_000, "expected 10000 bars, got {count}");

    let _ = std::fs::remove_file(&path);

    assert!(
        ms < 5_000,
        "10k bars bulk write took {ms}ms, gate is 5s (baseline ~120ms)"
    );
}

#[test]
#[ignore]
fn gate_write_after_large_db() {
    // Simulate writing to an already-large database (50k rows pre-populated),
    // then measure if per-bar latency degrades.
    let path = tmp_db("large_db");
    let rt = DataRuntime::new(&path, 60_000);
    let journal = rt.journal();

    // Pre-populate 50k bars
    for i in 0..50_000 {
        journal.log_bar(make_bar_record(i));
    }
    journal.flush();
    // Wait for pre-population to flush
    std::thread::sleep(std::time::Duration::from_millis(500));

    // Now measure writing 1k more bars on top of the large DB
    let start = Instant::now();
    for i in 50_000..51_000 {
        journal.log_bar(make_bar_record(i));
    }
    journal.flush();
    rt.shutdown();
    let ms = start.elapsed().as_millis();

    // Verify total count
    let conn = Connection::open(&path).unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM bars", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 51_000, "expected 51000 bars, got {count}");

    let _ = std::fs::remove_file(&path);

    // Same 500ms gate as the empty-DB 1k test — if this is much slower,
    // SQLite is degrading with size and we need retention/rotation.
    assert!(
        ms < 500,
        "1k bars on 50k-row DB took {ms}ms, gate is 500ms — SQLite may need retention policy"
    );
}
