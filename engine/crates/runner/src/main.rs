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
mod bar_cache;
mod bar_source;
mod basket_fits;
mod basket_journal;
mod basket_live;
mod broker;
mod clock;
mod earnings;
mod market_session;
mod pair_picker_service;
mod parquet_bar_source;
pub mod refresh;
mod replay_clock;
mod replay_report;
mod session_trigger;
mod simulated_broker;
mod stream;

use alpaca::ExecutionMode;
use clap::Parser;
use openquant_core::config::ConfigFile;
use openquant_core::pairs::engine::PairsEngine;
use pair_picker::pipeline::PipelineConfig;
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
    /// Build a frozen basket fit artifact from the current universe and parquet history.
    FreezeBasketFits(BasketFitArgs),
}

/// Asset class / strategy variant.
///
/// Pair engines (`snp500`, `metals`) share the `PairsEngine` pipeline and
/// are driven by `--config` + `--candidates`. The `basket` engine runs
/// `BasketEngine` instead and is driven by `--universe` + `--fit-artifact`;
/// its `--config`/`--candidates`/`--pipeline` flags are ignored.
///
/// Usage:
///   openquant-runner paper --engine basket --execution paper
///   openquant-runner paper --engine snp500
///   openquant-runner replay --engine metals --start 2025-07-01 --end 2026-03-28
#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum Engine {
    /// S&P 500 equities — ADF cointegration, GICS sector pairs.
    Snp500,
    /// Metals — curated structurally-similar pairs, lab pipeline (structural gates relaxed).
    Metals,
    /// Basket spread strategy — OU/Bertram symmetric state machine.
    /// Defaults to `config/basket_universe_v1.toml`; override with `--universe`.
    Basket,
    // Future: Bitcoin, etc.
}

impl Engine {
    /// Default basket universe TOML. `--universe` overrides.
    /// Only meaningful for `Basket`; pair engines ignore this.
    fn universe_path(&self) -> Option<&'static str> {
        match self {
            Engine::Snp500 | Engine::Metals => None,
            Engine::Basket => Some("config/basket_universe_v1.toml"),
        }
    }

    fn is_basket(&self) -> bool {
        matches!(self, Engine::Basket)
    }
}

/// Shared args for live and paper (both use WebSocket streaming).
#[derive(clap::Args, Debug, Clone)]
struct StreamArgs {
    /// Asset class / strategy variant.
    /// Pair engines (snp500, metals) use --config/--candidates;
    /// basket uses --universe/--fit-artifact.
    #[arg(long, value_enum)]
    engine: Engine,

    /// Override config file (pair engines only; basket ignores this).
    #[arg(long)]
    config: Option<PathBuf>,

    #[arg(long, default_value = "data")]
    data_dir: PathBuf,

    #[arg(long, default_value = "pairs")]
    trading_dir: PathBuf,

    /// Override pair candidates JSON (default: selected by --engine).
    #[arg(long)]
    candidates: Option<PathBuf>,

    /// Override pipeline profile (default: selected by --engine).
    #[arg(long)]
    pipeline: Option<String>,

    /// Basket universe TOML file. Defaults to `config/basket_universe_v1.toml` when --engine basket.
    #[arg(long)]
    universe: Option<PathBuf>,

    /// Directory containing per-symbol 1-min parquets. Required when --engine basket.
    /// Defaults to $QUANT_DATA_DIR if set, else `~/quant-data/bars/v3_sp500_2024-2026_1min_adjusted`.
    #[arg(long)]
    bars_dir: Option<PathBuf>,

    /// Execution mode for --engine basket.
    ///   noop:  log intents only, no orders placed (default, shadow mode)
    ///   paper: paper Alpaca account
    ///   live:  real-money Alpaca (explicit opt-in required)
    #[arg(long, default_value = "noop")]
    execution: String,

    /// Frozen basket fit artifact. Defaults to `<universe>.fits.json`.
    #[arg(long)]
    fit_artifact: Option<PathBuf>,

    /// Persisted basket engine state. Defaults to `<fit-artifact>.state.json`.
    #[arg(long)]
    state_path: Option<PathBuf>,

    /// Starting capital for basket paper/live sizing (basket only).
    #[arg(long, default_value_t = 10_000.0)]
    capital: f64,

    /// Max active baskets for basket paper/live sizing (basket only).
    #[arg(long, default_value_t = 15)]
    n_active_baskets: usize,

    /// Override the durable basket paper/live journal path.
    /// Defaults to `<data-dir>/journal/basket_live.sqlite3`.
    #[arg(long)]
    basket_journal_path: Option<PathBuf>,
}

/// Args for replay (adds date range).
#[derive(clap::Args, Debug, Clone)]
struct ReplayArgs {
    /// Asset class / strategy variant.
    /// Pair engines (snp500, metals) use --config/--candidates;
    /// basket uses --universe/--fit-artifact.
    #[arg(long, value_enum)]
    engine: Engine,

    /// Override config file (pair engines only; basket ignores this).
    #[arg(long)]
    config: Option<PathBuf>,

    #[arg(long, default_value = "data")]
    data_dir: PathBuf,

    #[arg(long, default_value = "pairs")]
    trading_dir: PathBuf,

    /// Override pair candidates JSON (default: selected by --engine).
    #[arg(long)]
    candidates: Option<PathBuf>,

    /// Override pipeline profile (default: selected by --engine).
    #[arg(long)]
    pipeline: Option<String>,

    /// Replay start date (YYYY-MM-DD).
    #[arg(long)]
    start: String,

    /// Replay end date (YYYY-MM-DD).
    #[arg(long)]
    end: String,

    /// Bar cache directory. When set, bars are read from cache and fetched
    /// bars are written to cache for future runs.
    #[arg(long)]
    bar_cache: Option<PathBuf>,

    /// Basket universe TOML file. Defaults to `config/basket_universe_v1.toml` when --engine basket.
    #[arg(long)]
    universe: Option<PathBuf>,

    /// Directory containing per-symbol 1-min parquets. Required when --engine basket.
    /// Defaults to $QUANT_DATA_DIR if set, else /Users/$USER/quant-data/bars/v3_sp500_2024-2026_1min_adjusted
    #[arg(long)]
    bars_dir: Option<PathBuf>,

