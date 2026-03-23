//! Integration tests for `Command::Live` — the stdin→engine→stdout pipeline.
//!
//! These tests verify the paper trading path end-to-end by spawning the runner
//! binary, piping synthetic bars to stdin, and asserting on stdout intents.
//!
//! Run with: `cargo test --test live_mode -p openquant-runner`

use std::io::{BufRead, Write};
use std::process::{Command, Stdio};
use tempfile::TempDir;

/// Minimal TOML for pairs-only live mode.
/// Time guards fully permissive (last_entry_hour=24, force_close_minute=1500)
/// so synthetic timestamps work without market-hours constraints.
fn live_toml() -> String {
    r#"
mode = "pairs"

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

[data]
max_bar_age_seconds = 0
timezone_offset_hours = 0

[pairs_trading]
entry_z = 2.0
exit_z = 0.3
stop_z = 5.0
lookback = 32
max_hold_bars = 150
min_hold_bars = 0
notional_per_leg = 10000.0
last_entry_hour = 24
force_close_minute = 1500
tz_offset_hours = 0
"#
    .to_string()
}

/// Active pairs JSON with one pair (AAA/BBB, beta=1.0).
fn active_pairs_json() -> String {
    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ");
    format!(
        r#"{{
  "generated_at": "{now}",
  "pairs": [{{
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
  }}]
}}"#
    )
}

/// Format a bar as a JSON line.
fn bar_json(symbol: &str, ts: i64, close: f64) -> String {
    format!(
        r#"{{"symbol":"{symbol}","timestamp":{ts},"open":{close},"high":{h},"low":{l},"close":{close},"volume":1000}}"#,
        h = close + 0.5,
        l = close - 0.5,
    )
}

/// Set up a temp dir with config + active_pairs.json, return (dir, config_path).
fn setup_live_env() -> (TempDir, std::path::PathBuf) {
    let dir = TempDir::new().unwrap();

    let config_path = dir.path().join("pairs.toml");
    std::fs::write(&config_path, live_toml()).unwrap();

    let trading_dir = dir.path().join("trading");
    std::fs::create_dir_all(&trading_dir).unwrap();
    std::fs::write(trading_dir.join("active_pairs.json"), active_pairs_json()).unwrap();
    std::fs::write(
        trading_dir.join("pair_trading_history.json"),
        r#"{"trades":[]}"#,
    )
    .unwrap();

    (dir, config_path)
}

