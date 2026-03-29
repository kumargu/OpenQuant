//! openquant-runner — trading engine binary.
//!
//! Three commands, one engine, one `on_bar()` code path:
//!
//! - `live`   — WebSocket bars, real Alpaca orders
//! - `paper`  — WebSocket bars, paper Alpaca orders
//! - `replay` — historical REST bars, no orders (engine doesn't know)
//!
//! # Usage
//!
//! ```bash
//! openquant-runner live --engine pairs
//! openquant-runner paper --engine pairs
//! openquant-runner replay --engine pairs --start 2026-03-01 --end 2026-03-28
//! ```

mod alpaca;
mod stream;

use alpaca::ExecutionMode;
use clap::Parser;
use openquant_core::config::ConfigFile;
use openquant_core::pairs::engine::PairsEngine;
use std::io::Write;
use std::path::PathBuf;
use tracing::{error, info, warn};

// ── CLI ──────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
#[command(name = "openquant-runner", about = "OpenQuant trading engine")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(clap::Subcommand, Debug)]
enum Command {
    /// Live trading — WebSocket bars, real Alpaca orders.
    Live(StreamArgs),
    /// Paper trading — WebSocket bars, paper Alpaca orders.
    Paper(StreamArgs),
    /// Replay — historical minute bars from Alpaca REST, no orders.
    /// The engine processes bars identically to live; it doesn't know it's replaying.
    Replay(ReplayArgs),
}

/// Shared args for live and paper (both use WebSocket streaming).
#[derive(clap::Args, Debug, Clone)]
struct StreamArgs {
    #[arg(long, value_enum, default_value = "pairs")]
    engine: EngineMode,

    #[arg(long)]
    config: Option<PathBuf>,

    #[arg(long, default_value = "data")]
    data_dir: PathBuf,

    #[arg(long, default_value = "trading")]
    trading_dir: PathBuf,
}

/// Args for replay (adds date range, no execution flag).
#[derive(clap::Args, Debug, Clone)]
struct ReplayArgs {
    #[arg(long, value_enum, default_value = "pairs")]
    engine: EngineMode,

    #[arg(long)]
    config: Option<PathBuf>,

    #[arg(long, default_value = "data")]
    data_dir: PathBuf,

    #[arg(long, default_value = "trading")]
    trading_dir: PathBuf,

    /// Replay start date (YYYY-MM-DD).
    #[arg(long)]
    start: String,

    /// Replay end date (YYYY-MM-DD).
    #[arg(long)]
    end: String,
}

#[derive(clap::ValueEnum, Debug, Clone, Copy, PartialEq)]
enum EngineMode {
    Single,
    Pairs,
    Both,
}

impl EngineMode {
    fn run_pairs(self) -> bool {
        matches!(self, Self::Pairs | Self::Both)
    }

    fn default_config(self) -> &'static str {
        match self {
            Self::Single => "config/single.toml",
            Self::Pairs | Self::Both => "config/pairs.toml",
        }
    }
}

/// What the runner does after the engine emits intents.
enum RunMode {
    /// WebSocket bars + place orders on Alpaca.
    Stream(ExecutionMode),
    /// Historical REST bars + log only.
    Replay { start: String, end: String },
}

// ── Main ─────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    // Extract common fields for tracing init
    let data_dir = match &cli.command {
        Command::Live(a) | Command::Paper(a) => &a.data_dir,
        Command::Replay(a) => &a.data_dir,
    };

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

    // Convert CLI command → (engine_mode, config, trading_dir, data_dir, run_mode)
    let (engine_mode, config, trading_dir, data_dir, run_mode) = match cli.command {
        Command::Live(a) => (
            a.engine,
            a.config,
            a.trading_dir,
            a.data_dir,
            RunMode::Stream(ExecutionMode::Live),
        ),
        Command::Paper(a) => (
            a.engine,
            a.config,
            a.trading_dir,
            a.data_dir,
            RunMode::Stream(ExecutionMode::Paper),
        ),
        Command::Replay(a) => (
            a.engine,
            a.config,
            a.trading_dir,
            a.data_dir,
            RunMode::Replay {
                start: a.start,
                end: a.end,
            },
        ),
    };

    run(engine_mode, config, trading_dir, data_dir, run_mode).await;
}

// ── Unified run function ─────────────────────────────────────────────

