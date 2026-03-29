//! Integration load test — sustained bar throughput with latency measurement.
//!
//! Exercises the full Rust hot path (features + signals + risk + exit) under
//! sustained load and reports percentile latencies. Catches regressions that
//! micro-benchmarks miss: memory pressure, cache effects, allocation drift.
//!
//! Run:
//!   cargo test --test load_test --release -- --ignored --nocapture
//!
//! The test generates a summary table with p50/p95/p99/max latencies.

use std::time::{Duration, Instant};

use openquant_core::backtest;
use openquant_core::engine::{SingleEngine as Engine, SingleEngineConfig as EngineConfig};
use openquant_core::market_data::Bar;

// ---------------------------------------------------------------------------
// Helpers (same LCG as bench_gate for reproducibility)
// ---------------------------------------------------------------------------

struct Rng {
    state: u64,
}

impl Rng {
    fn new(seed: u64) -> Self {
        Self { state: seed }
    }
    fn next_f64(&mut self) -> f64 {
        self.state = self
            .state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (self.state >> 33) as f64 / (1u64 << 31) as f64
    }
    fn uniform(&mut self, lo: f64, hi: f64) -> f64 {
        lo + self.next_f64() * (hi - lo)
    }
}

fn generate_bars(symbol: &str, n: usize, seed: u64) -> Vec<Bar> {
    let mut rng = Rng::new(seed);
    let mut price = 100.0_f64;
    let mut bars = Vec::with_capacity(n);

    for i in 0..n {
        let ret = rng.uniform(-0.02, 0.02) + (100.0 - price) * 0.001;
        price *= 1.0 + ret;
        price = price.max(10.0);

        let range = price * rng.uniform(0.001, 0.01);
        let open = price + rng.uniform(-range, range) * 0.5;
        let high = open.max(price) + range * rng.uniform(0.0, 1.0);
        let low = open.min(price) - range * rng.uniform(0.0, 1.0);
        let volume = 1000.0 + rng.uniform(0.0, 2000.0);

        bars.push(Bar {
            symbol: symbol.to_string(),
            timestamp: 1700000000000 + (i as i64 * 60_000),
            open,
            high,
            low,
            close: price,
            volume,
        });
    }
    bars
}

fn generate_multi_symbol_bars(symbols: &[&str], n_per: usize, seed: u64) -> Vec<Bar> {
    let mut all: Vec<Bar> = Vec::with_capacity(symbols.len() * n_per);
    for (idx, sym) in symbols.iter().enumerate() {
        all.extend(generate_bars(sym, n_per, seed + idx as u64));
    }
    all.sort_by_key(|b| b.timestamp);
    all
}

#[allow(dead_code)]
struct LatencyStats {
    bars: usize,
    p50_ns: f64,
    p95_ns: f64,
    p99_ns: f64,
    max_ns: f64,
    mean_ns: f64,
    total_ms: f64,
}

fn measure_on_bar(engine: &mut Engine, bars: &[Bar]) -> LatencyStats {
    let mut latencies: Vec<u64> = Vec::with_capacity(bars.len());

    for bar in bars {
        let start = Instant::now();
        std::hint::black_box(engine.on_bar(bar));
        latencies.push(start.elapsed().as_nanos() as u64);
    }

    latencies.sort_unstable();
    let n = latencies.len();
    let sum: u64 = latencies.iter().sum();

    LatencyStats {
        bars: n,
        p50_ns: latencies[n / 2] as f64,
        p95_ns: latencies[(n as f64 * 0.95) as usize] as f64,
        p99_ns: latencies[(n as f64 * 0.99) as usize] as f64,
        max_ns: latencies[n - 1] as f64,
        mean_ns: sum as f64 / n as f64,
        total_ms: sum as f64 / 1_000_000.0,
    }
}

fn measure_on_bar_journaled(engine: &mut Engine, bars: &[Bar]) -> LatencyStats {
    let mut latencies: Vec<u64> = Vec::with_capacity(bars.len());

    for bar in bars {
        let start = Instant::now();
        std::hint::black_box(engine.on_bar_journaled(bar));
        latencies.push(start.elapsed().as_nanos() as u64);
    }

    latencies.sort_unstable();
    let n = latencies.len();
    let sum: u64 = latencies.iter().sum();

    LatencyStats {
        bars: n,
        p50_ns: latencies[n / 2] as f64,
        p95_ns: latencies[(n as f64 * 0.95) as usize] as f64,
        p99_ns: latencies[(n as f64 * 0.99) as usize] as f64,
        max_ns: latencies[n - 1] as f64,
        mean_ns: sum as f64 / n as f64,
        total_ms: sum as f64 / 1_000_000.0,
    }
}