/// Spawn the runner in live mode with piped stdin/stdout.
fn spawn_live(config_path: &std::path::Path, dir: &std::path::Path) -> std::process::Child {
    let binary = env!("CARGO_BIN_EXE_openquant-runner");
    Command::new(binary)
        .args([
            "live",
            "--config",
            config_path.to_str().unwrap(),
            "--trading-dir",
            dir.join("trading").to_str().unwrap(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn runner")
}

// ─────────────────────────────────────────────────────────────────────
// Test 1: Basic lifecycle — bars in, intents out, clean shutdown on EOF
// ─────────────────────────────────────────────────────────────────────

#[test]
fn live_basic_lifecycle() {
    let (dir, config_path) = setup_live_env();
    let mut child = spawn_live(&config_path, dir.path());

    let stdin = child.stdin.as_mut().unwrap();
    let base_ts: i64 = 1_700_000_000_000;

    // Feed 200 bars with oscillating spread to trigger entry/exit
    for i in 0..200 {
        let ts = base_ts + i * 60_000;
        let price_a = 100.0 * (1.0 + 0.3 * (i as f64 * 0.08).sin());
        let price_b = 100.0;
        writeln!(stdin, "{}", bar_json("AAA", ts, price_a)).unwrap();
        writeln!(stdin, "{}", bar_json("BBB", ts, price_b)).unwrap();
    }

    // Close stdin → EOF → engine should shut down cleanly
    drop(child.stdin.take());

    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "runner exited with error:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Parse stdout intents
    let stdout = String::from_utf8_lossy(&output.stdout);
    let intents: Vec<serde_json::Value> = stdout
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();

    assert!(
        !intents.is_empty(),
        "expected intents on stdout, got none.\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Every intent should have required fields
    for intent in &intents {
        assert!(intent["symbol"].is_string(), "missing symbol: {intent}");
        assert!(intent["side"].is_string(), "missing side: {intent}");
        assert!(intent["qty"].as_f64().unwrap() > 0.0, "zero qty: {intent}");
        assert!(intent["pair_id"].is_string(), "missing pair_id: {intent}");
    }
}

// ─────────────────────────────────────────────────────────────────────
// Test 2: Warmup — first 32 bars produce no intents
// ─────────────────────────────────────────────────────────────────────

#[test]
fn live_warmup_no_intents() {
    let (dir, config_path) = setup_live_env();
    let mut child = spawn_live(&config_path, dir.path());

    let stdin = child.stdin.as_mut().unwrap();
    let base_ts: i64 = 1_700_000_000_000;

    // Feed only 30 bars (< lookback=32) — should produce zero intents
    // even with extreme prices, because z-score needs warmup
    for i in 0..30 {
        let ts = base_ts + i * 60_000;
        writeln!(stdin, "{}", bar_json("AAA", ts, 50.0)).unwrap(); // extreme deviation
        writeln!(stdin, "{}", bar_json("BBB", ts, 100.0)).unwrap();
    }

    drop(child.stdin.take());
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    let intents: Vec<serde_json::Value> = stdout
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();

    assert!(
        intents.is_empty(),
        "expected no intents during warmup, got {}",
        intents.len()
    );
}

// ─────────────────────────────────────────────────────────────────────
// Test 3: Entry signal — z exceeds entry_z → intent on stdout
// ─────────────────────────────────────────────────────────────────────

#[test]
fn live_entry_signal() {
    let (dir, config_path) = setup_live_env();
    let mut child = spawn_live(&config_path, dir.path());

    let stdin = child.stdin.as_mut().unwrap();
    let base_ts: i64 = 1_700_000_000_000;

    // Warmup with stable prices (spread ≈ 0)
    for i in 0..35 {
        let ts = base_ts + i * 60_000;
        writeln!(stdin, "{}", bar_json("AAA", ts, 100.0)).unwrap();
        writeln!(stdin, "{}", bar_json("BBB", ts, 100.0)).unwrap();
    }

    // Sharp drop in AAA → negative z-score → long spread entry
    let ts = base_ts + 35 * 60_000;
    writeln!(stdin, "{}", bar_json("AAA", ts, 90.0)).unwrap();
    writeln!(stdin, "{}", bar_json("BBB", ts, 100.0)).unwrap();

    drop(child.stdin.take());
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    let intents: Vec<serde_json::Value> = stdout
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();

    assert_eq!(
        intents.len(),
        2,
        "entry should produce 2 intents (both legs), got {}\nstderr: {}",
        intents.len(),
        String::from_utf8_lossy(&output.stderr)
    );

    // First intent: BUY AAA (long leg A when z < -entry_z)
    assert_eq!(intents[0]["symbol"].as_str().unwrap(), "AAA");
    assert_eq!(intents[0]["side"].as_str().unwrap(), "buy");
    assert_eq!(intents[0]["pair_id"].as_str().unwrap(), "AAA/BBB");

    // Second intent: SELL BBB
    assert_eq!(intents[1]["symbol"].as_str().unwrap(), "BBB");
    assert_eq!(intents[1]["side"].as_str().unwrap(), "sell");
}

// ─────────────────────────────────────────────────────────────────────
// Test 4: Exit signal — after entry, reversion triggers exit
// ─────────────────────────────────────────────────────────────────────

#[test]
fn live_entry_then_exit() {
    let (dir, config_path) = setup_live_env();
    let mut child = spawn_live(&config_path, dir.path());

    let stdin = child.stdin.as_mut().unwrap();
    let base_ts: i64 = 1_700_000_000_000;

    // Warmup
    for i in 0..35 {
        let ts = base_ts + i * 60_000;
        writeln!(stdin, "{}", bar_json("AAA", ts, 100.0)).unwrap();
        writeln!(stdin, "{}", bar_json("BBB", ts, 100.0)).unwrap();
    }

    // Entry: sharp drop in AAA
    let ts = base_ts + 35 * 60_000;
    writeln!(stdin, "{}", bar_json("AAA", ts, 90.0)).unwrap();
    writeln!(stdin, "{}", bar_json("BBB", ts, 100.0)).unwrap();

    // Reversion: AAA returns to normal → z returns near 0 → exit
    let ts = base_ts + 36 * 60_000;
    writeln!(stdin, "{}", bar_json("AAA", ts, 100.0)).unwrap();
    writeln!(stdin, "{}", bar_json("BBB", ts, 100.0)).unwrap();

    drop(child.stdin.take());
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    let intents: Vec<serde_json::Value> = stdout
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();

    // Should have at least 4 intents: 2 for entry + 2 for exit
    assert!(
        intents.len() >= 4,
        "expected entry + exit (4+ intents), got {}",
        intents.len()
    );

    // Check that we have both entry and exit reasons
    let reasons: Vec<&str> = intents
        .iter()
        .filter_map(|i| i["reason"].as_str())
        .collect();
    assert!(
        reasons.iter().any(|r| r.contains("Entry")),
        "missing entry reason in: {reasons:?}"
    );
    assert!(
        reasons
            .iter()
            .any(|r| r.contains("Exit") || r.contains("StopLoss") || r.contains("MaxHold")),
        "missing exit reason in: {reasons:?}"
    );
}

// ─────────────────────────────────────────────────────────────────────
// Test 5: Invalid JSON lines — skipped, engine continues
// ─────────────────────────────────────────────────────────────────────

#[test]
fn live_invalid_json_skipped() {
    let (dir, config_path) = setup_live_env();
    let mut child = spawn_live(&config_path, dir.path());

    let stdin = child.stdin.as_mut().unwrap();
    let base_ts: i64 = 1_700_000_000_000;

    // Feed some valid warmup bars
    for i in 0..35 {
        let ts = base_ts + i * 60_000;
        writeln!(stdin, "{}", bar_json("AAA", ts, 100.0)).unwrap();
        writeln!(stdin, "{}", bar_json("BBB", ts, 100.0)).unwrap();
    }

    // Inject garbage lines
    writeln!(stdin, "NOT VALID JSON").unwrap();
    writeln!(stdin, "{{{{broken").unwrap();
    writeln!(stdin, "").unwrap(); // empty line

    // Feed a valid entry bar after the garbage
    let ts = base_ts + 35 * 60_000;
    writeln!(stdin, "{}", bar_json("AAA", ts, 90.0)).unwrap();
    writeln!(stdin, "{}", bar_json("BBB", ts, 100.0)).unwrap();

    drop(child.stdin.take());
    let output = child.wait_with_output().unwrap();

    // Engine should survive and exit cleanly
    assert!(
        output.status.success(),
        "runner should survive invalid JSON:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Should still produce intents from the valid entry bar
    let stdout = String::from_utf8_lossy(&output.stdout);
    let intents: Vec<serde_json::Value> = stdout
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect();

    assert!(
        !intents.is_empty(),
        "engine should produce intents after recovering from invalid JSON"
    );
}

// ─────────────────────────────────────────────────────────────────────
// Test 6: Empty stdin — immediate EOF → clean shutdown
// ─────────────────────────────────────────────────────────────────────

#[test]
fn live_empty_stdin_clean_shutdown() {
    let (dir, config_path) = setup_live_env();
    let mut child = spawn_live(&config_path, dir.path());

    // Immediately close stdin
    drop(child.stdin.take());

    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "runner should exit cleanly on empty stdin:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    // No intents expected
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.trim().is_empty(),
        "expected no output on empty stdin, got: {stdout}"
    );
}

// ─────────────────────────────────────────────────────────────────────
// Test 7: Intent fields are well-formed JSON
// ─────────────────────────────────────────────────────────────────────

#[test]
fn live_intent_json_schema() {
    let (dir, config_path) = setup_live_env();
    let mut child = spawn_live(&config_path, dir.path());

    let stdin = child.stdin.as_mut().unwrap();
    let base_ts: i64 = 1_700_000_000_000;

    // Warmup + entry
    for i in 0..35 {
        let ts = base_ts + i * 60_000;
        writeln!(stdin, "{}", bar_json("AAA", ts, 100.0)).unwrap();
        writeln!(stdin, "{}", bar_json("BBB", ts, 100.0)).unwrap();
    }
    let ts = base_ts + 35 * 60_000;
    writeln!(stdin, "{}", bar_json("AAA", ts, 90.0)).unwrap();
    writeln!(stdin, "{}", bar_json("BBB", ts, 100.0)).unwrap();

    drop(child.stdin.take());
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());

    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines().filter(|l| !l.trim().is_empty()) {
        let v: serde_json::Value = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("stdout line is not valid JSON: {e}\nline: {line}"));

        // Validate required fields and types
        assert!(v["symbol"].is_string(), "symbol must be string: {v}");
        assert!(
            v["side"].as_str().unwrap() == "buy" || v["side"].as_str().unwrap() == "sell",
            "side must be buy/sell: {v}"
        );
        assert!(v["qty"].as_f64().unwrap() > 0.0, "qty must be > 0: {v}");
        assert!(v["pair_id"].is_string(), "pair_id must be string: {v}");
        assert!(
            v["z_score"].as_f64().is_some(),
            "z_score must be number: {v}"
        );
        assert!(
            v["timestamp"].as_i64().is_some(),
            "timestamp must be integer: {v}"
        );
    }
}
