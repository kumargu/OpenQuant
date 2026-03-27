//! openquant-runner — standalone trading engine binary.
//!
//! Processes historical 1-min bar data through the trading engine and produces
//! deterministic P&L results. Functions as both a backtester and a walk-forward
//! validator — bars are processed sequentially with rolling state, exactly as
//! they would be in live trading, just faster.
//!
//! # How it works
//!
//! 1. **Load config** — reads `config/*.toml` for trading mode (pairs/single/both),
//!    strategy parameters, market hours, timezone.
//! 2. **Load pairs** — reads `data/active_pairs.json` for pair identity (legs, beta).
//!    Trading params (entry_z, exit_z, etc.) come from TOML, not the pair JSON.
//! 3. **Load bars** — reads `data/experiment_bars_*.json`, filters pre-market/after-hours,
//!    sorts by `(timestamp, symbol)` for deterministic ordering.
//! 4. **Process bars** — feeds each bar to the engine(s) sequentially. State carries
//!    across days (spread rolling stats, positions, warmup). This is walk-forward
//!    by design — no look-ahead bias, no train/test split needed.
//! 5. **Track P&L** — matches entry/exit pair intents, applies configurable cost
//!    (3 bps/leg = 12 bps round-trip), writes `data/trade_results.json`.
//! 6. **Log everything** — appends to `data/journal/engine.log` with run IDs
//!    (`git_commit-timestamp`) for audit trail. Every ENTRY, EXIT, P&L, stop loss,
//!    and config change is logged with structured tracing fields.
//!
//! # Walk-forward validation
//!
//! The runner naturally performs walk-forward testing because:
//! - Bars are processed in chronological order (no shuffling)
//! - Spread statistics warm up from scratch on first run, carry across days
//! - No parameters are optimized during the run (all from TOML)
//! - Day boundaries trigger daily resets (risk state, VWAP)
//! - Results can be sliced into time windows post-hoc for stability analysis
//!
//! # Determinism
//!
//! Identical config + data = identical results every time. Achieved by:
//! - Sorting bars by `(timestamp, symbol)` (not HashMap iteration order)
//! - No randomness in the engine (no Monte Carlo, no random seeds)
//! - P&L tracker uses deterministic entry/exit matching
//!
//! # Usage
//!
//! ```bash
//! ./run.sh pairs              # build + run pairs trading
//! ./run.sh single             # build + run single-symbol
//! ./run.sh test               # build + run with test config
//!
//! # Or directly:
//! cargo run -p openquant-runner --release -- \
//!   --config config/pairs.toml \
//!   --data-dir data \
//!   --output-dir data \
//!   --warmup-bars 0
//! ```
//!
//! # Input/Output
//!
//! - Input:  `config/*.toml`, `data/active_pairs.json`, `data/experiment_bars_*.json`
//! - Output: `data/order_intents.json`, `data/trade_results.json`
//! - Logs:   `data/journal/engine.log` (append mode, run IDs for correlation)

mod alpaca;
mod bars;
mod intents;
mod pnl;

use clap::Parser;
use intents::{write_intents, write_trade_results, OrderIntentRecord};
use openquant_core::config::ConfigFile;
use openquant_core::engine::SingleEngine;
use openquant_core::pairs::engine::PairsEngine;
use std::io::Write;
use std::path::PathBuf;
use tracing::{error, info, warn};

#[derive(Parser, Debug)]
#[command(
    name = "openquant-runner",
    about = "OpenQuant trading engine — live, backtest, or walk-forward"
)]
struct Cli {
    /// Running mode.
    #[command(subcommand)]
    command: Command,
}

#[derive(clap::Subcommand, Debug)]
enum Command {
    /// Walk-forward — replay historical bars sequentially through the engine.
    /// Bars are processed in order with rolling state — no look-ahead bias.
    /// This is the only way to evaluate strategy performance.
    WalkForward(RunArgs),

    /// Live — streaming bars from stdin, order intents to stdout.
    /// Python feeds Alpaca bars → Rust decides → Python executes orders.
    Live(RunArgs),
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
}

