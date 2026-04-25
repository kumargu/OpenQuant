//! Live / paper / replay runner for the basket spread engine.
//!
//! Drives `basket_engine::BasketEngine` (continuous streaming state machine)
//! with bars from either the Alpaca WebSocket (live, paper) or per-symbol
//! parquet files via [`crate::parquet_bar_source::ParquetBarSource`] (replay).
//! All three modes flow through `run_basket_live`; the only difference is
//! which `Broker`, `BarSource`, `Clock`, and `SessionTrigger` impls are
//! passed in.
//!
//! Flow per trading day:
//!   1. Startup: load the frozen basket fit artifact and build `BasketEngine`
//!      from those persisted `BasketFit`s. Engine enters with empty state.
//!   2. Bar loop: for each 1-min bar, update per-symbol "last RTH bar".
//!   3. Session close (final RTH minute after close+grace): snapshot the
//!      day's closes, call
//!      `BasketEngine::on_bars()`, get `PositionIntent`s.
//!   4. Portfolio: aggregate intents → admit active baskets → convert target
//!      notionals to target shares → `OrderIntent`s via `diff_to_orders()`.
//!   5. Execute: depending on `BasketExecution`, log only (Noop), or place
//!      orders on paper/live Alpaca.
//!
//! Three execution modes:
//!   - `Noop`: log intents, place no orders. Use this for the first sessions
//!     to verify engine behavior before any capital moves.
//!   - `Paper`: paper-api.alpaca.markets (paper money).
//!   - `Live`: api.alpaca.markets (real money). Gated behind explicit opt-in.

use std::collections::HashMap;
use std::path::Path;

use basket_engine::{
    diff_to_orders, plan_portfolio_for_equity, BasketEngine, DailyBar, OrderIntent,
    PortfolioConfig, PositionIntent, Side,
};
use basket_picker::{load_universe, BasketFit};
use chrono::{DateTime, NaiveDate, Utc};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use tracing::{debug, error, info, warn};

use crate::alpaca::ExecutionMode;
use crate::bar_source::BarSource;
use crate::broker::Broker;
use crate::clock::Clock;
use crate::market_session;
use crate::session_trigger::SessionTrigger;
use crate::stream;

/// Execution mode for basket live/paper.
///
/// Distinct from [`ExecutionMode`] because basket adds a `Noop` shadow mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BasketExecution {
    /// Log intents only; no Alpaca order placed.
    Noop,
    /// Paper trading API.
    Paper,
    /// Real-money trading API. Requires explicit `--execution live`.
    Live,
}

impl BasketExecution {
    /// Map to the Alpaca adapter's [`ExecutionMode`]. Noop returns None.
    fn alpaca_mode(self) -> Option<ExecutionMode> {
        match self {
            Self::Noop => None,
            Self::Paper => Some(ExecutionMode::Paper),
            Self::Live => Some(ExecutionMode::Live),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Noop => "NOOP (shadow)",
            Self::Paper => "PAPER",
            Self::Live => "LIVE",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StartupPhase {
    NonTradingDay,
    PreOpen,
    Intraday,
    PostClosePendingCatchup,
    PostCloseProcessed,
}

impl StartupPhase {
    fn as_str(self) -> &'static str {
        match self {
            Self::NonTradingDay => "non_trading_day",
            Self::PreOpen => "pre_open",
            Self::Intraday => "intraday",
            Self::PostClosePendingCatchup => "post_close_pending_catchup",
            Self::PostCloseProcessed => "post_close_processed",
        }
    }
}

async fn preflight_account_check(broker: &impl Broker, mode: ExecutionMode) -> Result<(), String> {
    let account = broker.get_account(mode).await?;
    let buying_power = parse_buying_power(&account)?;
    if account.status != "ACTIVE" {
        return Err(format!(
            "Alpaca account not ACTIVE: status={}",
            account.status
        ));
    }
    if account.trading_blocked || account.account_blocked {
        return Err(format!(
            "Alpaca account blocked: trading_blocked={}, account_blocked={}",
            account.trading_blocked, account.account_blocked
        ));
    }
    info!(
        mode = ?mode,
        buying_power = %format!("{:.0}", buying_power),
        status = account.status.as_str(),
        "startup account preflight passed"
    );
    Ok(())
}

fn parse_buying_power(account: &crate::alpaca::AlpacaAccount) -> Result<f64, String> {
    let buying_power = account.buying_power.parse::<f64>().map_err(|e| {
        format!(
            "invalid Alpaca buying_power '{}': {e}",
            account.buying_power
        )
    })?;
    if !buying_power.is_finite() || buying_power <= 0.0 {
        return Err(format!(
            "Alpaca buying power is not positive: {}",
            account.buying_power
        ));
    }
    Ok(buying_power)
}

/// Parse account equity from the broker snapshot. Used at session-close
/// time to size per-basket notionals dynamically from current equity
/// instead of static initial capital — defends against the Q4 2024
/// feedback loop where a drawdown left target gross above buying power
/// and the broker rejected orders piecemeal, lopsiding the hedge book.
fn parse_equity(account: &crate::alpaca::AlpacaAccount) -> Result<f64, String> {
    let equity = account
        .equity
        .parse::<f64>()
        .map_err(|e| format!("invalid Alpaca equity '{}': {e}", account.equity))?;
    if !equity.is_finite() || equity <= 0.0 {
        return Err(format!("Alpaca equity is not positive: {}", account.equity));
    }
    Ok(equity)
}

async fn check_order_set_affordability(
    broker: &impl Broker,
    mode: ExecutionMode,
    date: NaiveDate,
    current_shares: &HashMap<String, f64>,
    target_shares: &HashMap<String, f64>,
    orders: &[OrderIntent],
    closes: &HashMap<String, f64>,
) -> Result<(), String> {
    let account = broker.get_account(mode).await?;
    let buying_power = parse_buying_power(&account)?;
    let (current_long_gross, current_short_gross) = gross_by_side(current_shares, closes);
    let (target_long_gross, target_short_gross) = gross_by_side(target_shares, closes);
    let incremental_long = (target_long_gross - current_long_gross).max(0.0);
    let incremental_short = (target_short_gross - current_short_gross).max(0.0);
    let incremental_exposure = incremental_long + incremental_short;
    let order_turnover: f64 = orders
        .iter()
        .filter_map(|o| closes.get(&o.symbol).map(|p| p * o.qty as f64))
        .sum();
    if incremental_exposure > buying_power + 1.0 {
        return Err(format!(
            "incremental exposure {:.2} exceeds Alpaca buying power {:.2} on {}",
            incremental_exposure, buying_power, date
        ));
    }
    info!(
        date = %date,
        current_long_gross = %format!("{:.0}", current_long_gross),
        current_short_gross = %format!("{:.0}", current_short_gross),
        target_long_gross = %format!("{:.0}", target_long_gross),
        target_short_gross = %format!("{:.0}", target_short_gross),
        incremental_long_notional = %format!("{:.0}", incremental_long),
        incremental_short_notional = %format!("{:.0}", incremental_short),
        incremental_exposure_notional = %format!("{:.0}", incremental_exposure),
        order_turnover_notional = %format!("{:.0}", order_turnover),
        buying_power = %format!("{:.0}", buying_power),
        "order-set affordability check passed"
    );
    Ok(())
}

fn gross_by_side(shares: &HashMap<String, f64>, closes: &HashMap<String, f64>) -> (f64, f64) {
    let mut long_gross = 0.0;
    let mut short_gross = 0.0;
    for (symbol, qty) in shares {
        let Some(price) = closes.get(symbol) else {
            continue;
        };
        let notional = qty * price;
        if notional > 0.0 {
            long_gross += notional;
        } else {
            short_gross += notional.abs();
        }
    }
    (long_gross, short_gross)
}

fn summarize_orders_by_side(
    orders: &[OrderIntent],
    closes: &HashMap<String, f64>,
) -> (usize, usize, f64, f64) {
    let mut buy_count = 0usize;
    let mut sell_count = 0usize;
    let mut buy_notional = 0.0_f64;
    let mut sell_notional = 0.0_f64;
    for order in orders {
        let notional = closes
            .get(&order.symbol)
            .map(|price| *price * order.qty as f64)
            .filter(|n| n.is_finite() && *n > 0.0)
            .unwrap_or(0.0);
        match order.side {
            Side::Buy => {
                buy_count += 1;
                buy_notional += notional;
            }
            Side::Sell => {
                sell_count += 1;
                sell_notional += notional;
            }
        }
    }
    (buy_count, sell_count, buy_notional, sell_notional)
}

fn order_reason_fields(reason: &basket_engine::OrderReason) -> (&'static str, Option<&str>) {
    match reason {
        basket_engine::OrderReason::Entry { basket_id } => ("entry", Some(basket_id.as_str())),
        basket_engine::OrderReason::Flip { basket_id } => ("flip", Some(basket_id.as_str())),
        basket_engine::OrderReason::Rebalance => ("rebalance", None),
        basket_engine::OrderReason::Aggregated => ("aggregated", None),
    }
}

async fn wait_for_stream_health(
    bar_rx: &mut tokio::sync::mpsc::Receiver<stream::StreamBar>,
    timeout_secs: u64,
) -> Result<Option<stream::StreamBar>, String> {
    match tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), bar_rx.recv()).await {
        Ok(Some(bar)) => Ok(Some(bar)),
        Ok(None) => Err("stream closed before first startup bar arrived".to_string()),
        Err(_) => Err(format!(
            "stream health gate timed out after {}s without any live bar",
            timeout_secs
        )),
    }
}

