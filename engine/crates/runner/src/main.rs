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
use std::io::Write;
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
    let cli = Cli::parse();
    let output_dir = cli.output_dir.as_ref().unwrap_or(&cli.data_dir);

    // Log to both stderr and data/journal/engine.log (append mode)
    let journal_dir = cli.data_dir.join("journal");
    std::fs::create_dir_all(&journal_dir).ok();
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(journal_dir.join("engine.log"))
        .expect("cannot open engine.log");
    let tee = TeeWriter(std::sync::Mutex::new(log_file));

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_ansi(false)
        .with_writer(tee)
        .init();

    // Run ID: git commit short hash + sequence number for log correlation
    let git_commit = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_else(|| "unknown".into())
        .trim()
        .to_string();
    let run_id = format!(
        "{}-{}",
        git_commit,
        chrono::Utc::now().format("%Y%m%d-%H%M%S")
    );

    info!("================================================================");
    info!(run_id = run_id.as_str(), "========== OPENQUANT RUN START ==========");
    info!(
        config = %cli.config.display(),
        data_dir = %cli.data_dir.display(),
        output_dir = %output_dir.display(),
        warmup_bars = cli.warmup_bars,
        git_commit = git_commit.as_str(),
        "CLI args"
    );

    // ── Load config ──
    let cfg_file = match ConfigFile::load(&cli.config) {
        Ok(c) => {
            info!("config loaded successfully");
            c
        }
        Err(e) => {
            error!("failed to load config: {e}");
            std::process::exit(1);
        }
    };

    let trading_mode = cfg_file.mode.clone();
    let run_single = trading_mode == "single" || trading_mode == "both";
    let run_pairs = trading_mode == "pairs" || trading_mode == "both";
    info!(mode = trading_mode.as_str(), run_single, run_pairs, "trading mode");

    let pairs_trading_config = cfg_file.pairs_trading.clone();
    let notional_per_leg = pairs_trading_config.notional_per_leg;
    let data_config = cfg_file.data.clone();

    info!(
        buy_z = format!("{:.2}", cfg_file.signal.buy_z_threshold).as_str(),
        sell_z = format!("{:.2}", cfg_file.signal.sell_z_threshold).as_str(),
        max_position = format!("{:.0}", cfg_file.risk.max_position_notional).as_str(),
        max_daily_loss = format!("{:.0}", cfg_file.risk.max_daily_loss).as_str(),
        combiner_enabled = cfg_file.combiner.enabled,
        "single-symbol engine config"
    );

    let engine_config = cfg_file.into_engine_config();

    // ── Initialize engines ──
    let mut engine = Engine::new(engine_config);
    engine.set_warmup_mode(true);
    info!("single-symbol engine initialized (warmup mode)");

    let active_pairs_path = cli.data_dir.join("active_pairs.json");
    let history_path = cli.data_dir.join("pair_trading_history.json");

    let mut pairs_engine = if active_pairs_path.exists() {
        info!(path = %active_pairs_path.display(), "loading active pairs");
        PairsEngine::from_active_pairs(
            &active_pairs_path,
            &history_path,
            vec![],
            pairs_trading_config,
        )
    } else {
        warn!("no active_pairs.json found — running single-symbol engine only");
        PairsEngine::new(vec![], pairs_trading_config)
    };

    info!(
        pairs = pairs_engine.pair_count(),
        "engines initialized"
    );
    info!("========== STARTUP COMPLETE ==========");

    // ── Load bars ──
    info!(
        timezone_offset = data_config.timezone_offset_hours,
        market_open = data_config.market_open.as_str(),
        market_close = data_config.market_close.as_str(),
        "market hours config"
    );

    let all_bars = match bars::load_days(&cli.data_dir, &data_config) {
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
    let mut pnl_tracker = pnl::PairPnlTracker::new(3.0); // 3 bps per leg = 12 bps round trip
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

        // Single-symbol engine (skip in pairs-only mode)
        if run_single {
            let single_intents = engine.on_bar(bar);
            for intent in &single_intents {
                all_intents.push(OrderIntentRecord::from_engine_intent(intent, bar.timestamp));
                engine.on_fill(&intent.symbol, intent.side, intent.qty, bar.close);
            }
            single_intent_count += single_intents.len();
        }

        // Pairs engine (skip in single-only mode)
        let pair_intents = if run_pairs {
            pairs_engine.on_bar(&bar.symbol, bar.timestamp, bar.close)
        } else {
            vec![]
        };
        if !pair_intents.is_empty() {
            pnl_tracker.on_intents(&pair_intents, bar.timestamp);
            for intent in &pair_intents {
                all_intents.push(OrderIntentRecord::from_pair_intent(intent, bar.timestamp));
            }
            pair_intent_count += pair_intents.len();
        }

        pnl_tracker.tick_bars(bar.timestamp);
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
    let dollar_pnl = summary.total_pnl_bps * 2.0 * notional_per_leg / 10_000.0;
    let days = all_bars.iter().map(|b| b.timestamp / 86_400_000).collect::<std::collections::HashSet<_>>().len();
    let dollar_per_day = if days > 0 { dollar_pnl / days as f64 } else { 0.0 };
    info!(
        run_id = run_id.as_str(),
        total_trades = summary.total_trades,
        total_pnl_bps = format!("{:.1}", summary.total_pnl_bps).as_str(),
        dollar_pnl = format!("{:.2}", dollar_pnl).as_str(),
        dollar_per_day = format!("{:.2}", dollar_per_day).as_str(),
        trading_days = days,
        win_rate = format!("{:.1}%", summary.win_rate * 100.0).as_str(),
        avg_win_bps = format!("{:.1}", summary.avg_win_bps).as_str(),
        avg_loss_bps = format!("{:.1}", summary.avg_loss_bps).as_str(),
        "P&L summary"
    );

    info!(run_id = run_id.as_str(), "========== OPENQUANT RUN END ==========");
    info!("================================================================");
}

/// Writer that tees output to both stderr and a file.
struct TeeWriter(std::sync::Mutex<std::fs::File>);

impl Write for TeeWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        std::io::stderr().write_all(buf)?;
        self.0.lock().unwrap().write_all(buf)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        std::io::stderr().flush()?;
        self.0.lock().unwrap().flush()
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for TeeWriter {
    type Writer = TeeWriterGuard<'a>;

    fn make_writer(&'a self) -> Self::Writer {
        TeeWriterGuard(&self.0)
    }
}

struct TeeWriterGuard<'a>(&'a std::sync::Mutex<std::fs::File>);

impl<'a> Write for TeeWriterGuard<'a> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        std::io::stderr().write_all(buf)?;
        self.0.lock().unwrap().write_all(buf)?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        std::io::stderr().flush()?;
        self.0.lock().unwrap().flush()
    }
}