fn main() {
    let cli = Cli::parse();

    // Resolve engine mode and config path from CLI args (before tracing init)
    let args = match &cli.command {
        Command::WalkForward(a) | Command::Live(a) => a,
    };

    // ── Initialize tracing ONCE — stderr + data/journal/engine.log ──
    // All tracing from Rust (PairsEngine, SingleEngine, pair-picker, runner)
    // flows here. This is the single source of logs for the entire process.
    let journal_dir = args.data_dir.join("journal");
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
        Command::WalkForward(args) => run_walkforward(args),
        Command::Live(args) => run_live(args),
    }
}

fn run_walkforward(args: RunArgs) {
    let mode = args.engine;
    let config_path = args.config.clone()
        .unwrap_or_else(|| PathBuf::from(mode.default_config()));
    let output_dir = args.output_dir.as_ref().unwrap_or(&args.data_dir);
    let trading_dir = if args.trading_dir.exists() {
        args.trading_dir.clone()
    } else {
        args.data_dir.clone()
    };
    // Tracing already initialized in main()

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
    info!(
        run_id = run_id.as_str(),
        "========== OPENQUANT RUN START =========="
    );
    info!(
        config = %config_path.display(),
        data_dir = %args.data_dir.display(),
        output_dir = %output_dir.display(),
        warmup_bars = args.warmup_bars,
        git_commit = git_commit.as_str(),
        "CLI args"
    );

    // ── Load config ──
    let cfg_file = match ConfigFile::load(&config_path) {
        Ok(c) => {
            info!("config loaded successfully");
            c
        }
        Err(e) => {
            error!("failed to load config: {e}");
            std::process::exit(1);
        }
    };

    info!(?mode, single = mode.run_single(), pairs = mode.run_pairs(), "engine mode");

    let mut pairs_trading_config = cfg_file.pairs_trading.clone();
    // Sync timezone from [data] config so pairs engine uses the same offset
    pairs_trading_config.tz_offset_hours = cfg_file.data.timezone_offset_hours;
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

    // ── Initialize engines (only what EngineMode requires) ──
    let mut single_engine = if mode.run_single() {
        let engine_config = cfg_file.into_engine_config();
        let mut e = SingleEngine::new(engine_config);
        e.set_warmup_mode(true);
        info!("single-symbol engine initialized (warmup mode)");
        Some(e)
    } else {
        None
    };

    let active_pairs_path = trading_dir.join("active_pairs.json");
    let history_path = trading_dir.join("pair_trading_history.json");

    let mut pairs_engine = if mode.run_pairs() {
        if active_pairs_path.exists() {
            info!(path = %active_pairs_path.display(), "loading active pairs");
            Some(PairsEngine::from_active_pairs(
                &active_pairs_path,
                &history_path,
                vec![],
                pairs_trading_config,
            ))
        } else {
            warn!("no active_pairs.json found — pairs engine disabled");
            None
        }
    } else {
        None
    };

    info!(
        pairs = pairs_engine.as_ref().map_or(0, |e| e.pair_count()),
        single = single_engine.is_some(),
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

    let all_bars = match bars::load_days(&args.data_dir, &data_config) {
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
        if let Some(ref mut se) = single_engine {
            if i == args.warmup_bars {
                se.set_warmup_mode(false);
                info!("warmup complete — live signal generation enabled");
            }
        }

        // Detect day boundary and reset daily state
        if let Some(prev) = prev_day {
            if bar.timestamp - prev > DAY_GAP_MS {
                if let Some(ref mut se) = single_engine { se.reset_daily(); }
                if let Some(ref mut pe) = pairs_engine { pe.reset_daily(); }
                info!(timestamp = bar.timestamp, "day boundary — reset daily state");
            }
        }
        prev_day = Some(bar.timestamp);

        // Track prices for P&L computation
        pnl_tracker.update_price(&bar.symbol, bar.close);

        // Single-symbol engine
        if let Some(ref mut se) = single_engine {
            let single_intents = se.on_bar(bar);
            for intent in &single_intents {
                all_intents.push(OrderIntentRecord::from_engine_intent(intent, bar.timestamp));
                se.on_fill(&intent.symbol, intent.side, intent.qty, bar.close);
            }
            single_intent_count += single_intents.len();
        }

        // Pairs engine
        let pair_intents = if let Some(ref mut pe) = pairs_engine {
            pe.on_bar(&bar.symbol, bar.timestamp, bar.close)
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
    let days = all_bars
        .iter()
        .map(|b| b.timestamp / 86_400_000)
        .collect::<std::collections::HashSet<_>>()
        .len();
    let dollar_per_day = if days > 0 {
        dollar_pnl / days as f64
    } else {
        0.0
    };
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

    info!(
        run_id = run_id.as_str(),
        "========== OPENQUANT RUN END =========="
    );
    info!("================================================================");
}

/// Live trading mode — fetches daily bars from Alpaca, feeds to engine, outputs intents.
///
/// Pure Rust end-to-end. No Python dependency for data or decisions.
/// Reads API keys from .env file. Outputs order intents as JSON to stdout.
/// All decisions logged via tracing to stderr + engine.log.
///
/// Usage:
///   openquant-runner live --engine pairs                    # one-shot: fetch + decide
///   openquant-runner live --engine pairs --loop 300         # re-run every 5 min
fn run_live(args: RunArgs) {
    let mode = args.engine;
    let config_path = args.config.clone()
        .unwrap_or_else(|| PathBuf::from(mode.default_config()));

    info!(?mode, config = %config_path.display(), "========== OPENQUANT LIVE MODE ==========");

    // Load .env for Alpaca API keys
    let env_path = std::path::PathBuf::from(".env");
    let env = alpaca::load_env(&env_path);
    let api_key = env.get("ALPACA_API_KEY").cloned().unwrap_or_default();
    let api_secret = env.get("ALPACA_SECRET_KEY").cloned().unwrap_or_default();
    if api_key.is_empty() || api_secret.is_empty() {
        error!("ALPACA_API_KEY or ALPACA_SECRET_KEY missing from .env");
        std::process::exit(1);
    }

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
        let mut pairs_trading_config = cfg_file.pairs_trading.clone();
        pairs_trading_config.tz_offset_hours = cfg_file.data.timezone_offset_hours;

        let active_pairs_path = trading_dir.join("active_pairs.json");
        let history_path = trading_dir.join("pair_trading_history.json");

        if active_pairs_path.exists() {
            info!(path = %active_pairs_path.display(), "loading active pairs");
            Some(PairsEngine::from_active_pairs(
                &active_pairs_path,
                &history_path,
                vec![],
                pairs_trading_config,
            ))
        } else {
            error!("no active_pairs.json found");
            std::process::exit(1);
        }
    } else {
        None
    };

    // Collect all symbols from active pairs
    let symbols: Vec<String> = if let Some(ref pe) = pairs_engine {
        pe.positions().iter()
            .flat_map(|(cfg, _)| vec![cfg.leg_a.clone(), cfg.leg_b.clone()])
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect()
    } else {
        vec![]
    };

    info!(
        ?mode,
        pairs = pairs_engine.as_ref().map_or(0, |e| e.pair_count()),
        symbols = symbols.len(),
        "live engine ready"
    );

    // Fetch historical bars for warmup + today's bar
    let lookback = cfg_file.pairs_trading.lookback + 10; // extra buffer
    info!(lookback, "fetching daily bars from Alpaca for warmup");

    let all_bars = match alpaca::fetch_daily_bars(&symbols, &api_key, &api_secret, lookback + 5) {
        Ok(bars) => bars,
        Err(e) => {
            error!("failed to fetch bars: {e}");
            std::process::exit(1);
        }
    };

    if all_bars.is_empty() {
        error!("no bars fetched — check API keys and symbol list");
        std::process::exit(1);
    }

    info!(bars = all_bars.len(), "processing bars through engine");

    // Feed all bars to the engine (warmup + current)
    let stdout = std::io::stdout();
    let mut stdout = stdout.lock();

    for (symbol, timestamp, close) in &all_bars {
        if let Some(ref mut pe) = pairs_engine {
            let intents = pe.on_bar(symbol, *timestamp, *close);
            for intent in &intents {
                let out = serde_json::json!({
                    "symbol": intent.symbol,
                    "side": format!("{:?}", intent.side).to_lowercase(),
                    "qty": intent.qty,
                    "pair_id": intent.pair_id,
                    "reason": format!("{:?}", intent.reason),
                    "z_score": intent.z_score,
                    "spread": intent.spread,
                    "priority_score": intent.priority_score,
                    "timestamp": timestamp,
                });
                let _ = writeln!(stdout, "{}", out);
                let _ = stdout.flush();
            }
        }
    }

    info!("========== OPENQUANT LIVE END ==========");
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
