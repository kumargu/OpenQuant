//! Benchmark the overhead of metrics operations.
//!
//! Measures three scenarios:
//! 1. **Noop** — no recorder installed (production-disabled path)
//! 2. **Active recorder** — JSONL recorder installed, metrics flowing
//! 3. **Simulated hot path** — counter + histogram per "bar" (what on_bar will look like)
//!
//! These numbers set the overhead budget. The noop path must be <5ns.
//! The active path must be <200ns total for all metrics per bar.
//!
//! Run: `cd engine && cargo bench --bench metrics_overhead`

use std::sync::Once;
use std::time::Duration;

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use metrics::{counter, gauge, histogram};
use openquant_metrics::SymbolMetrics;

// We can only install a global recorder once per process, so we use
// criterion's benchmark groups carefully.

static INSTALL_RECORDER: Once = Once::new();

fn ensure_recorder() {
    INSTALL_RECORDER.call_once(|| {
        // Build a Tokio runtime for the recorder's flush task
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();

        // Install from within the runtime context
        rt.block_on(async {
            openquant_metrics::install(
                "/tmp/oq-bench-metrics",
                Duration::from_secs(3600), // very long interval — don't flush during bench
            )
            .expect("failed to install recorder");
        });

        // Leak the runtime so the recorder's flush task stays alive
        std::mem::forget(rt);
    });
}

// ---------------------------------------------------------------------------
// Noop benchmarks (no recorder installed — run BEFORE ensure_recorder)
// ---------------------------------------------------------------------------
// NOTE: Because criterion runs all groups in one process and we can only
// install a recorder once, the "noop" benchmarks are only truly noop
// on the FIRST run before ensure_recorder() is called. To get clean
// noop numbers, run: `cargo bench --bench metrics_overhead -- noop`
// in a fresh process (before any "active" bench warms up the recorder).

fn bench_noop_counter(c: &mut Criterion) {
    // Don't install recorder — this measures the noop atomic load path
    c.bench_function("noop_counter_increment", |b| {
        b.iter(|| {
            counter!("bench.noop.counter", "symbol" => "BTCUSD").increment(1);
        })
    });
}

fn bench_noop_histogram(c: &mut Criterion) {
    c.bench_function("noop_histogram_record", |b| {
        b.iter(|| {
            histogram!("bench.noop.histogram", "symbol" => "BTCUSD").record(black_box(63.0));
        })
    });
}

// ---------------------------------------------------------------------------
// Active recorder benchmarks
// ---------------------------------------------------------------------------

fn bench_active_counter(c: &mut Criterion) {
    ensure_recorder();

    c.bench_function("active_counter_increment", |b| {
        b.iter(|| {
            counter!("bench.active.counter", "symbol" => "BTCUSD").increment(1);
        })
    });
}

fn bench_active_gauge(c: &mut Criterion) {
    ensure_recorder();

    c.bench_function("active_gauge_set", |b| {
        b.iter(|| {
            gauge!("bench.active.gauge").set(black_box(42.0));
        })
    });
}

fn bench_active_histogram(c: &mut Criterion) {
    ensure_recorder();

    c.bench_function("active_histogram_record", |b| {
        b.iter(|| {
            histogram!("bench.active.histogram", "symbol" => "BTCUSD").record(black_box(63.0));
        })
    });
}

// ---------------------------------------------------------------------------
// Simulated hot-path: all metrics that on_bar() will emit
// ---------------------------------------------------------------------------

fn bench_simulated_on_bar_metrics(c: &mut Criterion) {
    ensure_recorder();

    // Simulate the metrics that on_bar() will emit per bar:
    // 1 counter (bars_processed) + 3 histograms (on_bar duration, z_score, rel_volume)
    // This is the total overhead budget.
    c.bench_function("simulated_on_bar_all_metrics", |b| {
        b.iter(|| {
            // Counter: bars processed
            counter!("engine.bars_processed", "symbol" => "BTCUSD").increment(1);

            // Timer: on_bar duration (recorded as histogram)
            histogram!("engine.on_bar.duration_ns", "symbol" => "BTCUSD").record(black_box(63.0));

            // Feature distribution: z-score
            histogram!("features.z_score", "symbol" => "BTCUSD").record(black_box(-1.2));

            // Feature distribution: relative volume
            histogram!("features.relative_volume", "symbol" => "BTCUSD").record(black_box(1.4));

            // Signal counter (fires ~1% of bars, but we measure the cost)
            counter!("signal.fired", "symbol" => "BTCUSD", "side" => "buy").increment(1);

            // Risk check counter
            counter!("risk.passed", "symbol" => "BTCUSD").increment(1);
        })
    });
}

