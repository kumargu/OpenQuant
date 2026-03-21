//! Criterion benchmarks for the trading hot path.
//!
//! Measures per-bar latency for each component and end-to-end.
//! Run: `cd engine && cargo bench`
//! Compare: `cargo bench -- --save-baseline main` then `cargo bench -- --baseline main`

use criterion::{Criterion, black_box, criterion_group, criterion_main};

use openquant_core::backtest;
use openquant_core::engine::{Engine, EngineConfig};
use openquant_core::exit::{ExitConfig, OpenPosition};
use openquant_core::features::FeatureState;
use openquant_core::features::regime::MarketRegime;
use openquant_core::market_data::Bar;
use openquant_core::pairs::{PairConfig, PairState};
use openquant_core::risk::{self, BayesianKellyState, RiskConfig, RiskState};
use openquant_core::signals::mean_reversion::{Config as SignalConfig, MeanReversion};
use openquant_core::signals::{Side, SignalOutput, SignalReason, Strategy};

// ---------------------------------------------------------------------------
// Synthetic data generator (deterministic, reproducible)
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
            symbol: "TEST".to_string(),
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

// ---------------------------------------------------------------------------
// Benchmarks
// ---------------------------------------------------------------------------

fn bench_feature_update(c: &mut Criterion) {
    let bars = generate_bars(200, 42);
    let mut state = FeatureState::new();
    for b in &bars[..100] {
        state.update(b.close, b.high, b.low, b.volume, b.timestamp);
    }

    let mut idx = 100;
    c.bench_function("feature_update", |b| {
        b.iter(|| {
            let bar = &bars[idx % bars.len()];
            black_box(state.update(bar.close, bar.high, bar.low, bar.volume, bar.timestamp));
            idx += 1;
        })
    });
}

fn bench_signal_no_fire(c: &mut Criterion) {
    let strategy = MeanReversion::new(SignalConfig::default());
    let features = openquant_core::features::FeatureValues {
        return_z_score: 0.0,
        relative_volume: 1.0,
        warmed_up: true,
        trend_up: true,
        ..Default::default()
    };

    c.bench_function("signal_eval_no_fire", |b| {
        b.iter(|| black_box(strategy.score(black_box(&features), false)))
    });
}

fn bench_signal_buy_fire(c: &mut Criterion) {
    let strategy = MeanReversion::new(SignalConfig::default());
    let features = openquant_core::features::FeatureValues {
        return_z_score: -2.8,
        relative_volume: 1.8,
        warmed_up: true,
        trend_up: true,
        sma_20: 100.0,
        sma_50: 99.0,
        atr: 1.5,
        return_1: -0.03,
        return_std_20: 0.01,
        ..Default::default()
    };

    c.bench_function("signal_eval_buy_fire", |b| {
        b.iter(|| black_box(strategy.score(black_box(&features), false)))
    });
}

fn bench_risk_check_pass(c: &mut Criterion) {
    let config = RiskConfig::default();
    let state = RiskState::new();
    let kelly = BayesianKellyState::new(config.kelly_prior_wins, config.kelly_prior_losses);
    let signal = SignalOutput {
        side: Side::Buy,
        score: 1.5,
        reason: SignalReason::MeanReversionBuy,
        z_score: -2.8,
        relative_volume: 1.8,
        votes: String::new(),
    };

    c.bench_function("risk_check_pass", |b| {
        b.iter(|| {
            black_box(risk::check(
                &signal,
                100.0,
                0.0,
                &state,
                &kelly,
                MarketRegime::Normal,
                &config,
            ))
        })
    });
}

fn bench_exit_check_no_trigger(c: &mut Criterion) {
    let config = ExitConfig::default();
    let pos = OpenPosition {
        symbol: "TEST".to_string(),
        entry_price: 100.0,
        qty: 1.0,
        entry_bar: 0,
    };

    c.bench_function("exit_check_no_trigger", |b| {
        b.iter(|| {
            black_box(openquant_core::exit::check(
                &pos,
                black_box(101.0),
                50,
                1.5,
                0.01,
                &config,
            ))
        })
    });
}

