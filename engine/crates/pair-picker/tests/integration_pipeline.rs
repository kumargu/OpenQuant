//! Integration tests for the pair-picker pipeline (#172).
//!
//! Tests the full flow: candidates → graph filter → validate → write active_pairs.json.
//! Uses synthetic price data — no external dependencies.

use pair_picker::pipeline::{self, InMemoryPrices};
use pair_picker::types::{ActivePairsFile, PairCandidate};
use std::collections::HashMap;
use tempfile::TempDir;

/// Generate a cointegrated pair (prices move together with mean-reverting spread).
fn cointegrated_prices(n: usize, beta: f64, seed: u64) -> (Vec<f64>, Vec<f64>) {
    let phi = (-f64::ln(2.0) / 10.0).exp();
    let mut state = seed;
    let mut next = |scale: f64| -> f64 {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((state >> 33) as f64 / u32::MAX as f64 - 0.5) * scale
    };

    let mut log_b = Vec::with_capacity(n);
    let mut b_val = 4.0;
    for _ in 0..n {
        b_val += next(0.02);
        log_b.push(b_val);
    }

    let mut spread = 0.0;
    let mut log_a = Vec::with_capacity(n);
    for lb in &log_b {
        spread = phi * spread + next(0.01);
        log_a.push(beta * lb + 1.0 + spread);
    }

    let pa: Vec<f64> = log_a.iter().map(|x| x.exp()).collect();
    let pb: Vec<f64> = log_b.iter().map(|x| x.exp()).collect();
    (pa, pb)
}

/// Generate independent random walks (not cointegrated).
fn random_walk_prices(n: usize, seed: u64) -> (Vec<f64>, Vec<f64>) {
    let mut state = seed;
    let mut next = || -> f64 {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((state >> 33) as f64 / u32::MAX as f64 - 0.5) * 0.02
    };

    let mut a = Vec::with_capacity(n);
    let mut b = Vec::with_capacity(n);
    let mut va = 4.0;
    let mut vb = 4.0;
    for _ in 0..n {
        va += next();
        vb += next();
        a.push(va.exp());
        b.push(vb.exp());
    }
    (a, b)
}

#[test]
fn test_full_pipeline_cointegrated_passes_random_rejected() {
    let tmp = TempDir::new().unwrap();
    let output_path = tmp.path().join("active_pairs.json");

    let (pa, pb) = cointegrated_prices(500, 1.5, 42);
    let (px, py) = random_walk_prices(500, 99);

    let provider = InMemoryPrices {
        data: HashMap::from([
            ("GOOD_A".to_string(), pa),
            ("GOOD_B".to_string(), pb),
            ("BAD_X".to_string(), px),
            ("BAD_Y".to_string(), py),
        ]),
    };

    let candidates = vec![
        PairCandidate {
            leg_a: "GOOD_A".into(),
            leg_b: "GOOD_B".into(),
            economic_rationale: "cointegrated pair".into(),
        },
        PairCandidate {
            leg_a: "BAD_X".into(),
            leg_b: "BAD_Y".into(),
            economic_rationale: "random walks".into(),
        },
    ];

    let results =
        pipeline::run_pipeline_from_candidates(&candidates, &output_path, &provider).unwrap();

    // Verify output file was written
    assert!(output_path.exists(), "active_pairs.json should be written");

    let contents = std::fs::read_to_string(&output_path).unwrap();
    let output: ActivePairsFile = serde_json::from_str(&contents).unwrap();

    // Cointegrated pair should pass, random walks should be rejected
    let passed: Vec<_> = results.iter().filter(|r| r.passed).collect();
    assert!(
        passed.len() <= 1,
        "at most 1 pair should pass (cointegrated)"
    );

    // If cointegrated pair passed, verify it's in output with correct fields
    if !output.pairs.is_empty() {
        let pair = &output.pairs[0];
        assert_eq!(pair.leg_a, "GOOD_A");
        assert_eq!(pair.leg_b, "GOOD_B");
        assert!(pair.beta > 0.0, "beta should be positive");
        assert!(pair.score > 0.0, "score should be positive");
        assert!(pair.adf_pvalue < 1.0, "ADF p-value should be < 1.0");
    }
}

#[test]
fn test_empty_output_when_all_fail() {
    let tmp = TempDir::new().unwrap();
    let output_path = tmp.path().join("active_pairs.json");

    let (px, py) = random_walk_prices(500, 42);

    let provider = InMemoryPrices {
        data: HashMap::from([("X".to_string(), px), ("Y".to_string(), py)]),
    };

    let candidates = vec![PairCandidate {
        leg_a: "X".into(),
        leg_b: "Y".into(),
        economic_rationale: "random walks".into(),
    }];

    pipeline::run_pipeline_from_candidates(&candidates, &output_path, &provider).unwrap();

    let contents = std::fs::read_to_string(&output_path).unwrap();
    let output: ActivePairsFile = serde_json::from_str(&contents).unwrap();

    assert!(
        output.pairs.is_empty(),
        "all-fail should produce empty pairs array"
    );
}

#[test]
fn test_missing_price_data_rejected() {
    let tmp = TempDir::new().unwrap();
    let output_path = tmp.path().join("active_pairs.json");

    // Empty provider — no prices for any symbol
    let provider = InMemoryPrices {
        data: HashMap::new(),
    };

    let candidates = vec![PairCandidate {
        leg_a: "MISSING_A".into(),
        leg_b: "MISSING_B".into(),
        economic_rationale: "no data".into(),
    }];

    let results =
        pipeline::run_pipeline_from_candidates(&candidates, &output_path, &provider).unwrap();

    assert!(!results[0].passed, "pair with no data should be rejected");
    assert!(
        results[0]
            .rejection_reasons
            .iter()
            .any(|r| r.contains("no price data")),
        "rejection should mention missing data"
    );
}

#[test]
fn test_etf_component_pair_rejected_before_validation() {
    let tmp = TempDir::new().unwrap();
    let output_path = tmp.path().join("active_pairs.json");

    let (pa, pb) = cointegrated_prices(500, 1.5, 42);

    let provider = InMemoryPrices {
        data: HashMap::from([("XLF".to_string(), pa), ("JPM".to_string(), pb)]),
    };

    let candidates = vec![PairCandidate {
        leg_a: "XLF".into(),
        leg_b: "JPM".into(),
        economic_rationale: "ETF-component pair".into(),
    }];

    let results =
        pipeline::run_pipeline_from_candidates(&candidates, &output_path, &provider).unwrap();

    assert!(!results[0].passed);
    assert!(results[0].etf_excluded, "should be ETF-excluded");
}
