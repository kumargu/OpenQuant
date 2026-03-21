//! Benchmarks for statistical computations in pair-picker.

use criterion::{black_box, criterion_group, criterion_main, Criterion};
use pair_picker::pipeline::{validate_pair, InMemoryPrices};
use pair_picker::stats::adf::adf_test;
use pair_picker::stats::beta_stability::check_beta_stability;
use pair_picker::stats::halflife::estimate_half_life;
use pair_picker::stats::ols::ols_simple;
use pair_picker::types::PairCandidate;
use std::collections::HashMap;

/// Deterministic noise generator.
fn lcg_noise(state: &mut u64, scale: f64) -> f64 {
    *state = state
        .wrapping_mul(6364136223846793005)
        .wrapping_add(1442695040888963407);
    ((*state >> 33) as f64 / u32::MAX as f64 - 0.5) * scale
}

/// Generate a cointegrated pair with n data points.
fn gen_cointegrated(n: usize) -> (Vec<f64>, Vec<f64>) {
    let phi = (-f64::ln(2.0) / 10.0_f64).exp();
    let mut state: u64 = 42;
    let mut log_b = Vec::with_capacity(n);
    let mut b_val = 4.0;
    for _ in 0..n {
        b_val += lcg_noise(&mut state, 0.02);
        log_b.push(b_val);
    }
    let mut spread = 0.0;
    let mut log_a = Vec::with_capacity(n);
    for i in 0..n {
        spread = phi * spread + lcg_noise(&mut state, 0.01);
        log_a.push(1.5 * log_b[i] + 1.0 + spread);
    }
    let pa: Vec<f64> = log_a.iter().map(|x| x.exp()).collect();
    let pb: Vec<f64> = log_b.iter().map(|x| x.exp()).collect();
    (pa, pb)
}

fn bench_ols_200(c: &mut Criterion) {
    let (pa, pb) = gen_cointegrated(200);
    let log_a: Vec<f64> = pa.iter().map(|p| p.ln()).collect();
    let log_b: Vec<f64> = pb.iter().map(|p| p.ln()).collect();

    c.bench_function("ols_simple_200", |b| {
        b.iter(|| ols_simple(black_box(&log_b), black_box(&log_a)))
    });
}

fn bench_ols_500(c: &mut Criterion) {
    let (pa, pb) = gen_cointegrated(500);
    let log_a: Vec<f64> = pa.iter().map(|p| p.ln()).collect();
    let log_b: Vec<f64> = pb.iter().map(|p| p.ln()).collect();

    c.bench_function("ols_simple_500", |b| {
        b.iter(|| ols_simple(black_box(&log_b), black_box(&log_a)))
    });
}

fn bench_adf_200(c: &mut Criterion) {
    let (pa, pb) = gen_cointegrated(200);
    let log_a: Vec<f64> = pa.iter().map(|p| p.ln()).collect();
    let log_b: Vec<f64> = pb.iter().map(|p| p.ln()).collect();
    let ols = ols_simple(&log_b, &log_a).unwrap();
    let spread: Vec<f64> = log_a
        .iter()
        .zip(log_b.iter())
        .map(|(a, b)| a - ols.beta * b)
        .collect();

    c.bench_function("adf_test_200", |b| {
        b.iter(|| adf_test(black_box(&spread), None, true))
    });
}

fn bench_adf_500(c: &mut Criterion) {
    let (pa, pb) = gen_cointegrated(500);
    let log_a: Vec<f64> = pa.iter().map(|p| p.ln()).collect();
    let log_b: Vec<f64> = pb.iter().map(|p| p.ln()).collect();
    let ols = ols_simple(&log_b, &log_a).unwrap();
    let spread: Vec<f64> = log_a
        .iter()
        .zip(log_b.iter())
        .map(|(a, b)| a - ols.beta * b)
        .collect();

    c.bench_function("adf_test_500", |b| {
        b.iter(|| adf_test(black_box(&spread), None, true))
    });
}

fn bench_halflife_500(c: &mut Criterion) {
    let (pa, pb) = gen_cointegrated(500);
    let log_a: Vec<f64> = pa.iter().map(|p| p.ln()).collect();
    let log_b: Vec<f64> = pb.iter().map(|p| p.ln()).collect();
    let ols = ols_simple(&log_b, &log_a).unwrap();
    let spread: Vec<f64> = log_a
        .iter()
        .zip(log_b.iter())
        .map(|(a, b)| a - ols.beta * b)
        .collect();

    c.bench_function("halflife_500", |b| {
        b.iter(|| estimate_half_life(black_box(&spread)))
    });
}

fn bench_beta_stability_500(c: &mut Criterion) {
    let (pa, pb) = gen_cointegrated(500);
    let log_a: Vec<f64> = pa.iter().map(|p| p.ln()).collect();
    let log_b: Vec<f64> = pb.iter().map(|p| p.ln()).collect();

    c.bench_function("beta_stability_500", |b| {
        b.iter(|| check_beta_stability(black_box(&log_a), black_box(&log_b)))
    });
}

fn bench_full_pipeline_single_pair(c: &mut Criterion) {
    let (pa, pb) = gen_cointegrated(500);
    let provider = InMemoryPrices {
        data: HashMap::from([("A".to_string(), pa), ("B".to_string(), pb)]),
    };
    let candidate = PairCandidate {
        leg_a: "A".into(),
        leg_b: "B".into(),
        economic_rationale: "benchmark".into(),
    };

    c.bench_function("validate_pair_full_500", |b| {
        b.iter(|| validate_pair(black_box(&candidate), &provider))
    });
}

criterion_group!(
    benches,
    bench_ols_200,
    bench_ols_500,
    bench_adf_200,
    bench_adf_500,
    bench_halflife_500,
    bench_beta_stability_500,
    bench_full_pipeline_single_pair,
);
criterion_main!(benches);
