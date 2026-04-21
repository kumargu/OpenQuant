//! Basket-spread walk-forward replay — matches quant-lab's baseline simulation.
//!
//! For each of N configured fit_dates:
//!   1. Per basket (target, peers), fit OU on the last `residual_window` days ending at fit_date
//!   2. Compute Bertram symmetric k, clip to [k_clip_min, k_clip_max]
//!   3. Simulate `test_days` forward with `pnl_t = pos_{t-1} * (spread_t - spread_{t-1})`
//!   4. Apply per-change transaction cost: `cost_series[t] -= cost/2` when position changes
//!
//! Dedupe: for each calendar day present in multiple panels, keep LATEST fit_date's signal.
//! Portfolio: daily = mean(basket_pnl) × leverage.
//! Stats: Sharpe (mean/std × √252), cum_return ((1+r).prod()-1), max_dd from equity cummax.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::Path;

use arrow::array::{Array, Float64Array, TimestampMicrosecondArray};
use basket_picker::{fit_ou_ar1, load_universe, optimize_symmetric_thresholds, Universe};
use chrono::{NaiveDate, Timelike};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

// Quant-lab walk-forward fit_dates (matches baseline.json)
const FIT_DATES: &[&str] = &[
    "2024-06-30",
    "2024-09-30",
    "2024-12-31",
    "2025-03-31",
    "2025-06-30",
    "2025-09-30",
    "2025-12-31",
    "2026-01-31",
];

// RTH session: 13:30–20:00 UTC
const RTH_START_MIN: u32 = 13 * 60 + 30;
const RTH_END_MIN: u32 = 20 * 60;

/// Portfolio stats matching quant-lab's portfolio_stats.
#[derive(Debug, Clone, Serialize)]
pub struct PortfolioStats {
    pub cum_return: f64,
    pub ann_return: f64,
    pub sharpe: f64,
    pub max_drawdown: f64,
    pub n_days: usize,
    pub daily_mean: f64,
    pub daily_std: f64,
}

/// Per-basket-fit metrics row.
#[derive(Debug, Clone, Serialize)]
pub struct BasketRun {
    pub fit_date: String,
    pub sector: String,
    pub target: String,
    pub k: f64,
    pub n_trades: usize,
    pub cum_return: f64,
    pub sharpe: f64,
    pub n_days: usize,
}

/// Full replay result.
pub struct ReplayResult {
    pub stats: PortfolioStats,
    pub runs: Vec<BasketRun>,
    pub daily_pnl: Vec<(NaiveDate, f64)>,
}

/// Baseline JSON schema (quant-lab/statarb/autoresearch/baseline/baseline.json).
#[derive(Debug, Deserialize)]
pub struct BaselineJson {
    pub stats: BaselineStats,
}

#[derive(Debug, Deserialize)]
pub struct BaselineStats {
    pub cum_return: f64,
    pub ann_return: f64,
    pub sharpe: f64,
    pub max_drawdown: f64,
    pub n_days: usize,
    pub daily_mean: f64,
    pub daily_std: f64,
}