fn print_stats(name: &str, s: &LatencyStats) {
    let throughput = s.bars as f64 / (s.total_ms / 1000.0);
    eprintln!(
        "  {:<25} {:>8} bars | p50={:>6.0}ns  p95={:>6.0}ns  p99={:>6.0}ns  max={:>8.0}ns | {:.0}k bars/s",
        name,
        s.bars,
        s.p50_ns,
        s.p95_ns,
        s.p99_ns,
        s.max_ns,
        throughput / 1000.0
    );
}

// ---------------------------------------------------------------------------
// Load test scenarios
// ---------------------------------------------------------------------------

/// Single symbol, 100k bars, fast path (no journal).
#[test]
#[ignore]
fn load_single_symbol_100k() {
    let bars = generate_bars("BTCUSD", 100_000, 42);
    let mut engine = Engine::new(EngineConfig::default());

    // Warmup
    for b in &bars[..100] {
        engine.on_bar(b);
    }

    let stats = measure_on_bar(&mut engine, &bars[100..]);
    eprintln!("\n=== Load Test: Single Symbol (100k bars, fast path) ===");
    print_stats("on_bar (fast)", &stats);

    // Gate: p99 under 5µs in release mode
    assert!(
        stats.p99_ns < 5_000.0,
        "p99 = {:.0}ns, expected < 5µs",
        stats.p99_ns
    );
}

/// Single symbol, 100k bars, journaled path.
#[test]
#[ignore]
fn load_single_symbol_journaled_100k() {
    let bars = generate_bars("BTCUSD", 100_000, 42);
    let mut engine = Engine::new(EngineConfig::default());

    for b in &bars[..100] {
        engine.on_bar_journaled(b);
    }

    let stats = measure_on_bar_journaled(&mut engine, &bars[100..]);
    eprintln!("\n=== Load Test: Single Symbol (100k bars, journaled path) ===");
    print_stats("on_bar_journaled", &stats);

    // Journaled path has more work (BarOutcome construction)
    assert!(
        stats.p99_ns < 10_000.0,
        "p99 = {:.0}ns, expected < 10µs",
        stats.p99_ns
    );
}

/// 10 symbols interleaved, 50k bars each (500k total).
#[test]
#[ignore]
fn load_multi_symbol_500k() {
    let symbols: Vec<&str> = (0..10)
        .map(|i| {
            // Static strings to avoid lifetime issues
            match i {
                0 => "SYM0",
                1 => "SYM1",
                2 => "SYM2",
                3 => "SYM3",
                4 => "SYM4",
                5 => "SYM5",
                6 => "SYM6",
                7 => "SYM7",
                8 => "SYM8",
                _ => "SYM9",
            }
        })
        .collect();
    let bars = generate_multi_symbol_bars(&symbols, 50_000, 42);
    let mut engine = Engine::new(EngineConfig::default());

    // Warmup all symbols
    for b in &bars[..1000] {
        engine.on_bar(b);
    }

    let stats = measure_on_bar(&mut engine, &bars[1000..]);
    eprintln!("\n=== Load Test: Multi-Symbol (10 symbols, 500k bars) ===");
    print_stats("on_bar (10 sym)", &stats);

    // Multi-symbol may have higher p99 due to HashMap lookups
    assert!(
        stats.p99_ns < 10_000.0,
        "p99 = {:.0}ns, expected < 10µs",
        stats.p99_ns
    );
}

/// Burst: steady state then sudden 10x bar rate.
/// Measures if latency degrades under burst.
#[test]
#[ignore]
fn load_burst() {
    let warmup = generate_bars("BTCUSD", 10_000, 1);
    let burst = generate_bars("BTCUSD", 200_000, 2);

    let mut engine = Engine::new(EngineConfig::default());

    // Steady state warmup
    for b in &warmup {
        engine.on_bar(b);
    }
    let warmup_stats = measure_on_bar(&mut engine, &warmup[100..]);

    // Burst
    let burst_stats = measure_on_bar(&mut engine, &burst);

    eprintln!("\n=== Load Test: Burst (10k steady → 200k burst) ===");
    print_stats("steady state", &warmup_stats);
    print_stats("burst (200k)", &burst_stats);

    // Burst p99 should not be more than 3x steady p99
    let ratio = burst_stats.p99_ns / warmup_stats.p99_ns.max(1.0);
    eprintln!("  Burst/steady p99 ratio: {ratio:.1}x");
    assert!(
        ratio < 3.0,
        "burst p99 {:.0}ns is {ratio:.1}x steady {:.0}ns (limit 3x)",
        burst_stats.p99_ns,
        warmup_stats.p99_ns
    );
}

