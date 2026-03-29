//! openquant-runner — live trading engine binary.
//!
//! Connects to Alpaca, feeds bars to the PairsEngine, and executes order intents.
//! The engine is the single source of truth — all trading decisions happen in Rust.
//! P&L is derived from structured logs, not a separate tracking system.
//!
//! # Execution modes
//!
//! - `--execution noop`  — log intents only, no orders submitted (for replay/testing)
//! - `--execution paper` — Alpaca paper trading API (default)
//! - `--execution live`  — Alpaca live trading API (real money)
//!
//! # Usage
//!
//! ```bash
//! openquant-runner live --engine pairs                     # paper trading (default)
//! openquant-runner live --engine pairs --execution noop    # dry run
//! openquant-runner live --engine pairs --execution live    # real money
//! ```
//!
//! # Logs
//!
//! All output goes to `data/journal/engine.log` (append mode) with structured
//! tracing fields. Every INTENT, ORDER, EXIT, stop loss, and config change is
//! logged. P&L can be computed from logs post-hoc.

mod alpaca;
mod bars;
mod intents;
mod stream;

use alpaca::ExecutionMode;
use clap::Parser;
use openquant_core::config::ConfigFile;
use openquant_core::pairs::engine::PairsEngine;
use std::io::Write;
use std::path::PathBuf;
use tracing::{error, info, warn};

#[derive(Parser, Debug)]
#[command(name = "openquant-runner", about = "OpenQuant live trading engine")]
struct Cli {
    /// Running mode.
    #[command(subcommand)]
    command: Command,
}

#[derive(clap::Subcommand, Debug)]
enum Command {
    /// Live — connect to Alpaca, stream bars, execute intents.
    Live(RunArgs),
    /// Replay — fetch historical minute bars from Alpaca and feed them through
    /// the exact same engine code path as live mode. No orders placed (noop).
    /// The engine doesn't know it's replaying history.
    Replay(ReplayArgs),
}

#[derive(clap::Args, Debug, Clone)]
struct ReplayArgs {
    /// Which engine to run.
    #[arg(long, value_enum, default_value = "pairs")]
    engine: EngineMode,

    /// Path to TOML config file.
    #[arg(long)]
    config: Option<PathBuf>,

    /// Directory containing active_pairs.json.
    #[arg(long, default_value = "trading")]
    trading_dir: PathBuf,

    /// Data directory for logs.
    #[arg(long, default_value = "data")]
    data_dir: PathBuf,

    /// Replay start date (YYYY-MM-DD).
    #[arg(long)]
    start: String,

    /// Replay end date (YYYY-MM-DD).
    #[arg(long)]
    end: String,
}

/// Which engine to run — CLI flag picks the engine AND its config.
/// No ambiguity: `--engine pairs` uses config/pairs.toml, `--engine single` uses config/single.toml.
#[derive(clap::ValueEnum, Debug, Clone, Copy, PartialEq)]
enum EngineMode {
    /// Single-symbol mean-reversion engine (config/single.toml)
    Single,
    /// Pairs spread-trading engine (config/pairs.toml)
    Pairs,
    /// Run both engines simultaneously
    Both,
}

impl EngineMode {
    #[allow(dead_code)]
    fn run_single(self) -> bool {
        matches!(self, EngineMode::Single | EngineMode::Both)
    }

    fn run_pairs(self) -> bool {
        matches!(self, EngineMode::Pairs | EngineMode::Both)
    }

    /// Default config path for this engine mode.
    fn default_config(self) -> &'static str {
        match self {
            EngineMode::Single => "config/single.toml",
            EngineMode::Pairs => "config/pairs.toml",
            EngineMode::Both => "config/pairs.toml",
        }
    }
}

#[derive(clap::Args, Debug, Clone)]
struct RunArgs {
    /// Which engine to run.
    #[arg(long, value_enum, default_value = "pairs")]
    engine: EngineMode,

    /// Path to TOML config file. Defaults based on --engine flag.
    #[arg(long)]
    config: Option<PathBuf>,

