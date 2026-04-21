//! Live/paper runner for the basket spread engine.
//!
//! Drives `basket_engine::BasketEngine` (continuous streaming state machine,
//! NOT the walk-forward replay in `basket_runner.rs`) with real-time 1-min
//! bars from the Alpaca WebSocket.
//!
//! Flow per trading day:
//!   1. Warmup (startup): read last N days of parquets, validate candidates,
//!      build `BasketEngine` from `BasketFit`s. Engine enters with empty state.
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
    aggregate_positions, diff_to_orders, BasketEngine, DailyBar, OrderIntent, PortfolioConfig,
    PositionIntent, Side,
};
use basket_picker::{load_universe, validate, ValidatorConfig};
use chrono::{DateTime, NaiveDate, Timelike, Utc};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use tracing::{error, info, warn};

use crate::alpaca::{AlpacaClient, ExecutionMode};
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

/// Session close in UTC minutes (RTH end = 20:00). The last 1-min bar finalizes
/// at 19:59 (Alpaca's `t` is bar-open; close is `t + 60s`).
const SESSION_CLOSE_MIN: u32 = 20 * 60;

/// RTH window start in UTC minutes (13:30).
const RTH_START_MIN: u32 = 13 * 60 + 30;

/// Warmup window: days of history to seed state. Needs >= residual_window from
/// the universe config (60 by default) + a small buffer for safety.
const WARMUP_DAYS: i64 = 90;

/// Run the basket live/paper loop.
///
/// Returns on Ctrl+C or fatal error.
pub async fn run_basket_live(
    alpaca: &AlpacaClient,
    universe_path: &Path,
    bars_dir: &Path,
    execution: BasketExecution,
    portfolio_config: PortfolioConfig,
) -> Result<(), String> {
    info!(
        universe = %universe_path.display(),
        bars_dir = %bars_dir.display(),
        execution = execution.label(),
        "========== BASKET LIVE RUNNER =========="
    );

    if execution == BasketExecution::Live {
        warn!("LIVE MODE — real-money orders will be placed on every EOD signal");
    }

    // 1. Load universe + validate candidates using parquet warmup data.
    let universe = load_universe(universe_path)?;
    info!(
        baskets = universe.num_baskets(),
        sectors = universe.sectors.len(),
        "loaded universe"
    );

    let symbols = collect_symbols(&universe);
    let closes = load_warmup_closes(bars_dir, &symbols, WARMUP_DAYS)?;
    info!(
        symbols = symbols.len(),
        loaded_series = closes.len(),
        "loaded warmup closes"
    );

    // Build price-history map for validator (HashMap<symbol, Vec<f64>> aligned by date).
    let price_history = align_history(&closes);
    let validator_config = ValidatorConfig {
        residual_window: universe.strategy.residual_window_days,
        k_clip_min: universe.strategy.threshold_clip_min,
        k_clip_max: universe.strategy.threshold_clip_max,
        cost: universe.strategy.cost_bps_assumed / 10_000.0,
    };

    let fits: Vec<_> = universe
        .candidates
        .iter()
        .map(|c| validate(c, &price_history, &validator_config))
        .collect();
    let valid_count = fits.iter().filter(|f| f.valid).count();
    info!(
        total = fits.len(),
        valid = valid_count,
        "validated basket candidates"
    );
    if valid_count == 0 {
        return Err("no valid baskets after warmup validation".to_string());
    }

    let mut engine = BasketEngine::new(&fits);
    info!(baskets = engine.num_baskets(), "basket engine initialized");

    // 2. Seed current_notionals from Alpaca positions (startup reconciliation).
    //    Without this, a restart with live open positions would trigger
    //    target-minus-zero deltas, flooding Alpaca with duplicate orders.
    //    Noop skips this (no Alpaca account needed for shadow mode).
    let mut current_notionals = match execution.alpaca_mode() {
        None => {
            info!("noop mode — skipping startup position reconciliation");
            HashMap::new()
        }
        Some(mode) => seed_current_notionals_from_alpaca(alpaca, mode).await,
    };

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
    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);

    loop {
        tokio::select! {
            Some(bar) = bar_rx.recv() => {
                let dt = match DateTime::<Utc>::from_timestamp_millis(bar.timestamp) {
                    Some(d) => d,
                    None => continue,
                };
                let minute = dt.hour() * 60 + dt.minute();
                if !(RTH_START_MIN..SESSION_CLOSE_MIN).contains(&minute) {
                    // Outside RTH — ignore (do not contaminate daily close).
                    continue;
                }
                let date = dt.date_naive();
                if bar.close.is_finite() && bar.close > 0.0 {
                    day_closes
                        .entry(date)
                        .or_default()
                        .insert(bar.symbol.clone(), bar.close);
                }
            }
            _ = tick.tick() => {
                // Wall-clock trigger: if we are past session close + grace for a
                // given date and haven't processed it yet, fire now — regardless
                // of which symbols' 19:59 bars landed.
                let now = Utc::now();
                let today = now.date_naive();
                let minute_now = now.hour() * 60 + now.minute();
                let past_close = minute_now >= SESSION_CLOSE_MIN + CLOSE_GRACE_MIN;
                if past_close && !processed_sessions.contains(&today) {
                    let closes_for_day = day_closes.remove(&today).unwrap_or_default();
                    if closes_for_day.is_empty() {
                        // Weekend / holiday / blackout — mark processed to avoid busy-looping.
                        processed_sessions.insert(today);
                        continue;
                    }
                    processed_sessions.insert(today);
                    process_session_close(
                        &mut engine,
                        alpaca,
                        today,
                        &closes_for_day,
                        &portfolio_config,
                        &mut current_notionals,
                        execution,
                    )
                    .await;
                }
            }
            _ = &mut ctrl_c => {
                info!("========== SHUTDOWN ==========");
                break;
            }
        }
    }
    Ok(())
}

