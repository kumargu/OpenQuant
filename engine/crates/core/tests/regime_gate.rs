//! Integration tests for the regime gate pause/resume lifecycle.
//!
//! The regime gate pauses entries after:
//!   - 5 consecutive losing trades, OR
//!   - 3 consecutive stop-loss exits
//!
//! Entries resume after a cooldown period.
//!
//! Run with: `cargo test --test regime_gate -p openquant-core`

use openquant_core::pairs::{PairConfig, PairPosition, PairState, PairsTradingConfig};

/// Pair config with beta=1.0 for simple spread = ln(A) - ln(B).
fn test_config() -> PairConfig {
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

/// Trading config with easy-to-trigger thresholds.
/// max_hold_bars=3 for quick exits. cooldown = max(3,5)*2 = 10 bars.
fn easy_trading() -> PairsTradingConfig {
    PairsTradingConfig {
        entry_z: 1.5,
        exit_z: 0.3,
        stop_z: 5.0,
        lookback: 32,
        max_hold_bars: 3,
        min_hold_bars: 0,
        notional_per_leg: 10_000.0,
        last_entry_hour: 24,
        force_close_minute: 1_500,
        tz_offset_hours: 0,
        cost_bps: 10.0,
        max_concurrent_pairs: 0,
    }
}

/// Feed both legs at stable-ish prices to warm up or re-center rolling stats.
///
/// Uses realistic spread oscillation (±5%) so that `entry_std` at the end of warmup
/// is proportional to the entry spread magnitude (A=90 → spread ≈ -0.105). With
/// tiny jitter (±0.1%), entry_std ≈ 0.001 and the entry at A=90 would be ≈100σ —
/// far beyond any reasonable stop-loss threshold — triggering a stop on the very
/// first hold bar. Realistic oscillation gives entry_std ≈ 0.05, so entry at A=90
/// produces exit_z ≈ -2, within the normal [-stop_z, -entry_z] operating range.
/// Increment between bars in tests. Use daily intervals so each bar lands
/// at the same time of day (daily close window).
const BAR_STEP: i64 = 86_400_000; // 1 day in ms

/// Base timestamp: 15:55 UTC on day 1. With tz_offset=0, et_minutes=955 >= 950
/// → is_daily_close = true. Each BAR_STEP adds 1 day, keeping the same time.
const BASE_TS: i64 = 57_300_000; // 15:55:00 UTC in millis from epoch

fn feed_neutral(
    state: &mut PairState,
    config: &PairConfig,
    trading: &PairsTradingConfig,
    ts: &mut i64,
    count: usize,
) {
    for i in 0..count {
        // Realistic oscillation: ±5% so spread std_dev ≈ 0.05
        let jitter = 0.05 * ((i as f64 * 0.7).sin());
        state.on_price(&config.leg_a, 100.0 * (1.0 + jitter), config, trading, *ts);
        state.on_price(&config.leg_b, 100.0, config, trading, *ts);
        *ts += BAR_STEP;
    }
}

/// Execute one losing trade cycle:
/// 1. Re-center rolling stats with neutral bars
/// 2. Force entry with sharp drop
/// 3. Hold at entry price for max_hold_bars → forced exit at ~0 gross → net = -cost_bps (loss)
///
/// Returns true if the entry was allowed (not blocked by regime gate).
fn execute_losing_trade(
    state: &mut PairState,
    config: &PairConfig,
    trading: &PairsTradingConfig,
    ts: &mut i64,
) -> bool {
    // Re-center: feed neutral bars so the rolling mean returns to ~0
    feed_neutral(state, config, trading, ts, 35);

    // Entry: sharp drop in leg_a
    state.on_price(&config.leg_a, 90.0, config, trading, *ts);
    let entry = state.on_price(&config.leg_b, 100.0, config, trading, *ts);
    *ts += BAR_STEP;

    if entry.is_empty() {
        return false; // blocked by regime gate
    }

    assert_eq!(state.position(), PairPosition::LongSpread);

    // Hold at entry price → max_hold exit (3 bars). A stays at 90, B at 100.
    // Gross ≈ 0 (no price change), net = -cost_bps (loss).
    for _ in 0..5 {
        state.on_price(&config.leg_a, 90.0, config, trading, *ts);
        let exit = state.on_price(&config.leg_b, 100.0, config, trading, *ts);
        *ts += BAR_STEP;
        if !exit.is_empty() {
            break;
        }
    }

    assert_eq!(
        state.position(),
        PairPosition::Flat,
        "should have exited via max_hold"
    );
    true
}

/// Execute one stop-loss trade cycle:
/// 1. Re-center rolling stats
/// 2. Force entry
/// 3. Force stop loss by diverging spread further
fn execute_stop_loss_trade(
    state: &mut PairState,
    config: &PairConfig,
    trading: &PairsTradingConfig,
    ts: &mut i64,
) -> bool {
    feed_neutral(state, config, trading, ts, 35);

    state.on_price(&config.leg_a, 90.0, config, trading, *ts);
    let entry = state.on_price(&config.leg_b, 100.0, config, trading, *ts);
    *ts += BAR_STEP;

    if entry.is_empty() {
        return false;
    }

    assert_eq!(state.position(), PairPosition::LongSpread);

    // Stop loss: drop leg_a much further → spread diverges → |z| > stop_z
    state.on_price(&config.leg_a, 70.0, config, trading, *ts);
    let exit = state.on_price(&config.leg_b, 100.0, config, trading, *ts);
    *ts += BAR_STEP;

    assert!(
        !exit.is_empty(),
        "stop loss should fire on extreme spread divergence"
    );
    assert_eq!(state.position(), PairPosition::Flat);
    true
}

/// Attempt an entry (without re-centering) and return whether it was allowed.
fn try_entry(
    state: &mut PairState,
    config: &PairConfig,
    trading: &PairsTradingConfig,
    ts: &mut i64,
) -> bool {
    state.on_price(&config.leg_a, 90.0, config, trading, *ts);
    let intents = state.on_price(&config.leg_b, 100.0, config, trading, *ts);
    *ts += BAR_STEP;
    !intents.is_empty()
}

// ─────────────────────────────────────────────────────────────────────
// Test 1: Pause after 5 consecutive losing trades
// ─────────────────────────────────────────────────────────────────────

#[test]
fn regime_gate_pauses_after_five_losers() {
    let config = test_config();
    let trading = easy_trading();
    let mut state = PairState::new();
    let mut ts: i64 = BASE_TS;

    // Execute 5 losing trades (re-centering between each)
    for i in 0..5 {
        let entered = execute_losing_trade(&mut state, &config, &trading, &mut ts);
        assert!(entered, "trade {i}: entry should be allowed");
    }

    // Immediately try entry — should be blocked by regime gate.
    // No re-centering: the paused flag was set on the 5th trade exit.
    // Feed just 1 neutral bar so the gate check runs (needs both legs).
    state.on_price(&config.leg_a, 100.0, &config, &trading, ts);
    state.on_price(&config.leg_b, 100.0, &config, &trading, ts);
    ts += BAR_STEP;

    let blocked = !try_entry(&mut state, &config, &trading, &mut ts);
    assert!(
        blocked,
        "regime gate should block entry after 5 consecutive losers"
    );
    assert_eq!(state.position(), PairPosition::Flat);
}

// ─────────────────────────────────────────────────────────────────────
// Test 2: Cooldown resume — entries resume after enough bars
// ─────────────────────────────────────────────────────────────────────

#[test]
fn regime_gate_resumes_after_cooldown() {
    let config = test_config();
    let trading = easy_trading();
    let mut state = PairState::new();
    let mut ts: i64 = BASE_TS;

    // Trigger regime gate with 5 losers
    for _ in 0..5 {
        execute_losing_trade(&mut state, &config, &trading, &mut ts);
    }

    // Verify paused (without re-centering)
    assert!(
        !try_entry(&mut state, &config, &trading, &mut ts),
        "should be paused"
    );

    // Feed enough bars to exhaust cooldown (cooldown = max(3,5)*2 = 10).
    // Then re-center with enough neutral bars to generate valid z-score.
    feed_neutral(&mut state, &config, &trading, &mut ts, 35);

    // After cooldown + re-centering, entry should be allowed
    let entered = try_entry(&mut state, &config, &trading, &mut ts);
    assert!(entered, "entries should resume after cooldown");
    assert_eq!(state.position(), PairPosition::LongSpread);
}

// ─────────────────────────────────────────────────────────────────────
// Test 3: Re-pause — resume, lose again, re-pause fires
// ─────────────────────────────────────────────────────────────────────

#[test]
fn regime_gate_repauses_after_resume() {
    let config = test_config();
    let trading = easy_trading();
    let mut state = PairState::new();
    let mut ts: i64 = BASE_TS;

    // Trigger first pause
    for _ in 0..5 {
        execute_losing_trade(&mut state, &config, &trading, &mut ts);
    }

    // Cooldown: feed enough bars to resume
    feed_neutral(&mut state, &config, &trading, &mut ts, 35);

    // Resume — should be able to trade
    let entered = try_entry(&mut state, &config, &trading, &mut ts);
    assert!(entered, "should resume after cooldown");

    // Close this position via max hold
    for _ in 0..5 {
        state.on_price(&config.leg_a, 90.0, &config, &trading, ts);
        let exit = state.on_price(&config.leg_b, 100.0, &config, &trading, ts);
        ts += BAR_STEP;
        if !exit.is_empty() {
            break;
        }
    }

    // That was loss #6 in total, should re-pause (last 5 still all negative)
    assert!(
        !try_entry(&mut state, &config, &trading, &mut ts),
        "should re-pause after losing trade post-resume"
    );
}

// ─────────────────────────────────────────────────────────────────────
// Test 4: 3 consecutive stop losses pauses entries
// ─────────────────────────────────────────────────────────────────────

#[test]
fn regime_gate_pauses_after_three_stop_losses() {
    let config = test_config();
    let trading = easy_trading();
    let mut state = PairState::new();
    let mut ts: i64 = BASE_TS;

    // Execute 3 stop-loss trades
    for i in 0..3 {
        let entered = execute_stop_loss_trade(&mut state, &config, &trading, &mut ts);
        assert!(entered, "stop-loss trade {i}: entry should be allowed");
    }

    // Immediately try entry — should be blocked
    let blocked = !try_entry(&mut state, &config, &trading, &mut ts);
    assert!(
        blocked,
        "regime gate should block entry after 3 consecutive stop losses"
    );
}

// ─────────────────────────────────────────────────────────────────────
// Test 5: Regime gate is per-pair (one pair paused, other unaffected)
// ─────────────────────────────────────────────────────────────────────

#[test]
fn regime_gate_is_per_pair() {
    let config_ab = test_config();
    let config_cd = PairConfig {
        leg_a: "C".into(),
        leg_b: "D".into(),
        alpha: 0.0,
        beta: 1.0,
        kappa: 0.0,
        max_hold_bars: 0,
        lookback_bars: 0,
    };
    let trading = easy_trading();

    let mut state_ab = PairState::new();
    let mut state_cd = PairState::new();
    let mut ts: i64 = BASE_TS;

    // Pause A/B with 5 losers
    for _ in 0..5 {
        execute_losing_trade(&mut state_ab, &config_ab, &trading, &mut ts);
    }

    // Warm up C/D independently
    feed_neutral(&mut state_cd, &config_cd, &trading, &mut ts, 35);

    // A/B should be blocked
    assert!(
        !try_entry(&mut state_ab, &config_ab, &trading, &mut ts),
        "A/B should be paused"
    );

    // C/D should be allowed (separate state)
    state_cd.on_price(&config_cd.leg_a, 90.0, &config_cd, &trading, ts);
    let cd_entry = state_cd.on_price(&config_cd.leg_b, 100.0, &config_cd, &trading, ts);
    assert!(
        !cd_entry.is_empty(),
        "C/D should not be affected by A/B regime gate"
    );
}