async fn run(
    engine_mode: EngineMode,
    config: Option<PathBuf>,
    trading_dir: PathBuf,
    data_dir: PathBuf,
    run_mode: RunMode,
) {
    let config_path = config.unwrap_or_else(|| PathBuf::from(engine_mode.default_config()));

    // ── Log mode ──
    match &run_mode {
        RunMode::Stream(ExecutionMode::Paper) => {
            info!(?engine_mode, config = %config_path.display(), "========== OPENQUANT PAPER MODE ==========");
        }
        RunMode::Stream(ExecutionMode::Live) => {
            info!(?engine_mode, config = %config_path.display(), "========== OPENQUANT LIVE MODE ==========");
            warn!("LIVE MODE — real money orders will be placed");
        }
        RunMode::Replay { start, end } => {
            info!(
                ?engine_mode,
                config = %config_path.display(),
                start = start.as_str(),
                end = end.as_str(),
                "========== OPENQUANT REPLAY MODE =========="
            );
        }
    }

    // ── Load Alpaca client (all modes need it — data API or trading API) ──
    let alpaca = match alpaca::AlpacaClient::from_env(&PathBuf::from(".env")) {
        Ok(c) => c,
        Err(e) => {
            error!("{e}");
            std::process::exit(1);
        }
    };

    // ── Load config ──
    let cfg_file = match ConfigFile::load(&config_path) {
        Ok(c) => c,
        Err(e) => {
            error!("failed to load config: {e}");
            std::process::exit(1);
        }
    };

    let trading_dir = if trading_dir.exists() {
        trading_dir
    } else {
        data_dir.clone()
    };

    // ── Initialize pairs engine ──
    let mut pairs_engine = if engine_mode.run_pairs() {
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

    // ── Collect symbols ──
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
        "engine ready"
    );

    // ── Warmup: fetch daily bars, feed engine, flatten ──
    let lookback = cfg_file.pairs_trading.lookback + 10;
    info!(lookback, "fetching daily bars for warmup");

    match alpaca.fetch_daily_bars(&symbols, lookback + 5).await {
        Ok(bars) => {
            info!(bars = bars.len(), "warmup: feeding historical bars");
            for (symbol, timestamp, close) in &bars {
                if let Some(ref mut pe) = pairs_engine {
                    let _intents = pe.on_bar(symbol, *timestamp, *close);
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

    // ── Bar loop — diverges only here ──
    match run_mode {
        RunMode::Stream(execution) => {
            run_stream(&alpaca, &mut pairs_engine, &symbols, execution).await;
        }
        RunMode::Replay { start, end } => {
            run_replay_bars(&alpaca, &mut pairs_engine, &symbols, &start, &end).await;
        }
    }
}

// ── Stream: WebSocket bars → engine → execute ────────────────────────

async fn run_stream(
    alpaca: &alpaca::AlpacaClient,
    pairs_engine: &mut Option<PairsEngine>,
    symbols: &[String],
    execution: ExecutionMode,
) {
    info!("starting Alpaca real-time bar stream");

    let mut bar_rx = stream::start_bar_stream(&alpaca.api_key, &alpaca.api_secret, symbols).await;

    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);

    info!("waiting for bars (Ctrl+C to stop)");

    loop {
        tokio::select! {
            Some(bar) = bar_rx.recv() => {
                if let Some(ref mut pe) = pairs_engine {
                    let intents = pe.on_bar(&bar.symbol, bar.timestamp, bar.close);
                    for intent in &intents {
                        let side = format!("{:?}", intent.side).to_lowercase();
                        log_intent(intent, &side);

                        // Paper and Live both call Alpaca; URL differs via trading_url()
                        match alpaca
                            .place_order(&intent.symbol, intent.qty, &side, execution)
                            .await
                        {
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

            _ = &mut ctrl_c => {
                info!("========== SHUTDOWN ==========");
                break;
            }
        }
    }
}

// ── Replay: REST minute bars → engine → log only ─────────────────────

async fn run_replay_bars(
    alpaca: &alpaca::AlpacaClient,
    pairs_engine: &mut Option<PairsEngine>,
    symbols: &[String],
    start: &str,
    end: &str,
) {
    info!(start, end, "fetching minute bars for replay");

    let bars = match alpaca.fetch_minute_bars(symbols, start, end).await {
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

    info!(bars = bars.len(), "starting replay");
    let mut intent_count: usize = 0;

    for (symbol, timestamp, close) in &bars {
        if let Some(ref mut pe) = pairs_engine {
            let intents = pe.on_bar(symbol, *timestamp, *close);
            for intent in &intents {
                let side = format!("{:?}", intent.side).to_lowercase();
                log_intent(intent, &side);
                intent_count += 1;
            }
        }
    }

    info!(
        bars = bars.len(),
        intents = intent_count,
        "========== REPLAY END =========="
    );
}

// ── Shared intent logger (identical format for all modes) ────────────

fn log_intent(intent: &openquant_core::pairs::PairOrderIntent, side: &str) {
    info!(
        symbol = intent.symbol.as_str(),
        side,
        qty = intent.qty,
        pair_id = intent.pair_id.as_str(),
        z = %format_args!("{:.2}", intent.z_score),
        priority = %format_args!("{:.1}", intent.priority_score),
        reason = ?intent.reason,
        "INTENT"
    );
}

// ── TeeWriter (stderr + file) ────────────────────────────────────────

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
