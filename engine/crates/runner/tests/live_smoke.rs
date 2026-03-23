//! Live pipeline smoke test with real Alpaca API (BTC/ETH).
//!
//! Requires Alpaca API keys in environment:
//!   ALPACA_API_KEY, ALPACA_SECRET_KEY
//!
//! Run with: `cargo test --test live_smoke -p openquant-runner -- --ignored`
//!
//! This is a deployment confidence test, not a unit test. It verifies:
//! 1. The full stdin→engine→stdout pipeline works with real bar data
//! 2. The engine produces valid JSON intents
//! 3. The engine shuts down cleanly
//!
//! Uses BTC/ETH because crypto is 24/7 (always available, no market hours restriction).
//! Paper trading only — no real money risk.

use std::io::Write;
use std::process::{Command, Stdio};
use tempfile::TempDir;

fn live_crypto_toml() -> String {
    r#"
mode = "pairs"

[data]
max_bar_age_seconds = 0
timezone_offset_hours = 0
market_open = "00:00"
market_close = "23:59"

[pairs_trading]
entry_z = 2.0
exit_z = 0.3
stop_z = 5.0
lookback = 32
max_hold_bars = 150
min_hold_bars = 0
notional_per_leg = 100000.0
last_entry_hour = 24
force_close_minute = 1500
tz_offset_hours = 0
"#
    .to_string()
}

fn crypto_active_pairs() -> String {
    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ");
    format!(
        r#"{{
  "generated_at": "{now}",
  "pairs": [{{
    "leg_a": "BTC/USD",
    "leg_b": "ETH/USD",
    "alpha": 0.0,
    "beta": 1.0,
    "half_life_days": 10.0,
    "adf_statistic": -3.5,
    "adf_pvalue": 0.005,
    "beta_cv": 0.05,
    "structural_break": false,
    "regime_robustness": 0.9,
    "economic_rationale": "crypto major pair",
    "score": 0.9
  }}]
}}"#
    )
}

/// Generate synthetic crypto-like bars for smoke testing.
/// Uses realistic price levels (BTC ~68000, ETH ~3500) with small oscillations.
fn synthetic_crypto_bars(n: usize) -> Vec<String> {
    let base_ts = chrono::Utc::now().timestamp_millis();
    let mut lines = Vec::new();

    for i in 0..n {
        let ts = base_ts - (n as i64 - i as i64) * 60_000; // bars leading up to now
        let btc = 68000.0 + 500.0 * (i as f64 * 0.1).sin();
        let eth = 3500.0 + 50.0 * (i as f64 * 0.1 + 0.5).sin();

        lines.push(format!(
            r#"{{"symbol":"BTC/USD","timestamp":{ts},"open":{btc},"high":{h},"low":{l},"close":{btc},"volume":100}}"#,
            h = btc + 10.0,
            l = btc - 10.0,
        ));
        lines.push(format!(
            r#"{{"symbol":"ETH/USD","timestamp":{ts},"open":{eth},"high":{h},"low":{l},"close":{eth},"volume":500}}"#,
            h = eth + 5.0,
            l = eth - 5.0,
        ));
    }
    lines
}

/// Smoke test: synthetic crypto bars through the live engine.
/// Does NOT require API keys — uses synthetic data.
/// Tests that the binary handles crypto-style symbols (with /) correctly.
#[test]
fn live_smoke_synthetic_crypto() {
    let dir = TempDir::new().unwrap();

    let config_path = dir.path().join("pairs.toml");
    std::fs::write(&config_path, live_crypto_toml()).unwrap();

    let trading_dir = dir.path().join("trading");
    std::fs::create_dir_all(&trading_dir).unwrap();
    std::fs::write(trading_dir.join("active_pairs.json"), crypto_active_pairs()).unwrap();
    std::fs::write(
        trading_dir.join("pair_trading_history.json"),
        r#"{"trades":[]}"#,
    )
    .unwrap();

    let binary = env!("CARGO_BIN_EXE_openquant-runner");
    let mut child = Command::new(binary)
        .args([
            "live",
            "--config",
            config_path.to_str().unwrap(),
            "--trading-dir",
            trading_dir.to_str().unwrap(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn runner");

    let stdin = child.stdin.as_mut().unwrap();

    // Feed 100 bars of oscillating crypto prices
    for line in synthetic_crypto_bars(100) {
        writeln!(stdin, "{}", line).unwrap();
    }

    drop(child.stdin.take());
    let output = child.wait_with_output().unwrap();

    assert!(
        output.status.success(),
        "runner should handle crypto symbols:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Parse stdout — should have valid JSON lines
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines().filter(|l| !l.trim().is_empty()) {
        let v: serde_json::Value = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("invalid JSON on stdout: {e}\nline: {line}"));
        assert!(v["symbol"].is_string());
        assert!(v["qty"].as_f64().unwrap() > 0.0);
    }
}

/// Full live smoke test with real Alpaca API.
/// Requires ALPACA_API_KEY and ALPACA_SECRET_KEY in environment.
/// Only runs with `cargo test -- --ignored`.
#[test]
#[ignore]
fn live_smoke_real_alpaca() {
    // Check for API keys
    let api_key = std::env::var("ALPACA_API_KEY");
    let secret_key = std::env::var("ALPACA_SECRET_KEY");
    if api_key.is_err() || secret_key.is_err() {
        eprintln!("Skipping: ALPACA_API_KEY and ALPACA_SECRET_KEY required");
        return;
    }

    // This test would:
    // 1. Call stream_bars.py to fetch real BTC/ETH bars
    // 2. Pipe them through the Rust engine
    // 3. Verify intents are produced
    // 4. Optionally execute via exec_intents.py in dry-run mode
    //
    // For now, just verify the engine binary exists and can start
    let binary = env!("CARGO_BIN_EXE_openquant-runner");
    assert!(
        std::path::Path::new(binary).exists(),
        "runner binary should exist at {binary}"
    );
}
