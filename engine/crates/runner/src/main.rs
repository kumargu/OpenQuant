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
mod basket_runner;
mod earnings;
mod pair_picker_service;
pub mod refresh;
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
}

/// Asset class / strategy variant. Each variant defines its own config,
/// pair candidates, and pipeline defaults.
///
/// Usage:
///   openquant-runner paper --engine snp500
///   openquant-runner replay --engine metals --start 2025-07-01 --end 2026-03-28
///   openquant-runner replay --engine basket --universe config/basket_universe_v1.toml --start 2024-07-01 --end 2026-04-13
#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum Engine {
    /// S&P 500 equities — ADF cointegration, GICS sector pairs.
    Snp500,
    /// Metals — curated structurally-similar pairs, lab pipeline (structural gates relaxed).
    Metals,
    /// Basket spread strategy — OU/Bertram symmetric state machine.
    /// Requires --universe flag pointing to a basket_universe_v1 TOML file.
    Basket,
    // Future: Bitcoin, etc.
}

impl Engine {
    fn config_path(&self) -> &'static str {
        match self {
            Engine::Snp500 => "config/pairs.toml",
            Engine::Metals => "config/metals.toml",
            Engine::Basket => "config/basket.toml", // Not used; basket uses universe TOML
        }
    }

    fn candidates_path(&self) -> Option<&'static str> {
        match self {
            Engine::Snp500 => None, // candidates must be provided via --candidates flag
            Engine::Metals => Some("pairs/metals_pairs.json"),
            Engine::Basket => None, // basket uses --universe flag instead
        }
    }

    fn pipeline(&self) -> &'static str {
        // All engines use "lab" pipeline — candidates come from quant-lab,
        // structural hard gates are relaxed, scoring + ranking active.
        // The "default" pipeline (strict ADF/R²/structural-break gates)
        // rejects 100% of lab candidates and is not used in production.
        match self {
            Engine::Snp500 => "lab",
            Engine::Metals => "lab",
            Engine::Basket => "basket", // basket has its own validation
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
    /// Required. Selects config, candidates, and pipeline for the asset class.
    #[arg(long, value_enum)]
    engine: Engine,

    /// Override config file (default: selected by --engine).
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
}

/// Args for replay (adds date range).
#[derive(clap::Args, Debug, Clone)]
struct ReplayArgs {
    /// Asset class / strategy variant.
    /// Required. Selects config, candidates, and pipeline for the asset class.
    #[arg(long, value_enum)]
    engine: Engine,

    /// Override config file (default: selected by --engine).
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

    /// Basket universe TOML file. Required when --engine basket.
    #[arg(long)]
    universe: Option<PathBuf>,
}

const DEFAULT_CONFIG: &str = "config/pairs.toml";

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
fn resolve_engine(
    engine: Engine,
    config: Option<PathBuf>,
    candidates: Option<PathBuf>,
    pipeline: Option<String>,
) -> (Option<PathBuf>, Option<PathBuf>, String) {
    let config = config.or_else(|| Some(PathBuf::from(engine.config_path())));
    let candidates = candidates.or_else(|| engine.candidates_path().map(PathBuf::from));
    let pipeline = pipeline.unwrap_or_else(|| engine.pipeline().to_string());
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
            // Handle basket engine separately — different architecture
            if a.engine.is_basket() {
                let universe_path = match &a.universe {
                    Some(p) => p.clone(),
                    None => {
                        error!("--universe is required when --engine basket");
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

                let portfolio_config = basket_engine::PortfolioConfig::default();
                let cost_bps = 5.0; // TODO: read from universe TOML

                info!(
                    universe = %universe_path.display(),
                    start = a.start.as_str(),
                    end = a.end.as_str(),
                    "========== BASKET REPLAY MODE =========="
                );

                match basket_runner::run_basket_replay(
                    &alpaca,
                    &universe_path,
                    &a.start,
                    &a.end,
                    &portfolio_config,
                    cost_bps,
                )
                .await
                {
                    Ok(result) => {
                        info!(
                            total_bars = result.total_bars,
                            total_intents = result.total_intents,
                            trading_days = result.daily_pnl.len(),
                            "========== BASKET REPLAY END =========="
                        );

                        // Write P&L CSV
                        let csv_path = a.data_dir.join("basket_daily_pnl.csv");
                        if let Err(e) = basket_runner::write_pnl_csv(&result.daily_pnl, &csv_path) {
                            error!(error = %e, "failed to write P&L CSV");
                        } else {
                            info!(path = %csv_path.display(), "wrote daily P&L CSV");
                        }
                    }
                    Err(e) => {
                        error!(error = %e, "basket replay failed");
                        std::process::exit(1);
                    }
                }
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

// ── Unified run function ─────────────────────────────────────────────

async fn run(
    config: Option<PathBuf>,
    trading_dir: PathBuf,
    data_dir: PathBuf,
    candidates: Option<PathBuf>,
    pipeline_profile: String,
    run_mode: RunMode,
) {
    let config_path = config.unwrap_or_else(|| PathBuf::from(DEFAULT_CONFIG));

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