/// Run walk-forward basket replay from parquets.
///
/// Reads per-symbol parquets from `bars_dir`, aggregates to daily closes (last RTH bar),
/// and runs walk-forward simulation matching quant-lab.
pub fn run_basket_replay(universe_path: &Path, bars_dir: &Path) -> Result<ReplayResult, String> {
    // 1. Load universe
    let universe = load_universe(universe_path)?;
    info!(
        baskets = universe.num_baskets(),
        sectors = universe.sectors.len(),
        "loaded universe"
    );

    let residual_window = universe.strategy.residual_window_days;
    let test_days = universe.strategy.forward_window_days;
    let cost = universe.strategy.cost_bps_assumed / 10_000.0;
    let leverage = universe.strategy.leverage_assumed;
    let k_clip_min = universe.strategy.threshold_clip_min;
    let k_clip_max = universe.strategy.threshold_clip_max;

    // 2. Collect symbols
    let symbols = collect_symbols(&universe);
    info!(n = symbols.len(), "collected symbols");

    // 3. Load daily closes from parquets
    let closes = load_all_daily_closes(bars_dir, &symbols);
    info!(
        loaded = closes.len(),
        requested = symbols.len(),
        "loaded daily closes"
    );

    // 4. Walk-forward simulation
    let mut runs: Vec<BasketRun> = Vec::new();
    // panel: BTreeMap<fit_date, BTreeMap<calendar_date, HashMap<basket_key, pnl>>>
    let mut panel: BTreeMap<String, BTreeMap<NaiveDate, HashMap<String, f64>>> = BTreeMap::new();

    for fd_str in FIT_DATES {
        let fd = NaiveDate::parse_from_str(fd_str, "%Y-%m-%d")
            .map_err(|e| format!("invalid fit_date '{fd_str}': {e}"))?;
        let mut day_map: BTreeMap<NaiveDate, HashMap<String, f64>> = BTreeMap::new();

        for candidate in &universe.candidates {
            let target = &candidate.target;
            let peers = &candidate.members;

            // Align prices: only keep dates where all (target + peers) are present
            let aligned = align_prices(&closes, target, peers);
            if aligned.is_empty() {
                continue;
            }

            // Compute log-spread: log(target) - mean(log(peers))
            let spread = compute_spread(&aligned);

            // Fit window: last `residual_window` dates with date <= fd
            let fit_pos = spread.partition_point(|(d, _)| *d <= fd);
            if fit_pos < residual_window {
                continue;
            }
            let fit_slice = &spread[fit_pos - residual_window..fit_pos];
            let fit_anchor = fit_slice.last().unwrap().0;
            let fit_values: Vec<f64> = fit_slice.iter().map(|(_, v)| *v).collect();

            // Need residual_window + test_days + 5 data points total for this basket
            // (matches quant-lab's minimum-data guard)
            if spread.len() < residual_window + test_days + 5 {
                continue;
            }

            // Fit OU
            let ou = match fit_ou_ar1(&fit_values) {
                Some(o) => o,
                None => continue,
            };

            // Forward window: first test_days dates after fit_anchor
            let post_start = spread.partition_point(|(d, _)| *d <= fit_anchor);
            let post_end = (post_start + test_days).min(spread.len());
            if post_end - post_start < 5 {
                continue;
            }
            let fwd = &spread[post_start..post_end];

            // Bertram k
            let bt = match optimize_symmetric_thresholds(&ou, cost) {
                Some(b) => b,
                None => continue,
            };
            let k = bt.k.clamp(k_clip_min, k_clip_max);

            // Simulate
            let (basket_pnl, n_trades) =
                simulate_symmetric_bertram(fwd, ou.mu, ou.sigma_eq, k, cost);

            // Record basket-level stats (diagnostics only)
            let (cum, sh) = basket_stats(&basket_pnl);
            runs.push(BasketRun {
                fit_date: fd_str.to_string(),
                sector: candidate.sector.clone(),
                target: target.clone(),
                k,
                n_trades,
                cum_return: cum,
                sharpe: sh,
                n_days: basket_pnl.len(),
            });

            // Insert into panel (keyed by target)
            for (date, pnl) in &basket_pnl {
                day_map
                    .entry(*date)
                    .or_default()
                    .insert(target.clone(), *pnl);
            }
        }

        panel.insert(fd_str.to_string(), day_map);
    }

    // 5. Dedupe: ISO fit_date strings sort chronologically; later entries win per calendar day
    let mut per_day_baskets: BTreeMap<NaiveDate, HashMap<String, f64>> = BTreeMap::new();
    for day_map in panel.values() {
        for (date, baskets) in day_map {
            per_day_baskets.insert(*date, baskets.clone());
        }
    }

    // 6. Portfolio: daily = mean(basket_pnl) × leverage
    let daily_pnl: Vec<(NaiveDate, f64)> = per_day_baskets
        .iter()
        .filter(|(_, b)| !b.is_empty())
        .map(|(d, baskets)| {
            let mean = baskets.values().sum::<f64>() / baskets.len() as f64;
            (*d, mean * leverage)
        })
        .collect();

    // 7. Portfolio stats
    let returns: Vec<f64> = daily_pnl.iter().map(|(_, p)| *p).collect();
    let stats = portfolio_stats(&returns);

    info!(
        n_days = stats.n_days,
        cum_return = %format!("{:+.4}", stats.cum_return),
        sharpe = %format!("{:.4}", stats.sharpe),
        max_dd = %format!("{:+.4}", stats.max_drawdown),
        "========== RUST PORTFOLIO RESULT =========="
    );

    Ok(ReplayResult {
        stats,
        runs,
        daily_pnl,
    })
}

