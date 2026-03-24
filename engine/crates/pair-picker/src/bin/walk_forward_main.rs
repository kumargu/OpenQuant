//! Walk-forward pair selection validation — standalone binary.
//!
//! Reads a price file and candidate pairs, runs walk-forward validation,
//! and prints a comparison table of in-sample vs out-of-sample performance.
//!
//! ## Usage
//!
//! ```text
//! walk-forward --prices data/pair_picker_prices.json \
//!              --candidates trading/pair_candidates.json \
//!              [--formation-days 90] \
//!              [--trading-days 21] \
//!              [--entry-zscore 2.0] \
//!              [--exit-zscore 0.5] \
//!              [--max-active-pairs 5] \
//!              [--capital-per-leg 10000] \
//!              [--max-hold-days 10]
//! ```
//!
//! ## Output
//!
//! Comparison table per window:
//! - N pairs selected in formation window
//! - N pairs actually traded (capped by max_active_pairs)
//! - In-sample Sharpe (formation window — expected to look good, this is the bias)
//! - Out-of-sample Sharpe (trading window — the true test)
//! - P&L in USD, win rate
//!
//! Summary: aggregate IS vs OOS Sharpe, total P&L, IS→OOS decay.

use pair_picker::pipeline::InMemoryPrices;
use pair_picker::types::{PairCandidate, PairCandidatesFile};
use pair_picker::walk_forward::{print_comparison_table, run_walk_forward, WalkForwardConfig};
use std::collections::HashMap;
use std::path::PathBuf;
use tracing::{error, info};

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args: Vec<String> = std::env::args().collect();

    // ── Parse arguments ──
    let prices_path = arg_value(&args, "--prices")
        .map(PathBuf::from)
        .unwrap_or_else(|| find_file("data/pair_picker_prices.json"));

    let candidates_path = arg_value(&args, "--candidates")
        .map(PathBuf::from)
        .unwrap_or_else(|| find_file("trading/pair_candidates.json"));

    let config = WalkForwardConfig {
        formation_days: arg_usize(&args, "--formation-days").unwrap_or(90),
        trading_days: arg_usize(&args, "--trading-days").unwrap_or(21),
        entry_zscore: arg_f64(&args, "--entry-zscore").unwrap_or(2.0),
        exit_zscore: arg_f64(&args, "--exit-zscore").unwrap_or(0.5),
        max_active_pairs: arg_usize(&args, "--max-active-pairs").unwrap_or(5),
        capital_per_leg_usd: arg_f64(&args, "--capital-per-leg").unwrap_or(10_000.0),
        max_hold_days: arg_usize(&args, "--max-hold-days").unwrap_or(10),
    };

    info!("Walk-forward validation");
    info!("  prices:          {}", prices_path.display());
    info!("  candidates:      {}", candidates_path.display());
    info!("  formation_days:  {}", config.formation_days);
    info!("  trading_days:    {}", config.trading_days);
    info!("  entry_zscore:    {}", config.entry_zscore);
    info!("  exit_zscore:     {}", config.exit_zscore);
    info!("  max_active_pairs:{}", config.max_active_pairs);
    info!("  capital_per_leg: {}", config.capital_per_leg_usd);
    info!("  max_hold_days:   {}", config.max_hold_days);

    // ── Load prices ──
    if !prices_path.exists() {
        error!(
            "Price file not found: {}. Run: python -m paper_trading.fetch_pair_prices",
            prices_path.display()
        );
        std::process::exit(1);
    }

    let prices = match load_prices(&prices_path) {
        Ok(p) => {
            info!("Loaded prices for {} symbols", p.data.len());
            p
        }
        Err(e) => {
            error!("Failed to load prices: {e}");
            std::process::exit(1);
        }
    };

    // Check data coverage
    let bars_available: Vec<usize> = prices.data.values().map(|v| v.len()).collect();
    if bars_available.is_empty() {
        error!("No price data found in {}", prices_path.display());
        std::process::exit(1);
    }
    let min_bars = *bars_available.iter().min().unwrap();
    let max_bars = *bars_available.iter().max().unwrap();
    info!(
        "Price history: {}-{} bars per symbol ({} symbols)",
        min_bars,
        max_bars,
        bars_available.len()
    );

    let min_needed = config.formation_days + config.trading_days;
    if min_bars < min_needed {
        error!(
            "Insufficient data: need {} bars (formation {} + trading {}), have {}",
            min_needed, config.formation_days, config.trading_days, min_bars
        );
        std::process::exit(1);
    }

    let expected_windows = (min_bars - config.formation_days) / config.trading_days;
    info!(
        "Expected windows: {} (need >= 3 for meaningful walk-forward)",
        expected_windows
    );
    if expected_windows < 3 {
        eprintln!(
            "WARNING: Only {} window(s) available. Walk-forward validation requires >= 3 \
             windows for meaningful results. Consider fetching more historical data.",
            expected_windows
        );
    }

    // ── Load candidates ──
    if !candidates_path.exists() {
        error!("Candidates file not found: {}", candidates_path.display());
        std::process::exit(1);
    }

    let candidates = match load_candidates(&candidates_path) {
        Ok(c) => {
            info!("Loaded {} candidate pairs", c.len());
            c
        }
        Err(e) => {
            error!("Failed to load candidates: {e}");
            std::process::exit(1);
        }
    };

    // ── Run walk-forward ──
    match run_walk_forward(&candidates, &prices, &config) {
        Some(summary) => {
            print_comparison_table(&summary);
            // Exit code: 0 if OOS Sharpe > 0 (positive alpha), 1 if not
            if summary.avg_oos_sharpe <= 0.0 {
                std::process::exit(1);
            }
        }
        None => {
            error!("Walk-forward validation could not run — check data and config");
            std::process::exit(1);
        }
    }
}

fn load_prices(path: &std::path::Path) -> Result<InMemoryPrices, Box<dyn std::error::Error>> {
    let contents = std::fs::read_to_string(path)?;
    let data: HashMap<String, Vec<f64>> = serde_json::from_str(&contents)?;
    Ok(InMemoryPrices { data })
}

fn load_candidates(
    path: &std::path::Path,
) -> Result<Vec<PairCandidate>, Box<dyn std::error::Error>> {
    let contents = std::fs::read_to_string(path)?;
    let file: PairCandidatesFile = serde_json::from_str(&contents)?;
    Ok(file.pairs)
}

fn arg_value<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .map(|s| s.as_str())
}

fn arg_usize(args: &[String], flag: &str) -> Option<usize> {
    arg_value(args, flag)?.parse().ok()
}

fn arg_f64(args: &[String], flag: &str) -> Option<f64> {
    arg_value(args, flag)?.parse().ok()
}

/// Find a file by walking up from CWD.
fn find_file(relative: &str) -> PathBuf {
    let mut dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    for _ in 0..5 {
        let candidate = dir.join(relative);
        if candidate.exists() {
            return candidate;
        }
        if !dir.pop() {
            break;
        }
    }
    // Return the path relative to CWD as a fallback
    PathBuf::from(relative)
}
