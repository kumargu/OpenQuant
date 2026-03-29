//! Performance gate — runs in CI to catch major regressions.
//!
//! These are NOT micro-benchmarks (use `cargo bench` for that). These are
//! coarse-grained checks with generous thresholds (3x measured baseline)
//! to catch "something got 5x slower" regressions without CI flakiness.
//!
//! Baseline measured on Apple M4 (2026-03-15). CI runs on GitHub-hosted
//! Ubuntu runners which are ~2-3x slower, hence the 3x buffer.
//!
//! If a test fails, run `cargo bench` locally to get precise numbers.

use std::time::Instant;

use openquant_core::backtest;
use openquant_core::engine::{SingleEngine as Engine, SingleEngineConfig as EngineConfig};
use openquant_core::features::FeatureState;
use openquant_core::market_data::Bar;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Simple LCG random number generator (same as bench).
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

fn generate_bars(n: usize, seed: u64) -> Vec<Bar> {
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
            symbol: "BTCUSD".to_string(),
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

/// Run a closure N times, return median duration in nanoseconds.
fn median_ns(iterations: usize, mut f: impl FnMut()) -> f64 {
    // Warmup
    for _ in 0..iterations / 10 {
        f();
    }

    let mut times = Vec::with_capacity(iterations);
    for _ in 0..iterations {
        let start = Instant::now();
        f();
        times.push(start.elapsed().as_nanos() as f64);
    }
    times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    times[times.len() / 2]
}

// ---------------------------------------------------------------------------
// Gate tests — thresholds are 3x the measured baseline
// ---------------------------------------------------------------------------
//
// Baselines (Apple M4, 2026-03-19, includes V5 GARCH + V6 BOCPD):
//   feature_update:   ~6.5µs  → gate: 15µs
//   on_bar:           ~6.5µs  → gate: 15µs
//   backtest_1k:      ~6.5ms  → gate: 15ms
//   backtest_10k:     ~65ms   → gate: 150ms
//
// V5 (GJR-GARCH) and V6 (BOCPD regime detection) added ~6µs per bar.
// Previous baselines (8.9ns, 64ns) predate these features.
// Gates set at ~2x measured to catch regressions without flakiness.

#[test]
#[ignore] // only run via: cargo test --test bench_gate --release -- --ignored
fn gate_feature_update() {
    let bars = generate_bars(200, 42);
    let mut state = FeatureState::new();
    for b in &bars[..100] {
        state.update(b.close, b.high, b.low, b.volume, b.timestamp);
    }

    let mut idx = 100;
    let ns = median_ns(10_000, || {
        let bar = &bars[idx % bars.len()];
        std::hint::black_box(state.update(bar.close, bar.high, bar.low, bar.volume, bar.timestamp));
        idx += 1;
    });

    assert!(
        ns < 15_000.0,
        "feature_update took {ns:.0}ns, gate is 15µs (baseline ~6.5µs incl GARCH+BOCPD)"
    );
}

#[test]
#[ignore] // only run via: cargo test --test bench_gate --release -- --ignored
fn gate_on_bar() {
    let bars = generate_bars(200, 42);
    let config = EngineConfig::default();
    let mut engine = Engine::new(config);
    for b in &bars[..100] {
        engine.on_bar(b);
    }

    let mut idx = 100;
    let ns = median_ns(10_000, || {
        let bar = &bars[idx % bars.len()];
        std::hint::black_box(engine.on_bar(bar));
        idx += 1;
    });

    assert!(
        ns < 15_000.0,
        "on_bar took {ns:.0}ns, gate is 15µs (baseline ~6.5µs incl GARCH+BOCPD)"
    );
}

#[test]
#[ignore] // only run via: cargo test --test bench_gate --release -- --ignored
fn gate_backtest_1k() {
    let bars = generate_bars(1_000, 42);
    let config = EngineConfig::default();

    let ns = median_ns(100, || {
        std::hint::black_box(backtest::run(&bars, config.clone()));
    });
    let us = ns / 1_000.0;

    assert!(
        us < 15_000.0,
        "backtest_1k took {us:.0}µs, gate is 15ms (baseline ~6.5ms incl GARCH+BOCPD)"
    );
}

#[test]
#[ignore] // only run via: cargo test --test bench_gate --release -- --ignored
fn gate_backtest_10k() {
    let bars = generate_bars(10_000, 42);
    let config = EngineConfig::default();

    let ns = median_ns(20, || {
        std::hint::black_box(backtest::run(&bars, config.clone()));
    });
    let ms = ns / 1_000_000.0;

    assert!(
        ms < 150.0,
        "backtest_10k took {ms:.1}ms, gate is 150ms (baseline ~65ms incl GARCH+BOCPD)"
    );
}
