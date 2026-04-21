//! Basket engine replay harness.
//!
//! Runs the basket-engine on historical data, producing position intents,
//! portfolio aggregation, and daily P&L output.

use std::collections::HashMap;
use std::path::Path;

use basket_engine::{
    aggregate_positions, diff_to_orders, BasketEngine, DailyBar, OrderIntent, PortfolioConfig,
    PositionIntent,
};
use basket_picker::{load_universe, validate, Universe, ValidatorConfig};
use chrono::NaiveDate;
use tracing::{info, warn};

use crate::alpaca::AlpacaClient;

/// Daily P&L record for CSV output.
#[derive(Debug)]
pub struct DailyPnl {
    pub date: NaiveDate,
    pub gross_pnl: f64,
    pub net_pnl: f64,
    pub n_trades: usize,
    pub cumulative_pnl: f64,
}

/// Basket replay result.
pub struct ReplayResult {
    pub daily_pnl: Vec<DailyPnl>,
    pub total_intents: usize,
    pub total_bars: usize,
}

/// Run basket engine replay over a date range.
pub async fn run_basket_replay(
    alpaca: &AlpacaClient,
    universe_path: &Path,
    start: &str,
    end: &str,
    portfolio_config: &PortfolioConfig,
    cost_bps: f64,
) -> Result<ReplayResult, String> {
    // Load and validate universe
    let universe = load_universe(universe_path)?;
    info!(
        baskets = universe.num_baskets(),
        sectors = universe.sectors.len(),
        "loaded basket universe"
    );

    // Collect all symbols needed
    let symbols = collect_symbols(&universe);
    info!(symbols = symbols.len(), "collected symbols for replay");

    // Validate baskets using historical data
    let start_date =
        NaiveDate::parse_from_str(start, "%Y-%m-%d").map_err(|e| format!("invalid start: {e}"))?;
    let end_date =
        NaiveDate::parse_from_str(end, "%Y-%m-%d").map_err(|e| format!("invalid end: {e}"))?;

    // Fetch warmup data (60 days before start for OU fitting)
    let warmup_start = start_date - chrono::Duration::days(70);
    let warmup_bars = fetch_daily_bars_range(alpaca, &symbols, warmup_start, start_date).await?;
    info!(
        bars = warmup_bars.len(),
        "fetched warmup bars for validation"
    );

    // Build price history for validation (HashMap<String, Vec<f64>>)
    let price_history = build_price_map(&warmup_bars);

    // Validate all candidates
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

    // Create engine from valid fits
    let mut engine = BasketEngine::new(&fits);
    info!(baskets = engine.num_baskets(), "basket engine initialized");

    if engine.num_baskets() == 0 {
        return Err("no valid baskets after validation".to_string());
    }

    // Run replay
    let mut daily_pnl = Vec::new();
    let mut total_intents = 0;
    let mut total_bars = 0;
    let mut cumulative_pnl = 0.0;
    let mut current_notionals: HashMap<String, f64> = HashMap::new();
    let mut prev_prices: HashMap<String, f64> = HashMap::new();

    let mut day = start_date;
    while day <= end_date {
        // Fetch daily bars for this day
        let day_bars = fetch_daily_bars_for_day(alpaca, &symbols, day).await;
        if day_bars.is_empty() {
            day += chrono::Duration::days(1);
            continue;
        }

        total_bars += day_bars.len();

        // Build price map
        let prices: HashMap<String, f64> = day_bars
            .iter()
            .map(|b| (b.symbol.clone(), b.close))
            .collect();

        // Calculate P&L from position changes
        let mut day_gross_pnl = 0.0;
        for (symbol, &notional) in &current_notionals {
            if let (Some(&prev_price), Some(&curr_price)) =
                (prev_prices.get(symbol), prices.get(symbol))
            {
                if prev_price > 0.0 && curr_price > 0.0 {
                    let position_shares = notional / prev_price;
                    day_gross_pnl += position_shares * (curr_price - prev_price);
                }
            }
        }

        // Process bars through engine
        let intents = engine.on_bars(&day_bars);
        let n_trades = intents.len();
        total_intents += n_trades;

        for intent in &intents {
            log_basket_intent(intent);
        }

        // Update portfolio
        let target_notionals = aggregate_positions(&engine, portfolio_config);
        let orders = diff_to_orders(&current_notionals, &target_notionals, &prices);

        // Calculate trading costs
        let trade_notional: f64 = orders.iter().map(|o| order_notional(o, &prices)).sum();
        let trading_cost = trade_notional * cost_bps / 10_000.0;

        let day_net_pnl = day_gross_pnl - trading_cost;
        cumulative_pnl += day_net_pnl;

        daily_pnl.push(DailyPnl {
            date: day,
            gross_pnl: day_gross_pnl,
            net_pnl: day_net_pnl,
            n_trades,
            cumulative_pnl,
        });

        if n_trades > 0 {
            info!(
                date = %day,
                trades = n_trades,
                gross_pnl = %format!("{:.2}", day_gross_pnl),
                net_pnl = %format!("{:.2}", day_net_pnl),
                cum_pnl = %format!("{:.2}", cumulative_pnl),
                "daily_summary"
            );
        }

        // Update state for next day
        current_notionals = target_notionals;
        prev_prices = prices;

        day += chrono::Duration::days(1);
    }

    Ok(ReplayResult {
        daily_pnl,
        total_intents,
        total_bars,
    })
}

