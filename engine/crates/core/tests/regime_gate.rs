//! Integration tests for the regime gate pause/resume lifecycle.
//!
//! The regime gate pauses entries after:
//!   - 5 consecutive losing trades, OR
//!   - 3 consecutive stop-loss exits
//!
//! Entries resume after a 500-bar cooldown.
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
    }
}

/// Trading config with easy-to-trigger thresholds.
fn easy_trading() -> PairsTradingConfig {
    PairsTradingConfig {
        entry_z: 1.5,
        exit_z: 0.3,
        stop_z: 5.0,
        lookback: 32,
        max_hold_bars: 3, // force exit quickly
        min_hold_bars: 0,
        notional_per_leg: 10_000.0,
        last_entry_hour: 24,
        force_close_minute: 1_500,
        tz_offset_hours: 0,
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
fn feed_neutral(
    state: &mut PairState,
    config: &PairConfig,
    trading: &PairsTradingConfig,
    ts: &mut i64,
    count: usize,
) {
    for i in 0..count {
        // Realistic oscillation: ±5% so spread std_dev ≈ 0.05
        // With A=90 entry, spread ≈ -0.105, entry_z ≈ -0.105/0.05 ≈ -2.1
        let jitter = 0.05 * ((i as f64 * 0.7).sin());
        state.on_price(&config.leg_a, 100.0 * (1.0 + jitter), config, trading, *ts);
        state.on_price(&config.leg_b, 100.0, config, trading, *ts);
        *ts += 60_000;
    }
}

/// Execute one losing trade cycle:
/// 1. Re-center rolling stats with neutral bars
/// 2. Force entry with sharp drop
/// 3. Hold at entry price for max_hold_bars → forced exit at ~0 gross → net = -12 bps (loss)
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
    *ts += 60_000;

    if entry.is_empty() {
        return false; // blocked by regime gate
    }

    assert_eq!(state.position(), PairPosition::LongSpread);

    // Hold at entry price → max_hold exit. A stays at 90, B at 100.
    // Gross ≈ 0 (no price change), net = -12 bps (cost only).
    for _ in 0..5 {
        state.on_price(&config.leg_a, 90.0, config, trading, *ts);
        let exit = state.on_price(&config.leg_b, 100.0, config, trading, *ts);
        *ts += 60_000;
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
    *ts += 60_000;

    if entry.is_empty() {
        return false;
    }

    assert_eq!(state.position(), PairPosition::LongSpread);

    // Stop loss: drop leg_a much further → spread diverges → |z| > stop_z
    state.on_price(&config.leg_a, 70.0, config, trading, *ts);
    let exit = state.on_price(&config.leg_b, 100.0, config, trading, *ts);
    *ts += 60_000;

    assert!(
        !exit.is_empty(),
        "stop loss should fire on extreme spread divergence"
    );
    assert_eq!(state.position(), PairPosition::Flat);
    true
}

// ─────────────────────────────────────────────────────────────────────
// Test 1: Pause after 5 consecutive losing trades
// ─────────────────────────────────────────────────────────────────────

#[test]
fn regime_gate_pauses_after_five_losers() {
    let config = test_config();
    let trading = easy_trading();
    let mut state = PairState::new();
    let mut ts: i64 = 1_000_000;

    // Execute 5 losing trades (re-centering between each)
    for i in 0..5 {
        let entered = execute_losing_trade(&mut state, &config, &trading, &mut ts);
        assert!(entered, "trade {i}: entry should be allowed");
    }

    // 6th entry should be blocked by regime gate
    feed_neutral(&mut state, &config, &trading, &mut ts, 35);
    state.on_price(&config.leg_a, 90.0, &config, &trading, ts);
    let intents = state.on_price(&config.leg_b, 100.0, &config, &trading, ts);
    assert!(
        intents.is_empty(),
        "regime gate should block entry after 5 consecutive losers"
    );
    assert_eq!(state.position(), PairPosition::Flat);
}

// ─────────────────────────────────────────────────────────────────────
// Test 2: Cooldown resume — entries resume after 500 bars
// ─────────────────────────────────────────────────────────────────────

#[test]
fn regime_gate_resumes_after_cooldown() {
    let config = test_config();
    let trading = easy_trading();
    let mut state = PairState::new();
    let mut ts: i64 = 1_000_000;

    // Trigger regime gate with 5 losers
    for _ in 0..5 {
        execute_losing_trade(&mut state, &config, &trading, &mut ts);
    }

    // Verify paused
    feed_neutral(&mut state, &config, &trading, &mut ts, 35);
    state.on_price(&config.leg_a, 90.0, &config, &trading, ts);
    let blocked = state.on_price(&config.leg_b, 100.0, &config, &trading, ts);
    assert!(blocked.is_empty(), "should be paused");
    ts += 60_000;

    // Feed 500+ bars to exhaust cooldown.
    // Each call to on_price with both legs increments the pause counter.
    feed_neutral(&mut state, &config, &trading, &mut ts, 500);

    // After cooldown, entry should be allowed
    state.on_price(&config.leg_a, 90.0, &config, &trading, ts);
    let intents = state.on_price(&config.leg_b, 100.0, &config, &trading, ts);
    assert!(
        !intents.is_empty(),
        "entries should resume after 500-bar cooldown"
    );
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
    let mut ts: i64 = 1_000_000;

    // Phase 1: 5 losers → pause
    for _ in 0..5 {
        execute_losing_trade(&mut state, &config, &trading, &mut ts);
    }

    // Phase 2: cooldown
    feed_neutral(&mut state, &config, &trading, &mut ts, 550);

    // Phase 3: After cooldown, first trade is allowed (gate lifted).
    // But trade history still has old losses. If this trade also loses,
    // the last 5 trades are all negative → immediate re-pause.
    let entered = execute_losing_trade(&mut state, &config, &trading, &mut ts);
    assert!(entered, "first post-cooldown trade should be allowed");

    // Should be re-paused immediately (old losses + new loss = 6 consecutive)
    feed_neutral(&mut state, &config, &trading, &mut ts, 35);
    state.on_price(&config.leg_a, 90.0, &config, &trading, ts);
    let intents = state.on_price(&config.leg_b, 100.0, &config, &trading, ts);
    assert!(
        intents.is_empty(),
        "regime gate should re-pause after losing post-cooldown"
    );
}

// ─────────────────────────────────────────────────────────────────────
// Test 4: Stop-loss trigger — 3 consecutive stop-outs → pause
// ─────────────────────────────────────────────────────────────────────

#[test]
fn regime_gate_pauses_after_three_stop_losses() {
    let config = test_config();
    let mut trading = easy_trading();
    trading.stop_z = 3.0; // lower threshold to trigger on A=70 deviation
    let mut state = PairState::new();
    let mut ts: i64 = 1_000_000;

    // Execute 3 stop-loss trades
    for i in 0..3 {
        let entered = execute_stop_loss_trade(&mut state, &config, &trading, &mut ts);
        assert!(entered, "stop trade {i}: entry should be allowed");
    }

    // 4th entry should be blocked
    feed_neutral(&mut state, &config, &trading, &mut ts, 35);
    state.on_price(&config.leg_a, 90.0, &config, &trading, ts);
    let intents = state.on_price(&config.leg_b, 100.0, &config, &trading, ts);
    assert!(
        intents.is_empty(),
        "regime gate should block entry after 3 consecutive stop-losses"
    );
}

// ─────────────────────────────────────────────────────────────────────
// Test 5: Regime gate is per-pair (not global)
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
    };
    let trading = easy_trading();

    let mut state_ab = PairState::new();
    let mut state_cd = PairState::new();
    let mut ts: i64 = 1_000_000;

    // Pause A/B with 5 losers
    for _ in 0..5 {
        execute_losing_trade(&mut state_ab, &config_ab, &trading, &mut ts);
    }

    // Warm up C/D (needs its own symbols)
    feed_neutral(&mut state_cd, &config_cd, &trading, &mut ts, 35);

    // A/B should be paused
    feed_neutral(&mut state_ab, &config_ab, &trading, &mut ts, 35);
    state_ab.on_price(&config_ab.leg_a, 90.0, &config_ab, &trading, ts);
    let intents_ab = state_ab.on_price(&config_ab.leg_b, 100.0, &config_ab, &trading, ts);
    assert!(intents_ab.is_empty(), "A/B should be paused");

    // C/D should NOT be paused
    state_cd.on_price(&config_cd.leg_a, 90.0, &config_cd, &trading, ts);
    let intents_cd = state_cd.on_price(&config_cd.leg_b, 100.0, &config_cd, &trading, ts);
    assert!(
        !intents_cd.is_empty(),
        "C/D should NOT be affected by A/B's regime gate"
    );
}