fn classify_startup_phase(
    now: DateTime<Utc>,
    last_processed_trading_day: Option<NaiveDate>,
    close_grace_min: u32,
) -> StartupPhase {
    let today = market_session::trading_day_utc(now);
    if !market_session::is_trading_day(today) {
        StartupPhase::NonTradingDay
    } else if market_session::is_after_close_grace_utc(now, close_grace_min) {
        if last_processed_trading_day == Some(today) {
            StartupPhase::PostCloseProcessed
        } else {
            StartupPhase::PostClosePendingCatchup
        }
    } else if market_session::is_rth_utc(now) {
        StartupPhase::Intraday
    } else {
        StartupPhase::PreOpen
    }
}

/// Run the basket live/paper loop.
///
/// Returns on Ctrl+C or fatal error.
#[allow(clippy::too_many_arguments)]
pub async fn run_basket_live(
    broker: &impl Broker,
    bar_source: &impl BarSource,
    clock: &impl Clock,
    session_trigger: &mut impl SessionTrigger,
    universe_path: &Path,
    state_path: &Path,
    bars_dir: &Path,
    execution: BasketExecution,
    portfolio_config: PortfolioConfig,
    fits: &[BasketFit],
) -> Result<(), String> {
    // Grace period after session close before firing the engine. Lets late-arriving
    // final-RTH-minute bars land in the buffer.
    //
    // The `clock` and `session_trigger` parameters MUST agree on this value:
    // `IntervalSessionTrigger` is constructed with the same constant in
    // `main.rs`. If they diverge, replay/live cadence drifts.
    const CLOSE_GRACE_MIN: u32 = 2;

    info!(
        universe = %universe_path.display(),
        state_path = %state_path.display(),
        bars_dir = %bars_dir.display(),
        execution = execution.label(),
        n_fits = fits.len(),
        "========== BASKET LIVE RUNNER =========="
    );
    portfolio_config.validate()?;

    if execution == BasketExecution::Live {
        warn!("LIVE MODE — real-money orders will be placed on every EOD signal");
    }
    if let Some(mode) = execution.alpaca_mode() {
        preflight_account_check(broker, mode).await?;
    }

    // 1. Load universe + frozen fit artifact.
    let universe = load_universe(universe_path)?;
    info!(
        baskets = universe.num_baskets(),
        sectors = universe.sectors.len(),
        "loaded universe"
    );

    let symbols = collect_symbols(&universe);
    info!(
        symbols = symbols.len(),
        fits = fits.len(),
        "loaded frozen basket fit artifact"
    );

    let valid_count = fits.iter().filter(|f| f.valid).count();
    info!(
        total = fits.len(),
        valid = valid_count,
        "loaded basket fits"
    );
    if valid_count == 0 {
        // Tally rejection reasons so the operator can see WHY all
        // baskets failed — vital when replay's auto-fit produces 0
        // valid fits and you don't know whether it's a data window
        // problem, a numerical fit problem, or a config problem.
        let mut reasons: std::collections::BTreeMap<String, usize> =
            std::collections::BTreeMap::new();
        for f in fits {
            let reason = f
                .reject_reason
                .clone()
                .unwrap_or_else(|| "unknown".to_string());
            *reasons.entry(reason).or_insert(0) += 1;
        }
        for (reason, count) in &reasons {
            error!(reason = %reason, count, "fit rejected");
        }
        return Err("no valid baskets in fit artifact".to_string());
    }

    let expected_ids: std::collections::HashSet<String> = fits
        .iter()
        .filter(|f| f.valid)
        .map(|f| f.candidate.id())
        .collect();
    let state_exists = state_path.exists();
    let mut last_processed_trading_day = None;
    let mut engine = if state_exists {
        let snapshot = BasketEngine::load_snapshot(state_path)?;
        let loaded_ids: std::collections::HashSet<String> =
            snapshot.states.keys().cloned().collect();
        if loaded_ids != expected_ids {
            return Err(format!(
                "state snapshot basket set mismatch: snapshot={}, artifact={}",
                loaded_ids.len(),
                expected_ids.len()
            ));
        }
        last_processed_trading_day = snapshot.last_processed_trading_day;
        let mut fresh = BasketEngine::new(fits);
        fresh.apply_states(snapshot.states)?;
        info!(
            baskets = fresh.num_baskets(),
            state_path = %state_path.display(),
            last_processed = ?last_processed_trading_day,
            "loaded basket runtime state onto current fit artifact params"
        );
        fresh
    } else {
        let fresh = BasketEngine::new(fits);
        info!(baskets = fresh.num_baskets(), "basket engine initialized");
        fresh
    };

    // Push the active-basket cap into the engine so it can self-enforce
    // at entry time. Without this, all 43 baskets generated initial
    // entries every session and the portfolio cap flattened the
    // lower-|z| ones — including baskets that were *mid-mean-reversion*
    // (smaller |z|) at exactly the moment they were about to pay off.
    // Q4 2025 telemetry confirmed: 2,153 transitions, 0 flips. Moving
    // the cap to entry time (FCFS-style admission) lets entered
    // positions live until their own engine-level exit signal fires.
    engine.set_max_active_positions(portfolio_config.n_active_baskets);
    info!(
        max_active_positions = portfolio_config.n_active_baskets,
        "engine configured with entry-time active-basket cap"
    );

    // 2. Seed current_shares from Alpaca positions (startup reconciliation).
    //    Without this, a restart with live open positions would trigger
    //    target-minus-zero share deltas, flooding Alpaca with duplicate orders.
    //    Noop skips this (no Alpaca account needed for shadow mode).
    //    Paper/Live FAIL CLOSED: if reconciliation cannot load open positions,
    //    we refuse to start. Trading from an empty share map would diff
    //    targets against zero and flood Alpaca with duplicate orders against
    //    already-open broker positions, potentially double-sizing every leg.
    let now = clock.now();
    let today = market_session::trading_day_utc(now);
    let mut current_shares = match execution.alpaca_mode() {
        None => {
            info!("noop mode — skipping startup position reconciliation");
            HashMap::new()
        }
        Some(mode) => seed_current_shares_from_alpaca(broker, mode, &symbols).await?,
    };
    if execution.alpaca_mode().is_some() && !state_exists && !current_shares.is_empty() {
        error!(
            state_path = %state_path.display(),
            broker_positions = current_shares.len(),
            "broker has open positions but no basket state snapshot was found"
        );
        return Err(format!(
            "open broker positions found but no basket state snapshot exists at {}",
            state_path.display()
        ));
    }
    let startup_phase = classify_startup_phase(now, last_processed_trading_day, CLOSE_GRACE_MIN);
    info!(
        now_utc = %now.to_rfc3339(),
        trading_day = %today,
        startup_phase = startup_phase.as_str(),
        state_exists,
        last_processed = ?last_processed_trading_day,
        broker_positions = current_shares.len(),
        "basket startup phase evaluated"
    );

    // 3. Bar loop: buffer per (symbol, date) → last RTH bar.
    //    Engine is triggered by a wall-clock timer (not by one symbol's final RTH bar
    //    arrival) so that no single symbol becoming a data source-of-failure can
    //    silently skip an entire session.
    let mut day_closes: HashMap<NaiveDate, HashMap<String, f64>> = HashMap::new();
    let mut processed_sessions: std::collections::HashSet<NaiveDate> = Default::default();
    if last_processed_trading_day == Some(today) {
        processed_sessions.insert(today);
    }

    if market_session::is_trading_day(today)
        && market_session::is_after_close_grace_utc(now, CLOSE_GRACE_MIN)
        && last_processed_trading_day != Some(today)
    {
        let catchup_closes = load_close_snapshot_for_day(bars_dir, &symbols, today)?;
        info!(
            date = %today,
            symbols = catchup_closes.len(),
            "startup is after close grace on an unprocessed trading day — running one catch-up close cycle"
        );
        process_session_close(
            &mut engine,
            broker,
            today,
            &catchup_closes,
            &portfolio_config,
            &mut current_shares,
            execution,
        )
        .await?;
        // Hook for replay's daily-equity time series. Noop on AlpacaClient.
        broker.record_eod(today).await;
        last_processed_trading_day = Some(today);
        engine.save_state_with_day(state_path, last_processed_trading_day)?;
        processed_sessions.insert(today);
        info!(
            date = %today,
            state_path = %state_path.display(),
            last_processed = ?last_processed_trading_day,
            "catch-up close cycle completed and startup state persisted"
        );
    }

    // 4. Subscribe to all universe symbols via the bar source.
    let mut bar_rx = bar_source.start(&symbols).await;
    info!("subscribed to bar source for 1-min bars");

    if market_session::is_trading_day(today) && market_session::is_rth_utc(now) {
        match wait_for_stream_health(&mut bar_rx, 90).await {
            Ok(Some(startup_bar)) => {
                let bar_open_ts_ms = startup_bar.timestamp - 60_000;
                if let Some(dt) = DateTime::<Utc>::from_timestamp_millis(bar_open_ts_ms) {
                    if market_session::is_rth_utc(dt)
                        && startup_bar.close.is_finite()
                        && startup_bar.close > 0.0
                    {
                        let date = market_session::trading_day_utc(dt);
                        day_closes
                            .entry(date)
                            .or_default()
                            .insert(startup_bar.symbol.clone(), startup_bar.close);
                        info!(
                            symbol = startup_bar.symbol.as_str(),
                            date = %date,
                            close = startup_bar.close,
                            "startup stream health gate passed"
                        );
                    }
                }
            }
            Ok(None) => {}
            Err(e) => warn!(
                error = e.as_str(),
                "startup stream health gate did not observe a first bar; continuing"
            ),
        }
    }

    // Dedicated 60s heartbeat that summarizes buffer state. The session-close
    // schedule is owned by `session_trigger` (see `SessionTrigger` trait); the
    // heartbeat is observability only.
    let mut heartbeat = tokio::time::interval(std::time::Duration::from_secs(60));
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // Skip first fire so we don't heartbeat with zero data.
    heartbeat.tick().await;
    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);

    let symbols_expected = symbols.len();
    let mut bars_processed_total: u64 = 0;
    let mut bars_processed_window: u64 = 0;
    let mut last_bar_rx_ts_ms: i64 = 0;

    loop {
        tokio::select! {
            Some(bar) = bar_rx.recv() => {
                // `stream.rs` shifts Alpaca bar timestamps by +60s (open→close time).
                // Undo that here so `minute` reflects bar-OPEN time, matching
                // `market_session::is_rth_utc` semantics. Without this, the last
                // RTH bar (e.g. open=19:59, stream=20:00 in DST) would be
                // excluded by `RTH_START_MIN..SESSION_CLOSE_MIN` and the close
                // would never enter the buffer — missing the daily close.
                let bar_open_ts_ms = bar.timestamp - 60_000;
                let dt = match DateTime::<Utc>::from_timestamp_millis(bar_open_ts_ms) {
                    Some(d) => d,
                    None => continue,
                };
                if !market_session::is_rth_utc(dt) {
                    // Outside RTH — ignore (do not contaminate daily close).
                    debug!(
                        symbol = bar.symbol.as_str(),
                        ts = bar.timestamp,
                        "bar outside RTH — discarded"
                    );
                    continue;
                }
                let date = market_session::trading_day_utc(dt);
                if bar.close.is_finite() && bar.close > 0.0 {
                    let entry = day_closes.entry(date).or_default();
                    entry.insert(bar.symbol.clone(), bar.close);
                    bars_processed_total += 1;
                    bars_processed_window += 1;
                    last_bar_rx_ts_ms = bar.timestamp;
                    debug!(
                        symbol = bar.symbol.as_str(),
                        close = bar.close,
                        date = %date,
                        buffer_size = entry.len(),
                        symbols_expected,
                        "buffered bar"
                    );
                }
            }
            _ = heartbeat.tick() => {
                // BAR_LOOP heartbeat — surfaces whether bars are making it
                // out of `stream.rs`'s channel into the basket buffer. If
                // this goes silent while `stream heartbeat` shows activity,
                // the channel is backed up or the RTH filter is rejecting
                // everything.
                let now = clock.now();
                let today = market_session::trading_day_utc(now);
                let in_rth = market_session::is_rth_utc(now);
                let past_close = market_session::is_after_close_grace_utc(now, CLOSE_GRACE_MIN);
                let buffered_today = day_closes.get(&today).map(|m| m.len()).unwrap_or(0);
                let last_bar_age_s = if last_bar_rx_ts_ms == 0 {
                    -1i64
                } else {
                    (now.timestamp_millis() - last_bar_rx_ts_ms) / 1000
                };
                info!(
                    bars_processed_total,
                    bars_processed_window,
                    buffered_today,
                    symbols_expected,
                    in_rth,
                    past_close,
                    processed_today = processed_sessions.contains(&today),
                    current_positions = current_shares.len(),
                    last_bar_age_s,
                    "BAR_LOOP heartbeat"
                );
                bars_processed_window = 0;
            }
            session_event = session_trigger.next() => {
                // Session-close trigger: the trigger has determined that
                // `today` is past session close + grace. Live uses
                // `IntervalSessionTrigger` (30s wall-clock poll); replay
                // uses `BarDrivenSessionTrigger` so cadence follows bar
                // timestamps. The trigger dedups internally, so `today`
                // is yielded at most once; `processed_sessions` is the
                // persisted-state dedup that catches restarts after a
                // session was already processed.
                //
                // `None` = trigger exhausted (replay drained its
                // parquet bars). Live's `IntervalSessionTrigger` never
                // returns `None`; this branch is the replay exit path.
                let today = match session_event {
                    Some(d) => d,
                    None => {
                        info!(
                            bars_processed_total,
                            sessions_processed = processed_sessions.len(),
                            "========== REPLAY EXHAUSTED — SHUTDOWN =========="
                        );
                        break;
                    }
                };
                if !processed_sessions.contains(&today) {
                    let closes_for_day = day_closes.remove(&today).unwrap_or_default();
                    if closes_for_day.is_empty() {
                        if !market_session::is_trading_day(today) {
                            info!(
                                date = %today,
                                "session close grace elapsed on non-trading day with zero buffered closes — marking processed"
                            );
                            processed_sessions.insert(today);
                            continue;
                        }
                        error!(
                            date = %today,
                            symbols_expected,
                            buffered_days = day_closes.len(),
                            current_positions = current_shares.len(),
                            "session close grace elapsed on trading day with zero buffered closes"
                        );
                        return Err(format!(
                            "session close grace elapsed on trading day {today} but no RTH closes were buffered"
                        ));
                    }
                    // Log exactly which symbols' closes we have and, crucially,
                    // which expected ones are missing. Yesterday we had no
                    // way to tell mid-incident whether this was a subscribe
                    // problem, a stream-drop problem, or a buffer problem.
                    let missing: Vec<&str> = symbols
                        .iter()
                        .filter(|s| !closes_for_day.contains_key(s.as_str()))
                        .map(|s| s.as_str())
                        .collect();
                    info!(
                        date = %today,
                        closes_in_buffer = closes_for_day.len(),
                        symbols_expected,
                        missing_count = missing.len(),
                        missing_sample = ?missing.iter().take(10).collect::<Vec<_>>(),
                        "session close firing"
                    );
                    if !missing.is_empty() {
                        error!(
                            date = %today,
                            closes_in_buffer = closes_for_day.len(),
                            symbols_expected,
                            missing_count = missing.len(),
                            missing_sample = ?missing.iter().take(20).collect::<Vec<_>>(),
                            "incomplete close snapshot at session close"
                        );
                        return Err(format!(
                            "incomplete close snapshot for {today}: missing {} symbols",
                            missing.len()
                        ));
                    }
                    processed_sessions.insert(today);
                    process_session_close(
                        &mut engine,
                        broker,
                        today,
                        &closes_for_day,
                        &portfolio_config,
                        &mut current_shares,
                        execution,
                    )
                    .await?;
                    // Hook for replay's daily-equity time series.
                    // Noop on AlpacaClient.
                    broker.record_eod(today).await;
                    last_processed_trading_day = Some(today);
                    engine.save_state_with_day(state_path, last_processed_trading_day)?;
                    info!(
                        date = %today,
                        state_path = %state_path.display(),
                        last_processed = ?last_processed_trading_day,
                        "persisted basket engine state after session close"
                    );
                }
            }
            _ = &mut ctrl_c => {
                info!(
                    bars_processed_total,
                    sessions_processed = processed_sessions.len(),
                    "========== SHUTDOWN =========="
                );
                break;
            }
        }
    }
    Ok(())
}

