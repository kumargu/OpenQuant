//! Pair Picker CLI — daily statistical validation of pairs trading candidates.
//!
//! Reads `pair_candidates.json`, validates each pair, writes `active_pairs.json`.
//! Uses a lock file to ensure at most one run per day.
//!
//! Integration pipeline:
//! 1. Load candidates from pair_candidates.json
//! 2. Filter through relationship graph (only screen connected pairs)
//! 3. Validate each pair statistically (ADF, half-life, beta stability, regime)
//! 4. Rank validated pairs via Thompson sampling (informed by trade history)
//! 5. Write active_pairs.json for PairsEngine consumption

use pair_picker::graph::RelationshipGraph;
use pair_picker::lockfile;
use pair_picker::pipeline::{self, InMemoryPrices, PipelineError};
use pair_picker::regime::regime_adjusted_prior;
use pair_picker::thompson::{pair_id, ThompsonState, TradeHistory};
use pair_picker::types::{PairCandidate, PairCandidatesFile};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tracing::{error, info, warn};

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args: Vec<String> = std::env::args().collect();

    let data_dir = args
        .iter()
        .position(|a| a == "--data-dir")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from)
        .unwrap_or_else(find_data_dir);

    // --check mode: just report whether today's run is done
    if args.iter().any(|a| a == "--check") {
        if lockfile::has_run_today(&data_dir) {
            info!("Pair picker has already run today");
            std::process::exit(0);
        } else {
            info!("Pair picker has NOT run today");
            std::process::exit(1);
        }
    }

    // --force: skip lock file check
    let force = args.iter().any(|a| a == "--force");

    if !force && lockfile::has_run_today(&data_dir) {
        info!("Pair picker already ran today. Use --force to re-run.");
        return;
    }

    let candidates_path = args
        .iter()
        .position(|a| a == "--candidates")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from)
        .unwrap_or_else(|| data_dir.join("pair_candidates.json"));

    let output_path = args
        .iter()
        .position(|a| a == "--output")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from)
        .unwrap_or_else(|| data_dir.join("active_pairs.json"));

    info!("Pair Picker starting");
    info!("  data_dir:   {}", data_dir.display());
    info!("  candidates: {}", candidates_path.display());
    info!("  output:     {}", output_path.display());

    // ── Step 1: Load candidates ──
    let contents = match std::fs::read_to_string(&candidates_path) {
        Ok(c) => c,
        Err(e) => {
            error!("Failed to read candidates: {e}");
            std::process::exit(1);
        }
    };

    let candidates: PairCandidatesFile = match serde_json::from_str(&contents) {
        Ok(c) => c,
        Err(e) => {
            error!("Failed to parse candidates JSON: {e}");
            std::process::exit(1);
        }
    };

    info!("Loaded {} candidate pairs", candidates.pairs.len());

    // ── Step 2: Filter through relationship graph ──
    let graph_path = data_dir.join("stock_relationships.json");
    let filtered_candidates = if let Some(graph) = RelationshipGraph::load(&graph_path) {
        let before = candidates.pairs.len();
        let filtered: Vec<PairCandidate> = candidates
            .pairs
            .into_iter()
            .filter(|c| graph.are_connected(&c.leg_a, &c.leg_b))
            .collect();
        let after = filtered.len();
        info!(
            "Graph filter: {before} candidates → {after} graph-connected ({} filtered out)",
            before - after
        );
        filtered
    } else {
        warn!("No relationship graph found — screening all candidates (no graph filter)");
        candidates.pairs
    };

    // ── Step 3: Load price data ──
    let price_file = data_dir.join("pair_picker_prices.json");
    let provider = if price_file.exists() {
        match load_prices_from_file(&price_file) {
            Ok(p) => {
                info!("Loaded prices for {} symbols", p.data.len());
                p
            }
            Err(e) => {
                error!("Failed to load prices: {e}");
                std::process::exit(1);
            }
        }
    } else {
        warn!(
            "No price data at {}. Run: python -m paper_trading.fetch_pair_prices",
            price_file.display()
        );
        InMemoryPrices {
            data: HashMap::new(),
        }
    };

    // ── Step 4: Validate each pair statistically ──
    match pipeline::run_pipeline_from_candidates(&filtered_candidates, &output_path, &provider) {
        Ok(results) => {
            let passed: Vec<_> = results.iter().filter(|r| r.passed).collect();
            let rejected = results.len() - passed.len();
            info!(
                "Pipeline complete: {} passed, {} rejected",
                passed.len(),
                rejected
            );

            // ── Step 5: Thompson sampling ranking ──
            let mut thompson = ThompsonState::load(&data_dir);

            // Load trade history for feedback
            let history_path = data_dir.join("pair_trading_history.json");
            let history = TradeHistory::load(&history_path);
            let returns_by_pair = history.returns_by_pair();

            for result in &passed {
                let pid = pair_id(&result.leg_a, &result.leg_b);

                // Use regime-adjusted prior: penalize fragile pairs
                let base_score = result.score;
                let regime_robustness = result.regime_robustness.unwrap_or(-1.0);
                let adjusted_score = regime_adjusted_prior(base_score, regime_robustness);

                thompson.get_or_create(&pid, adjusted_score);

                // Feed trade history if available
                if let Some(returns) = returns_by_pair.get(&pid) {
                    thompson.update_pair(&pid, returns, adjusted_score);
                }

                info!(
                    pair = pid.as_str(),
                    score = format!("{:.3}", result.score).as_str(),
                    regime_robustness = format!("{:.2}", regime_robustness).as_str(),
                    adjusted_prior = format!("{:.3}", adjusted_score).as_str(),
                    "Thompson arm updated"
                );
            }

            // Rank by Thompson sampling
            let top_k = args
                .iter()
                .position(|a| a == "--top-k")
                .and_then(|i| args.get(i + 1))
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or(10);

            let seed = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            let ranking = thompson.rank_pairs(seed);

            info!("Thompson ranking (top {top_k}):");
            for (i, (pid, sampled_value)) in ranking.iter().take(top_k).enumerate() {
                let arm = thompson.arms.get(pid).unwrap();
                info!(
                    "  #{}: {} — sampled={:.2}, posterior_mu={:.2}, n_trades={}",
                    i + 1,
                    pid,
                    sampled_value,
                    arm.posterior_mean(),
                    arm.n_trades
                );
            }

            // Save Thompson state
            if let Err(e) = thompson.save(&data_dir) {
                error!("Failed to save Thompson state: {e}");
            }

            info!(
                "Thompson: {} arms, exploration_rate={:.0}%",
                thompson.arms.len(),
                thompson.exploration_rate() * 100.0
            );

            // Create lock file
            if let Err(e) = lockfile::create_lock(&data_dir) {
                error!("Failed to create lock file: {e}");
            }

            // Cleanup old locks
            match lockfile::cleanup_old_locks(&data_dir) {
                Ok(n) if n > 0 => info!("Cleaned up {n} old lock files"),
                _ => {}
            }
        }
        Err(e) => {
            error!("Pipeline failed: {e}");
            std::process::exit(1);
        }
    }
}

fn find_data_dir() -> PathBuf {
    // Walk up from CWD looking for a data/ directory that contains
    // pair_candidates.json (the expected input file). This avoids
    // false matches like engine/data/ (journal storage) when running
    // from the engine/ subdirectory.
    let mut dir = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    for _ in 0..5 {
        let data = dir.join("data");
        if data.join("pair_candidates.json").exists()
            || data.join("stock_relationships.json").exists()
        {
            return data;
        }
        if !dir.pop() {
            break;
        }
    }
    // Fallback: use CWD/data
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("data")
}

/// Load price data from a JSON file.
/// Format: { "SYMBOL": [close1, close2, ...], ... }
fn load_prices_from_file(path: &Path) -> Result<InMemoryPrices, PipelineError> {
    let contents = std::fs::read_to_string(path).map_err(PipelineError::Io)?;
    let data: HashMap<String, Vec<f64>> =
        serde_json::from_str(&contents).map_err(PipelineError::Json)?;
    Ok(InMemoryPrices { data })
}