/// Print side-by-side comparison vs quant-lab baseline and tolerance check.
pub fn print_parity_comparison(rust: &PortfolioStats, py: &BaselineStats) -> bool {
    println!("\n=== PARITY COMPARISON (Rust vs quant-lab) ===");
    println!(
        "{:20} {:>15} {:>15} {:>15}",
        "metric", "rust", "python", "diff"
    );
    let rows = [
        ("cum_return", rust.cum_return, py.cum_return),
        ("ann_return", rust.ann_return, py.ann_return),
        ("sharpe", rust.sharpe, py.sharpe),
        ("max_drawdown", rust.max_drawdown, py.max_drawdown),
        ("n_days", rust.n_days as f64, py.n_days as f64),
        ("daily_mean", rust.daily_mean, py.daily_mean),
        ("daily_std", rust.daily_std, py.daily_std),
    ];
    for (name, r, p) in rows {
        println!("{:20} {:>15.6} {:>15.6} {:>15.6}", name, r, p, r - p);
    }

    // Tolerance from issue #256
    println!("\n=== TOLERANCE CHECK ===");
    let cum_ok = (rust.cum_return - py.cum_return).abs() < 0.02;
    let sharpe_ok = rust.sharpe >= 2.70 && rust.sharpe <= 2.90;
    let dd_ok = (rust.max_drawdown - py.max_drawdown).abs() < 0.01;
    println!(
        "cum_return ±2%:        {} (rust={:+.4}, target={:+.4})",
        pass(cum_ok),
        rust.cum_return,
        py.cum_return
    );
    println!(
        "sharpe [2.70, 2.90]:   {} (rust={:.4})",
        pass(sharpe_ok),
        rust.sharpe
    );
    println!(
        "max_dd ±1%:            {} (rust={:+.4}, target={:+.4})",
        pass(dd_ok),
        rust.max_drawdown,
        py.max_drawdown
    );

    cum_ok && sharpe_ok && dd_ok
}

fn pass(ok: bool) -> &'static str {
    if ok {
        "PASS"
    } else {
        "FAIL"
    }
}

/// Write daily P&L + per-basket runs to TSV.
pub fn write_tsv(
    path: &Path,
    stats: &PortfolioStats,
    runs: &[BasketRun],
    daily: &[(NaiveDate, f64)],
) -> Result<(), String> {
    use std::io::Write;
    let mut f = fs::File::create(path).map_err(|e| format!("create {}: {e}", path.display()))?;

    // Header comment with portfolio summary
    writeln!(
        f,
        "# cum_return={:.6}\tann_return={:.6}\tsharpe={:.6}\tmax_dd={:.6}\tn_days={}\tdaily_mean={:.6}\tdaily_std={:.6}",
        stats.cum_return,
        stats.ann_return,
        stats.sharpe,
        stats.max_drawdown,
        stats.n_days,
        stats.daily_mean,
        stats.daily_std
    )
    .map_err(|e| e.to_string())?;

    // Per-basket runs
    writeln!(
        f,
        "fit_date\tsector\ttarget\tk\tn_trades\tcum_return\tsharpe\tn_days"
    )
    .map_err(|e| e.to_string())?;
    for r in runs {
        writeln!(
            f,
            "{}\t{}\t{}\t{:.6}\t{}\t{:.6}\t{:.6}\t{}",
            r.fit_date, r.sector, r.target, r.k, r.n_trades, r.cum_return, r.sharpe, r.n_days
        )
        .map_err(|e| e.to_string())?;
    }

    // Daily P&L
    writeln!(f, "# daily_portfolio_pnl").map_err(|e| e.to_string())?;
    writeln!(f, "date\tdaily_pnl").map_err(|e| e.to_string())?;
    for (d, p) in daily {
        writeln!(f, "{}\t{:.10}", d, p).map_err(|e| e.to_string())?;
    }

    Ok(())
}

// ─── helpers ────────────────────────────────────────────────────────

fn collect_symbols(universe: &Universe) -> Vec<String> {
    let mut symbols: Vec<String> = universe
        .sectors
        .values()
        .flat_map(|s| s.members.iter().cloned())
        .collect();
    symbols.sort();
    symbols.dedup();
    symbols
}

fn load_all_daily_closes(
    bars_dir: &Path,
    symbols: &[String],
) -> HashMap<String, BTreeMap<NaiveDate, f64>> {
    let mut closes = HashMap::new();
    for symbol in symbols {
        let path = bars_dir.join(format!("{symbol}.parquet"));
        match read_daily_closes(&path) {
            Ok(daily) => {
                closes.insert(symbol.clone(), daily);
            }
            Err(e) => {
                warn!(symbol = %symbol, error = %e, "failed to read parquet");
            }
        }
    }
    closes
}