/// Fetch open positions from Alpaca and express them as signed shares per symbol.
/// Used on startup so `diff_to_orders` computes correct deltas from the engine's target.
///
/// Returns `Err` on any fetch failure; the caller must treat this as fatal for
/// paper/live execution (trading from an empty share map would double-size
/// every already-open leg on the first session).
async fn seed_current_shares_from_alpaca(
    broker: &impl Broker,
    mode: ExecutionMode,
    allowed_symbols: &[String],
) -> Result<HashMap<String, f64>, String> {
    let positions = broker.get_positions(mode).await.map_err(|e| {
        format!(
            "startup position reconciliation failed — refusing to trade without a \
             trusted share inventory (fetch error: {e})"
        )
    })?;
    let allowed: std::collections::HashSet<&str> =
        allowed_symbols.iter().map(|s| s.as_str()).collect();
    let mut ignored_symbols = Vec::new();
    let shares: HashMap<String, f64> = positions
        .into_iter()
        .filter_map(|(sym, (qty, _avg_entry))| {
            if allowed.contains(sym.as_str()) {
                Some((sym, qty))
            } else {
                ignored_symbols.push(sym);
                None
            }
        })
        .collect();
    if !ignored_symbols.is_empty() {
        ignored_symbols.sort();
        warn!(
            ignored_positions = ignored_symbols.len(),
            ignored_sample = ?ignored_symbols.iter().take(10).collect::<Vec<_>>(),
            "ignoring non-basket broker positions during startup reconciliation"
        );
    }
    info!(
        n_positions = shares.len(),
        "seeded current_shares from Alpaca open positions"
    );
    Ok(shares)
}

