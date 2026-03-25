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

fn bench_thompson_sample_20_arms(c: &mut Criterion) {
    use pair_picker::thompson::{ArmState, ThompsonState};

    let mut state = ThompsonState::new();
    for i in 0..20 {
        let pair_id = format!("A{i}/B{i}");
        let score = 0.5 + (i as f64) * 0.025;
        state.get_or_create(&pair_id, score);
        // Add some trade history to half the arms
        if i % 2 == 0 {
            state.update_pair(&pair_id, &[10.0, 15.0, -5.0, 20.0, 8.0], score);
        }
    }

    c.bench_function("thompson_sample_20_arms", |b| {
        let mut seed = 42u64;
        b.iter(|| {
            seed += 1;
            state.rank_pairs(black_box(seed))
        })
    });
}

fn bench_thompson_update(c: &mut Criterion) {
    use pair_picker::thompson::ArmState;

    c.bench_function("thompson_posterior_update", |b| {
        b.iter(|| {
            let mut arm = ArmState::from_quality_score(0.70);
            arm.update(black_box(&[15.0, -5.0, 20.0, 10.0, -8.0, 25.0, 12.0, -3.0]));
            arm
        })
    });
}

fn bench_priority_score(c: &mut Criterion) {
    use pair_picker::scorer::{compute_priority_score, PriorityConfig};

    let cfg = PriorityConfig::default();
    // Simulate 41 pairs firing simultaneously: rank by priority
    let pairs: Vec<(f64, f64, f64)> = (0..41)
        .map(|i| {
            let z = 2.0 + (i as f64) * 0.05;
            let kappa = f64::ln(2.0) / (5.0 + (i % 10) as f64); // 5-15 day HL
            let sigma = 0.01 + (i as f64) * 0.001;
            (z, kappa, sigma)
        })
        .collect();

    c.bench_function("priority_score_rank_41_pairs", |b| {
        b.iter(|| {
            let mut scores: Vec<f64> = pairs
                .iter()
                .map(|&(z, k, s)| compute_priority_score(black_box(z), k, s, &cfg))
                .collect();
            // Sort descending by priority (as the queue would do)
            scores.sort_by(|a, b| b.partial_cmp(a).unwrap());
            scores
        })
    });
}

fn bench_expected_return(c: &mut Criterion) {
    use pair_picker::scorer::expected_return_per_dollar_per_day;

    let pairs: Vec<(f64, f64, f64, f64)> = (0..41)
        .map(|i| {
            let z = 2.0 + (i as f64) * 0.05;
            let sigma = 0.01 + (i as f64) * 0.001;
            let kappa = f64::ln(2.0) / (5.0 + (i % 10) as f64);
            let hold = 5.0 + (i % 5) as f64;
            (z, sigma, kappa, hold)
        })
        .collect();

    c.bench_function("expected_return_41_pairs", |b| {
        b.iter(|| {
            let mut scores: Vec<f64> = pairs
                .iter()
                .map(|&(z, s, k, h)| expected_return_per_dollar_per_day(black_box(z), s, k, h))
                .collect();
            scores.sort_by(|a, b| b.partial_cmp(a).unwrap());
            scores
        })
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
    bench_thompson_sample_20_arms,
    bench_thompson_update,
    bench_priority_score,
    bench_expected_return,
);
criterion_main!(benches);
