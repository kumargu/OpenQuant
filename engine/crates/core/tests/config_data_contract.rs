//! Integration test: Config TOML → PairsTradingConfig → PairState data contract.
//!
//! Verifies that config values flow correctly through the full chain:
//!   TOML file → ConfigFile → PairsTradingConfig → PairState::on_price()
//!
//! Run with: `cargo test --test config_data_contract -p openquant-core`

use openquant_core::config::ConfigFile;
use openquant_core::pairs::{PairConfig, PairPosition, PairState, PairsTradingConfig};

/// Load a config from a TOML string and extract pairs_trading + tz_offset.
fn parse_pairs_config(toml: &str) -> PairsTradingConfig {
    let cfg: ConfigFile = toml::from_str(toml).unwrap();
    let mut ptc = cfg.pairs_trading.clone();
    ptc.tz_offset_hours = cfg.data.timezone_offset_hours;
    ptc
}

fn test_pair() -> PairConfig {
    PairConfig {
        leg_a: "A".into(),
        leg_b: "B".into(),
        alpha: 0.0,
        beta: 1.0,
        kappa: 0.0,
        max_hold_bars: 0,
        lookback_bars: 0,
    }
}

/// Feed neutral bars with tiny jitter to build spread history.
/// 15:55 UTC in millis from epoch. With tz_offset=0: et_minutes=955 >= 950 → daily close.
const DAILY_CLOSE_TS: i64 = 57_300_000;
/// 1 day in ms — bars step by 1 day to stay at the same time of day.
const DAY_MS: i64 = 86_400_000;

fn warmup(
    state: &mut PairState,
    config: &PairConfig,
    trading: &PairsTradingConfig,
    ts: &mut i64,
    count: usize,
) {
    for i in 0..count {
        let jitter = 0.001 * ((i as f64 * 0.7).sin());
        state.on_price(&config.leg_a, 100.0 * (1.0 + jitter), config, trading, *ts);
        state.on_price(&config.leg_b, 100.0, config, trading, *ts);
        *ts += DAY_MS;
    }
}

// ─────────────────────────────────────────────────────────────────────
// Test 1: entry_z threshold propagation
// ─────────────────────────────────────────────────────────────────────

#[test]
fn entry_z_threshold_from_toml() {
    let config = test_pair();

    // Two configs: one with entry_z=1.5 (enters), one with entry_z=100.0 (never enters)
    let enters_cfg = parse_pairs_config(
        r#"
[pairs_trading]
entry_z = 1.5
min_hold_bars = 0
last_entry_hour = 24
force_close_minute = 1500
tz_offset_hours = 0

[data]
timezone_offset_hours = 0
"#,
    );

    let blocks_cfg = parse_pairs_config(
        r#"
[pairs_trading]
entry_z = 100.0
min_hold_bars = 0
last_entry_hour = 24
force_close_minute = 1500
tz_offset_hours = 0

[data]
timezone_offset_hours = 0
"#,
    );

    let mut ts1: i64 = DAILY_CLOSE_TS;
    let mut ts2: i64 = DAILY_CLOSE_TS;
    let mut state_enters = PairState::new();
    let mut state_blocks = PairState::new();

    warmup(&mut state_enters, &config, &enters_cfg, &mut ts1, 35);
    warmup(&mut state_blocks, &config, &blocks_cfg, &mut ts2, 35);

    // Large drop: A=90 → huge z-score. entry_z=1.5 enters, entry_z=100 doesn't.
    state_enters.on_price("A", 90.0, &config, &enters_cfg, ts1);
    let intents_enters = state_enters.on_price("B", 100.0, &config, &enters_cfg, ts1);

    state_blocks.on_price("A", 90.0, &config, &blocks_cfg, ts2);
    let intents_blocks = state_blocks.on_price("B", 100.0, &config, &blocks_cfg, ts2);

    assert!(
        !intents_enters.is_empty(),
        "entry_z=1.5 should trigger on A=90 drop"
    );
    assert!(
        intents_blocks.is_empty(),
        "entry_z=100.0 should NOT trigger (z never reaches 100)"
    );
}

// ─────────────────────────────────────────────────────────────────────
// Test 2: min_hold_bars blocks early reversion exit
// ─────────────────────────────────────────────────────────────────────

