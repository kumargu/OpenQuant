//! Pair-picker integration — validates lab-provided candidates using Alpaca
//! daily bars and the pair-picker validation pipeline.
//!
//! Lab discovers and ranks candidate pairs. This service validates them
//! against structural quality gates (ADF, R², half-life, beta stability)
//! using price data fetched from Alpaca (or a mock server via ALPACA_DATA_URL).
//!
//! Candidates are always provided via `--candidates <path>`. There is no
//! built-in candidate discovery — that responsibility lives in quant-lab.

use crate::alpaca::AlpacaClient;
use openquant_core::pairs::PairConfig;
use pair_picker::pipeline::{validate_candidates_with_config, InMemoryPrices, PipelineConfig};
use pair_picker::types::ActivePair;
use std::path::Path;
use tracing::info;

/// Validate lab-provided candidates using Alpaca daily bars.
///
/// `price_end_date`: daily bars are fetched up to (but not including) this date.
/// For replay: set to the replay start date to prevent look-ahead bias.
/// For live/paper: set to today.
///
/// `candidates_path`: path to the candidates JSON file (from quant-lab).
/// Required — the runner must always provide this via `--candidates`.
pub async fn generate_pairs_with_config(
    alpaca: &AlpacaClient,
    _trading_dir: &Path,
    price_end_date: chrono::NaiveDate,
    top_k: usize,
    candidates_path: Option<&Path>,
    pipeline_cfg: &PipelineConfig,
) -> Result<Vec<ActivePair>, String> {
    let path = candidates_path
        .ok_or("--candidates is required: provide a lab-generated candidates JSON file")?;

    let contents = std::fs::read_to_string(path).map_err(|e| format!("read candidates: {e}"))?;
    let file: pair_picker::types::PairCandidatesFile =
        serde_json::from_str(&contents).map_err(|e| format!("parse error: {e}"))?;
    info!(
        candidates = file.pairs.len(),
        path = %path.display(),
        "loaded candidates from lab"
    );
    let candidates = file.pairs;

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
    let mut active_pairs = validate_candidates_with_config(&candidates, &provider, pipeline_cfg);

    // Truncate to top_k
    active_pairs.truncate(top_k);

    info!(pairs = active_pairs.len(), top_k, "pair-picker complete");

    Ok(active_pairs)
}

/// Write active pairs to JSON file (for engine reload).
pub fn write_active_pairs(pairs: &[ActivePair], path: &Path) -> Result<(), String> {
    let file = pair_picker::types::ActivePairsFile {
        generated_at: chrono::Utc::now(),
        pairs: pairs.to_vec(),
    };
    let json = serde_json::to_string_pretty(&file).map_err(|e| format!("serialize error: {e}"))?;
    std::fs::write(path, json).map_err(|e| format!("write error: {e}"))?;
    info!(pairs = pairs.len(), path = %path.display(), "wrote active_pairs.json");
    Ok(())
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