fn load_close_snapshot_for_day(
    bars_dir: &Path,
    symbols: &[String],
    day: NaiveDate,
) -> Result<HashMap<String, f64>, String> {
    let closes = load_daily_closes_with_timestamps(bars_dir, symbols, 10, Some(day))?;
    let mut snapshot = HashMap::new();
    let mut missing = Vec::new();
    let expected_last_bar_ts_us =
        (market_session::close_timestamp_utc_for_day(day) - 60_000) * 1_000;
    for symbol in symbols {
        match closes.get(symbol).and_then(|series| {
            series.iter().find_map(|(d, ts_us, c)| {
                if *d == day && *ts_us == expected_last_bar_ts_us {
                    Some(*c)
                } else {
                    None
                }
            })
        }) {
            Some(close) if close.is_finite() && close > 0.0 => {
                snapshot.insert(symbol.clone(), close);
            }
            _ => missing.push(symbol.clone()),
        }
    }
    if missing.is_empty() {
        info!(
            date = %day,
            symbols = snapshot.len(),
            expected_last_bar_ts_us,
            "loaded finalized close snapshot for trading day"
        );
        Ok(snapshot)
    } else {
        missing.sort();
        Err(format!(
            "close snapshot incomplete for {}: missing {} symbols (sample: {})",
            day,
            missing.len(),
            missing.into_iter().take(10).collect::<Vec<_>>().join(", ")
        ))
    }
}

