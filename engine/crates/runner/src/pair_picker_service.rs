//! Pair-picker integration — generates pairs from Alpaca data using the pair-picker library.
//!
//! Called from the runner at startup and periodically during replay to regenerate pairs
//! using only data available at that point in time (no look-ahead bias).

use crate::alpaca::AlpacaClient;
use openquant_core::pairs::PairConfig;
use pair_picker::graph::RelationshipGraph;
use pair_picker::pipeline::{validate_candidates, InMemoryPrices};
use pair_picker::types::ActivePair;
use std::path::Path;
use tracing::info;

/// Run pair-picker using Alpaca daily bars as the price source.
///
/// `price_end_date`: daily bars are fetched up to (but not including) this date.
/// For replay: set to the replay start date to prevent look-ahead bias.
/// For live/paper: set to today.
pub async fn generate_pairs(
    alpaca: &AlpacaClient,
    trading_dir: &Path,
    price_end_date: chrono::NaiveDate,
    top_k: usize,
) -> Result<Vec<ActivePair>, String> {
    // Load relationship graph for candidate generation
    let graph_path = trading_dir.join("stock_relationships.json");
    let candidates_path = trading_dir.join("pair_candidates.json");

    let candidates = if graph_path.exists() {
        let graph = RelationshipGraph::load(&graph_path)
            .ok_or("Failed to load stock_relationships.json")?;
        let c = graph.to_candidates();
        info!(
            candidates = c.len(),
            "generated candidates from relationship graph"
        );
        c
    } else if candidates_path.exists() {
        let contents =
            std::fs::read_to_string(&candidates_path).map_err(|e| format!("read error: {e}"))?;
        let file: pair_picker::types::PairCandidatesFile =
            serde_json::from_str(&contents).map_err(|e| format!("parse error: {e}"))?;
        info!(
            candidates = file.pairs.len(),
            "loaded candidates from pair_candidates.json"
        );
        file.pairs
    } else {
        return Err("No stock_relationships.json or pair_candidates.json found".into());
    };

    // Collect unique symbols
    let mut symbols: Vec<String> = candidates
        .iter()
        .flat_map(|c| vec![c.leg_a.clone(), c.leg_b.clone()])
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();
    symbols.sort();

    // Fetch 180 calendar days of daily bars ending before price_end_date
    let end_str = price_end_date.format("%Y-%m-%d").to_string();
    let start_date = price_end_date - chrono::Duration::days(180);
    let start_str = start_date.format("%Y-%m-%d").to_string();

    info!(
        symbols = symbols.len(),
        start = start_str.as_str(),
        end = end_str.as_str(),
        "fetching daily bars for pair-picker"
    );

    let prices = alpaca
        .fetch_daily_bars_range(&symbols, &start_str, &end_str)
        .await?;

    // Build InMemoryPrices provider
    let provider = InMemoryPrices { data: prices };

    // Run validation pipeline (in-memory, no file I/O)
    let mut active_pairs = validate_candidates(&candidates, &provider);

    // Truncate to top_k
    active_pairs.truncate(top_k);

    info!(pairs = active_pairs.len(), top_k, "pair-picker complete");

    Ok(active_pairs)
}

/// Convert ActivePair (pair-picker type) to PairConfig (core type).
/// This keeps the dependency direction clean: runner depends on both, core doesn't know pair-picker.
pub fn to_pair_configs(pairs: &[ActivePair]) -> Vec<PairConfig> {
    pairs
        .iter()
        .map(|p| {
            let kappa = if p.half_life_days > 0.0 && p.half_life_days.is_finite() {
                f64::ln(2.0) / p.half_life_days
            } else {
                0.0
            };
            let lookback_bars = if p.half_life_days.is_finite() && p.half_life_days > 0.0 {
                let hl_bars = p.half_life_days.ceil() as usize;
                (2 * hl_bars).min(60)
            } else {
                0
            };
            PairConfig {
                leg_a: p.leg_a.clone(),
                leg_b: p.leg_b.clone(),
                alpha: p.alpha,
                beta: p.beta,
                kappa,
                max_hold_bars: p.max_hold_days,
                lookback_bars,
            }
        })
        .collect()
}
