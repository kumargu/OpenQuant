//! Integration tests for the pair discovery → trading pipeline.
//!
//! These tests exercise the seams between modules, not individual modules.
//! Every bug found in the pair discovery system (#117) was at a module boundary.
//!
//! Run with: `cargo test --test integration_pairs`
//! (They're not #[ignore] since they use synthetic data and run fast.)

use openquant_core::pairs::active_pairs::{ActivePairEntry, ActivePairsFile};
use openquant_core::pairs::engine::PairsEngine;
use openquant_core::pairs::{PairConfig, PairsTradingConfig};
use tempfile::TempDir;

/// Helper: write an active_pairs.json file.
fn write_active_pairs(dir: &std::path::Path, pairs: &[ActivePairEntry]) -> std::path::PathBuf {
    let file = ActivePairsFile {
        generated_at: chrono::Utc::now(),
        pairs: pairs.to_vec(),
    };
    let path = dir.join("active_pairs.json");
    let json = serde_json::to_string_pretty(&file).unwrap();
    std::fs::write(&path, json).unwrap();
    path
}

/// Helper: write an empty trading history file.
fn write_empty_history(dir: &std::path::Path) -> std::path::PathBuf {
    let path = dir.join("pair_trading_history.json");
    std::fs::write(&path, r#"{"trades":[]}"#).unwrap();
    path
}

/// Helper: make a test pair entry.
fn test_pair(leg_a: &str, leg_b: &str, alpha: f64, beta: f64) -> ActivePairEntry {
    ActivePairEntry {
        leg_a: leg_a.into(),
        leg_b: leg_b.into(),
        alpha,
        beta,
        half_life_days: 10.0,
        adf_statistic: -4.0,
        adf_pvalue: 0.005,
        beta_cv: 0.05,
        structural_break: false,
        regime_robustness: 0.9,
        economic_rationale: "test".into(),
        score: 0.9,
    }
}

/// Helper: make a PairConfig for fallback.
fn fallback_config(leg_a: &str, leg_b: &str, beta: f64) -> PairConfig {
    PairConfig {
        leg_a: leg_a.into(),
        leg_b: leg_b.into(),
        alpha: 0.0,
        beta,
    }
}

/// Test-friendly trading config with no min_hold_bars.
fn test_trading() -> PairsTradingConfig {
    PairsTradingConfig {
        min_hold_bars: 0,
        ..PairsTradingConfig::default()
    }
}

// ─────────────────────────────────────────────────────────────────────
// T1: Full pipeline round-trip
// ─────────────────────────────────────────────────────────────────────

#[test]
fn t1_full_pipeline_round_trip() {
    let dir = TempDir::new().unwrap();

    // pair-picker output → active_pairs.json (beta=1.0 for simple spread)
    let pairs_path = write_active_pairs(dir.path(), &[test_pair("AAA", "BBB", 0.0, 1.0)]);
    let history_path = write_empty_history(dir.path());

    // Load into PairsEngine
    let mut engine =
        PairsEngine::from_active_pairs(&pairs_path, &history_path, vec![], test_trading());

    assert_eq!(engine.pair_count(), 1);

    // Feed bars where prices diverge and converge to trigger entry/exit.
    // With beta=1.0, spread = ln(A) - ln(B). We keep B constant and
    // oscillate A so the spread swings past entry_z=2.0 after warmup.
    let mut intents_total = 0;
    for i in 0..300 {
        let ts = 1_000_000 + i * 60_000;
        // Large oscillation: price_a swings ±30% around 100
        let price_a = 100.0 * (1.0 + 0.3 * (i as f64 * 0.08).sin());
        let price_b = 100.0;

        let intents_a = engine.on_bar("AAA", ts, price_a);
        let intents_b = engine.on_bar("BBB", ts, price_b);
        intents_total += intents_a.len() + intents_b.len();
    }

    // Should have generated some trades
    assert!(
        intents_total > 0,
        "expected trade intents from pipeline round-trip"
    );
}

// ─────────────────────────────────────────────────────────────────────
// T2: Alpha correctness — z-scores must differ with alpha
// ─────────────────────────────────────────────────────────────────────

#[test]
fn t2_alpha_affects_spread() {
    let dir = TempDir::new().unwrap();
    let history_path = write_empty_history(dir.path());

    // Engine WITHOUT alpha
    let pairs_no_alpha = write_active_pairs(dir.path(), &[test_pair("AAA", "BBB", 0.0, 1.0)]);
    let mut engine_no_alpha =
        PairsEngine::from_active_pairs(&pairs_no_alpha, &history_path, vec![], test_trading());

    // Engine WITH alpha = 2.0 (large enough to shift spread significantly)
    let dir2 = TempDir::new().unwrap();
    let history_path2 = write_empty_history(dir2.path());
    let pairs_with_alpha = write_active_pairs(dir2.path(), &[test_pair("AAA", "BBB", 2.0, 1.0)]);
    let mut engine_with_alpha =
        PairsEngine::from_active_pairs(&pairs_with_alpha, &history_path2, vec![], test_trading());

    // Feed identical bars and collect spread values from intents.
    // Alpha shifts the raw spread by a constant. Since z-score subtracts
    // the rolling mean, alpha cancels out in z-score — that's correct.
    // But the raw spread values reported in intents should differ.
    let mut spreads_no_alpha = Vec::new();
    let mut spreads_with_alpha = Vec::new();
    for i in 0..300 {
        let ts = 1_000_000 + i * 60_000;
        let price = 100.0 * (1.0 + 0.3 * (i as f64 * 0.08).sin());

        for intent in engine_no_alpha.on_bar("AAA", ts, price) {
            spreads_no_alpha.push(intent.spread);
        }
        for intent in engine_no_alpha.on_bar("BBB", ts, 100.0) {
            spreads_no_alpha.push(intent.spread);
        }

        for intent in engine_with_alpha.on_bar("AAA", ts, price) {
            spreads_with_alpha.push(intent.spread);
        }
        for intent in engine_with_alpha.on_bar("BBB", ts, 100.0) {
            spreads_with_alpha.push(intent.spread);
        }
    }

    // Both should generate intents
    assert!(
        !spreads_no_alpha.is_empty(),
        "no intents from alpha=0.0 engine"
    );
    assert!(
        !spreads_with_alpha.is_empty(),
        "no intents from alpha=2.0 engine"
    );

    // Alpha=2.0 shifts spread by -2.0, so the first spread values should differ
    // by approximately 2.0.
    let first_no = spreads_no_alpha[0];
    let first_with = spreads_with_alpha[0];
    let diff = (first_no - first_with).abs();
    assert!(
        diff > 1.0,
        "alpha had no effect on spread: no_alpha={first_no}, with_alpha={first_with}, diff={diff}"
    );
}

// ─────────────────────────────────────────────────────────────────────
// T3: Canonical ID consistency across modules
// ─────────────────────────────────────────────────────────────────────

#[test]
fn t3_canonical_pair_id_consistency() {
    let dir = TempDir::new().unwrap();

    // Write pairs with legs in non-alphabetical order
    let pairs_path = write_active_pairs(dir.path(), &[test_pair("SLV", "GLD", 0.0, 1.0)]);
    let history_path = write_empty_history(dir.path());

    let mut engine =
        PairsEngine::from_active_pairs(&pairs_path, &history_path, vec![], test_trading());

    // Feed bars until we get an intent
    for i in 0..200 {
        let ts = 1_000_000 + i * 60_000;
        let spread = 3.0 * (i as f64 * 0.15).sin();

        let intents = engine.on_bar("GLD", ts, 200.0 + spread);
        for intent in &intents {
            // pair_id must be alphabetically ordered regardless of input order
            assert!(
                intent.pair_id == "GLD/SLV",
                "expected canonical 'GLD/SLV', got '{}'",
                intent.pair_id
            );
        }

        engine.on_bar("SLV", ts, 25.0);
    }
}

// ─────────────────────────────────────────────────────────────────────
// T4: Beta refresh on reload
// ─────────────────────────────────────────────────────────────────────

#[test]
fn t4_beta_refresh_on_reload() {
    let dir = TempDir::new().unwrap();
    let history_path = write_empty_history(dir.path());

    // Start with beta = 1.0
    write_active_pairs(dir.path(), &[test_pair("AAA", "BBB", 0.0, 1.0)]);
    let pairs_path = dir.path().join("active_pairs.json");

    let mut engine =
        PairsEngine::from_active_pairs(&pairs_path, &history_path, vec![], test_trading());

    // Feed some warmup bars
    for i in 0..30 {
        let ts = 1_000_000 + i * 60_000;
        engine.on_bar("AAA", ts, 100.0);
        engine.on_bar("BBB", ts, 100.0);
    }

    // Update active_pairs.json with new beta = 2.0
    write_active_pairs(dir.path(), &[test_pair("AAA", "BBB", 0.0, 2.0)]);

    // Reload
    let reloaded = engine.reload();
    assert!(reloaded, "reload should succeed");

    // After reload, the engine should use beta=2.0.
    // We can verify by checking that spread computation changes.
    // Feed a bar where AAA=200, BBB=100:
    //   With beta=1.0: spread = ln(200) - 0 - 1.0*ln(100) = 5.298 - 4.605 = 0.693
    //   With beta=2.0: spread = ln(200) - 0 - 2.0*ln(100) = 5.298 - 9.210 = -3.912
    // Different betas → different z-scores → different trade behavior.

    // Collect intents with new beta
    for i in 30..100 {
        let ts = 1_000_000 + i * 60_000;
        let spread = 3.0 * (i as f64 * 0.15).sin();
        engine.on_bar("AAA", ts, 100.0 + spread);
        engine.on_bar("BBB", ts, 100.0);
    }

    // Verify beta actually changed: positions()[0].0.beta should be 2.0
    let positions = engine.positions();
    assert!(
        (positions[0].0.beta - 2.0).abs() < 0.01,
        "beta should be 2.0 after reload, got {}",
        positions[0].0.beta
    );
}

// ─────────────────────────────────────────────────────────────────────
// T5: Stale data rejection — falls back to fallback configs
// ─────────────────────────────────────────────────────────────────────

#[test]
fn t5_stale_data_falls_back() {
    let dir = TempDir::new().unwrap();
    let history_path = write_empty_history(dir.path());

    // Write active_pairs.json with a timestamp 72 hours ago (>48h staleness limit)
    let stale_file = ActivePairsFile {
        generated_at: chrono::Utc::now() - chrono::Duration::hours(72),
        pairs: vec![test_pair("OLD", "STALE", 0.0, 1.0)],
    };
    let pairs_path = dir.path().join("active_pairs.json");
    let json = serde_json::to_string_pretty(&stale_file).unwrap();
    std::fs::write(&pairs_path, json).unwrap();

    // Provide fallback configs
    let fallback = vec![fallback_config("GLD", "SLV", 0.37)];

    let engine =
        PairsEngine::from_active_pairs(&pairs_path, &history_path, fallback, test_trading());

    // Should use fallback (GLD/SLV), not stale (OLD/STALE)
    assert_eq!(engine.pair_count(), 1);

    // Verify it's the fallback pair, not the stale one
    let positions = engine.positions();
    let pair_config = positions[0].0;
    assert_eq!(
        pair_config.leg_a, "GLD",
        "should use fallback, not stale pair"
    );
    assert_eq!(pair_config.leg_b, "SLV");
    assert_ne!(
        pair_config.leg_a, "OLD",
        "stale pair OLD/STALE must NOT be loaded"
    );
}

// ─────────────────────────────────────────────────────────────────────
// T11: Graceful pair transition — tightened stops on removal
// ─────────────────────────────────────────────────────────────────────

#[test]
fn t11_graceful_pair_transition() {
    let dir = TempDir::new().unwrap();
    let history_path = write_empty_history(dir.path());

    // Start with two pairs
    write_active_pairs(
        dir.path(),
        &[
            test_pair("GLD", "SLV", 0.0, 0.37),
            test_pair("C", "JPM", 0.0, 1.39),
        ],
    );
    let pairs_path = dir.path().join("active_pairs.json");

    let mut engine =
        PairsEngine::from_active_pairs(&pairs_path, &history_path, vec![], test_trading());

    assert_eq!(engine.pair_count(), 2);

    // Feed bars to warm up and potentially enter a position on C/JPM
    for i in 0..100 {
        let ts = 1_000_000 + i * 60_000;
        let spread = 3.0 * (i as f64 * 0.15).sin();
        engine.on_bar("GLD", ts, 200.0);
        engine.on_bar("SLV", ts, 25.0);
        engine.on_bar("C", ts, 70.0 + spread);
        engine.on_bar("JPM", ts, 200.0);
    }

    // Remove C/JPM from active pairs (pair-picker no longer validates it)
    write_active_pairs(dir.path(), &[test_pair("GLD", "SLV", 0.0, 0.37)]);

    let reloaded = engine.reload();
    assert!(reloaded, "reload should succeed");

    // If C/JPM had an open position, it should have tightened stops.
    // If flat, it should be removed entirely.
    // Either way, the engine should not panic and should continue working.
    for i in 100..150 {
        let ts = 1_000_000 + i * 60_000;
        engine.on_bar("GLD", ts, 200.0);
        engine.on_bar("SLV", ts, 25.0);
        engine.on_bar("C", ts, 70.0);
        engine.on_bar("JPM", ts, 200.0);
    }

    // After removing C/JPM and it being flat, only GLD/SLV should remain
    assert_eq!(
        engine.pair_count(),
        1,
        "only GLD/SLV should remain after removing flat C/JPM"
    );
}

// ─────────────────────────────────────────────────────────────────────
// T13: NaN propagation sweep — caught at boundaries
// ─────────────────────────────────────────────────────────────────────

#[test]
fn t13_nan_does_not_propagate() {
    let dir = TempDir::new().unwrap();
    let history_path = write_empty_history(dir.path());
    let pairs_path = write_active_pairs(dir.path(), &[test_pair("AAA", "BBB", 0.0, 1.0)]);

    let mut engine =
        PairsEngine::from_active_pairs(&pairs_path, &history_path, vec![], test_trading());

    // Feed some valid bars first
    for i in 0..30 {
        let ts = 1_000_000 + i * 60_000;
        engine.on_bar("AAA", ts, 100.0);
        engine.on_bar("BBB", ts, 100.0);
    }

    // Inject NaN — should produce zero intents (NaN rejected at boundary)
    let nan_intents = engine.on_bar("AAA", 2_000_000, f64::NAN);
    assert!(
        nan_intents.is_empty(),
        "NaN input should produce no intents, got {}",
        nan_intents.len()
    );

    // Feed more valid bars — engine should recover
    for i in 30..60 {
        let ts = 3_000_000 + i * 60_000;
        let intents = engine.on_bar("AAA", ts, 100.0);
        // Should not panic
        for intent in &intents {
            assert!(intent.qty.is_finite(), "NaN propagated to later bars");
        }
        engine.on_bar("BBB", ts, 100.0);
    }
}

// ─────────────────────────────────────────────────────────────────────
// T14: Empty/missing file resilience
// ─────────────────────────────────────────────────────────────────────

#[test]
fn t14_missing_files_graceful_fallback() {
    let dir = TempDir::new().unwrap();

    let missing_pairs = dir.path().join("nonexistent_active_pairs.json");
    let missing_history = dir.path().join("nonexistent_history.json");
    let fallback = vec![fallback_config("GLD", "SLV", 0.37)];

    // Missing active_pairs.json → uses fallback
    let engine =
        PairsEngine::from_active_pairs(&missing_pairs, &missing_history, fallback, test_trading());
    assert_eq!(engine.pair_count(), 1);
}

#[test]
fn t14_empty_active_pairs_graceful_fallback() {
    let dir = TempDir::new().unwrap();
    let history_path = write_empty_history(dir.path());

    // Empty JSON object (no pairs array)
    let pairs_path = dir.path().join("active_pairs.json");
    std::fs::write(&pairs_path, "{}").unwrap();

    let fallback = vec![fallback_config("GLD", "SLV", 0.37)];
    let engine =
        PairsEngine::from_active_pairs(&pairs_path, &history_path, fallback, test_trading());

    // Should fall back gracefully
    assert_eq!(engine.pair_count(), 1);
    let positions = engine.positions();
    assert_eq!(positions[0].0.leg_a, "GLD");
}

#[test]
fn t14_corrupt_active_pairs_graceful_fallback() {
    let dir = TempDir::new().unwrap();
    let history_path = write_empty_history(dir.path());

    // Corrupt JSON
    let pairs_path = dir.path().join("active_pairs.json");
    std::fs::write(&pairs_path, "NOT VALID JSON {{{").unwrap();

    let fallback = vec![fallback_config("GLD", "SLV", 0.37)];
    let engine =
        PairsEngine::from_active_pairs(&pairs_path, &history_path, fallback, test_trading());

    assert_eq!(engine.pair_count(), 1);
}