/// Fetch open positions from Alpaca and express them as signed notional per symbol.
/// Used on startup so `diff_to_orders` computes correct deltas from the engine's target.
async fn seed_current_notionals_from_alpaca(
    alpaca: &AlpacaClient,
    mode: ExecutionMode,
) -> HashMap<String, f64> {
    match alpaca.get_positions(mode).await {
        Ok(positions) => {
            let notionals: HashMap<String, f64> = positions
                .into_iter()
                .map(|(sym, (qty, avg_entry))| (sym, qty * avg_entry))
                .collect();
            info!(
                n_positions = notionals.len(),
                "seeded current_notionals from Alpaca open positions"
            );
            notionals
        }
        Err(e) => {
            // Fail closed: if we can't read positions, we can't safely diff. Warn loudly
            // and return empty so the operator notices in the first session's logs.
            warn!(
                error = %e,
                "failed to fetch Alpaca positions on startup — current_notionals empty; \
                 first session will emit target-minus-zero deltas and may double-size"
            );
            HashMap::new()
        }
    }
}

/// Run the engine for one session close and dispatch orders.
#[allow(clippy::too_many_arguments)]
async fn process_session_close(
    engine: &mut BasketEngine,
    alpaca: &AlpacaClient,
    date: NaiveDate,
    closes: &HashMap<String, f64>,
    portfolio_config: &PortfolioConfig,
    current_notionals: &mut HashMap<String, f64>,
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
    let orders = diff_to_orders(current_notionals, &target_notionals, closes);
    if orders.is_empty() {
        return;
    }

    info!(date = %date, n_orders = orders.len(), "emitting orders");

    match execution.alpaca_mode() {
        None => {
            // Noop — log only.
            for order in &orders {
                log_order(order, "NOOP");
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
                    Ok(o) => info!(
                        symbol = order.symbol.as_str(),
                        qty = order.qty,
                        side = side_str,
                        order_id = o.id.as_str(),
                        status = o.status.as_str(),
                        "ORDER PLACED"
                    ),
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

    // After dispatching, adopt target notionals as current (ignoring reject/partial-fill
    // edge cases — reconciliation lives in a separate follow-up).
    *current_notionals = target_notionals;
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

fn collect_symbols(universe: &basket_picker::Universe) -> Vec<String> {
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
fn load_warmup_closes(
    bars_dir: &Path,
    symbols: &[String],
    window_days: i64,
) -> Result<HashMap<String, Vec<(NaiveDate, f64)>>, String> {
    use arrow::array::{Array, Float64Array, TimestampMicrosecondArray};
    use std::collections::BTreeMap;
    let cutoff = Utc::now().date_naive() - chrono::Duration::days(window_days);

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
                let minute = dt.hour() * 60 + dt.minute();
                if !(RTH_START_MIN..SESSION_CLOSE_MIN).contains(&minute) {
                    continue;
                }
                let px = close.value(i);
                if !px.is_finite() || px <= 0.0 {
                    continue;
                }
                let date = dt.date();
                if date < cutoff {
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

/// Transform per-symbol `Vec<(date, close)>` into aligned `HashMap<symbol, Vec<close>>`
/// by the intersection of dates — required shape for `basket_picker::validate`.
fn align_history(closes: &HashMap<String, Vec<(NaiveDate, f64)>>) -> HashMap<String, Vec<f64>> {
    if closes.is_empty() {
        return HashMap::new();
    }
    // Intersection of dates across all symbols.
    let first = closes.values().next().unwrap();
    let mut common: std::collections::BTreeSet<NaiveDate> = first.iter().map(|(d, _)| *d).collect();
    for series in closes.values() {
        let s: std::collections::BTreeSet<NaiveDate> = series.iter().map(|(d, _)| *d).collect();
        common = common.intersection(&s).cloned().collect();
    }

    let mut out = HashMap::new();
    for (symbol, series) in closes {
        let map: HashMap<NaiveDate, f64> = series.iter().copied().collect();
        let aligned: Vec<f64> = common.iter().filter_map(|d| map.get(d).copied()).collect();
        out.insert(symbol.clone(), aligned);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn test_align_history_intersects_dates() {
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
        let aligned = align_history(&closes);
        // Intersection is [2026-01-02, 2026-01-03] — each series has 2 entries.
        assert_eq!(aligned.get("A").unwrap().len(), 2);
        assert_eq!(aligned.get("B").unwrap().len(), 2);
        assert_eq!(aligned.get("A").unwrap()[0], 11.0);
        assert_eq!(aligned.get("B").unwrap()[0], 20.0);
    }
}
