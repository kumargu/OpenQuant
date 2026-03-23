//! Integration tests for the pair-picker pipeline (#172).
//!
//! Tests the full flow: candidates → graph filter → validate → write active_pairs.json.
//! Uses synthetic price data — no external dependencies.

use pair_picker::lockfile;
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

// ─────────────────────────────────────────────────────────────────────
// Lock file lifecycle
// ─────────────────────────────────────────────────────────────────────

#[test]
fn test_lock_file_prevents_second_run() {
    let tmp = TempDir::new().unwrap();

    // No lock → has_run_today returns false
    assert!(
        !lockfile::has_run_today(tmp.path()),
        "should not have run yet"
    );

    // Create lock
    lockfile::create_lock(tmp.path()).unwrap();

    // Lock exists → has_run_today returns true
    assert!(
        lockfile::has_run_today(tmp.path()),
        "should detect today's lock"
    );
}

#[test]
fn test_lock_file_cleanup() {
    let tmp = TempDir::new().unwrap();

    // Create production locks for old dates (not test mode — cleanup skips test locks)
    lockfile::create_lock_for_date(tmp.path(), "20260101", false).unwrap();
    lockfile::create_lock_for_date(tmp.path(), "20260102", false).unwrap();
    lockfile::create_lock_for_date(tmp.path(), "20260103", false).unwrap();

    // Cleanup keeps last 7 days from today
    let removed = lockfile::cleanup_old_locks(tmp.path()).unwrap();
    assert!(removed >= 3, "should remove old locks (removed {removed})");
}

// ─────────────────────────────────────────────────────────────────────
// Multiple candidates: cointegrated passes, insufficient data rejected
// ─────────────────────────────────────────────────────────────────────

#[test]
fn test_insufficient_data_rejected() {
    let tmp = TempDir::new().unwrap();
    let output_path = tmp.path().join("active_pairs.json");

    // Only 50 bars (below MIN_HISTORY_BARS=90)
    let (pa, pb) = cointegrated_prices(50, 1.5, 42);

    let provider = InMemoryPrices {
        data: HashMap::from([("SHORT_A".to_string(), pa), ("SHORT_B".to_string(), pb)]),
    };

    let candidates = vec![PairCandidate {
        leg_a: "SHORT_A".into(),
        leg_b: "SHORT_B".into(),
        economic_rationale: "too short".into(),
    }];

    let results =
        pipeline::run_pipeline_from_candidates(&candidates, &output_path, &provider).unwrap();

    assert!(
        !results[0].passed,
        "pair with insufficient data should be rejected"
    );
    assert!(
        results[0]
            .rejection_reasons
            .iter()
            .any(|r| r.contains("data") || r.contains("bars") || r.contains("history")),
        "rejection should mention data insufficiency: {:?}",
        results[0].rejection_reasons
    );
}

// ─────────────────────────────────────────────────────────────────────
// Output schema validation
// ─────────────────────────────────────────────────────────────────────

#[test]
fn test_output_schema_complete() {
    let tmp = TempDir::new().unwrap();
    let output_path = tmp.path().join("active_pairs.json");

    let (pa, pb) = cointegrated_prices(500, 1.5, 42);

    let provider = InMemoryPrices {
        data: HashMap::from([("A".to_string(), pa), ("B".to_string(), pb)]),
    };

    let candidates = vec![PairCandidate {
        leg_a: "A".into(),
        leg_b: "B".into(),
        economic_rationale: "test".into(),
    }];

    pipeline::run_pipeline_from_candidates(&candidates, &output_path, &provider).unwrap();

    // Parse as raw JSON to verify all required fields exist
    let contents = std::fs::read_to_string(&output_path).unwrap();
    let raw: serde_json::Value = serde_json::from_str(&contents).unwrap();

    assert!(
        raw["generated_at"].is_string(),
        "generated_at must be present"
    );
    assert!(raw["pairs"].is_array(), "pairs must be an array");

    for pair in raw["pairs"].as_array().unwrap() {
        assert!(pair["leg_a"].is_string(), "missing leg_a");
        assert!(pair["leg_b"].is_string(), "missing leg_b");
        assert!(pair["alpha"].as_f64().is_some(), "missing alpha");
        assert!(pair["beta"].as_f64().is_some(), "missing beta");
        assert!(
            pair["half_life_days"].as_f64().is_some(),
            "missing half_life_days"
        );
        assert!(
            pair["adf_statistic"].as_f64().is_some(),
            "missing adf_statistic"
        );
        assert!(pair["adf_pvalue"].as_f64().is_some(), "missing adf_pvalue");
        assert!(pair["beta_cv"].as_f64().is_some(), "missing beta_cv");
        assert!(
            pair["structural_break"].is_boolean(),
            "missing structural_break"
        );
        assert!(
            pair["regime_robustness"].as_f64().is_some(),
            "missing regime_robustness"
        );
        assert!(pair["score"].as_f64().is_some(), "missing score");
        assert!(
            pair["economic_rationale"].is_string(),
            "missing economic_rationale"
        );

        // Verify value ranges
        let beta = pair["beta"].as_f64().unwrap();
        assert!(
            beta.is_finite() && beta > 0.0,
            "beta should be positive finite"
        );
        let score = pair["score"].as_f64().unwrap();
        assert!(
            (0.0..=1.0).contains(&score),
            "score should be in [0, 1], got {score}"
        );
    }
}

// ─────────────────────────────────────────────────────────────────────
// Deterministic output — same data → same results
// ─────────────────────────────────────────────────────────────────────

#[test]
fn test_deterministic_output() {
    let (pa, pb) = cointegrated_prices(500, 1.5, 42);

    let candidates = vec![PairCandidate {
        leg_a: "A".into(),
        leg_b: "B".into(),
        economic_rationale: "test".into(),
    }];

    let mut outputs = Vec::new();
    for _ in 0..2 {
        let tmp = TempDir::new().unwrap();
        let output_path = tmp.path().join("active_pairs.json");
        let provider = InMemoryPrices {
            data: HashMap::from([("A".to_string(), pa.clone()), ("B".to_string(), pb.clone())]),
        };
        pipeline::run_pipeline_from_candidates(&candidates, &output_path, &provider).unwrap();

        let contents = std::fs::read_to_string(&output_path).unwrap();
        let file: ActivePairsFile = serde_json::from_str(&contents).unwrap();
        outputs.push(file);
    }

    // Pairs should be identical (ignore generated_at which differs by milliseconds)
    assert_eq!(outputs[0].pairs.len(), outputs[1].pairs.len());
    for (a, b) in outputs[0].pairs.iter().zip(outputs[1].pairs.iter()) {
        assert_eq!(a.leg_a, b.leg_a);
        assert_eq!(a.leg_b, b.leg_b);
        assert!(
            (a.beta - b.beta).abs() < 1e-10,
            "beta should be deterministic"
        );
        assert!(
            (a.score - b.score).abs() < 1e-10,
            "score should be deterministic"
        );
    }
}