/// Write daily P&L to CSV.
pub fn write_pnl_csv(pnl: &[DailyPnl], path: &Path) -> Result<(), String> {
    use std::io::Write;
    let mut file = std::fs::File::create(path).map_err(|e| format!("failed to create CSV: {e}"))?;

    writeln!(file, "date,gross_pnl,net_pnl,n_trades,cumulative_pnl")
        .map_err(|e| format!("write error: {e}"))?;

    for row in pnl {
        writeln!(
            file,
            "{},{:.2},{:.2},{},{:.2}",
            row.date, row.gross_pnl, row.net_pnl, row.n_trades, row.cumulative_pnl
        )
        .map_err(|e| format!("write error: {e}"))?;
    }

    Ok(())
}

fn collect_symbols(universe: &Universe) -> Vec<String> {
    let mut symbols: Vec<String> = universe
        .sectors
        .values()
        .flat_map(|s| s.members.iter().cloned())
        .collect();
    symbols.sort();
    symbols.dedup();
    symbols
}

async fn fetch_daily_bars_range(
    alpaca: &AlpacaClient,
    symbols: &[String],
    start: NaiveDate,
    end: NaiveDate,
) -> Result<Vec<(String, NaiveDate, f64)>, String> {
    let start_str = start.format("%Y-%m-%d").to_string();
    let end_str = end.format("%Y-%m-%d").to_string();

    let raw_bars = alpaca
        .fetch_minute_bars(symbols, &start_str, &end_str)
        .await
        .map_err(|e| format!("fetch failed: {e}"))?;

    // Aggregate minute bars to daily closes (last bar of each day)
    let mut daily: HashMap<(String, NaiveDate), f64> = HashMap::new();
    for (symbol, ts_ms, close) in raw_bars {
        let dt = chrono::DateTime::from_timestamp_millis(ts_ms)
            .ok_or("invalid timestamp")?
            .naive_utc()
            .date();
        daily.insert((symbol, dt), close);
    }

    Ok(daily.into_iter().map(|((s, d), c)| (s, d, c)).collect())
}

async fn fetch_daily_bars_for_day(
    alpaca: &AlpacaClient,
    symbols: &[String],
    day: NaiveDate,
) -> Vec<DailyBar> {
    let start = day.format("%Y-%m-%d").to_string();
    let end = (day + chrono::Duration::days(1))
        .format("%Y-%m-%d")
        .to_string();

    let raw_bars = match alpaca.fetch_minute_bars(symbols, &start, &end).await {
        Ok(b) => b,
        Err(e) => {
            warn!(day = %day, error = %e, "fetch failed");
            return vec![];
        }
    };

    // Aggregate to daily close (use last bar timestamp for each symbol)
    let mut last_bars: HashMap<String, (i64, f64)> = HashMap::new();
    for (symbol, ts, close) in raw_bars {
        last_bars
            .entry(symbol)
            .and_modify(|(prev_ts, prev_close)| {
                if ts > *prev_ts {
                    *prev_ts = ts;
                    *prev_close = close;
                }
            })
            .or_insert((ts, close));
    }

    last_bars
        .into_iter()
        .map(|(symbol, (_, close))| DailyBar {
            symbol,
            date: day,
            close,
        })
        .collect()
}

fn build_price_map(bars: &[(String, NaiveDate, f64)]) -> HashMap<String, Vec<f64>> {
    let mut history: HashMap<String, Vec<(NaiveDate, f64)>> = HashMap::new();
    for (symbol, date, close) in bars {
        history
            .entry(symbol.clone())
            .or_default()
            .push((*date, *close));
    }
    // Sort by date and extract prices only
    history
        .into_iter()
        .map(|(symbol, mut prices)| {
            prices.sort_by_key(|(d, _)| *d);
            (symbol, prices.into_iter().map(|(_, p)| p).collect())
        })
        .collect()
}

fn log_basket_intent(intent: &PositionIntent) {
    info!(
        basket_id = %intent.basket_id,
        position = intent.target_position,
        z = %format!("{:.4}", intent.z_score),
        spread = %format!("{:.6}", intent.spread),
        reason = %intent.reason.as_str(),
        date = %intent.date,
        "BASKET_TRANSITION"
    );
}

fn order_notional(order: &OrderIntent, prices: &HashMap<String, f64>) -> f64 {
    prices
        .get(&order.symbol)
        .map(|p| order.qty as f64 * p)
        .unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_collect_symbols() {
        // Minimal test - just verifies the function doesn't panic
        let toml = r#"
[version]
schema = "basket_universe"
version = "v1"
frozen_at = "2026-04-20"

[strategy]
method = "basket_spread_ou_bertram"
spread_formula = "log(target) - mean(log(peers))"
threshold_method = "bertram_symmetric"
threshold_clip_min = 0.15
threshold_clip_max = 2.5
residual_window_days = 60
forward_window_days = 60
refit_cadence = "quarterly"
cost_bps_assumed = 5.0
leverage_assumed = 4.0
sizing = "equal_weight_across_baskets"

[sectors.chips]
members = ["NVDA", "AMD", "INTC"]
traded_targets = ["AMD"]
"#;
        let universe = basket_picker::load_universe_from_str(toml).unwrap();
        let symbols = collect_symbols(&universe);
        assert!(symbols.contains(&"NVDA".to_string()));
        assert!(symbols.contains(&"AMD".to_string()));
        assert!(symbols.contains(&"INTC".to_string()));
    }
}