    /// Persisted basket engine state (basket only). Defaults to an isolated
    /// path under `<data-dir>/replay/<universe-stem>.state.json` so replay
    /// never reads or writes the live default `.state.json`.
    #[arg(long)]
    state_path: Option<PathBuf>,

    /// Starting capital for the simulated broker (basket only, default 10000).
    #[arg(long, default_value_t = 10_000.0)]
    capital: f64,

    /// Max active baskets (basket only, default 5).
    #[arg(long, default_value_t = 5)]
    n_active_baskets: usize,

    /// One-sided fill slippage in basis points (basket only, default 0).
    #[arg(long, default_value_t = 0.0)]
    slippage_bps: f64,

    /// Per-order probability of simulated rejection (0.0..=1.0, default 0).
    /// Exercises the `error!("ORDER FAILED")` path in basket_live.
    #[arg(long, default_value_t = 0.0)]
    reject_rate: f64,

    /// Per-order probability of partial fill in [0.6, 0.9] of requested
    /// qty (0.0..=1.0, default 0). Drives BROKER DIVERGENCE on the
    /// post-submit reconciliation path.
    #[arg(long, default_value_t = 0.0)]
    partial_fill_rate: f64,

    /// Per-`get_positions` probability of returning the previous
    /// snapshot instead of the current one (0.0..=1.0, default 0).
    #[arg(long, default_value_t = 0.0)]
    stale_position_rate: f64,

    /// Deterministic seed for failure-injection RNG (default 0).
    #[arg(long, default_value_t = 0)]
    failure_seed: u64,

    /// Write a replay report TSV (summary stats + per-day equity / P&L)
    /// to the given path after replay completes.
    #[arg(long)]
    report_tsv: Option<PathBuf>,

    /// Resume replay from an existing state snapshot at `--state-path`
    /// instead of starting from empty engine + simulated broker state.
    /// Default: false (fresh start). When false, any existing state
    /// file at the resolved `state_path` is deleted before the replay
    /// runs so replay results are deterministic across re-runs.
    #[arg(long, default_value_t = false)]
    resume_state: bool,
}

#[derive(clap::Args, Debug, Clone)]
struct BasketFitArgs {
    /// Basket universe TOML file. Defaults to `config/basket_universe_v1.toml`.
    #[arg(long)]
    universe: Option<PathBuf>,

    #[arg(long, default_value = "data")]
    data_dir: PathBuf,

    /// Directory containing per-symbol 1-min parquets.
    #[arg(long)]
    bars_dir: Option<PathBuf>,

    /// Output fit artifact path. Defaults to `<universe>.fits.json`.
    #[arg(long)]
    out: Option<PathBuf>,

    /// Build the fit using only data strictly before this date (YYYY-MM-DD).
    /// Lets us reproduce the walk-forward fit a replay computes at its --start.
    #[arg(long)]
    as_of: Option<String>,
}

/// Extract the unique symbols (leg_a/leg_b) from a candidates JSON file.
///
/// Used to narrow the refresh pass in live/paper mode — the engine only reads bars
/// for symbols that appear in the candidate pairs, so refreshing the rest is waste.
/// Returns `None` on read/parse failure so the caller can fall back to a full refresh.
fn load_symbols_from_candidates(path: &std::path::Path) -> Option<Vec<String>> {
    #[derive(serde::Deserialize)]
    struct Pair {
        leg_a: String,
        leg_b: String,
    }
    #[derive(serde::Deserialize)]
    struct File {
        pairs: Vec<Pair>,
    }
    let content = std::fs::read_to_string(path).ok()?;
    let file: File = serde_json::from_str(&content).ok()?;
    let mut symbols: Vec<String> = file
        .pairs
        .into_iter()
        .flat_map(|p| [p.leg_a, p.leg_b])
        .collect();
    symbols.sort();
    symbols.dedup();
    Some(symbols)
}

/// Resolve engine-specific defaults for config, candidates, and pipeline.
/// Explicit CLI flags always override engine defaults.
///
/// Only called for pair-based engines (snp500, metals); `Basket` is
/// short-circuited earlier in `main()` and never reaches this path.
fn resolve_engine(
    engine: Engine,
    config: Option<PathBuf>,
    candidates: Option<PathBuf>,
    pipeline: Option<String>,
) -> (PathBuf, Option<PathBuf>, String) {
    let (default_config, default_candidates) = match engine {
        Engine::Snp500 => ("config/pairs.toml", None),
        Engine::Metals => ("config/metals.toml", Some("pairs/metals_pairs.json")),
        Engine::Basket => unreachable!(
            "resolve_engine should never be called for --engine basket; \
             basket paths short-circuit to run_basket_live / run_basket_replay_live_path"
        ),
    };
    let config = config.unwrap_or_else(|| PathBuf::from(default_config));
    let candidates = candidates.or_else(|| default_candidates.map(PathBuf::from));
    // Every engine uses the "lab" pipeline today. The strict default pipeline
    // rejects 100% of lab candidates and is not used in production.
    let pipeline = pipeline.unwrap_or_else(|| "lab".to_string());
    (config, candidates, pipeline)
}

/// What the runner does after the engine emits intents.
enum RunMode {
    /// WebSocket bars + place orders on Alpaca.
    Stream(ExecutionMode),
    /// Historical REST bars + log only.
    Replay {
        start: String,
        end: String,
        bar_cache: Option<PathBuf>,
    },
}

