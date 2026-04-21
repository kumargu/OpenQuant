//! Criterion benchmarks for basket-picker.

use std::collections::HashMap;

use criterion::{black_box, criterion_group, criterion_main, Criterion};

use basket_picker::{
    fit_ou_ar1, optimize_symmetric_thresholds, validate, BasketCandidate, OuFit, ValidatorConfig,
};

/// Generate synthetic price data.
fn make_prices(n: usize, seed: u64) -> Vec<f64> {
    let mut state = seed;
    let mut prices = Vec::with_capacity(n);
    let mut price = 100.0;
    for _ in 0..n {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
        let r = ((state >> 33) as f64 / (1u64 << 31) as f64) - 0.5;
        price *= 1.0 + 0.005 * r;
        prices.push(price);
    }
    prices
}

/// Generate synthetic spread data (mean-reverting).
fn make_spread(n: usize, seed: u64) -> Vec<f64> {
    let mut state = seed;
    let kappa = 0.1;
    let mu = 0.0;
    let sigma = 0.02;
    let dt: f64 = 1.0 / 252.0;

    let mut spread = Vec::with_capacity(n);
    let mut x = 0.0;
    for _ in 0..n {
        spread.push(x);
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
        let z = ((state >> 33) as f64 / (1u64 << 31) as f64) * 2.0 - 1.0;
        x += kappa * (mu - x) * dt + sigma * dt.sqrt() * z;
    }
    spread
}

fn bench_fit_ou_ar1(c: &mut Criterion) {
    let spread = make_spread(252, 12345);

    c.bench_function("fit_ou_ar1_252_bars", |b| {
        b.iter(|| fit_ou_ar1(black_box(&spread)))
    });

    let spread_1000 = make_spread(1000, 12345);
    c.bench_function("fit_ou_ar1_1000_bars", |b| {
        b.iter(|| fit_ou_ar1(black_box(&spread_1000)))
    });
}

fn bench_bertram(c: &mut Criterion) {
    let ou = OuFit {
        a: 0.001,
        b: 0.95,
        kappa: 12.92,
        mu: 0.02,
        sigma: 0.01,
        sigma_eq: 0.032,
        half_life_days: 13.51,
    };

    c.bench_function("optimize_symmetric_thresholds", |b| {
        b.iter(|| optimize_symmetric_thresholds(black_box(&ou), black_box(0.0005)))
    });
}

fn bench_validate_single_basket(c: &mut Criterion) {
    let candidate = BasketCandidate {
        target: "AAPL".to_string(),
        members: vec![
            "MSFT".to_string(),
            "GOOGL".to_string(),
            "META".to_string(),
            "AMZN".to_string(),
            "NVDA".to_string(),
        ],
        sector: "faang".to_string(),
        fit_date: chrono::NaiveDate::from_ymd_opt(2026, 4, 20).unwrap(),
    };

    let mut bars = HashMap::new();
    bars.insert("AAPL".to_string(), make_prices(100, 1));
    bars.insert("MSFT".to_string(), make_prices(100, 2));
    bars.insert("GOOGL".to_string(), make_prices(100, 3));
    bars.insert("META".to_string(), make_prices(100, 4));
    bars.insert("AMZN".to_string(), make_prices(100, 5));
    bars.insert("NVDA".to_string(), make_prices(100, 6));

    let config = ValidatorConfig::default();

    c.bench_function("validate_single_basket_6_members", |b| {
        b.iter(|| validate(black_box(&candidate), black_box(&bars), black_box(&config)))
    });
}

criterion_group!(
    benches,
    bench_fit_ou_ar1,
    bench_bertram,
    bench_validate_single_basket
);
criterion_main!(benches);