/// Run the engine for one session close and dispatch orders.
#[allow(clippy::too_many_arguments)]
async fn process_session_close(
    engine: &mut BasketEngine,
    broker: &impl Broker,
    date: NaiveDate,
    closes: &HashMap<String, f64>,
    portfolio_config: &PortfolioConfig,
    current_shares: &mut HashMap<String, f64>,
    execution: BasketExecution,
) -> Result<(), String> {
    debug_assert!(
        portfolio_config.validate().is_ok(),
        "process_session_close received invalid PortfolioConfig"
    );
    if closes.is_empty() {
        warn!(date = %date, "no RTH closes buffered for session — skipping engine");
        return Ok(());
    }

    // Build DailyBar slice for BasketEngine.
    let daily_bars: Vec<DailyBar> = closes
        .iter()
        .map(|(symbol, &close)| DailyBar {
            symbol: symbol.clone(),
            date,
            close,
        })
        .collect();

    let intents = engine.on_bars(&daily_bars);
    info!(
        date = %date,
        symbols = closes.len(),
        intents = intents.len(),
        "session close processed"
    );

    for intent in &intents {
        log_intent(intent);
    }

    let allowed_symbols: Vec<String> = closes.keys().cloned().collect();
    if let Some(mode) = execution.alpaca_mode() {
        *current_shares = seed_current_shares_from_alpaca(broker, mode, &allowed_symbols).await?;
        info!(
            date = %date,
            current_positions = current_shares.len(),
            "refreshed broker share inventory before computing basket order deltas"
        );
    }

    // Query current account equity from the broker so portfolio sizing
    // tracks live capital instead of the static `config.capital`. This
    // is the load-bearing fix for the Q4 2024 feedback-loop bug:
    // sizing off initial capital meant a drawdown made `target_gross >
    // buying_power`, broker rejected orders piecemeal, hedge book ended
    // up lopsided, next day's losses widened the gap further.
    //
    // Noop mode (no broker connection in shadow runs) falls back to
    // `config.capital` so the planning math stays valid.
    let sizing_equity = match execution.alpaca_mode() {
        Some(mode) => match broker.get_account(mode).await {
            Ok(account) => match parse_equity(&account) {
                Ok(equity) => {
                    info!(
                        date = %date,
                        equity = %format_args!("{:.2}", equity),
                        "sized portfolio off live broker equity"
                    );
                    equity
                }
                Err(e) => {
                    warn!(
                        date = %date,
                        error = %e,
                        fallback = %format_args!("{:.2}", portfolio_config.capital),
                        "failed to parse broker equity; falling back to config.capital"
                    );
                    portfolio_config.capital
                }
            },
            Err(e) => {
                warn!(
                    date = %date,
                    error = %e,
                    fallback = %format_args!("{:.2}", portfolio_config.capital),
                    "failed to fetch broker account; falling back to config.capital"
                );
                portfolio_config.capital
            }
        },
        None => portfolio_config.capital,
    };

    // Portfolio layer: apply active-basket admission first, then convert
    // admitted target notionals to target shares. Dynamic sizing off
    // current equity (via plan_portfolio_for_equity) leaves a 10%
    // buying-power buffer so per-symbol rounding + plan/submit drift
    // can't push the actual gross over the broker cap.
    let plan = plan_portfolio_for_equity(engine, portfolio_config, sizing_equity);
    let target_notionals = plan.symbol_notionals;
    if !plan.excluded_baskets.is_empty() {
        engine.flatten_baskets(&plan.excluded_baskets);
    }
    let target_shares = target_shares_from_notionals(&target_notionals, closes)?;

    // Summary of the notional plan before we diff — this is where yesterday's
    // $340K-on-$100K problem was invisible. Emit gross long, gross short,
    // net, absolute max leg, and median leg so we can spot sizing anomalies
    // without shelling into sqlite. `gross_long + gross_short` = gross
    // notional = leverage × equity (should be ≤ equity × leverage_assumed
    // from the universe TOML).
    let (gross_long, gross_short, max_abs, sorted_abs) = summarize_notionals(&target_notionals);
    let median_abs = if sorted_abs.is_empty() {
        0.0
    } else {
        sorted_abs[sorted_abs.len() / 2]
    };
    let gross_notional = gross_long + gross_short.abs();
    let gross_cap = portfolio_config.capital * portfolio_config.leverage;
    info!(
        date = %date,
        targets = target_notionals.len(),
        target_positions = target_shares.len(),
        current_positions = current_shares.len(),
        gross_long = %format!("{:.0}", gross_long),
        gross_short = %format!("{:.0}", gross_short),
        gross_notional = %format!("{:.0}", gross_notional),
        gross_cap = %format!("{:.0}", gross_cap),
        net_notional = %format!("{:.0}", gross_long + gross_short),
        max_abs_leg = %format!("{:.0}", max_abs),
        median_abs_leg = %format!("{:.0}", median_abs),
        "target notionals summary"
    );
    if gross_notional > gross_cap + 1.0 {
        return Err(format!(
            "target gross notional {:.2} exceeds configured cap {:.2}",
            gross_notional, gross_cap
        ));
    }
    if !plan.excluded_baskets.is_empty() {
        warn!(
            date = %date,
            active_baskets = plan.active_baskets,
            cap = portfolio_config.n_active_baskets,
            admitted = plan.selected_baskets.len(),
            excluded = plan.excluded_baskets.len(),
            excluded_sample = ?plan.excluded_baskets.iter().take(10).collect::<Vec<_>>(),
            "active-basket cap excluded baskets from the target portfolio"
        );
    } else {
        info!(
            date = %date,
            active_baskets = plan.active_baskets,
            cap = portfolio_config.n_active_baskets,
            admitted = plan.selected_baskets.len(),
            "portfolio admission completed without exclusions"
        );
    }

    let orders = diff_to_orders(current_shares, &target_shares);
    if orders.is_empty() {
        info!(date = %date, "no orders to emit — targets already match current");
        return Ok(());
    }

    // Distribution of order notionals — flags the "one leg $30K, rest $200"
    // case that we saw yesterday. Computed cheaply from prices + qtys.
    let order_notionals: Vec<f64> = orders
        .iter()
        .filter_map(|o| {
            closes
                .get(&o.symbol)
                .map(|p| p * o.qty as f64)
                .filter(|n| n.is_finite() && *n > 0.0)
        })
        .collect();
    let order_gross: f64 = order_notionals.iter().sum();
    let order_max = order_notionals.iter().cloned().fold(0.0_f64, f64::max);
    let (buy_orders, sell_orders, buy_notional, sell_notional) =
        summarize_orders_by_side(&orders, closes);
    info!(
        date = %date,
        n_orders = orders.len(),
        buy_orders,
        sell_orders,
        buy_notional = %format!("{:.0}", buy_notional),
        sell_notional = %format!("{:.0}", sell_notional),
        order_gross_notional = %format!("{:.0}", order_gross),
        order_max_notional = %format!("{:.0}", order_max),
        "emitting orders"
    );

    match execution.alpaca_mode() {
        None => {
            // Noop — log only, then advance the simulated share state directly
            // to the target so shadow mode stays deterministic across sessions.
            for order in &orders {
                log_order(order, "NOOP");
            }
            *current_shares = target_shares;
        }
        Some(mode) => {
            check_order_set_affordability(
                broker,
                mode,
                date,
                current_shares,
                &target_shares,
                &orders,
                closes,
            )
            .await?;
            let mut ordered_refs: Vec<&OrderIntent> = orders.iter().collect();
            ordered_refs.sort_by_key(|o| match o.side {
                Side::Sell => 0_u8,
                Side::Buy => 1_u8,
            });
            let mut accepted_orders = 0usize;
            let mut failed_orders = 0usize;
            for order in ordered_refs {
                log_order(order, execution.label());
                let side_str = match order.side {
                    Side::Buy => "buy",
                    Side::Sell => "sell",
                };
                let (reason, basket_id) = order_reason_fields(&order.reason);
                match broker
                    .place_order(&order.symbol, order.qty as f64, side_str, mode)
                    .await
                {
                    Ok(o) => {
                        info!(
                            symbol = order.symbol.as_str(),
                            qty = order.qty,
                            side = side_str,
                            reason,
                            basket_id,
                            order_id = o.id.as_str(),
                            status = o.status.as_str(),
                            "ORDER PLACED"
                        );
                        accepted_orders += 1;
                    }
                    Err(e) => {
                        failed_orders += 1;
                        error!(
                            symbol = order.symbol.as_str(),
                            qty = order.qty,
                            side = side_str,
                            reason,
                            basket_id,
                            error = e.as_str(),
                            "ORDER FAILED"
                        );
                    }
                }
            }
            info!(
                date = %date,
                accepted_orders,
                failed_orders,
                "submitted basket orders without mutating in-memory share inventory; next session refresh will reconcile actual fills"
            );

            // Post-submission broker reconciliation: after letting fills settle,
            // refetch positions and compare actual gross to target. Catches silent
            // portfolio drift from partial fills / rejections (the failure mode
            // that turned yesterday's $100K config into a $341K lopsided book).
            if accepted_orders + failed_orders > 0 {
                tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                match seed_current_shares_from_alpaca(broker, mode, &allowed_symbols).await {
                    Ok(actual_shares) => {
                        let actual_gross: f64 = actual_shares
                            .iter()
                            .filter_map(|(sym, qty)| closes.get(sym).map(|p| (qty * p).abs()))
                            .sum();
                        let target_gross = gross_notional;
                        let divergence_pct = if target_gross > 0.0 {
                            ((actual_gross - target_gross).abs() / target_gross) * 100.0
                        } else {
                            0.0
                        };
                        if divergence_pct > 10.0 {
                            error!(
                                date = %date,
                                target_gross = %format!("{:.0}", target_gross),
                                actual_gross = %format!("{:.0}", actual_gross),
                                divergence_pct = %format!("{:.1}", divergence_pct),
                                accepted_orders,
                                failed_orders,
                                broker_positions = actual_shares.len(),
                                "BROKER DIVERGENCE: actual gross differs from target by >10%"
                            );
                        } else {
                            info!(
                                date = %date,
                                target_gross = %format!("{:.0}", target_gross),
                                actual_gross = %format!("{:.0}", actual_gross),
                                divergence_pct = %format!("{:.1}", divergence_pct),
                                broker_positions = actual_shares.len(),
                                "post-submission reconciliation OK"
                            );
                        }
                    }
                    Err(e) => {
                        error!(
                            date = %date,
                            error = e.as_str(),
                            "post-submission reconciliation failed — could not refetch broker positions"
                        );
                    }
                }
            }
        }
    }
    Ok(())
}

