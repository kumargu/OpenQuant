//! Parameter sensitivity analysis — standalone binary.
//!
//! Sweeps each strategy parameter independently, holding all others at their
//! defaults. Uses the walk-forward framework as the evaluation harness, so
//! every evaluation is automatically out-of-sample.
//!
//! ## Usage
//!
//! ```text
//! sensitivity --prices data/pair_picker_prices.json \
//!             --candidates trading/pair_candidates.json \
//!             [--formation-days 90] \
//!             [--trading-days 21] \
//!             [--entry-zscore 2.0] \
//!             [--exit-zscore 0.5] \
//!             [--max-active-pairs 5] \
//!             [--capital-per-leg 10000] \
//!             [--max-hold-days 10]
//! ```
//!
//! ## Output
//!
//! Markdown table suitable for inclusion in a PR description:
//! - Per-parameter sweep: OOS Sharpe, win rate, P&L, number of trades
//! - Cliff edge flags: sharp performance drops (> 50%) at parameter boundaries
//! - Overall assessment: robust edge vs narrow-band overfit
//!
//! Exit codes:
//! - 0: analysis completed and the default configuration is profitable (OOS Sharpe > 0)
//! - 1: default configuration is not profitable, or analysis could not run

use pair_picker::pipeline::InMemoryPrices;
use pair_picker::sensitivity::{
    print_sensitivity_table, run_sensitivity_analysis, SensitivityConfig,
};
use pair_picker::types::{PairCandidate, PairCandidatesFile};
use pair_picker::walk_forward::WalkForwardConfig;
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

    // ── Parse arguments ──────────────────────────────────────────────────────
    let prices_path = arg_value(&args, "--prices")
        .map(PathBuf::from)
        .unwrap_or_else(|| find_file("data/pair_picker_prices.json"));

    let candidates_path = arg_value(&args, "--candidates")
        .map(PathBuf::from)
        .unwrap_or_else(|| find_file("trading/pair_candidates.json"));

    // Base walk-forward config — parameters not swept use these values
    let base_wf = WalkForwardConfig {
        formation_days: arg_usize(&args, "--formation-days").unwrap_or(90),
        trading_days: arg_usize(&args, "--trading-days").unwrap_or(21),
        entry_zscore: arg_f64(&args, "--entry-zscore").unwrap_or(2.0),
        exit_zscore: arg_f64(&args, "--exit-zscore").unwrap_or(0.5),
        max_active_pairs: arg_usize(&args, "--max-active-pairs").unwrap_or(5),
        capital_per_leg_usd: arg_f64(&args, "--capital-per-leg").unwrap_or(10_000.0),
        max_hold_days: arg_usize(&args, "--max-hold-days").unwrap_or(10),
    };

    let sensitivity_config = SensitivityConfig {
        base_walk_forward: base_wf.clone(),
        ..Default::default()
    };

    info!("Parameter sensitivity analysis");
    info!("  prices:           {}", prices_path.display());
    info!("  candidates:       {}", candidates_path.display());
    info!("  formation_days:   {}", base_wf.formation_days);
    info!("  trading_days:     {}", base_wf.trading_days);
    info!("  entry_zscore:     {}", base_wf.entry_zscore);
    info!("  exit_zscore:      {}", base_wf.exit_zscore);
    info!("  max_active_pairs: {}", base_wf.max_active_pairs);
    info!("  capital_per_leg:  {}", base_wf.capital_per_leg_usd);
    info!("  max_hold_days:    {}", base_wf.max_hold_days);

    // ── Load prices ──────────────────────────────────────────────────────────
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

    let min_needed = base_wf.formation_days + base_wf.trading_days;
    if min_bars < min_needed {
        error!(
            "Insufficient data: need {} bars (formation {} + trading {}), have {}",
            min_needed, base_wf.formation_days, base_wf.trading_days, min_bars
        );
        std::process::exit(1);
    }

    // ── Load candidates ──────────────────────────────────────────────────────
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

    // ── Run sensitivity analysis ─────────────────────────────────────────────
    info!(
        "Running sensitivity analysis ({} parameters × multiple sweep values)",
        6
    );

    match run_sensitivity_analysis(&candidates, &prices, &sensitivity_config) {
        Some(result) => {
            print_sensitivity_table(&result);

            // Determine exit code from default parameter performance.
            // Find the entry z sweep and locate the default value point.
            let default_profitable = result
                .sweeps
                .iter()
                .find(|s| s.param_name.contains("Entry"))
                .and_then(|s| {
                    s.points.iter().find(|p| {
                        (p.param_value - pair_picker::sensitivity::DEFAULT_ENTRY_Z).abs() < 1e-9
                    })
                })
                .map(|p| p.oos_sharpe > 0.0)
                .unwrap_or(false);

            if !default_profitable {
                eprintln!("Default configuration is not profitable OOS — review strategy.");
                std::process::exit(1);
            }
        }
        None => {
            error!("Sensitivity analysis could not run — check data and config");
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
    PathBuf::from(relative)
}