    /// Directory containing experiment_bars.json (bar data).
    #[arg(long, default_value = "data")]
    data_dir: PathBuf,

    /// Directory containing active_pairs.json, pair_candidates.json (committed to git).
    #[arg(long, default_value = "trading")]
    trading_dir: PathBuf,

    /// Directory for output files. Defaults to data_dir.
    #[arg(long)]
    output_dir: Option<PathBuf>,

    /// Number of warmup bars before signal generation begins.
    #[arg(long, default_value = "0")]
    warmup_bars: usize,

    /// Execution mode: noop (log only), paper, or live.
    #[arg(long, value_enum, default_value = "paper")]
    execution: ExecutionMode,
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    // Resolve args from CLI (for tracing init we need data_dir)
    let data_dir = match &cli.command {
        Command::Live(a) => &a.data_dir,
        Command::Replay(a) => &a.data_dir,
    };

    // ── Initialize tracing ONCE — stderr + data/journal/engine.log ──
    // All tracing from Rust (PairsEngine, SingleEngine, pair-picker, runner)
    // flows here. This is the single source of logs for the entire process.
    let journal_dir = data_dir.join("journal");
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

    // ── Dispatch ──
    match cli.command {
        Command::Live(args) => run_live(args).await,
        Command::Replay(args) => run_replay(args).await,
    }
}