fn target_shares_from_notionals(
    target_notionals: &HashMap<String, f64>,
    closes: &HashMap<String, f64>,
) -> Result<HashMap<String, f64>, String> {
    let mut target_shares = HashMap::new();
    let mut missing_prices = Vec::new();
    for (symbol, notional) in target_notionals {
        let price = match closes.get(symbol) {
            Some(price) if price.is_finite() && *price > 0.0 => *price,
            _ => {
                missing_prices.push(symbol.clone());
                continue;
            }
        };
        let shares = (notional / price).round();
        if shares.abs() >= 1.0 {
            target_shares.insert(symbol.clone(), shares);
        }
    }
    if missing_prices.is_empty() {
        Ok(target_shares)
    } else {
        missing_prices.sort();
        Err(format!(
            "missing close prices for target-share conversion: {}",
            missing_prices.join(", ")
        ))
    }
}

/// Summarize a `target_notionals` map into (gross_long, gross_short, max_abs,
/// sorted_abs). `gross_short` is returned as a negative number.
fn summarize_notionals(targets: &HashMap<String, f64>) -> (f64, f64, f64, Vec<f64>) {
    let mut gross_long = 0.0_f64;
    let mut gross_short = 0.0_f64;
    let mut max_abs = 0.0_f64;
    let mut abs: Vec<f64> = Vec::with_capacity(targets.len());
    for &n in targets.values() {
        if !n.is_finite() {
            continue;
        }
        if n > 0.0 {
            gross_long += n;
        } else {
            gross_short += n;
        }
        let a = n.abs();
        abs.push(a);
        if a > max_abs {
            max_abs = a;
        }
    }
    abs.sort_by(|x, y| x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal));
    (gross_long, gross_short, max_abs, abs)
}

fn log_intent(intent: &PositionIntent) {
    info!(
        basket_id = %intent.basket_id,
        target_position = intent.target_position,
        z = %format!("{:.4}", intent.z_score),
        spread = %format!("{:.6}", intent.spread),
        reason = intent.reason.as_str(),
        date = %intent.date,
        "BASKET_INTENT"
    );
}

fn log_order(order: &OrderIntent, label: &str) {
    let side_str = match order.side {
        Side::Buy => "buy",
        Side::Sell => "sell",
    };
    let (reason, basket_id) = order_reason_fields(&order.reason);
    info!(
        mode = label,
        symbol = order.symbol.as_str(),
        qty = order.qty,
        side = side_str,
        reason,
        basket_id,
        "BASKET_ORDER"
    );
}

pub(crate) fn collect_symbols(universe: &basket_picker::Universe) -> Vec<String> {
    let mut symbols: Vec<String> = universe
        .sectors
        .values()
        .flat_map(|s| s.members.iter().cloned())
        .collect();
    symbols.sort();
    symbols.dedup();
    symbols
}

/// Read the last `window_days` trading days of daily closes for each symbol.
/// Aggregates 1-min parquets to RTH-last-bar closes (same rule as replay).
pub(crate) fn load_warmup_closes(
    bars_dir: &Path,
    symbols: &[String],
    window_days: i64,
) -> Result<HashMap<String, Vec<(NaiveDate, f64)>>, String> {
    let today = Utc::now().date_naive();
    load_daily_closes(bars_dir, symbols, window_days, today.pred_opt())
}

/// Same as [`load_warmup_closes`] but with an explicit "as-of" cutoff.
///
/// Used by the replay path to build a fit using data **strictly before**
/// the replay window, so the fit can't peek at the data it's about to
/// trade against.
pub(crate) fn load_warmup_closes_as_of(
    bars_dir: &Path,
    symbols: &[String],
    window_days: i64,
    as_of: NaiveDate,
) -> Result<HashMap<String, Vec<(NaiveDate, f64)>>, String> {
    load_daily_closes(bars_dir, symbols, window_days, as_of.pred_opt())
}

fn load_daily_closes(
    bars_dir: &Path,
    symbols: &[String],
    window_days: i64,
    max_day_inclusive: Option<NaiveDate>,
) -> Result<HashMap<String, Vec<(NaiveDate, f64)>>, String> {
    let closes =
        load_daily_closes_with_timestamps(bars_dir, symbols, window_days, max_day_inclusive)?;
    Ok(closes
        .into_iter()
        .map(|(symbol, series)| {
            (
                symbol,
                series
                    .into_iter()
                    .map(|(date, _ts_us, close)| (date, close))
                    .collect(),
            )
        })
        .collect())
}