// ── Main ─────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    // Extract common fields for tracing init
    let data_dir = match &cli.command {
        Command::Live(a) | Command::Paper(a) => &a.data_dir,
        Command::Replay(a) => &a.data_dir,
        Command::FreezeBasketFits(a) => &a.data_dir,
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

    // Basket engine in Live/Paper mode takes a dedicated path — it uses
    // BasketEngine (continuous state machine) driven by 1-min bars, not the
    // PairsEngine pipeline below. Early-return after the basket runner finishes.
    if let Command::Live(a) | Command::Paper(a) = &cli.command {
        if a.engine.is_basket() {
            run_basket_stream(a.clone(), matches!(&cli.command, Command::Live(_))).await;
            return;
        }
    }

    if let Command::FreezeBasketFits(a) = &cli.command {
        run_freeze_basket_fits(a.clone());
        return;
    }

    // Convert CLI command → (config, trading_dir, data_dir, candidates, pipeline, run_mode)
    let (config, trading_dir, data_dir, candidates, pipeline, run_mode) = match cli.command {
        Command::Live(a) => {
            let (config, candidates, pipeline) =
                resolve_engine(a.engine, a.config, a.candidates, a.pipeline);
            (
                config,
                a.trading_dir,
                a.data_dir,
                candidates,
                pipeline,
                RunMode::Stream(ExecutionMode::Live),
            )
        }
        Command::Paper(a) => {
            let (config, candidates, pipeline) =
                resolve_engine(a.engine, a.config, a.candidates, a.pipeline);
            (
                config,
                a.trading_dir,
                a.data_dir,
                candidates,
                pipeline,
                RunMode::Stream(ExecutionMode::Paper),
            )
        }
        Command::Replay(a) => {
            if a.engine.is_basket() {
                run_basket_replay_live_path(a).await;
                return;
            }

            let (config, candidates, pipeline) =
                resolve_engine(a.engine, a.config, a.candidates, a.pipeline);
            (
                config,
                a.trading_dir,
                a.data_dir,
                candidates,
                pipeline,
                RunMode::Replay {
                    start: a.start,
                    end: a.end,
                    bar_cache: a.bar_cache,
                },
            )
        }
        Command::FreezeBasketFits(_) => unreachable!("handled before run-mode dispatch"),
    };

    run(
        config,
        trading_dir,
        data_dir,
        candidates,
        pipeline,
        run_mode,
    )
    .await;
}

// ── Basket live/paper dispatch ──────────────────────────────────────

async fn run_basket_stream(args: StreamArgs, is_live_command: bool) {
    let universe_path = args
        .universe
        .clone()
        .or_else(|| args.engine.universe_path().map(PathBuf::from))
        .unwrap_or_else(|| {
            error!("--universe is required when --engine basket");
            std::process::exit(1);
        });

    let bars_dir = args.bars_dir.unwrap_or_else(|| {
        std::env::var("QUANT_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                let home = std::env::var("HOME").unwrap_or_default();
                PathBuf::from(home).join("quant-data/bars/v3_sp500_2024-2026_1min_adjusted")
            })
    });
    let fit_artifact_path = args
        .fit_artifact
        .clone()
        .unwrap_or_else(|| basket_fits::default_fit_artifact_path(&universe_path));
    let state_path = args
        .state_path
        .clone()
        .unwrap_or_else(|| basket_fits::default_live_state_path(&fit_artifact_path));

    // Parse execution mode. Default = noop (shadow).
    // Extra safety: `paper live` must come from `Command::Live` (real-money path),
    // otherwise treat as paper.
    let execution = match args.execution.as_str() {
        "noop" => basket_live::BasketExecution::Noop,
        "paper" => basket_live::BasketExecution::Paper,
        "live" => {
            if !is_live_command {
                warn!(
                    "--execution live requested but command is 'paper'; \
                     downgrading to Paper to prevent accidental real-money orders"
                );
                basket_live::BasketExecution::Paper
            } else {
                basket_live::BasketExecution::Live
            }
        }
        other => {
            error!(requested = %other, "unknown --execution (expected noop|paper|live)");
            std::process::exit(1);
        }
    };

    let alpaca = match alpaca::AlpacaClient::from_env(&PathBuf::from(".env")) {
        Ok(c) => c,
        Err(e) => {
            error!("{e}");
            std::process::exit(1);
        }
    };

    // Refresh quant-data before the session starts so the universe's parquet
    // history stays current for any future artifact rebuilds and operator
    // diagnostics. Filter refresh to only the universe's symbols to avoid
    // touching unrelated SP500 names.
    if bars_dir.exists() {
        let filter = match basket_picker::load_universe(&universe_path) {
            Ok(u) => {
                let mut syms: Vec<String> = u
                    .sectors
                    .values()
                    .flat_map(|s| s.members.iter().cloned())
                    .collect();
                syms.sort();
                syms.dedup();
                Some(syms)
            }
            Err(e) => {
                error!(error = %e, "failed to load universe for refresh filter");
                std::process::exit(1);
            }
        };
        let target = chrono::Utc::now().format("%Y-%m-%d").to_string();
        info!(
            target = target.as_str(),
            filter_count = filter.as_ref().map(Vec::len).unwrap_or(0),
            "refreshing quant-data bars (basket universe)"
        );
        match refresh::refresh_all(&bars_dir, &target, &alpaca, filter.as_deref()).await {
            Ok(n) if n > 0 => info!(bars = n, "quant-data refreshed"),
            Ok(_) => info!("quant-data already up to date"),
            Err(e) => warn!(
                error = e.as_str(),
                "quant-data refresh failed — continuing with possibly stale data"
            ),
        }
    } else {
        warn!(path = %bars_dir.display(), "quant-data dir not found — skipping refresh");
    }

    let universe = match basket_picker::load_universe(&universe_path) {
        Ok(u) => u,
        Err(e) => {
            error!(error = %e, "failed to load basket universe");
            std::process::exit(1);
        }
    };
    let portfolio_config = basket_engine::PortfolioConfig {
        capital: args.capital,
        leverage: universe.strategy.leverage_assumed,
        n_active_baskets: args.n_active_baskets,
    };
    if let Err(e) = portfolio_config.validate() {
        error!(error = %e, "invalid basket portfolio config");
        std::process::exit(1);
    }
    let fit_artifact = match basket_fits::load_fit_artifact(&fit_artifact_path, &universe) {
        Ok(a) => a,
        Err(e) => {
            error!(
                error = %e,
                fit_artifact = %fit_artifact_path.display(),
                "failed to load frozen basket fit artifact"
            );
            error!(
                "build it first with: openquant-runner freeze-basket-fits --universe {} --bars-dir {} --out {}",
                universe_path.display(),
                bars_dir.display(),
                fit_artifact_path.display()
            );
            std::process::exit(1);
        }
    };

    let bar_source =
        bar_source::AlpacaBarSource::new(alpaca.api_key.clone(), alpaca.api_secret.clone());

    // Live/paper use wall-clock time and a 30s polling session trigger.
    // Replay (#294c-2) will swap these for bar-driven equivalents. Keep
    // the grace constant in sync with `basket_live::CLOSE_GRACE_MIN`.
    let clock = clock::SystemClock;
    let mut session_trigger = session_trigger::IntervalSessionTrigger::new(clock::SystemClock, 2);
    let basket_journal_path = args
        .basket_journal_path
        .clone()
        .unwrap_or_else(|| args.data_dir.join("journal").join("basket_live.sqlite3"));

    if let Err(e) = basket_live::run_basket_live(
        &alpaca,
        &bar_source,
        &clock,
        &mut session_trigger,
        &universe_path,
        &state_path,
        &bars_dir,
        execution,
        portfolio_config,
        &fit_artifact.fits,
        basket_live::BasketRunOptions {
            fit_artifact_path: Some(fit_artifact_path.clone()),
            journal_path: Some(basket_journal_path),
            decision_offset_minutes_before_close: universe
                .runner
                .decision_offset_minutes_before_close,
        },
    )
    .await
    {
        error!(error = %e, "basket live runner failed");
        std::process::exit(1);
    }
}

