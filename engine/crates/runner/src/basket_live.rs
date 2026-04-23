//! Live/paper runner for the basket spread engine.
//!
//! Drives `basket_engine::BasketEngine` (continuous streaming state machine,
//! NOT the walk-forward replay in `basket_runner.rs`) with real-time 1-min
//! bars from the Alpaca WebSocket.
//!
//! Flow per trading day:
//!   1. Startup: load the frozen basket fit artifact and build `BasketEngine`
//!      from those persisted `BasketFit`s. Engine enters with empty state.
//!   2. Bar loop: for each 1-min bar, update per-symbol "last RTH bar".
//!   3. Session close (19:59 UTC): snapshot the day's closes, call
//!      `BasketEngine::on_bars()`, get `PositionIntent`s.
//!   4. Portfolio: aggregate intents → target notionals → `OrderIntent`s via
//!      `diff_to_orders()`.
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
    aggregate_positions, diff_to_orders, target_shares_from_notionals, BasketEngine, DailyBar,
    OrderIntent, PortfolioConfig, PositionIntent, Side,
};
use basket_picker::{load_universe, BasketFit};
use chrono::{DateTime, Datelike, NaiveDate, Utc, Weekday};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use tracing::{debug, error, info, warn};

use crate::alpaca::{AlpacaClient, ExecutionMode};
use crate::market_session;
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