/// Full backtest throughput: how fast can we replay 1M bars?
#[test]
#[ignore]
fn load_backtest_1m() {
    let bars = generate_bars("BTCUSD", 1_000_000, 42);
    let config = EngineConfig::default();

    let start = Instant::now();
    let result = backtest::run(&bars, config);
    let elapsed = start.elapsed();

    let throughput = bars.len() as f64 / elapsed.as_secs_f64();
    eprintln!("\n=== Load Test: Backtest 1M bars ===");
    eprintln!(
        "  {} bars in {:.1}ms = {:.0}k bars/sec | {} trades",
        bars.len(),
        elapsed.as_secs_f64() * 1000.0,
        throughput / 1000.0,
        result.total_trades
    );

    // Should process 1M bars in under 30 seconds (even on slow CI)
    assert!(
        elapsed < Duration::from_secs(30),
        "1M backtest took {:.1}s, expected < 30s",
        elapsed.as_secs_f64()
    );
}

/// Summary: runs all scenarios and prints a combined table.
#[test]
#[ignore]
fn load_test_summary() {
    eprintln!("\n{}", "=".repeat(60));
    eprintln!("  OpenQuant Load Test Summary (Rust hot path)");
    eprintln!("{}\n", "=".repeat(60));

    // Single symbol 100k
    let bars_100k = generate_bars("BTCUSD", 100_000, 42);
    let mut engine = Engine::new(EngineConfig::default());
    for b in &bars_100k[..100] {
        engine.on_bar(b);
    }
    let single = measure_on_bar(&mut engine, &bars_100k[100..]);

    // Journaled
    let mut engine2 = Engine::new(EngineConfig::default());
    for b in &bars_100k[..100] {
        engine2.on_bar_journaled(b);
    }
    let journaled = measure_on_bar_journaled(&mut engine2, &bars_100k[100..]);

    // Multi-symbol
    let symbols = &["S0", "S1", "S2", "S3", "S4", "S5", "S6", "S7", "S8", "S9"];
    let multi_bars = generate_multi_symbol_bars(symbols, 50_000, 42);
    let mut engine3 = Engine::new(EngineConfig::default());
    for b in &multi_bars[..1000] {
        engine3.on_bar(b);
    }
    let multi = measure_on_bar(&mut engine3, &multi_bars[1000..]);

    // Backtest 1M
    let bars_1m = generate_bars("BTCUSD", 1_000_000, 42);
    let start = Instant::now();
    let bt_result = backtest::run(&bars_1m, EngineConfig::default());
    let bt_elapsed = start.elapsed();

    eprintln!(
        "  {:<25} {:>8} | {:>8} | {:>8} | {:>8} | {:>10}",
        "Scenario", "p50 (ns)", "p95 (ns)", "p99 (ns)", "max (ns)", "throughput"
    );
    eprintln!(
        "  {:-<25} {:-<8}-+-{:-<8}-+-{:-<8}-+-{:-<8}-+-{:-<10}",
        "", "", "", "", "", ""
    );

    for (name, s) in [
        ("single (100k)", &single),
        ("journaled (100k)", &journaled),
        ("multi-sym (500k)", &multi),
    ] {
        let tp = s.bars as f64 / (s.total_ms / 1000.0);
        eprintln!(
            "  {:<25} {:>8.0} | {:>8.0} | {:>8.0} | {:>8.0} | {:>7.0}k/s",
            name,
            s.p50_ns,
            s.p95_ns,
            s.p99_ns,
            s.max_ns,
            tp / 1000.0
        );
    }

    let bt_tp = 1_000_000.0 / bt_elapsed.as_secs_f64();
    eprintln!(
        "  {:<25} {:>8} | {:>8} | {:>8} | {:>8} | {:>7.0}k/s",
        "backtest (1M)",
        "-",
        "-",
        "-",
        "-",
        bt_tp / 1000.0
    );

    eprintln!(
        "\n  Backtest 1M: {:.1}ms, {} trades",
        bt_elapsed.as_secs_f64() * 1000.0,
        bt_result.total_trades
    );
}