fn run_freeze_basket_fits(args: BasketFitArgs) {
    let universe_path = args
        .universe
        .unwrap_or_else(|| PathBuf::from("config/basket_universe_v1.toml"));
    let bars_dir = args.bars_dir.unwrap_or_else(|| {
        std::env::var("QUANT_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                let home = std::env::var("HOME").unwrap_or_default();
                PathBuf::from(home).join("quant-data/bars/v3_sp500_2024-2026_1min_adjusted")
            })
    });
    let out = args
        .out
        .unwrap_or_else(|| basket_fits::default_fit_artifact_path(&universe_path));

    info!(
        universe = %universe_path.display(),
        bars_dir = %bars_dir.display(),
        out = %out.display(),
        "========== FREEZE BASKET FITS =========="
    );

    let artifact = match args.as_of.as_deref() {
        Some(s) => {
            let as_of = match chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d") {
                Ok(d) => d,
                Err(e) => {
                    error!(error = %e, value = s, "invalid --as-of date");
                    std::process::exit(1);
                }
            };
            let today = chrono::Utc::now().date_naive();
            if as_of > today {
                error!(
                    as_of = %as_of, today = %today,
                    "--as-of is in the future — basket fit needs 180 cal-days of \
                     prior bars and would necessarily produce zero valid baskets"
                );
                std::process::exit(1);
            }
            match basket_fits::build_replay_fit_artifact_as_of(&universe_path, &bars_dir, as_of) {
                Ok(a) => a,
                Err(e) => {
                    error!(error = %e, "failed to build basket fit artifact (as-of)");
                    std::process::exit(1);
                }
            }
        }
        None => match basket_fits::build_live_fit_artifact(&universe_path, &bars_dir) {
            Ok(a) => a,
            Err(e) => {
                error!(error = %e, "failed to build basket fit artifact");
                std::process::exit(1);
            }
        },
    };
    let valid = artifact.fits.iter().filter(|f| f.valid).count();
    if valid == 0 {
        error!(
            total = artifact.fits.len(),
            out = %out.display(),
            "refusing to write basket fit artifact with zero valid baskets"
        );
        std::process::exit(1);
    }
    if let Err(e) = basket_fits::save_fit_artifact(&out, &artifact) {
        error!(error = %e, "failed to save basket fit artifact");
        std::process::exit(1);
    }
    info!(
        path = %out.display(),
        total = artifact.fits.len(),
        valid,
        invalid = artifact.fits.len().saturating_sub(valid),
        generated_at = artifact.generated_at.as_str(),
        "wrote frozen basket fit artifact"
    );
}

// ── Unified run function ─────────────────────────────────────────────