/// Run the basket live/paper loop.
///
/// Returns on Ctrl+C or fatal error.
pub async fn run_basket_live(
    alpaca: &AlpacaClient,
    universe_path: &Path,
    fit_artifact_path: &Path,
    execution: BasketExecution,
    portfolio_config: PortfolioConfig,
    fits: &[BasketFit],
    state_path: &Path,
) -> Result<(), String> {
    info!(
        universe = %universe_path.display(),
        fit_artifact = %fit_artifact_path.display(),
        state_path = %state_path.display(),
        execution = execution.label(),
        "========== BASKET LIVE RUNNER =========="
    );

    if execution == BasketExecution::Live {
        warn!("LIVE MODE — real-money orders will be placed on every EOD signal");
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
        return Err("no valid baskets in fit artifact".to_string());
    }

    let expected_ids: std::collections::HashSet<String> =
        fits.iter().filter(|f| f.valid).map(|f| f.candidate.id()).collect();
    let state_exists = state_path.exists();
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
        let mut fresh = BasketEngine::new(fits);
        fresh.apply_states(snapshot.states)?;
        info!(
            baskets = fresh.num_baskets(),
            state_path = %state_path.display(),
            "loaded basket runtime state onto frozen fit artifact params"
        );
        fresh
    } else {
        let fresh = BasketEngine::new(fits);
        info!(baskets = fresh.num_baskets(), "basket engine initialized from frozen fits");
        fresh
    };

    // 2. Seed current_shares from Alpaca positions (startup reconciliation).
    //    Without this, a restart with live open positions would trigger
    //    target-minus-zero deltas, flooding Alpaca with duplicate orders.
    //    Noop skips this (no Alpaca account needed for shadow mode).
    //    Paper/Live FAIL CLOSED: if reconciliation cannot load open positions,
    //    we refuse to start. Trading from an empty notional map would diff
    //    targets against zero and flood Alpaca with duplicate orders against
    //    already-open broker positions, potentially double-sizing every leg.
    let mut current_shares = match execution.alpaca_mode() {
        None => {
            info!("noop mode — skipping startup position reconciliation");
            HashMap::new()
        }
        Some(mode) => seed_current_shares_from_alpaca(alpaca, mode).await?,
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

    // 3. Subscribe to all universe symbols over WebSocket.
    let mut bar_rx = stream::start_bar_stream(&alpaca.api_key, &alpaca.api_secret, &symbols).await;
    info!("subscribed to Alpaca 1-min bar stream");

    // 4. Bar loop: buffer per (symbol, date) → last RTH bar.
    //    Engine is triggered by a wall-clock timer (not by `symbols[0]`'s 19:59 bar
    //    arrival) so that no single symbol becoming a data source-of-failure can
    //    silently skip an entire session.
    let mut day_closes: HashMap<NaiveDate, HashMap<String, f64>> = HashMap::new();
    let mut processed_sessions: std::collections::HashSet<NaiveDate> = Default::default();

    // Grace period after session close before firing the engine. Lets late-arriving
    // 19:59 bars land in the buffer.
    const CLOSE_GRACE_MIN: u32 = 2;

    let mut tick = tokio::time::interval(std::time::Duration::from_secs(30));
    // Dedicated 60s heartbeat that summarizes buffer state. Separate from the
    // 30s wall-clock tick so we get clean one-per-minute INFO lines
    // regardless of how often the close-firing check runs.
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
                // Undo that here so `minute` reflects bar-OPEN time, matching the
                // RTH filter used by replay (`basket_runner.rs::read_daily_closes`).
                // Without this, the last RTH bar (open=19:59, stream=20:00) would be
                // excluded by `RTH_START_MIN..SESSION_CLOSE_MIN` and the 19:59 close
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
                let now = Utc::now();
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
                    current_shares = current_shares.len(),
                    last_bar_age_s,
                    "BAR_LOOP heartbeat"
                );
                bars_processed_window = 0;
            }
            _ = tick.tick() => {
                // Wall-clock trigger: if we are past session close + grace for a
                // given date and haven't processed it yet, fire now — regardless
                // of which symbols' 19:59 bars landed.
                let now = Utc::now();
                let today = market_session::trading_day_utc(now);
                let past_close = market_session::is_after_close_grace_utc(now, CLOSE_GRACE_MIN);
                if past_close && !processed_sessions.contains(&today) {
                    let closes_for_day = day_closes.remove(&today).unwrap_or_default();
                    if closes_for_day.is_empty() {
                        if matches!(today.weekday(), Weekday::Sat | Weekday::Sun) {
                            info!(
                                date = %today,
                                "session close grace elapsed on weekend — marking processed"
                            );
                            processed_sessions.insert(today);
                            continue;
                        }
                        error!(
                            date = %today,
                            symbols_expected,
                            buffered_days = day_closes.len(),
                            current_notionals = current_notionals.len(),
                            "session close grace elapsed on weekday with zero buffered closes"
                        );
                        return Err(format!(
                            "session close grace elapsed on weekday {today} but no RTH closes were buffered"
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
                        alpaca,
                        today,
                        &closes_for_day,
                        &portfolio_config,
                        &mut current_shares,
                        execution,
                    )
                    .await;
                    if let Err(e) = engine.save_state(state_path) {
                        error!(
                            error = %e,
                            state_path = %state_path.display(),
                            "failed to persist basket engine state after session close"
                        );
                        return Err(e);
                    }
                    info!(
                        date = %today,
                        state_path = %state_path.display(),
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

/// Fetch open positions from Alpaca and express them as signed notional per symbol.
/// Used on startup so `diff_to_orders` computes correct deltas from the engine's target.
///
/// Returns `Err` on any fetch failure; the caller must treat this as fatal for
/// paper/live execution (trading from an empty notional map would double-size
/// every already-open leg on the first session).
async fn seed_current_shares_from_alpaca(
    alpaca: &AlpacaClient,
    mode: ExecutionMode,
) -> Result<HashMap<String, i64>, String> {
    let positions = alpaca.get_positions(mode).await.map_err(|e| {
        format!(
            "startup position reconciliation failed — refusing to trade without a \
             trusted notional map (fetch error: {e})"
        )
    })?;
    let shares: HashMap<String, i64> = positions
        .into_iter()
        .map(|(sym, (qty, _avg_entry))| (sym, qty.round() as i64))
        .collect();
    info!(
        n_positions = shares.len(),
        "seeded current_shares from Alpaca open positions"
    );
    Ok(shares)
}

/// Run the engine for one session close and dispatch orders.
#[allow(clippy::too_many_arguments)]
async fn process_session_close(
    engine: &mut BasketEngine,
    alpaca: &AlpacaClient,
    date: NaiveDate,
    closes: &HashMap<String, f64>,
    portfolio_config: &PortfolioConfig,
    current_shares: &mut HashMap<String, i64>,
    execution: BasketExecution,
) {
    if closes.is_empty() {
        warn!(date = %date, "no RTH closes buffered for session — skipping engine");
        return;
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

    // Portfolio layer: target notionals across symbols, then diff to orders.
    let target_notionals = aggregate_positions(engine, portfolio_config);
    let target_shares = target_shares_from_notionals(&target_notionals, closes);

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
    info!(
        date = %date,
        targets = target_notionals.len(),
        current_positions = current_shares.len(),
        gross_long = %format!("{:.0}", gross_long),
        gross_short = %format!("{:.0}", gross_short),
        gross_notional = %format!("{:.0}", gross_long + gross_short.abs()),
        net_notional = %format!("{:.0}", gross_long + gross_short),
        max_abs_leg = %format!("{:.0}", max_abs),
        median_abs_leg = %format!("{:.0}", median_abs),
        "target notionals summary"
    );

    let orders = diff_to_orders(current_shares, &target_shares);
    if orders.is_empty() {
        info!(date = %date, "no orders to emit — targets already match current");
        return;
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
    info!(
        date = %date,
        n_orders = orders.len(),
        order_gross_notional = %format!("{:.0}", order_gross),
        order_max_notional = %format!("{:.0}", order_max),
        "emitting orders"
    );

    // Track which symbols were successfully adjusted so we only update
    // `current_shares` for legs that actually executed.
    let mut successfully_adjusted: std::collections::HashSet<String> = Default::default();

    match execution.alpaca_mode() {
        None => {
            // Noop — log only, but treat every emitted order as "successful"
            // so the simulated state evolves consistently across sessions.
            for order in &orders {
                log_order(order, "NOOP");
                successfully_adjusted.insert(order.symbol.clone());
            }
        }
        Some(mode) => {
            for order in &orders {
                log_order(order, execution.label());
                let side_str = match order.side {
                    Side::Buy => "buy",
                    Side::Sell => "sell",
                };
                match alpaca
                    .place_order(&order.symbol, order.qty as f64, side_str, mode)
                    .await
                {
                    Ok(o) => {
                        info!(
                            symbol = order.symbol.as_str(),
                            qty = order.qty,
                            side = side_str,
                            order_id = o.id.as_str(),
                            status = o.status.as_str(),
                            "ORDER PLACED"
                        );
                        successfully_adjusted.insert(order.symbol.clone());
                    }
                    Err(e) => error!(
                        symbol = order.symbol.as_str(),
                        qty = order.qty,
                        side = side_str,
                        error = e.as_str(),
                        "ORDER FAILED"
                    ),
                }
            }
        }
    }

    // Reconcile current_shares against what actually executed.
    let orders_by_symbol: std::collections::HashSet<&str> =
        orders.iter().map(|o| o.symbol.as_str()).collect();
    let mut all_symbols: std::collections::HashSet<String> =
        current_shares.keys().cloned().collect();
    all_symbols.extend(target_shares.keys().cloned());
    let mut drift_count = 0usize;
    for sym in all_symbols {
        let target_opt = target_shares.get(&sym).copied();
        let apply_target = |current: &mut HashMap<String, i64>| match target_opt {
            Some(t) => {
                current.insert(sym.clone(), t);
            }
            None => {
                current.remove(&sym);
            }
        };
        if successfully_adjusted.contains(&sym) {
            apply_target(current_shares);
        } else if orders_by_symbol.contains(sym.as_str()) {
            // Order was emitted but did not succeed → preserve prior shares.
            drift_count += 1;
        } else {
            apply_target(current_shares);
        }
    }
    if drift_count > 0 {
        warn!(
            drift_count,
            total_orders = orders.len(),
            "some orders failed; current_shares preserves prior values for failed legs"
        );
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
    info!(
        mode = label,
        symbol = order.symbol.as_str(),
        qty = order.qty,
        side = side_str,
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
    use arrow::array::{Array, Float64Array, TimestampMicrosecondArray};
    use std::collections::BTreeMap;
    let cutoff = Utc::now().date_naive() - chrono::Duration::days(window_days);
    let today = Utc::now().date_naive();

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
                if date < cutoff || date >= today {
                    continue;
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
        let series: Vec<(NaiveDate, f64)> = daily.into_iter().map(|(d, (_, c))| (d, c)).collect();
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
    use chrono::Timelike;

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
}