#[test]
fn min_hold_bars_from_toml() {
    let config = test_pair();

    let trading = parse_pairs_config(
        r#"
[pairs_trading]
entry_z = 1.5
exit_z = 0.3
stop_z = 10.0
min_hold_bars = 5
max_hold_bars = 150
last_entry_hour = 24
force_close_minute = 1500
tz_offset_hours = 0

[data]
timezone_offset_hours = 0
"#,
    );

    let mut state = PairState::new();
    let mut ts: i64 = DAILY_CLOSE_TS;
    warmup(&mut state, &config, &trading, &mut ts, 35);

    // Entry
    state.on_price("A", 90.0, &config, &trading, ts);
    let entry = state.on_price("B", 100.0, &config, &trading, ts);
    assert!(!entry.is_empty(), "should enter");
    assert_eq!(state.position(), PairPosition::LongSpread);
    ts += DAY_MS;

    // Immediate reversion on bar 1 — should NOT exit (min_hold_bars=5)
    state.on_price("A", 100.0, &config, &trading, ts);
    let should_hold = state.on_price("B", 100.0, &config, &trading, ts);
    assert!(
        should_hold.is_empty(),
        "min_hold_bars=5 should block exit at bar 1"
    );
    assert_eq!(state.position(), PairPosition::LongSpread);
    ts += DAY_MS;

    // Feed a few more bars (bars 2-4) with A still reverted
    for _ in 0..3 {
        state.on_price("A", 100.0, &config, &trading, ts);
        state.on_price("B", 100.0, &config, &trading, ts);
        ts += DAY_MS;
    }

    // Bar 5 (past min_hold_bars) — reversion should now exit
    state.on_price("A", 100.0, &config, &trading, ts);
    let should_exit = state.on_price("B", 100.0, &config, &trading, ts);
    assert!(
        !should_exit.is_empty(),
        "should exit after min_hold_bars=5 passed"
    );
    assert_eq!(state.position(), PairPosition::Flat);
}

// ─────────────────────────────────────────────────────────────────────
// Test 3: notional_per_leg → intent qty = floor(notional / price)
// ─────────────────────────────────────────────────────────────────────

#[test]
fn notional_per_leg_sizing() {
    let config = test_pair();

    let trading = parse_pairs_config(
        r#"
[pairs_trading]
entry_z = 1.5
notional_per_leg = 5000.0
min_hold_bars = 0
last_entry_hour = 24
force_close_minute = 1500
tz_offset_hours = 0

[data]
timezone_offset_hours = 0
"#,
    );

    let mut state = PairState::new();
    let mut ts: i64 = DAILY_CLOSE_TS;
    warmup(&mut state, &config, &trading, &mut ts, 35);

    // Entry at A=90, B=100
    // qty_A = floor(5000 / 90) = 55
    // qty_B = floor(5000 / 100) = 50
    state.on_price("A", 90.0, &config, &trading, ts);
    let intents = state.on_price("B", 100.0, &config, &trading, ts);

    assert_eq!(intents.len(), 2);
    assert_eq!(intents[0].qty, 55.0, "qty_A = floor(5000/90) = 55");
    assert_eq!(intents[1].qty, 50.0, "qty_B = floor(5000/100) = 50");
}

// ─────────────────────────────────────────────────────────────────────
// Test 4: last_entry_hour blocks late-day entries
// ─────────────────────────────────────────────────────────────────────

#[test]
fn last_entry_hour_blocks_late_entries() {
    let config = test_pair();

    // Start with last_entry_hour = 16 (allows entries at 15:55 ET)
    let trading = parse_pairs_config(
        r#"
[pairs_trading]
entry_z = 1.5
min_hold_bars = 0
last_entry_hour = 16
force_close_minute = 1500
tz_offset_hours = -5

[data]
timezone_offset_hours = -5
"#,
    );

    let mut state = PairState::new();
    let base_utc = 1_768_435_200_000_i64;
    let mut ts = base_utc; // midnight UTC → daily close bar

    warmup(&mut state, &config, &trading, &mut ts, 35);

    // Entry at 15:55 ET — daily close bar, last_entry_hour=16 allows it
    ts = base_utc + 20 * 3600 * 1000 + 55 * 60 * 1000; // 15:55 EST
    state.on_price("A", 90.0, &config, &trading, ts);
    let intents = state.on_price("B", 100.0, &config, &trading, ts);
    assert!(
        !intents.is_empty(),
        "15:55 ET should allow entry (daily close, before last_entry_hour=16)"
    );

    // Exit
    ts += 86_400_000; // next day midnight (daily close)
    state.on_price("A", 100.0, &config, &trading, ts);
    state.on_price("B", 100.0, &config, &trading, ts);

    // Re-center
    for i in 0..35 {
        let jitter = 0.05 * ((i as f64 * 0.7).sin());
        ts = base_utc + 86_400_000 * (i + 2) as i64; // daily close timestamps
        state.on_price("A", 100.0 * (1.0 + jitter), &config, &trading, ts);
        state.on_price("B", 100.0, &config, &trading, ts);
    }

    // Now test with last_entry_hour = 14 — should block entry at 15:55 ET
    let mut trading_blocked = trading.clone();
    trading_blocked.last_entry_hour = 14;
    ts = base_utc + 20 * 3600 * 1000 + 55 * 60 * 1000; // 15:55 EST
    state.on_price("A", 90.0, &config, &trading_blocked, ts);
    let intents = state.on_price("B", 100.0, &config, &trading_blocked, ts);
    assert!(
        intents.is_empty(),
        "15:55 ET should block entry (past last_entry_hour=14)"
    );
}

