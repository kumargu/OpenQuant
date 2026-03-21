//! openquant-runner — standalone trading engine binary.
//!
//! Reads bars from JSON files, runs both Engine (single-symbol) and
//! PairsEngine (pairs), writes order intents and trade results to JSON.
//!
//! No Python bridge needed. Communication is filesystem-based:
//! - Input:  data/experiment_bars_*.json, openquant.toml, data/active_pairs.json
//! - Output: data/order_intents.json, data/trade_results.json

mod bars;
mod intents;
mod pnl;

use clap::Parser;
use intents::{write_intents, write_trade_results, OrderIntentRecord};
use openquant_core::config::ConfigFile;
use openquant_core::engine::Engine;
use openquant_core::pairs::engine::PairsEngine;
use std::path::PathBuf;
use tracing::{error, info, warn};

#[derive(Parser, Debug)]
#[command(
    name = "openquant-runner",
    about = "Standalone OpenQuant trading engine"
)]
struct Cli {
    /// Path to openquant.toml config file.
    #[arg(long, default_value = "openquant.toml")]
    config: PathBuf,

    /// Directory containing experiment_bars_*.json files.
    #[arg(long, default_value = "data")]
    data_dir: PathBuf,

    /// Directory for output files. Defaults to data_dir.
    #[arg(long)]
    output_dir: Option<PathBuf>,

    /// Number of warmup bars before signal generation begins.
    #[arg(long, default_value = "64")]
    warmup_bars: usize,
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();
    let output_dir = cli.output_dir.as_ref().unwrap_or(&cli.data_dir);

    info!(
        config = %cli.config.display(),
        data_dir = %cli.data_dir.display(),
        output_dir = %output_dir.display(),
        warmup_bars = cli.warmup_bars,
        "openquant-runner starting"
    );

    // ── Load config ──
    let cfg_file = match ConfigFile::load(&cli.config) {
        Ok(c) => c,
        Err(e) => {
            error!("failed to load config: {e}");
            std::process::exit(1);
        }
    };

    // pair_configs() must be called before into_engine_config() which consumes cfg_file.
    let pair_configs = cfg_file.pair_configs();
    let engine_config = cfg_file.into_engine_config();

    // ── Initialize engines ──
    let mut engine = Engine::new(engine_config);
    engine.set_warmup_mode(true);

    let active_pairs_path = cli.data_dir.join("active_pairs.json");
    let history_path = cli.data_dir.join("pair_trading_history.json");

    let mut pairs_engine = if active_pairs_path.exists() {
        info!(path = %active_pairs_path.display(), "loading active pairs");
        PairsEngine::from_active_pairs(&active_pairs_path, &history_path, pair_configs)
    } else if !pair_configs.is_empty() {
        info!(count = pair_configs.len(), "loading pairs from TOML config");
        PairsEngine::new(pair_configs)
    } else {
        warn!("no pairs configured — running single-symbol engine only");
        PairsEngine::new(vec![])
    };

    info!(pairs = pairs_engine.pair_count(), "engines initialized");

    // ── Load bars ──
    let all_bars = match bars::load_days(&cli.data_dir) {
        Ok(b) => b,
        Err(e) => {
            error!("failed to load bars: {e}");
            std::process::exit(1);
        }
    };

    if all_bars.is_empty() {
        error!("no bars loaded — nothing to process");
        std::process::exit(1);
    }

    // ── Process bars ──
    let mut all_intents: Vec<OrderIntentRecord> = Vec::new();
    let mut single_intent_count: usize = 0;
    let mut pair_intent_count: usize = 0;
    let mut pnl_tracker = pnl::PairPnlTracker::new();
    let mut prev_day: Option<i64> = None;
    // Day boundary: 24h in millis. Detect when timestamp jumps by >6h gap.
    const DAY_GAP_MS: i64 = 6 * 3600 * 1000;

    for (i, bar) in all_bars.iter().enumerate() {
        if i == cli.warmup_bars {
            engine.set_warmup_mode(false);
            info!("warmup complete — live signal generation enabled");
        }

        // Detect day boundary and reset daily state
        if let Some(prev) = prev_day {
            if bar.timestamp - prev > DAY_GAP_MS {
                engine.reset_daily();
                pairs_engine.reset_daily();
                info!(
                    timestamp = bar.timestamp,
                    "day boundary — reset daily state"
                );
            }
        }
        prev_day = Some(bar.timestamp);

        // Track prices for P&L computation
        pnl_tracker.update_price(&bar.symbol, bar.close);

        // Single-symbol engine
        let single_intents = engine.on_bar(bar);
        for intent in &single_intents {
            all_intents.push(OrderIntentRecord::from_engine_intent(intent, bar.timestamp));
            engine.on_fill(&intent.symbol, intent.side, intent.qty, bar.close);
        }
        single_intent_count += single_intents.len();

        // Pairs engine
        let pair_intents = pairs_engine.on_bar(&bar.symbol, bar.timestamp, bar.close);
        if !pair_intents.is_empty() {
            pnl_tracker.on_intents(&pair_intents, bar.timestamp);
            for intent in &pair_intents {
                all_intents.push(OrderIntentRecord::from_pair_intent(intent, bar.timestamp));
            }
            pair_intent_count += pair_intents.len();
        }

        pnl_tracker.tick_bars();
    }

    info!(
        bars = all_bars.len(),
        single_intents = single_intent_count,
        pair_intents = pair_intent_count,
        total_intents = all_intents.len(),
        "processing complete"
    );

    // ── Write outputs ──
    std::fs::create_dir_all(output_dir).unwrap_or_else(|e| {
        error!("cannot create output dir: {e}");
        std::process::exit(1);
    });

    let intents_path = output_dir.join("order_intents.json");
    if let Err(e) = write_intents(&all_intents, &intents_path) {
        error!("failed to write order intents: {e}");
    }

    let results_path = output_dir.join("trade_results.json");
    let closed = pnl_tracker.closed_trades();
    if let Err(e) = write_trade_results(closed, &results_path) {
        error!("failed to write trade results: {e}");
    }

    let summary = pnl_tracker.summary();
    info!(
        total_trades = summary.total_trades,
        total_pnl_bps = format!("{:.1}", summary.total_pnl_bps).as_str(),
        win_rate = format!("{:.1}%", summary.win_rate * 100.0).as_str(),
        avg_win_bps = format!("{:.1}", summary.avg_win_bps).as_str(),
        avg_loss_bps = format!("{:.1}", summary.avg_loss_bps).as_str(),
        "P&L summary (Rust-native, single source of truth)"
    );

    info!("openquant-runner complete");
}
