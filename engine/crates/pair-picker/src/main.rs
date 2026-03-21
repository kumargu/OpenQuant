//! Pair Picker CLI — daily statistical validation of pairs trading candidates.
//!
//! Reads `pair_candidates.json`, validates each pair, writes `active_pairs.json`.
//! Uses a lock file to ensure at most one run per day.

use pair_picker::lockfile;
use pair_picker::pipeline::{self, InMemoryPrices, PipelineError};
use pair_picker::types::PairCandidatesFile;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tracing::{error, info};

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args: Vec<String> = std::env::args().collect();

    let data_dir = args
        .iter()
        .position(|a| a == "--data-dir")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from)
        .unwrap_or_else(find_data_dir);

    // --check mode: just report whether today's run is done
    if args.iter().any(|a| a == "--check") {
        if lockfile::has_run_today(&data_dir) {
            info!("Pair picker has already run today");
            std::process::exit(0);
        } else {
            info!("Pair picker has NOT run today");
            std::process::exit(1);
        }
    }

    // --force: skip lock file check
    let force = args.iter().any(|a| a == "--force");

    if !force && lockfile::has_run_today(&data_dir) {
        info!("Pair picker already ran today. Use --force to re-run.");
        return;
    }

    let candidates_path = args
        .iter()
        .position(|a| a == "--candidates")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from)
        .unwrap_or_else(|| data_dir.join("pair_candidates.json"));

    let output_path = args
        .iter()
        .position(|a| a == "--output")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from)
        .unwrap_or_else(|| data_dir.join("active_pairs.json"));

    info!("Pair Picker starting");
    info!("  candidates: {}", candidates_path.display());
    info!("  output:     {}", output_path.display());

    // Read candidates
    let contents = match std::fs::read_to_string(&candidates_path) {
        Ok(c) => c,
        Err(e) => {
            error!("Failed to read candidates: {e}");
            std::process::exit(1);
        }
    };

    let candidates: PairCandidatesFile = match serde_json::from_str(&contents) {
        Ok(c) => c,
        Err(e) => {
            error!("Failed to parse candidates JSON: {e}");
            std::process::exit(1);
        }
    };

    info!("Loaded {} candidate pairs", candidates.pairs.len());

    // For now, use an empty provider (no price data).
    // In production, this will be replaced with an Alpaca API provider.
    // The binary is designed so the Python runner can pre-populate price data
    // as JSON files that the provider reads.
    let price_file = data_dir.join("pair_picker_prices.json");
    let provider = if price_file.exists() {
        match load_prices_from_file(&price_file) {
            Ok(p) => p,
            Err(e) => {
                error!("Failed to load prices: {e}");
                std::process::exit(1);
            }
        }
    } else {
        info!(
            "No price data file found at {}. Using empty provider.",
            price_file.display()
        );
        info!(
            "The Python runner should populate {} before calling pair-picker.",
            price_file.display()
        );
        InMemoryPrices {
            data: HashMap::new(),
        }
    };

    match pipeline::run_pipeline_from_candidates(&candidates.pairs, &output_path, &provider) {
        Ok(results) => {
            let passed = results.iter().filter(|r| r.passed).count();
            let rejected = results.len() - passed;
            info!("Pipeline complete: {passed} passed, {rejected} rejected");

            // Create lock file
            if let Err(e) = lockfile::create_lock(&data_dir) {
                error!("Failed to create lock file: {e}");
            }

            // Cleanup old locks
            match lockfile::cleanup_old_locks(&data_dir) {
                Ok(n) if n > 0 => info!("Cleaned up {n} old lock files"),
                _ => {}
            }
        }
        Err(e) => {
            error!("Pipeline failed: {e}");
            std::process::exit(1);
        }
    }
}

fn find_data_dir() -> PathBuf {
    // Walk up from CWD looking for a data/ directory that contains
    // pair_candidates.json (the expected input file). This avoids
    // false matches like engine/data/ (journal storage) when running
    // from the engine/ subdirectory.
    let mut dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    for _ in 0..5 {
        let data = dir.join("data");
        if data.join("pair_candidates.json").exists()
            || data.join("stock_relationships.json").exists()
        {
            return data;
        }
        if !dir.pop() {
            break;
        }
    }
    // Fallback: use CWD/data
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("data")
}

/// Load price data from a JSON file.
/// Format: { "SYMBOL": [close1, close2, ...], ... }
fn load_prices_from_file(path: &Path) -> Result<InMemoryPrices, PipelineError> {
    let contents = std::fs::read_to_string(path).map_err(PipelineError::Io)?;
    let data: HashMap<String, Vec<f64>> =
        serde_json::from_str(&contents).map_err(PipelineError::Json)?;
    Ok(InMemoryPrices { data })
}