fn bench_on_bar_no_signal(c: &mut Criterion) {
    let bars = generate_bars(200, 42);
    let config = EngineConfig::default();
    let mut engine = Engine::new(config);
    for b in &bars[..100] {
        engine.on_bar(b);
    }

    let mut idx = 100;
    c.bench_function("on_bar_no_signal", |b| {
        b.iter(|| {
            let bar = &bars[idx % bars.len()];
            black_box(engine.on_bar(black_box(bar)));
            idx += 1;
        })
    });
}

fn bench_on_bar_journaled(c: &mut Criterion) {
    let bars = generate_bars(200, 42);
    let config = EngineConfig::default();
    let mut engine = Engine::new(config);
    for b in &bars[..100] {
        engine.on_bar_journaled(b);
    }

    let mut idx = 100;
    c.bench_function("on_bar_journaled", |b| {
        b.iter(|| {
            let bar = &bars[idx % bars.len()];
            black_box(engine.on_bar_journaled(black_box(bar)));
            idx += 1;
        })
    });
}

// ---------------------------------------------------------------------------
// Pairs trading benchmarks
// ---------------------------------------------------------------------------

fn bench_pairs_on_price_no_signal(c: &mut Criterion) {
    let config = PairConfig {
        leg_a: "A".into(),
        leg_b: "B".into(),
        beta: 1.0,
        entry_z: 2.0,
        exit_z: 0.5,
        stop_z: 4.0,
        lookback: 32,
        max_hold_bars: 150,
        notional_per_leg: 10_000.0,
    };
    let mut state = PairState::new();

    // Warmup
    for i in 0..40 {
        state.on_price("A", 100.0 + (i as f64) * 0.01, &config);
        state.on_price("B", 100.0 + (i as f64) * 0.01, &config);
    }

    let mut idx = 0;
    c.bench_function("pairs_on_price_no_signal", |b| {
        b.iter(|| {
            // Small price variation — no signal expected
            let offset = (idx % 100) as f64 * 0.001;
            black_box(state.on_price("A", 100.0 + offset, &config));
            black_box(state.on_price("B", 100.0 + offset, &config));
            idx += 1;
        })
    });
}

fn bench_pairs_on_price_with_signal(c: &mut Criterion) {
    let config = PairConfig {
        leg_a: "A".into(),
        leg_b: "B".into(),
        beta: 1.0,
        entry_z: 1.5,
        exit_z: 0.3,
        stop_z: 5.0,
        lookback: 32,
        max_hold_bars: 150,
        notional_per_leg: 10_000.0,
    };

    c.bench_function("pairs_on_price_with_signal", |b| {
        b.iter(|| {
            let mut state = PairState::new();
            // Warmup
            for _ in 0..35 {
                state.on_price("A", 100.0, &config);
                state.on_price("B", 100.0, &config);
            }
            // Trigger entry
            state.on_price("A", 90.0, &config);
            let entry = state.on_price("B", 100.0, &config);
            black_box(&entry);
            // Trigger exit
            state.on_price("A", 100.0, &config);
            let exit = state.on_price("B", 100.0, &config);
            black_box(&exit);
        })
    });
}

fn bench_backtest_1k(c: &mut Criterion) {
    let bars = generate_bars(1_000, 42);
    let config = EngineConfig::default();

    c.bench_function("backtest_1k_bars", |b| {
        b.iter(|| black_box(backtest::run(black_box(&bars), config.clone())))
    });
}

fn bench_backtest_10k(c: &mut Criterion) {
    let bars = generate_bars(10_000, 42);
    let config = EngineConfig::default();

    c.bench_function("backtest_10k_bars", |b| {
        b.iter(|| black_box(backtest::run(black_box(&bars), config.clone())))
    });
}

criterion_group!(
    benches,
    bench_feature_update,
    bench_signal_no_fire,
    bench_signal_buy_fire,
    bench_risk_check_pass,
    bench_exit_check_no_trigger,
    bench_on_bar_no_signal,
    bench_on_bar_journaled,
    bench_pairs_on_price_no_signal,
    bench_pairs_on_price_with_signal,
    bench_backtest_1k,
    bench_backtest_10k,
);
criterion_main!(benches);
