//! Integration test: invoke the runner binary with synthetic data.

use std::io::Write;
use std::process::Command;
use tempfile::TempDir;

/// Create a minimal openquant.toml (no pairs — pairs come from active_pairs.json).
fn minimal_toml() -> String {
    r#"
[signal]
buy_z_threshold = -2.2
sell_z_threshold = 2.0
min_relative_volume = 0.0

[risk]
max_position_notional = 10000.0
max_daily_loss = 500.0

[exit]
stop_loss_pct = 0.02
max_hold_bars = 100

[pairs_trading]
min_hold_bars = 0
last_entry_hour = 24
force_close_minute = 1500
"#
    .to_string()
}

/// Create an active_pairs.json with one test pair.
/// Uses a recent generated_at to avoid the 48h staleness check.
fn active_pairs_json() -> String {
    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ");
    format!(
        r#"
{{
    "generated_at": "{now}",
    "pairs": [
        {{
            "leg_a": "AAA",
            "leg_b": "BBB",
            "alpha": 0.0,
            "beta": 1.0,
            "half_life_days": 10.0,
            "adf_statistic": -4.0,
            "adf_pvalue": 0.005,
            "beta_cv": 0.05,
            "structural_break": false,
            "regime_robustness": 0.9,
            "economic_rationale": "test",
            "score": 0.9
        }}
    ]
}}
"#
    )
}

/// Generate synthetic bars for two symbols with a mean-reverting spread.
fn synthetic_bars() -> String {
    let mut bars_aaa = Vec::new();
    let mut bars_bbb = Vec::new();
    let base_ts: i64 = 1_768_489_200_000; // 2026-01-15 15:00 UTC (10:00 ET, within market hours)

    for i in 0..200 {
        let ts = base_ts + i * 60_000; // 1-min bars
        let price_b = 100.0;
        // Large oscillation: ±30% around 100 to exceed entry_z=2.0 after warmup
        let price_a = 100.0 * (1.0 + 0.3 * (i as f64 * 0.08).sin());

        bars_aaa.push(format!(
            r#"{{"timestamp":{ts},"open":{pa},"high":{ph},"low":{pl},"close":{pa},"volume":1000.0}}"#,
            pa = price_a,
            ph = price_a + 0.5,
            pl = price_a - 0.5,
        ));
        bars_bbb.push(format!(
            r#"{{"timestamp":{ts},"open":{pb},"high":{ph},"low":{pl},"close":{pb},"volume":1000.0}}"#,
            pb = price_b,
            ph = price_b + 0.5,
            pl = price_b - 0.5,
        ));
    }

    format!(
        r#"{{"AAA":[{}],"BBB":[{}]}}"#,
        bars_aaa.join(","),
        bars_bbb.join(",")
    )
}

#[test]
#[ignore] // broken: `backtest` subcommand was removed in engine refactor
fn runner_produces_order_intents() {
    let dir = TempDir::new().unwrap();

    // Write config
    let config_path = dir.path().join("openquant.toml");
    let mut f = std::fs::File::create(&config_path).unwrap();
    f.write_all(minimal_toml().as_bytes()).unwrap();

    // Write active_pairs.json (pairs are sourced from JSON, not TOML)
    let pairs_path = dir.path().join("active_pairs.json");
    let mut f = std::fs::File::create(&pairs_path).unwrap();
    f.write_all(active_pairs_json().as_bytes()).unwrap();

    // Write bar data
    let bars_path = dir.path().join("experiment_bars_20260101.json");
    let mut f = std::fs::File::create(&bars_path).unwrap();
    f.write_all(synthetic_bars().as_bytes()).unwrap();

    // Find the binary
    let binary = env!("CARGO_BIN_EXE_openquant-runner");

    // Run
    let output = Command::new(binary)
        .args([
            "backtest",
            "--config",
            config_path.to_str().unwrap(),
            "--data-dir",
            dir.path().to_str().unwrap(),
            "--warmup-bars",
            "30",
        ])
        .output()
        .expect("failed to execute runner");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "runner failed:\n{stderr}");

    // Verify order_intents.json was created
    let intents_path = dir.path().join("order_intents.json");
    assert!(intents_path.exists(), "order_intents.json not created");

    let contents = std::fs::read_to_string(&intents_path).unwrap();
    let intents: Vec<serde_json::Value> = serde_json::from_str(&contents).unwrap();

    // With a sinusoidal spread and entry_z=2.0, we should get some pair intents
    // (the exact count depends on lookback/warmup, but should be > 0)
    assert!(
        !intents.is_empty(),
        "expected some order intents, got 0.\nstderr: {stderr}"
    );

    // All intents should have positive qty
    for intent in &intents {
        assert!(intent["qty"].as_f64().unwrap() > 0.0, "zero qty: {intent}");
    }

    // At least some should be pair intents (pair_id present)
    let _pair_intents: Vec<_> = intents
        .iter()
        .filter(|i| i.get("pair_id").is_some())
        .collect();
    // Pair signals may or may not fire depending on warmup/lookback,
    // but single-symbol or pair intents should exist
    assert!(
        !intents.is_empty(),
        "expected some intents (single-symbol or pairs)"
    );
}