async fn run(
    config_path: PathBuf,
    trading_dir: PathBuf,
    data_dir: PathBuf,
    candidates: Option<PathBuf>,
    pipeline_profile: String,
    run_mode: RunMode,
) {
    // ── Log mode ──
    match &run_mode {
        RunMode::Stream(ExecutionMode::Paper) => {
            info!(config = %config_path.display(), "========== OPENQUANT PAPER MODE ==========");
        }
        RunMode::Stream(ExecutionMode::Live) => {
            info!(config = %config_path.display(), "========== OPENQUANT LIVE MODE ==========");
            warn!("LIVE MODE — real money orders will be placed");
        }
        RunMode::Replay { start, end, .. } => {
            info!(
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

    // ── Refresh quant-data (live/paper only) ──
    // Replay reads static historical parquets — refresh would fetch [today, today+1)
    // which returns zero bars for every symbol and wastes ~4 minutes of startup.
    // Live/paper needs fresh bars, but only for symbols in the active candidates file;
    // refreshing the other ~436 symbols in a 501-symbol universe is dead work.
    if matches!(run_mode, RunMode::Stream(_)) {
        let bars_dir = refresh::default_bars_dir();
        if bars_dir.exists() {
            let filter = candidates.as_deref().and_then(load_symbols_from_candidates);
            let target = chrono::Utc::now().format("%Y-%m-%d").to_string();
            info!(
                target = target.as_str(),
                filter_count = filter.as_ref().map(Vec::len).unwrap_or(0),
                "refreshing quant-data bars"
            );
            match refresh::refresh_all(&bars_dir, &target, &alpaca, filter.as_deref()).await {
                Ok(n) if n > 0 => info!(bars = n, "quant-data refreshed"),
                Ok(_) => info!("quant-data already up to date"),
                Err(e) => warn!(
                    error = e.as_str(),
                    "quant-data refresh failed — continuing with stale data"
                ),
            }
        } else {
            warn!(path = %bars_dir.display(), "quant-data dir not found — skipping refresh");
        }
    } else {
        info!("replay mode: skipping quant-data refresh (parquets are static)");
    }

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

    // ── Resolve pipeline config ──
    let mut pipeline_cfg = match pipeline_profile.as_str() {
        "metals" => {
            info!("using METALS pipeline thresholds");
            PipelineConfig::metals()
        }
        "lab" => {
            info!("using LAB pipeline — structural gates relaxed, scoring + ranking active");
            PipelineConfig::force()
        }
        "default" | "" => PipelineConfig::default(),
        other => {
            error!(profile = other, "unknown pipeline profile");
            std::process::exit(1);
        }
    };
    // Overlay the preserve_input_order flag from TOML [pair_picker]. Defaults
    // to false, so existing profiles are unchanged unless the user opts in.
    pipeline_cfg.preserve_input_order = cfg_file.pair_picker.preserve_input_order;
    if pipeline_cfg.preserve_input_order {
        info!("pair-picker preserve_input_order=true (quant-lab rank preserved)");
    }

    // ── Initialize pairs engine ──
    let mut ptc = cfg_file.pairs_trading.clone();
    ptc.tz_offset_hours = cfg_file.data.timezone_offset_hours;

    let history_path = trading_dir.join("pair_trading_history.json");

    // Pairs file: monthly_pairs_YYYYMM.json (lab provides one per month).
    // Replay derives YYYYMM from the start date; live/paper uses the current month.
    let pairs_path = {
        let yyyymm = match &run_mode {
            RunMode::Replay { start, .. } => start[..7].replace('-', ""),
            RunMode::Stream(_) => chrono::Utc::now().format("%Y%m").to_string(),
        };
        trading_dir.join(format!("monthly_pairs_{yyyymm}.json"))
    };
    let picker_top_k = cfg_file.pair_picker.top_k;
    info!(top_k = picker_top_k, "pair-picker top_k from config");
    let mut pairs_engine = match &run_mode {
        RunMode::Replay { start, .. } => {
            // Always generate pairs from pair-picker (no stale pairs file).
            // Uses only data available before the replay start date (no look-ahead).
            let price_end =
                chrono::NaiveDate::parse_from_str(start, "%Y-%m-%d").expect("invalid start date");
            info!(
                price_end = %price_end,
                "replay: generating fresh pairs from pair-picker"
            );
            let active_pairs = match pair_picker_service::generate_pairs_with_config(
                &alpaca,
                &trading_dir,
                price_end,
                picker_top_k,
                candidates.as_deref(),
                &pipeline_cfg,
            )
            .await
            {
                Ok(p) => p,
                Err(e) => {
                    error!("pair-picker failed: {e}");
                    std::process::exit(1);
                }
            };
            // Write validated pairs so reload() works during weekly regen.
            if let Err(e) = pair_picker_service::write_active_pairs(&active_pairs, &pairs_path) {
                warn!(error = %e, path = %pairs_path.display(), "failed to write pairs file");
            }
            let configs = pair_picker_service::to_pair_configs(&active_pairs);
            let mut engine = PairsEngine::from_configs(configs, &history_path, ptc);
            engine.set_pairs_path(pairs_path.clone());
            engine
        }
        RunMode::Stream(_) => {
            if !pairs_path.exists() {
                error!(
                    path = %pairs_path.display(),
                    "no pairs file found — place a monthly_pairs_YYYYMM.json from quant-lab in pairs/"
                );
                std::process::exit(1);
            }
            info!(path = %pairs_path.display(), "loading pairs from file");
            PairsEngine::from_active_pairs(&pairs_path, &history_path, vec![], ptc, false)
        }
    };

    // ── Collect symbols ──
    let symbols: Vec<String> = pairs_engine
        .positions()
        .iter()
        .flat_map(|(cfg, _)| vec![cfg.leg_a.clone(), cfg.leg_b.clone()])
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();

    info!(
        pairs = pairs_engine.pair_count(),
        symbols = symbols.len(),
        "engine ready"
    );

    // ── Warmup: fetch daily bars, feed engine, flatten ──
    // Daily bars warm up the rolling stats (which only accept daily-close observations).
    // After warmup, flatten phantom positions — we don't want to hold positions that
    // weren't placed on Alpaca. Rolling stats remain warm.
    let lookback = cfg_file.pairs_trading.lookback + 10;
    info!(lookback, "fetching daily bars for warmup");

    // For replay: only feed warmup bars BEFORE the replay start date.
    // Otherwise the engine sees daily-close bars from the replay period during warmup,
    // which contaminates rolling stats with future data.
    // Cutoff: exclude daily bars for the replay start date and after.
    // Daily bars are adjusted +16h from midnight ET, so a bar for March 23
    // has ts at ~March 23 20:00 UTC. Cutoff at start_date 12:00 UTC ensures:
    //   March 22 bar (ts ~22 20:00 UTC) < March 23 12:00 UTC → included ✓
    //   March 23 bar (ts ~23 20:00 UTC) > March 23 12:00 UTC → excluded ✓
    // Exclude today's daily bar from warmup so the first live bar triggers is_new_day.
    // For replay: exclude bars on or after the replay start date.
    // For live/paper: exclude today's bar (use today at noon UTC as cutoff).
    let warmup_cutoff_ms: i64 = match &run_mode {
        RunMode::Replay { start, .. } => {
            let d =
                chrono::NaiveDate::parse_from_str(start, "%Y-%m-%d").expect("invalid start date");
            d.and_hms_opt(12, 0, 0)
                .unwrap()
                .and_utc()
                .timestamp_millis()
        }
        RunMode::Stream(_) => {
            // Today at noon UTC — excludes today's adjusted daily bar (~20:00 UTC)
            // but includes yesterday's (~yesterday 20:00 UTC).
            chrono::Utc::now()
                .date_naive()
                .and_hms_opt(12, 0, 0)
                .unwrap()
                .and_utc()
                .timestamp_millis()
        }
    };

    match alpaca.fetch_daily_bars(&symbols, lookback + 5).await {
        Ok(bars) => {
            let bars: Vec<_> = bars
                .into_iter()
                .filter(|(_, ts, _)| *ts < warmup_cutoff_ms)
                .collect();
            info!(bars = bars.len(), "warmup: feeding historical bars");
            for (symbol, timestamp, close) in &bars {
                let _intents = pairs_engine.on_bar(symbol, *timestamp, *close);
            }
            info!("warmup complete — flattening phantom positions");
            pairs_engine.flatten_all();
        }
        Err(e) => {
            error!("warmup fetch failed: {e}");
            std::process::exit(1);
        }
    }

    // ── Reconcile with Alpaca positions (live/paper only) ──
    // On restart, the engine is flat but Alpaca may have open positions from
    // a previous session. Restore them so stop losses and exits work correctly.
    if let RunMode::Stream(execution) = &run_mode {
        match alpaca.get_positions(*execution).await {
            Ok(positions) if !positions.is_empty() => {
                pairs_engine.reconcile_positions(&positions);
            }
            Ok(_) => info!("no Alpaca positions to reconcile"),
            Err(e) => warn!("position reconciliation failed: {e}"),
        }
    }

    // ── Bar loop — diverges only here ──
    match run_mode {
        RunMode::Stream(execution) => {
            run_stream(&alpaca, &mut pairs_engine, &symbols, execution).await;
        }
        RunMode::Replay {
            start,
            end,
            bar_cache: cache_dir,
        } => {
            let cache = cache_dir.map(bar_cache::BarCache::new);
            let ctx = ReplayContext {
                trading_dir: &trading_dir,
                cache: cache.as_ref(),
                candidates: candidates.as_deref(),
                pipeline_cfg: &pipeline_cfg,
                picker_top_k,
            };
            run_replay_bars(&alpaca, &mut pairs_engine, symbols, &start, &end, &ctx).await;
        }
    }
}

// ── Stream: WebSocket bars → engine → execute ────────────────────────

async fn run_stream(
    alpaca: &alpaca::AlpacaClient,
    engine: &mut PairsEngine,
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
                let intents = engine.on_bar(&bar.symbol, bar.timestamp, bar.close);
                for intent in &intents {
                    let side = format!("{:?}", intent.side).to_lowercase();
                    log_intent(intent, &side);

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

            _ = &mut ctrl_c => {
                info!("========== SHUTDOWN ==========");
                break;
            }
        }
    }
}

// ── Replay: REST minute bars → engine → log only ─────────────────────
//
// Fetches one day at a time from Alpaca REST, then feeds bars grouped by
// timestamp (one minute at a time). Within each minute, bars arrive in
// whatever order Alpaca returned them — no artificial sort. The engine
// never sees beyond the current minute.

/// Context for replay that doesn't change between days.
struct ReplayContext<'a> {
    trading_dir: &'a std::path::Path,
    cache: Option<&'a bar_cache::BarCache>,
    candidates: Option<&'a std::path::Path>,
    pipeline_cfg: &'a PipelineConfig,
    picker_top_k: usize,
}

#[allow(clippy::too_many_arguments)]
async fn run_replay_bars(
    alpaca: &alpaca::AlpacaClient,
    engine: &mut PairsEngine,
    mut symbols: Vec<String>,
    start: &str,
    end: &str,
    ctx: &ReplayContext<'_>,
) {
    info!(start, end, "starting replay");

    let start_date = chrono::NaiveDate::parse_from_str(start, "%Y-%m-%d")
        .expect("invalid start date (expected YYYY-MM-DD)");
    let end_date = chrono::NaiveDate::parse_from_str(end, "%Y-%m-%d")
        .expect("invalid end date (expected YYYY-MM-DD)");

    let mut total_bars: usize = 0;
    let mut total_intents: usize = 0;
    let mut last_picker_run = start_date;

    // Load earnings calendar for blackout filtering
    let earnings_calendar =
        earnings::load_earnings_calendar(&ctx.trading_dir.join("earnings_calendar.json"));
    info!(
        symbols = earnings_calendar.len(),
        "loaded earnings calendar"
    );

    // Fetch one day at a time — API efficiency without holding the full range
    let mut day = start_date;
    while day <= end_date {
        // Apply earnings blackouts for this day
        let day_ts = day
            .and_hms_opt(12, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp_millis();
        earnings::apply_blackouts(engine, &earnings_calendar, day_ts);

        let day_start = day.format("%Y-%m-%d").to_string();
        let day_end = (day + chrono::Duration::days(1))
            .format("%Y-%m-%d")
            .to_string();

        // Fetch bars — from cache if available, else from Alpaca API
        let bars = if let Some(c) = ctx.cache {
            let (mut cached_bars, uncached_syms) = c.read_day(&symbols, &day_start);
            if !uncached_syms.is_empty() {
                // Fetch only uncached symbols from API
                match alpaca
                    .fetch_minute_bars_raw(&uncached_syms, &day_start, &day_end)
                    .await
                {
                    Ok(raw) => {
                        // Write raw bars to cache
                        c.write_day(&raw, &day_start);
                        // Convert to (symbol, ts, close) and merge
                        const MINUTE_MS: i64 = 60_000;
                        for (symbol, bars) in &raw {
                            for bar in bars {
                                if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(&bar.t) {
                                    let close_ts = dt.timestamp_millis() + MINUTE_MS;
                                    cached_bars.push((symbol.clone(), close_ts, bar.c));
                                }
                            }
                        }
                    }
                    Err(e) => {
                        warn!(
                            day = day_start.as_str(),
                            error = e.as_str(),
                            "fetch failed for uncached symbols — using cached only"
                        );
                    }
                }
            }
            cached_bars
        } else {
            match alpaca
                .fetch_minute_bars(&symbols, &day_start, &day_end)
                .await
            {
                Ok(b) => b,
                Err(e) => {
                    warn!(
                        day = day_start.as_str(),
                        error = e.as_str(),
                        "fetch failed — skipping day"
                    );
                    day += chrono::Duration::days(1);
                    continue;
                }
            }
        };

        if bars.is_empty() {
            day += chrono::Duration::days(1);
            continue;
        }

        // Sort by (minute, symbol) for deterministic replay.
        // Alpaca's return order is undocumented and varies between requests.
        let mut bars = bars;
        bars.sort_by(|a, b| {
            let ma = a.1 / 60_000;
            let mb = b.1 / 60_000;
            ma.cmp(&mb).then(a.0.cmp(&b.0))
        });

        // Group bars by minute (truncate to 60s boundary).
        // IEX bars for different symbols may differ by sub-second timestamps.
        // Without truncation, pair legs land in separate groups and never match.
        let truncate = |ts: i64| ts / 60_000 * 60_000;
        let mut minute_group: Vec<&(String, i64, f64)> = Vec::new();
        let mut current_minute: i64 = truncate(bars[0].1);

        for bar in &bars {
            if truncate(bar.1) != current_minute {
                // New minute — feed the previous group
                for (symbol, timestamp, close) in &minute_group {
                    let intents = engine.on_bar(symbol, *timestamp, *close);
                    for intent in &intents {
                        let side = format!("{:?}", intent.side).to_lowercase();
                        log_intent(intent, &side);
                        total_intents += 1;
                    }
                }
                minute_group.clear();
                current_minute = truncate(bar.1);
            }
            minute_group.push(bar);
        }
        // Feed the last minute group
        for (symbol, timestamp, close) in &minute_group {
            let intents = engine.on_bar(symbol, *timestamp, *close);
            for intent in &intents {
                let side = format!("{:?}", intent.side).to_lowercase();
                log_intent(intent, &side);
                total_intents += 1;
            }
        }

        total_bars += bars.len();
        info!(day = day_start.as_str(), bars = bars.len(), "replayed day");

        // Weekly pair regeneration: re-run pair-picker every 7 days
        // using only data available at that point (no look-ahead).
        //
        // Weekly pair regeneration: re-run pair-picker every 7 days
        // using only data available at that point (no look-ahead).
        // Writes monthly_pairs file, then engine.reload() merges new pairs
        // in while preserving open positions.
        if (day - last_picker_run).num_days() >= 7 {
            info!(day = day_start.as_str(), "regenerating pairs (weekly)");
            match pair_picker_service::generate_pairs_with_config(
                alpaca,
                ctx.trading_dir,
                day,
                ctx.picker_top_k,
                ctx.candidates,
                ctx.pipeline_cfg,
            )
            .await
            {
                Ok(active_pairs) => {
                    let yyyymm = day.format("%Y%m").to_string();
                    let pairs_file = ctx.trading_dir.join(format!("monthly_pairs_{yyyymm}.json"));
                    if let Err(e) =
                        pair_picker_service::write_active_pairs(&active_pairs, &pairs_file)
                    {
                        warn!(error = e.as_str(), "failed to write monthly pairs file");
                    } else {
                        engine.set_pairs_path(pairs_file);
                        let old_count = engine.pair_count();
                        let old_open = engine.open_position_count();
                        engine.reload();

                        // Update symbols list — new pairs may have different symbols.
                        symbols = engine
                            .positions()
                            .iter()
                            .flat_map(|(c, _)| vec![c.leg_a.clone(), c.leg_b.clone()])
                            .collect::<std::collections::HashSet<_>>()
                            .into_iter()
                            .collect();

                        info!(
                            old_pairs = old_count,
                            new_pairs = engine.pair_count(),
                            preserved_open = old_open,
                            symbols = symbols.len(),
                            "pair universe refreshed (positions preserved)"
                        );
                    }
                }
                Err(e) => {
                    warn!(
                        error = e.as_str(),
                        "pair regeneration failed — keeping current pairs"
                    );
                }
            }
            last_picker_run = day;
        }

        day += chrono::Duration::days(1);
    }

    info!(
        total_bars,
        total_intents, "========== REPLAY END =========="
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

// ── Basket replay (live code path) ───────────────────────────────────
//
// `replay --engine basket` runs through the *exact same* `run_basket_live`
// function used by paper/live. The diff is the impls passed in:
//
//   - `Broker`        → `SimulatedBroker` (in-process fills + cash, no Alpaca)
//   - `BarSource`     → `ParquetBarSource` (per-symbol parquet walk)
//   - `Clock`         → `BarDrivenClock`   (bar-OPEN of latest emitted bar)
//   - `SessionTrigger`→ `BarDrivenSessionTrigger` (mpsc, fires once per date)
//
// State path defaults to an isolated `<data-dir>/replay/<universe-stem>.state.json`
// so replay never reads or writes the live default `.state.json`. There is
// no `if replay {}` branch anywhere in `basket_live.rs` — the seam is
// entirely at the call site.

async fn run_basket_replay_live_path(args: ReplayArgs) {
    let universe_path = args
        .universe
        .clone()
        .or_else(|| args.engine.universe_path().map(PathBuf::from))
        .unwrap_or_else(|| {
            error!("--universe is required when --engine basket");
            std::process::exit(1);
        });

    let bars_dir = args.bars_dir.clone().unwrap_or_else(|| {
        std::env::var("QUANT_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                let home = std::env::var("HOME").unwrap_or_default();
                PathBuf::from(home).join("quant-data/bars/v3_sp500_2024-2026_1min_adjusted")
            })
    });

    // Replay state isolation: never touch the live default state path.
    let state_path = args.state_path.clone().unwrap_or_else(|| {
        let stem = universe_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("basket_universe");
        args.data_dir
            .join("replay")
            .join(format!("{stem}.state.json"))
    });
    // `BasketEngine::save_state_with_day` calls `fs::write(path, ...)`
    // which fails if the parent directory does not exist. Ensure it
    // does up front so the first session-close persistence in a clean
    // checkout doesn't blow up mid-replay (#302 review finding).
    if let Some(parent) = state_path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            error!(
                error = %e,
                path = %parent.display(),
                "failed to create replay state directory"
            );
            std::process::exit(1);
        }
    }

    // Replay freshness contract: by default, every replay run starts
    // from empty engine + simulated broker state, regardless of whether
    // a prior replay's snapshot exists at `state_path`. This makes
    // replay results deterministic across re-runs. To resume from a
    // snapshot (e.g., for debugging mid-replay state), pass
    // `--resume-state`.
    if !args.resume_state && state_path.exists() {
        info!(
            path = %state_path.display(),
            "removing prior replay state for fresh start (use --resume-state to keep)"
        );
        if let Err(e) = std::fs::remove_file(&state_path) {
            error!(
                error = %e,
                path = %state_path.display(),
                "failed to remove prior replay state file"
            );
            std::process::exit(1);
        }
    }

    // Parse start/end.
    let parse_date = |s: &str, name: &str| match chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        Ok(d) => d,
        Err(e) => {
            error!(error = %e, value = %s, "invalid --{name} date (expected YYYY-MM-DD)");
            std::process::exit(1);
        }
    };
    let start = parse_date(&args.start, "start");
    let end = parse_date(&args.end, "end");
    if end < start {
        error!(start = %start, end = %end, "--end must be on or after --start");
        std::process::exit(1);
    }

    let universe = match basket_picker::load_universe(&universe_path) {
        Ok(u) => u,
        Err(e) => {
            error!(error = %e, "failed to load basket universe");
            std::process::exit(1);
        }
    };

    // Leverage comes from the universe TOML (`strategy.leverage_assumed`).
    // Replay must use the same leverage the strategy was designed against;
    // that's a config value, not a CLI knob.
    let portfolio_config = basket_engine::PortfolioConfig {
        capital: args.capital,
        leverage: universe.strategy.leverage_assumed,
        n_active_baskets: args.n_active_baskets,
    };
    if let Err(e) = portfolio_config.validate() {
        error!(error = %e, "invalid portfolio config");
        std::process::exit(1);
    }

    // Walk-forward fit: build the basket fit using data STRICTLY BEFORE
    // the replay window (`--start`). Without this, replay leaks future
    // information into the fit and produces fantasy Sharpe numbers
    // (#306 finding: a fit that overlapped the replay window by ~40%
    // produced Sharpe 10.66; with strict OOS data the same window
    // dropped to a realistic single-digit number).
    info!(
        as_of = %start,
        residual_window_days = universe.strategy.residual_window_days,
        "building replay fit from data strictly before --start"
    );
    let fit_artifact =
        match basket_fits::build_replay_fit_artifact_as_of(&universe_path, &bars_dir, start) {
            Ok(a) => a,
            Err(e) => {
                error!(error = %e, "failed to build replay fit artifact");
                std::process::exit(1);
            }
        };

    let symbols: Vec<String> = {
        let mut s: Vec<String> = universe
            .sectors
            .values()
            .flat_map(|sec| sec.members.iter().cloned())
            .collect();
        s.sort();
        s.dedup();
        s
    };

    info!(
        universe = %universe_path.display(),
        bars_dir = %bars_dir.display(),
        state_path = %state_path.display(),
        start = %start,
        end = %end,
        capital = portfolio_config.capital,
        leverage = portfolio_config.leverage,
        n_active_baskets = portfolio_config.n_active_baskets,
        slippage_bps = args.slippage_bps,
        symbols = symbols.len(),
        "========== BASKET REPLAY (LIVE PATH) =========="
    );

    let broker_config = simulated_broker::SimulatedBrokerConfig {
        slippage_bps: args.slippage_bps,
        reject_rate: args.reject_rate,
        partial_fill_rate: args.partial_fill_rate,
        stale_position_rate: args.stale_position_rate,
        seed: args.failure_seed,
    };
    let decision_offset_min = universe.runner.decision_offset_minutes_before_close;
    info!(
        decision_offset_min,
        "decision snapshot offset (issue #321) — `[runner].decision_offset_minutes_before_close`"
    );
    let replay = parquet_bar_source::new_replay_components(
        bars_dir.clone(),
        start,
        end,
        &portfolio_config,
        broker_config,
        decision_offset_min,
    );
    let parquet_bar_source::ReplayComponents {
        bar_source,
        broker,
        clock,
        mut session_trigger,
    } = replay;

    // Replay always uses Paper execution mode so the Broker contract
    // (preflight account check, position seeding, post-submit
    // reconciliation) runs identically to live. The execution flag
    // never reaches Alpaca because the broker IS our SimulatedBroker.
    let execution = basket_live::BasketExecution::Paper;

    let result = basket_live::run_basket_live(
        &broker,
        &bar_source,
        &clock,
        &mut session_trigger,
        &universe_path,
        &state_path,
        &bars_dir,
        execution,
        portfolio_config,
        &fit_artifact.fits,
        basket_live::BasketRunOptions {
            fit_artifact_path: None,
            journal_path: None,
            decision_offset_minutes_before_close: decision_offset_min,
        },
    )
    .await;

    // If the run failed partway through we have a partial daily-equity
    // history that doesn't represent the requested window. Don't
    // overwrite an existing report with that — exit non-zero and let
    // the operator inspect logs. The post-mortem snapshot is still
    // useful for debugging, so log it before exiting.
    if let Err(e) = result {
        let snap = broker.final_snapshot();
        error!(
            error = %e,
            initial_capital = snap.initial_capital,
            final_cash = %format_args!("{:.2}", snap.cash),
            final_equity = %format_args!("{:.2}", snap.equity),
            positions = snap.positions.len(),
            "basket replay failed; report TSV not written"
        );
        std::process::exit(1);
    }

    let snap = broker.final_snapshot();
    info!(
        initial_capital = snap.initial_capital,
        final_cash = %format_args!("{:.2}", snap.cash),
        final_equity = %format_args!("{:.2}", snap.equity),
        final_pnl = %format_args!("{:.2}", snap.equity - snap.initial_capital),
        positions = snap.positions.len(),
        "REPLAY FINAL SNAPSHOT"
    );

    // Replay report: stats from the daily-equity time series the
    // SimulatedBroker built up via record_eod hooks. Pure
    // mark-to-market on simulated fills. Only runs on a successful
    // replay — see the early-exit above.
    let daily_equity = broker.daily_equity();
    if let Some(stats) = replay_report::compute_stats(&daily_equity) {
        info!(
            cum_return = %format_args!("{:+.4}", stats.cum_return),
            sharpe = %format_args!("{:.3}", stats.sharpe),
            max_dd = %format_args!("{:+.4}", stats.max_drawdown),
            n_days = stats.n_days,
            "REPLAY PORTFOLIO STATS"
        );

        if let Some(tsv_path) = args.report_tsv.as_ref() {
            if let Some(parent) = tsv_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            match replay_report::write_tsv(tsv_path, &stats, &daily_equity) {
                Ok(()) => info!(path = %tsv_path.display(), "wrote replay report TSV"),
                Err(e) => error!(error = %e, "failed to write replay report TSV"),
            }
        }
    } else if args.report_tsv.is_some() {
        warn!(
            n_days = daily_equity.len(),
            "fewer than 2 daily-equity points — skipping replay report TSV"
        );
    }
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