// ─────────────────────────────────────────────────────────────────────
// Test 5: force_close_minute triggers EOD exit
// ─────────────────────────────────────────────────────────────────────

#[test]
fn force_close_minute_triggers_exit() {
    let config = test_pair();

    let trading = parse_pairs_config(
        r#"
[pairs_trading]
entry_z = 1.5
exit_z = 0.3
stop_z = 10.0
min_hold_bars = 0
max_hold_bars = 999
last_entry_hour = 24
force_close_minute = 930
tz_offset_hours = -5

[data]
timezone_offset_hours = -5
"#,
    );

    let mut state = PairState::new();

    // Warmup: midnight UTC = 19:00 EST → et_minutes=1140 >= 950 → daily close
    let base_utc = 1_768_435_200_000_i64;
    let mut ts = base_utc;

    warmup(&mut state, &config, &trading, &mut ts, 35);

    // Entry at 15:55 ET (20:55 UTC) — daily close bar, last_entry_hour=24 allows it
    ts = base_utc + 20 * 3600 * 1000 + 55 * 60 * 1000; // 15:55 EST
    state.on_price("A", 90.0, &config, &trading, ts);
    let entry = state.on_price("B", 100.0, &config, &trading, ts);
    assert!(!entry.is_empty(), "should enter at 15:55 ET (daily close)");

    // Next day: hold with spread extended (no reversion) at 15:30 ET
    // 15:30 ET = 930 minutes from midnight ET → triggers force_close_minute=930
    let next_day = base_utc + 86_400_000; // next day
    let close_ts = next_day + 20 * 3600 * 1000 + 30 * 60 * 1000; // 15:30 EST
    state.on_price("A", 90.0, &config, &trading, close_ts);
    let exit = state.on_price("B", 100.0, &config, &trading, close_ts);

    assert!(
        !exit.is_empty(),
        "force_close_minute=930 should trigger exit at 15:30 ET"
    );
    assert_eq!(state.position(), PairPosition::Flat);
}

// ─────────────────────────────────────────────────────────────────────
// Test 6: tz_offset_hours propagation from [data] section
// ─────────────────────────────────────────────────────────────────────

#[test]
fn tz_offset_hours_synced_from_data() {
    // The runner syncs pairs_trading.tz_offset_hours = data.timezone_offset_hours.
    // Verify the config file parsing preserves these values.
    let toml = r#"
[data]
timezone_offset_hours = -4

[pairs_trading]
tz_offset_hours = -4
"#;
    let cfg: ConfigFile = toml::from_str(toml).unwrap();
    assert_eq!(cfg.data.timezone_offset_hours, -4);
    assert_eq!(cfg.pairs_trading.tz_offset_hours, -4);

    // When runner syncs them:
    let mut ptc = cfg.pairs_trading.clone();
    ptc.tz_offset_hours = cfg.data.timezone_offset_hours;
    assert_eq!(ptc.tz_offset_hours, -4, "tz_offset should be -4 (EDT)");
}

// ─────────────────────────────────────────────────────────────────────
// Test 7: Full TOML round-trip — parse, build engine, trade
// ─────────────────────────────────────────────────────────────────────

#[test]
fn full_toml_round_trip() {
    let toml = r#"
mode = "pairs"

[data]
timezone_offset_hours = 0

[pairs_trading]
entry_z = 1.5
exit_z = 0.3
stop_z = 5.0
lookback = 32
max_hold_bars = 150
min_hold_bars = 0
notional_per_leg = 7500.0
last_entry_hour = 24
force_close_minute = 1500
tz_offset_hours = 0
"#;

    let cfg: ConfigFile = toml::from_str(toml).unwrap();
    let mut ptc = cfg.pairs_trading.clone();
    ptc.tz_offset_hours = cfg.data.timezone_offset_hours;

    assert_eq!(ptc.entry_z, 1.5);
    assert_eq!(ptc.exit_z, 0.3);
    assert_eq!(ptc.stop_z, 5.0);
    assert_eq!(ptc.notional_per_leg, 7500.0);

    // Build a PairState and verify it uses these values
    let config = test_pair();
    let mut state = PairState::new();
    let mut ts: i64 = DAILY_CLOSE_TS;
    warmup(&mut state, &config, &ptc, &mut ts, 35);

    // Entry at A=90, B=100
    state.on_price("A", 90.0, &config, &ptc, ts);
    let intents = state.on_price("B", 100.0, &config, &ptc, ts);

    assert!(!intents.is_empty());
    // qty = floor(7500 / 90) = 83
    assert_eq!(
        intents[0].qty, 83.0,
        "notional_per_leg=7500, price=90 → qty=83"
    );
    // qty = floor(7500 / 100) = 75
    assert_eq!(
        intents[1].qty, 75.0,
        "notional_per_leg=7500, price=100 → qty=75"
    );
}