// ---------------------------------------------------------------------------
// Cached-handle benchmarks: pre-register and reuse handles
// ---------------------------------------------------------------------------

fn bench_cached_counter(c: &mut Criterion) {
    ensure_recorder();

    // Cache the handle once — subsequent calls skip registry lookup
    let ctr = counter!("bench.cached.counter", "symbol" => "BTCUSD");

    c.bench_function("cached_counter_increment", |b| {
        b.iter(|| {
            ctr.increment(1);
        })
    });
}

fn bench_cached_histogram(c: &mut Criterion) {
    ensure_recorder();

    let hist = histogram!("bench.cached.histogram", "symbol" => "BTCUSD");

    c.bench_function("cached_histogram_record", |b| {
        b.iter(|| {
            hist.record(black_box(63.0));
        })
    });
}

fn bench_cached_on_bar_metrics(c: &mut Criterion) {
    ensure_recorder();

    // Pre-register all handles — what on_bar() should do
    let bars_processed = counter!("engine.bars_processed", "symbol" => "BTCUSD");
    let duration_hist = histogram!("engine.on_bar.duration_ns", "symbol" => "BTCUSD");
    let z_score_hist = histogram!("features.z_score", "symbol" => "BTCUSD");
    let rel_vol_hist = histogram!("features.relative_volume", "symbol" => "BTCUSD");
    let signal_ctr = counter!("signal.fired", "symbol" => "BTCUSD", "side" => "buy");
    let risk_ctr = counter!("risk.passed", "symbol" => "BTCUSD");

    c.bench_function("cached_on_bar_all_metrics", |b| {
        b.iter(|| {
            bars_processed.increment(1);
            duration_hist.record(black_box(63.0));
            z_score_hist.record(black_box(-1.2));
            rel_vol_hist.record(black_box(1.4));
            signal_ctr.increment(1);
            risk_ctr.increment(1);
        })
    });
}

// ---------------------------------------------------------------------------
// High-throughput stress test: 10k metric operations
// ---------------------------------------------------------------------------

fn bench_burst_10k_metrics(c: &mut Criterion) {
    ensure_recorder();

    c.bench_function("burst_10k_counter_increments", |b| {
        b.iter(|| {
            for _ in 0..10_000 {
                counter!("bench.burst.counter", "symbol" => "BTCUSD").increment(1);
            }
        })
    });
}

fn bench_symbol_metrics_on_bar(c: &mut Criterion) {
    ensure_recorder();

    let sm = SymbolMetrics::new("BTCUSD");

    c.bench_function("symbol_metrics_on_bar", |b| {
        b.iter(|| {
            sm.bars_processed.increment(1);
            sm.on_bar_duration_ns.record(black_box(63.0));
            sm.z_score.record(black_box(-1.2));
            sm.relative_volume.record(black_box(1.4));
            sm.signal_buy.increment(1);
            sm.risk_passed.increment(1);
        })
    });
}

fn bench_burst_10k_cached(c: &mut Criterion) {
    ensure_recorder();

    let ctr = counter!("bench.burst.cached", "symbol" => "BTCUSD");

    c.bench_function("burst_10k_cached_increments", |b| {
        b.iter(|| {
            for _ in 0..10_000 {
                ctr.increment(1);
            }
        })
    });
}

// Run noop group first (before recorder is installed), then active group
criterion_group!(noop, bench_noop_counter, bench_noop_histogram,);
criterion_group!(
    active,
    bench_active_counter,
    bench_active_gauge,
    bench_active_histogram,
    bench_simulated_on_bar_metrics,
    bench_cached_counter,
    bench_cached_histogram,
    bench_cached_on_bar_metrics,
    bench_symbol_metrics_on_bar,
    bench_burst_10k_metrics,
    bench_burst_10k_cached,
);
criterion_main!(noop, active);