/// Live trading mode — async event loop with Alpaca WebSocket.
/// Execution mode controls whether orders are submitted (noop/paper/live).
async fn run_live(args: RunArgs) {
    let mode = args.engine;
    let config_path = args
        .config
        .clone()
        .unwrap_or_else(|| PathBuf::from(mode.default_config()));

    info!(?mode, config = %config_path.display(), "========== OPENQUANT LIVE MODE ==========");

    // Log execution mode
    match args.execution {
        ExecutionMode::Noop => info!("execution mode: NOOP (dry run — no orders)"),
        ExecutionMode::Paper => info!("execution mode: PAPER"),
        ExecutionMode::Live => warn!("execution mode: LIVE — real money orders will be placed"),
    }

    // Load Alpaca client from .env (needed even in noop for bar streaming)
    let alpaca = match alpaca::AlpacaClient::from_env(&PathBuf::from(".env")) {
        Ok(c) => c,
        Err(e) => {
            error!("{e}");
            std::process::exit(1);
        }
    };

    // Load config
    let cfg_file = match ConfigFile::load(&config_path) {
        Ok(c) => c,
        Err(e) => {
            error!("failed to load config: {e}");
            std::process::exit(1);
        }
    };

    let trading_dir = if args.trading_dir.exists() {
        args.trading_dir.clone()
    } else {
        args.data_dir.clone()
    };

    // Initialize pairs engine
    let mut pairs_engine = if mode.run_pairs() {
        let mut ptc = cfg_file.pairs_trading.clone();
        ptc.tz_offset_hours = cfg_file.data.timezone_offset_hours;

        let active_pairs_path = trading_dir.join("active_pairs.json");
        let history_path = trading_dir.join("pair_trading_history.json");

        if active_pairs_path.exists() {
            info!(path = %active_pairs_path.display(), "loading active pairs");
            Some(PairsEngine::from_active_pairs(
                &active_pairs_path,
                &history_path,
                vec![],
                ptc,
            ))
        } else {
            error!("no active_pairs.json found");
            std::process::exit(1);
        }
    } else {
        None
    };

    // Collect symbols
    let symbols: Vec<String> = if let Some(ref pe) = pairs_engine {
        pe.positions()
            .iter()
            .flat_map(|(cfg, _)| vec![cfg.leg_a.clone(), cfg.leg_b.clone()])
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect()
    } else {
        vec![]
    };

    info!(
        pairs = pairs_engine.as_ref().map_or(0, |e| e.pair_count()),
        symbols = symbols.len(),
        "live engine ready"
    );

    // ── Initial warmup: fetch historical bars and run through engine ──
    let lookback = cfg_file.pairs_trading.lookback + 10;
    info!(lookback, "fetching historical bars for warmup");

    match alpaca.fetch_daily_bars(&symbols, lookback + 5).await {
        Ok(bars) => {
            info!(bars = bars.len(), "warmup: feeding historical bars");
            for (symbol, timestamp, close) in &bars {
                if let Some(ref mut pe) = pairs_engine {
                    let intents = pe.on_bar(symbol, *timestamp, *close);
                    // During warmup, log intents but don't execute
                    for intent in &intents {
                        info!(
                            symbol = intent.symbol.as_str(),
                            side = ?intent.side,
                            qty = intent.qty,
                            pair_id = intent.pair_id.as_str(),
                            z = %format_args!("{:.2}", intent.z_score),
                            priority = %format_args!("{:.1}", intent.priority_score),
                            "warmup intent (not executed)"
                        );
                    }
                }
            }
            info!("warmup complete — flattening phantom positions");
            // Reset all positions opened during warmup. Rolling stats are warm,
            // but we don't want to hold positions that weren't placed on Alpaca.
            if let Some(ref mut pe) = pairs_engine {
                pe.flatten_all();
            }
        }
        Err(e) => {
            error!("warmup fetch failed: {e}");
            std::process::exit(1);
        }
    }

    // ── Start real-time bar stream from Alpaca WebSocket ──
    // The stream drives the engine — when a bar arrives, the engine evaluates.
    // No polling, no timers. Data in → decisions out.
    info!("starting Alpaca real-time bar stream");

    let mut bar_rx = stream::start_bar_stream(&alpaca.api_key, &alpaca.api_secret, &symbols).await;

    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);

    info!("live — waiting for bars (process stays alive until killed)");

    loop {
        tokio::select! {
            // New bar from Alpaca stream → feed engine → execute intents
            Some(bar) = bar_rx.recv() => {
                if let Some(ref mut pe) = pairs_engine {
                    let intents = pe.on_bar(&bar.symbol, bar.timestamp, bar.close);

                    for intent in &intents {
                        let side = format!("{:?}", intent.side).to_lowercase();
                        info!(
                            symbol = intent.symbol.as_str(),
                            side = side.as_str(),
                            qty = intent.qty,
                            pair_id = intent.pair_id.as_str(),
                            z = %format_args!("{:.2}", intent.z_score),
                            priority = %format_args!("{:.1}", intent.priority_score),
                            reason = ?intent.reason,
                            "INTENT"
                        );

                        // Execute based on mode
                        match args.execution {
                            ExecutionMode::Noop => {
                                // Intent already logged above — nothing to execute
                            }
                            ExecutionMode::Paper | ExecutionMode::Live => {
                                match alpaca.place_order(
                                    &intent.symbol, intent.qty, &side, args.execution,
                                ).await {
                                    Ok(order) => {
                                        info!(
                                            order_id = order.id.as_str(),
                                            status = order.status.as_str(),
                                            "ORDER FILLED"
                                        );
                                    }
                                    Err(e) => {
                                        error!(
                                            symbol = intent.symbol.as_str(),
                                            error = e.as_str(),
                                            "ORDER FAILED"
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // Graceful shutdown
            _ = &mut ctrl_c => {
                info!("========== OPENQUANT LIVE END (shutdown) ==========");
                break;
            }
        }
    }
}

/// Replay mode — fetch historical minute bars from Alpaca, feed through the
/// exact same engine code path as live. The engine doesn't know it's replaying.
/// Always noop execution — no orders placed.
async fn run_replay(args: ReplayArgs) {
    let mode = args.engine;
    let config_path = args
        .config
        .clone()
        .unwrap_or_else(|| PathBuf::from(mode.default_config()));

    info!(
        ?mode,
        config = %config_path.display(),
        start = args.start.as_str(),
        end = args.end.as_str(),
        "========== OPENQUANT REPLAY MODE =========="
    );
    info!("execution mode: NOOP (replay — no orders placed)");

    // Load Alpaca client (for data API only — no orders)
    let alpaca = match alpaca::AlpacaClient::from_env(&PathBuf::from(".env")) {
        Ok(c) => c,
        Err(e) => {
            error!("{e}");
            std::process::exit(1);
        }
    };

    // Load config — same as live
    let cfg_file = match ConfigFile::load(&config_path) {
        Ok(c) => c,
        Err(e) => {
            error!("failed to load config: {e}");
            std::process::exit(1);
        }
    };

    let trading_dir = if args.trading_dir.exists() {
        args.trading_dir.clone()
    } else {
        args.data_dir.clone()
    };

    // Initialize pairs engine — same as live
    let mut pairs_engine = if mode.run_pairs() {
        let mut ptc = cfg_file.pairs_trading.clone();
        ptc.tz_offset_hours = cfg_file.data.timezone_offset_hours;

        let active_pairs_path = trading_dir.join("active_pairs.json");
        let history_path = trading_dir.join("pair_trading_history.json");

        if active_pairs_path.exists() {
            info!(path = %active_pairs_path.display(), "loading active pairs");
            Some(PairsEngine::from_active_pairs(
                &active_pairs_path,
                &history_path,
                vec![],
                ptc,
            ))
        } else {
            error!("no active_pairs.json found");
            std::process::exit(1);
        }
    } else {
        None
    };

    // Collect symbols — same as live
    let symbols: Vec<String> = if let Some(ref pe) = pairs_engine {
        pe.positions()
            .iter()
            .flat_map(|(cfg, _)| vec![cfg.leg_a.clone(), cfg.leg_b.clone()])
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect()
    } else {
        vec![]
    };

    info!(
        pairs = pairs_engine.as_ref().map_or(0, |e| e.pair_count()),
        symbols = symbols.len(),
        "replay engine ready"
    );

    // ── Warmup: fetch daily bars before replay start (same as live) ──
    let lookback = cfg_file.pairs_trading.lookback + 10;
    info!(lookback, "fetching daily bars for warmup");

    match alpaca.fetch_daily_bars(&symbols, lookback + 5).await {
        Ok(bars) => {
            info!(bars = bars.len(), "warmup: feeding historical daily bars");
            for (symbol, timestamp, close) in &bars {
                if let Some(ref mut pe) = pairs_engine {
                    let _intents = pe.on_bar(symbol, *timestamp, *close);
                    // Warmup intents are discarded — same as live
                }
            }
            info!("warmup complete — flattening phantom positions");
            if let Some(ref mut pe) = pairs_engine {
                pe.flatten_all();
            }
        }
        Err(e) => {
            error!("warmup fetch failed: {e}");
            std::process::exit(1);
        }
    }

    // ── Fetch minute bars for replay period ──
    info!(
        start = args.start.as_str(),
        end = args.end.as_str(),
        "fetching minute bars for replay"
    );

    let bars = match alpaca
        .fetch_minute_bars(&symbols, &args.start, &args.end)
        .await
    {
        Ok(b) => b,
        Err(e) => {
            error!("minute bar fetch failed: {e}");
            std::process::exit(1);
        }
    };

    if bars.is_empty() {
        error!("no bars fetched — nothing to replay");
        std::process::exit(1);
    }

    // ── Replay: feed bars through engine — same on_bar() as live ──
    info!(bars = bars.len(), "starting replay");
    let mut intent_count: usize = 0;

    for (symbol, timestamp, close) in &bars {
        if let Some(ref mut pe) = pairs_engine {
            let intents = pe.on_bar(symbol, *timestamp, *close);

            for intent in &intents {
                let side = format!("{:?}", intent.side).to_lowercase();
                info!(
                    symbol = intent.symbol.as_str(),
                    side = side.as_str(),
                    qty = intent.qty,
                    pair_id = intent.pair_id.as_str(),
                    z = %format_args!("{:.2}", intent.z_score),
                    priority = %format_args!("{:.1}", intent.priority_score),
                    reason = ?intent.reason,
                    "INTENT"
                );
                intent_count += 1;
            }
        }
    }

    info!(
        bars = bars.len(),
        intents = intent_count,
        "========== OPENQUANT REPLAY END =========="
    );
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