#[allow(clippy::type_complexity)]
fn load_daily_closes_with_timestamps(
    bars_dir: &Path,
    symbols: &[String],
    window_days: i64,
    max_day_inclusive: Option<NaiveDate>,
) -> Result<HashMap<String, Vec<(NaiveDate, i64, f64)>>, String> {
    use arrow::array::{Array, Float64Array, TimestampMicrosecondArray};
    use std::collections::BTreeMap;
    // The window anchor is the most recent date the caller wants to
    // include — `max_day_inclusive` if provided (replay's "as-of"
    // cutoff), or "today" otherwise (live warm-up). The lower bound
    // is `anchor - window_days`. Anchoring on `Utc::now()` here would
    // make `as_of`-based callers fail silently when their requested
    // window doesn't overlap "now − window_days" (#306 finding).
    let anchor = max_day_inclusive.unwrap_or_else(|| Utc::now().date_naive());
    let cutoff = anchor - chrono::Duration::days(window_days);

    let mut out = HashMap::new();
    for symbol in symbols {
        let path = bars_dir.join(format!("{symbol}.parquet"));
        let file = match std::fs::File::open(&path) {
            Ok(f) => f,
            Err(e) => {
                warn!(symbol = %symbol, error = %e, "skip symbol — parquet missing");
                continue;
            }
        };
        let builder = ParquetRecordBatchReaderBuilder::try_new(file)
            .map_err(|e| format!("reader {symbol}: {e}"))?;
        let reader = builder
            .build()
            .map_err(|e| format!("build {symbol}: {e}"))?;

        let mut daily: BTreeMap<NaiveDate, (i64, f64)> = BTreeMap::new();
        for batch in reader {
            let batch = batch.map_err(|e| format!("batch {symbol}: {e}"))?;
            let ts = batch
                .column(0)
                .as_any()
                .downcast_ref::<TimestampMicrosecondArray>()
                .ok_or_else(|| format!("ts cast {symbol}"))?;
            let close = batch
                .column(4)
                .as_any()
                .downcast_ref::<Float64Array>()
                .ok_or_else(|| format!("close cast {symbol}"))?;

            for i in 0..batch.num_rows() {
                let ts_us = ts.value(i);
                let secs = ts_us / 1_000_000;
                let dt = match DateTime::<Utc>::from_timestamp(secs, 0) {
                    Some(d) => d.naive_utc(),
                    None => continue,
                };
                let dt_utc = dt.and_utc();
                if !market_session::is_rth_utc(dt_utc) {
                    continue;
                }
                let px = close.value(i);
                if !px.is_finite() || px <= 0.0 {
                    continue;
                }
                let date = market_session::trading_day_utc(dt_utc);
                if date < cutoff {
                    continue;
                }
                if let Some(max_day) = max_day_inclusive {
                    if date > max_day {
                        continue;
                    }
                }
                daily
                    .entry(date)
                    .and_modify(|(prev_ts, prev_close)| {
                        if ts_us > *prev_ts {
                            *prev_ts = ts_us;
                            *prev_close = px;
                        }
                    })
                    .or_insert((ts_us, px));
            }
        }
        let series: Vec<(NaiveDate, i64, f64)> = daily
            .into_iter()
            .map(|(d, (ts_us, c))| (d, ts_us, c))
            .collect();
        if !series.is_empty() {
            out.insert(symbol.clone(), series);
        }
    }
    Ok(out)
}