/// Read per-symbol parquet; aggregate to daily closes (last 1-min bar in RTH).
fn read_daily_closes(path: &Path) -> Result<BTreeMap<NaiveDate, f64>, String> {
    let file = fs::File::open(path).map_err(|e| format!("open: {e}"))?;
    let builder =
        ParquetRecordBatchReaderBuilder::try_new(file).map_err(|e| format!("reader: {e}"))?;
    let reader = builder.build().map_err(|e| format!("build: {e}"))?;

    let mut daily: BTreeMap<NaiveDate, (i64, f64)> = BTreeMap::new();

    for batch in reader {
        let batch = batch.map_err(|e| format!("batch: {e}"))?;
        let ts = batch
            .column(0)
            .as_any()
            .downcast_ref::<TimestampMicrosecondArray>()
            .ok_or("ts cast")?;
        let close = batch
            .column(4)
            .as_any()
            .downcast_ref::<Float64Array>()
            .ok_or("close cast")?;

        for i in 0..batch.num_rows() {
            let ts_us = ts.value(i);
            let secs = ts_us / 1_000_000;
            let dt = match chrono::DateTime::from_timestamp(secs, 0) {
                Some(d) => d.naive_utc(),
                None => continue,
            };
            let minute = dt.hour() * 60 + dt.minute();
            if !(RTH_START_MIN..RTH_END_MIN).contains(&minute) {
                continue;
            }
            let px = close.value(i);
            if !px.is_finite() || px <= 0.0 {
                continue;
            }
            let date = dt.date();
            daily
                .entry(date)
                .and_modify(|(prev_ts, prev_close)| {
                    if ts_us > *prev_ts {
                        *prev_ts = ts_us;
                        *prev_close = px;
                    }
                })
                .or_insert((ts_us, px));
        }
    }

    Ok(daily.into_iter().map(|(d, (_, c))| (d, c)).collect())
}

/// Return (date, target_px, peer_pxs) only for dates where ALL symbols have prices.
fn align_prices(
    closes: &HashMap<String, BTreeMap<NaiveDate, f64>>,
    target: &str,
    peers: &[String],
) -> Vec<(NaiveDate, f64, Vec<f64>)> {
    let target_series = match closes.get(target) {
        Some(s) => s,
        None => return vec![],
    };
    let mut out = Vec::new();
    for (date, &tpx) in target_series.iter() {
        let peer_pxs: Option<Vec<f64>> = peers
            .iter()
            .map(|p| closes.get(p).and_then(|s| s.get(date)).copied())
            .collect();
        if let Some(pxs) = peer_pxs {
            if pxs.len() == peers.len() {
                out.push((*date, tpx, pxs));
            }
        }
    }
    out
}

fn compute_spread(aligned: &[(NaiveDate, f64, Vec<f64>)]) -> Vec<(NaiveDate, f64)> {
    aligned
        .iter()
        .filter_map(|(d, tpx, peer_pxs)| {
            if *tpx <= 0.0 || peer_pxs.iter().any(|p| *p <= 0.0) {
                return None;
            }
            let log_target = tpx.ln();
            let log_peers_mean =
                peer_pxs.iter().map(|p| p.ln()).sum::<f64>() / peer_pxs.len() as f64;
            Some((*d, log_target - log_peers_mean))
        })
        .collect()
}

/// Pure Bertram symmetric simulator — mirrors quant-lab's simulate_symmetric_bertram.
///
/// Loop (matches Python exactly):
///   for each bar: record pnl = pos * dspread (dspread[0] = 0)
///                 evaluate new_pos from z-score
///                 if changed: pnl -= cost/2, update pos
fn simulate_symmetric_bertram(
    fwd: &[(NaiveDate, f64)],
    mu: f64,
    sigma_eq: f64,
    k: f64,
    cost: f64,
) -> (Vec<(NaiveDate, f64)>, usize) {
    let mut out: Vec<(NaiveDate, f64)> = Vec::with_capacity(fwd.len());
    let mut pos: i32 = 0;
    let mut prev_spread: f64 = fwd.first().map(|(_, s)| *s).unwrap_or(0.0);
    let mut n_trades = 0;

    for (idx, (date, spread_val)) in fwd.iter().enumerate() {
        let dsp = if idx == 0 {
            0.0
        } else {
            spread_val - prev_spread
        };
        let mut pnl = pos as f64 * dsp;

        let z = (spread_val - mu) / sigma_eq;
        let new_pos = if !z.is_finite() {
            pos
        } else if z > k {
            -1
        } else if z < -k {
            1
        } else {
            pos
        };

        if new_pos != pos {
            pnl -= cost / 2.0;
            n_trades += 1;
            pos = new_pos;
        }

        out.push((*date, pnl));
        prev_spread = *spread_val;
    }

    (out, n_trades)
}

