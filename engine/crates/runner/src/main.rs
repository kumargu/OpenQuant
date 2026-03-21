//! OpenQuant Runner — standalone trading engine binary.
//!
//! Reads bars from JSON files, runs both Engine (single-symbol) and
//! PairsEngine (pairs), writes order intents to JSON.
//!
//! No Python bridge needed. Communication is filesystem-based:
//! - Input: data/experiment_bars_*.json, openquant.toml, data/active_pairs.json
//! - Output: data/order_intents.json, data/trade_results.json

mod bars;
mod intents;

use intents::{write_intents, OrderIntentRecord};
use openquant_core::config::ConfigFile;
use openquant_core::engine::Engine;
use openquant_core::pairs::engine::PairsEngine;
use std::path::PathBuf;
use tracing::{error, info, warn};

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args: Vec<String> = std::env::args().collect();

    let config_path = args
        .iter()
        .position(|a| a == "--config")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("openquant.toml"));

    let data_dir = args
        .iter()
        .position(|a| a == "--data-dir")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("data"));

    let output_dir = args
        .iter()
        .position(|a| a == "--output-dir")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from)
        .unwrap_or_else(|| data_dir.clone());

    info!("OpenQuant Runner starting");
    info!("  config:     {}", config_path.display());
    info!("  data_dir:   {}", data_dir.display());
    info!("  output_dir: {}", output_dir.display());

    // ── Load config ──
    let cfg_file = match ConfigFile::load(&config_path) {
        Ok(c) => c,
        Err(e) => {
            error!("Failed to load config: {e}");
            std::process::exit(1);
        }
    };

    let pair_configs = cfg_file.pair_configs();
    let engine_config = cfg_file.into_engine_config();

    // ── Initialize engines ──
    let mut engine = Engine::new(engine_config);
    engine.set_warmup_mode(true);

    let active_pairs_path = data_dir.join("active_pairs.json");
    let history_path = data_dir.join("pair_trading_history.json");

    let mut pairs_engine = if active_pairs_path.exists() {
        info!("Loading pairs from active_pairs.json");
        PairsEngine::from_active_pairs(&active_pairs_path, &history_path, pair_configs)
    } else if !pair_configs.is_empty() {
        info!("Loading {} pairs from TOML config", pair_configs.len());
        PairsEngine::new(pair_configs)
    } else {
        warn!("No pairs configured — running single-symbol engine only");
        PairsEngine::new(vec![])
    };

    info!("Engines initialized: pairs={}", pairs_engine.pair_count());

    // ── Load bars ──
    let all_bars = match bars::load_days(&data_dir) {
        Ok(b) => b,
        Err(e) => {
            error!("Failed to load bars: {e}");
            std::process::exit(1);
        }
    };

    if all_bars.is_empty() {
        error!("No bars loaded — nothing to process");
        std::process::exit(1);
    }

    // ── Process bars ──
    let warmup_bars = 64;
    let mut all_intents = Vec::new();
    let mut pair_intent_count = 0;
    let mut single_intent_count = 0;

    for (i, bar) in all_bars.iter().enumerate() {
        // Switch off warmup after initial period
        if i == warmup_bars {
            engine.set_warmup_mode(false);
            info!("Warmup complete — live signal generation enabled");
        }

        // Single-symbol engine
        let single_intents = engine.on_bar(bar);
        single_intent_count += single_intents.len();

        // Pairs engine
        let pair_intents = pairs_engine.on_bar(&bar.symbol, bar.timestamp, bar.close);
        for intent in &pair_intents {
            all_intents.push(OrderIntentRecord::from_pair_intent(intent, bar.timestamp));
        }
        pair_intent_count += pair_intents.len();
    }

    info!(
        "Processing complete: {} bars, {} pair intents, {} single intents",
        all_bars.len(),
        pair_intent_count,
        single_intent_count,
    );

    // ── Write outputs ──
    let intents_path = output_dir.join("order_intents.json");
    if let Err(e) = write_intents(&all_intents, &intents_path) {
        error!("Failed to write intents: {e}");
    }

    info!("Runner complete");
}