/// Align the date index for ONE basket (`target` + its peer `members`).
///
/// Produces the `HashMap<symbol, Vec<f64>>` shape that `basket_picker::validate`
/// requires, intersecting dates across ONLY this basket's symbols. Missing
/// symbols are passed through unaligned (length 0 after intersection with
/// nothing), so the validator emits a precise "missing symbol" rejection.
pub(crate) fn align_basket_history(
    closes: &HashMap<String, Vec<(NaiveDate, f64)>>,
    symbols: &[&str],
) -> HashMap<String, Vec<f64>> {
    let mut series_by_symbol: Vec<(&str, &Vec<(NaiveDate, f64)>)> =
        Vec::with_capacity(symbols.len());
    for s in symbols {
        if let Some(v) = closes.get(*s) {
            series_by_symbol.push((*s, v));
        }
    }
    if series_by_symbol.is_empty() {
        return HashMap::new();
    }

    // Intersection of dates across ONLY this basket's symbols.
    let mut common: std::collections::BTreeSet<NaiveDate> =
        series_by_symbol[0].1.iter().map(|(d, _)| *d).collect();
    for (_, series) in &series_by_symbol[1..] {
        let s: std::collections::BTreeSet<NaiveDate> = series.iter().map(|(d, _)| *d).collect();
        common = common.intersection(&s).cloned().collect();
    }

    let mut out = HashMap::new();
    for (symbol, series) in &series_by_symbol {
        let map: HashMap<NaiveDate, f64> = series.iter().copied().collect();
        let aligned: Vec<f64> = common.iter().filter_map(|d| map.get(d).copied()).collect();
        out.insert((*symbol).to_string(), aligned);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alpaca::AlpacaAccount;
    use chrono::{TimeZone, Timelike};

    #[test]
    fn test_basket_execution_alpaca_mode_mapping() {
        assert!(BasketExecution::Noop.alpaca_mode().is_none());
        assert_eq!(
            BasketExecution::Paper.alpaca_mode(),
            Some(ExecutionMode::Paper)
        );
        assert_eq!(
            BasketExecution::Live.alpaca_mode(),
            Some(ExecutionMode::Live)
        );
    }

    /// Verifies the bar-timestamp unshift needed in the live bar loop.
    /// stream.rs adds +60s (open→close); we subtract it to get bar-open
    /// time for RTH filtering, so the 19:59-open / 20:00-close bar is
    /// correctly classified as RTH rather than being filtered out.
    #[test]
    fn test_bar_timestamp_unshift_keeps_last_rth_bar() {
        // Alpaca bar open-time 19:59 UTC = 71940 minutes from epoch day start.
        // stream.rs adds 60_000 ms → stream timestamp = 20:00 UTC.
        let base = DateTime::<Utc>::from_timestamp(0, 0).unwrap();
        let _ = base; // sanity: construction works
                      // Build a millis value for some 2026-02-06 19:59 UTC, shift +60s.
        let open = chrono::NaiveDate::from_ymd_opt(2026, 2, 6)
            .unwrap()
            .and_hms_opt(19, 59, 0)
            .unwrap()
            .and_utc();
        let stream_ts_ms = open.timestamp_millis() + 60_000;
        // Replicate the unshift used in the bar loop.
        let bar_open_ts_ms = stream_ts_ms - 60_000;
        let dt = DateTime::<Utc>::from_timestamp_millis(bar_open_ts_ms).unwrap();
        let minute = dt.hour() * 60 + dt.minute();
        assert_eq!(minute, 19 * 60 + 59, "unshift must recover bar-open minute");
        assert!(
            market_session::is_rth_utc(dt),
            "last RTH bar (19:59 open) must pass RTH filter after unshift"
        );
    }

    #[test]
    fn test_align_basket_history_intersects_only_basket_symbols() {
        let mut closes = HashMap::new();
        closes.insert(
            "A".to_string(),
            vec![
                (NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(), 10.0),
                (NaiveDate::from_ymd_opt(2026, 1, 2).unwrap(), 11.0),
                (NaiveDate::from_ymd_opt(2026, 1, 3).unwrap(), 12.0),
            ],
        );
        closes.insert(
            "B".to_string(),
            vec![
                // Missing 2026-01-01
                (NaiveDate::from_ymd_opt(2026, 1, 2).unwrap(), 20.0),
                (NaiveDate::from_ymd_opt(2026, 1, 3).unwrap(), 21.0),
            ],
        );
        let aligned = align_basket_history(&closes, &["A", "B"]);
        // Intersection is [2026-01-02, 2026-01-03] — each series has 2 entries.
        assert_eq!(aligned.get("A").unwrap().len(), 2);
        assert_eq!(aligned.get("B").unwrap().len(), 2);
        assert_eq!(aligned.get("A").unwrap()[0], 11.0);
        assert_eq!(aligned.get("B").unwrap()[0], 20.0);
    }

    #[test]
    fn test_align_basket_history_ignores_unrelated_sparse_symbols() {
        // Basket X/Y both have full 3-day history; unrelated sparse C has
        // only 1 day. A universe-wide intersection would shrink X/Y to that
        // 1 day. Per-basket alignment must keep X/Y at 3.
        let mut closes = HashMap::new();
        closes.insert(
            "X".to_string(),
            vec![
                (NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(), 10.0),
                (NaiveDate::from_ymd_opt(2026, 1, 2).unwrap(), 11.0),
                (NaiveDate::from_ymd_opt(2026, 1, 3).unwrap(), 12.0),
            ],
        );
        closes.insert(
            "Y".to_string(),
            vec![
                (NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(), 20.0),
                (NaiveDate::from_ymd_opt(2026, 1, 2).unwrap(), 21.0),
                (NaiveDate::from_ymd_opt(2026, 1, 3).unwrap(), 22.0),
            ],
        );
        closes.insert(
            "C_SPARSE".to_string(),
            vec![(NaiveDate::from_ymd_opt(2026, 1, 3).unwrap(), 99.0)],
        );
        let aligned = align_basket_history(&closes, &["X", "Y"]);
        assert_eq!(aligned.get("X").unwrap().len(), 3);
        assert_eq!(aligned.get("Y").unwrap().len(), 3);
        assert!(
            !aligned.contains_key("C_SPARSE"),
            "symbols outside the basket must not appear in the aligned map"
        );
    }

    #[test]
    fn test_target_shares_from_notionals_rounds_to_whole_shares() {
        let mut notionals = HashMap::new();
        notionals.insert("AMD".to_string(), 5050.0);
        notionals.insert("NVDA".to_string(), -2400.0);

        let mut closes = HashMap::new();
        closes.insert("AMD".to_string(), 101.0);
        closes.insert("NVDA".to_string(), 200.0);

        let shares = target_shares_from_notionals(&notionals, &closes).unwrap();
        assert_eq!(shares.get("AMD").copied(), Some(50.0));
        assert_eq!(shares.get("NVDA").copied(), Some(-12.0));
    }

    #[test]
    fn test_target_shares_from_notionals_fails_closed_on_missing_price() {
        let mut notionals = HashMap::new();
        notionals.insert("AMD".to_string(), 5000.0);
        notionals.insert("NVDA".to_string(), 2500.0);

        let mut closes = HashMap::new();
        closes.insert("AMD".to_string(), 100.0);

        let err = target_shares_from_notionals(&notionals, &closes).unwrap_err();
        assert!(err.contains("NVDA"));
    }

    #[test]
    fn test_classify_startup_phase_distinguishes_post_close_catchup() {
        let dt = Utc.with_ymd_and_hms(2026, 4, 22, 20, 5, 0).unwrap();
        let today = market_session::trading_day_utc(dt);

        assert_eq!(
            classify_startup_phase(dt, None, 2),
            StartupPhase::PostClosePendingCatchup
        );
        assert_eq!(
            classify_startup_phase(dt, Some(today), 2),
            StartupPhase::PostCloseProcessed
        );
    }

    #[test]
    fn test_summarize_orders_by_side_reports_counts_and_notionals() {
        let orders = vec![
            OrderIntent {
                symbol: "AMD".to_string(),
                qty: 10,
                side: Side::Buy,
                reason: basket_engine::OrderReason::Entry {
                    basket_id: "test".to_string(),
                },
            },
            OrderIntent {
                symbol: "NVDA".to_string(),
                qty: 5,
                side: Side::Sell,
                reason: basket_engine::OrderReason::Flip {
                    basket_id: "test".to_string(),
                },
            },
            OrderIntent {
                symbol: "AAPL".to_string(),
                qty: 4,
                side: Side::Buy,
                reason: basket_engine::OrderReason::Aggregated,
            },
        ];
        let closes = HashMap::from([
            ("AMD".to_string(), 100.0),
            ("NVDA".to_string(), 200.0),
            ("AAPL".to_string(), 50.0),
        ]);

        let (buy_count, sell_count, buy_notional, sell_notional) =
            summarize_orders_by_side(&orders, &closes);

        assert_eq!(buy_count, 2);
        assert_eq!(sell_count, 1);
        assert_eq!(buy_notional, 1_200.0);
        assert_eq!(sell_notional, 1_000.0);
    }

    #[test]
    fn test_parse_buying_power_rejects_nonpositive_values() {
        let account = AlpacaAccount {
            status: "ACTIVE".to_string(),
            buying_power: "0".to_string(),
            equity: "100000".to_string(),
            trading_blocked: false,
            account_blocked: false,
        };
        let err = parse_buying_power(&account).unwrap_err();
        assert!(err.contains("not positive"));
    }

    #[test]
    fn test_incremental_gross_logic_allows_self_financing_rotation_shape() {
        let mut current: HashMap<String, f64> = HashMap::new();
        current.insert("AMD".to_string(), 100.0);
        let mut target: HashMap<String, f64> = HashMap::new();
        target.insert("NVDA".to_string(), 50.0);
        let mut closes: HashMap<String, f64> = HashMap::new();
        closes.insert("AMD".to_string(), 100.0);
        closes.insert("NVDA".to_string(), 200.0);

        let (current_long, current_short) = gross_by_side(&current, &closes);
        let (target_long, target_short) = gross_by_side(&target, &closes);
        let incremental_exposure =
            (target_long - current_long).max(0.0) + (target_short - current_short).max(0.0);

        assert_eq!(current_long, 10_000.0);
        assert_eq!(current_short, 0.0);
        assert_eq!(target_long, 10_000.0);
        assert_eq!(target_short, 0.0);
        assert_eq!(incremental_exposure, 0.0);
    }

    #[test]
    fn test_incremental_exposure_counts_long_to_short_reversal() {
        let mut current: HashMap<String, f64> = HashMap::new();
        current.insert("AMD".to_string(), 100.0);
        let mut target: HashMap<String, f64> = HashMap::new();
        target.insert("AMD".to_string(), -100.0);
        let mut closes: HashMap<String, f64> = HashMap::new();
        closes.insert("AMD".to_string(), 100.0);

        let (current_long, current_short) = gross_by_side(&current, &closes);
        let (target_long, target_short) = gross_by_side(&target, &closes);
        let incremental_exposure =
            (target_long - current_long).max(0.0) + (target_short - current_short).max(0.0);

        assert_eq!(current_long, 10_000.0);
        assert_eq!(current_short, 0.0);
        assert_eq!(target_long, 0.0);
        assert_eq!(target_short, 10_000.0);
        assert_eq!(incremental_exposure, 10_000.0);
    }
}