fn basket_stats(pnl: &[(NaiveDate, f64)]) -> (f64, f64) {
    if pnl.is_empty() {
        return (0.0, 0.0);
    }
    let r: Vec<f64> = pnl.iter().map(|(_, p)| *p).collect();
    let cum = r.iter().fold(1.0, |a, x| a * (1.0 + x)) - 1.0;
    let n = r.len() as f64;
    let mean = r.iter().sum::<f64>() / n;
    let var = r.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / (n - 1.0).max(1.0);
    let std = var.sqrt();
    let sharpe = if std > 0.0 {
        (mean / std) * 252.0_f64.sqrt()
    } else {
        0.0
    };
    (cum, sharpe)
}

fn portfolio_stats(r: &[f64]) -> PortfolioStats {
    if r.is_empty() {
        return PortfolioStats {
            cum_return: 0.0,
            ann_return: 0.0,
            sharpe: 0.0,
            max_drawdown: 0.0,
            n_days: 0,
            daily_mean: 0.0,
            daily_std: 0.0,
        };
    }
    let n = r.len() as f64;
    let mean = r.iter().sum::<f64>() / n;
    let var = r.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / (n - 1.0).max(1.0);
    let std = var.sqrt();
    let sharpe = if std > 0.0 {
        (mean / std) * 252.0_f64.sqrt()
    } else {
        0.0
    };
    let cum = r.iter().fold(1.0, |a, x| a * (1.0 + x)) - 1.0;
    let ann_return = (1.0 + cum).powf(252.0 / n) - 1.0;

    let mut equity = 1.0;
    let mut peak = 1.0;
    let mut max_dd = 0.0_f64;
    for &x in r {
        equity *= 1.0 + x;
        if equity > peak {
            peak = equity;
        }
        let dd = equity / peak - 1.0;
        if dd < max_dd {
            max_dd = dd;
        }
    }

    PortfolioStats {
        cum_return: cum,
        ann_return,
        sharpe,
        max_drawdown: max_dd,
        n_days: r.len(),
        daily_mean: mean,
        daily_std: std,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_collect_symbols() {
        let toml = r#"
[version]
schema = "basket_universe"
version = "v1"
frozen_at = "2026-04-20"

[strategy]
method = "basket_spread_ou_bertram"
spread_formula = "log(target) - mean(log(peers))"
threshold_method = "bertram_symmetric"
threshold_clip_min = 0.15
threshold_clip_max = 2.5
residual_window_days = 60
forward_window_days = 60
refit_cadence = "quarterly"
cost_bps_assumed = 5.0
leverage_assumed = 4.0
sizing = "equal_weight_across_baskets"

[sectors.chips]
members = ["NVDA", "AMD", "INTC"]
traded_targets = ["AMD"]
"#;
        let universe = basket_picker::load_universe_from_str(toml).unwrap();
        let symbols = collect_symbols(&universe);
        assert!(symbols.contains(&"NVDA".to_string()));
        assert!(symbols.contains(&"AMD".to_string()));
        assert!(symbols.contains(&"INTC".to_string()));
    }

    #[test]
    fn test_portfolio_stats_empty() {
        let s = portfolio_stats(&[]);
        assert_eq!(s.n_days, 0);
        assert_eq!(s.sharpe, 0.0);
    }

    #[test]
    fn test_portfolio_stats_const_returns() {
        let r = vec![0.01; 100];
        let s = portfolio_stats(&r);
        assert_eq!(s.n_days, 100);
        // cum = 1.01^100 - 1 ≈ 1.7048
        assert!((s.cum_return - (1.01_f64.powi(100) - 1.0)).abs() < 1e-9);
        assert!((s.daily_mean - 0.01).abs() < 1e-15);
        // std effectively 0 (floating point noise) → sharpe is numerically ill-defined
        assert!(s.daily_std < 1e-15);
    }

    #[test]
    fn test_simulate_flat_stays_flat() {
        let fwd = vec![
            (NaiveDate::from_ymd_opt(2025, 1, 1).unwrap(), 0.0),
            (NaiveDate::from_ymd_opt(2025, 1, 2).unwrap(), 0.0),
            (NaiveDate::from_ymd_opt(2025, 1, 3).unwrap(), 0.0),
        ];
        let (pnl, n) = simulate_symmetric_bertram(&fwd, 0.0, 0.1, 1.0, 0.0);
        assert_eq!(pnl.len(), 3);
        for (_, p) in &pnl {
            assert_eq!(*p, 0.0);
        }
        assert_eq!(n, 0);
    }
}
