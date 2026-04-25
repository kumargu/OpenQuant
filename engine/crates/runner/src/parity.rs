//! Parity TSV writer + quant-lab baseline comparison (#294c-3).
//!
//! Restores the parity reporting that lived in the deleted
//! `basket_runner.rs`, but as a thin wrapper over the new replay
//! output: stats are computed from `SimulatedBroker::daily_equity()`,
//! NOT from a closed-form spread-delta P&L formula.
//!
//! The TSV format and the baseline JSON schema are kept identical to
//! what the legacy walk-forward path used, so existing dashboards and
//! comparison harnesses (e.g., quant-lab/statarb/autoresearch) keep
//! working.

use std::fs;
use std::path::Path;

use chrono::NaiveDate;
use serde::Deserialize;

/// Portfolio summary stats — same field names and types the legacy
/// `basket_runner::PortfolioStats` exposed, so downstream consumers
/// do not need to change.
#[derive(Debug, Clone)]
pub struct PortfolioStats {
    pub cum_return: f64,
    pub ann_return: f64,
    pub sharpe: f64,
    pub max_drawdown: f64,
    pub n_days: usize,
    pub daily_mean: f64,
    pub daily_std: f64,
}

/// Schema for `quant-lab/statarb/autoresearch/baseline/baseline.json`.
/// Identical to the legacy struct — preserved so the existing
/// baseline file format keeps loading.
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

/// Compute portfolio stats from a daily-equity time series. The series
/// is the output of `SimulatedBroker::daily_equity()` — already in
/// chronological order because it's a `BTreeMap`.
///
/// Definitions (matching the legacy walk-forward harness):
///
///   - daily return r_t = (E_t - E_{t-1}) / E_{t-1}, t≥1
///   - cum_return     = E_n / E_0 - 1
///   - daily_mean     = mean(r_t)
///   - daily_std      = stdev(r_t, ddof=1)
///   - sharpe         = daily_mean / daily_std × √252
///   - ann_return     = daily_mean × 252
///   - max_drawdown   = max over t of (peak_so_far - E_t) / peak_so_far
///
/// Returns `None` when fewer than 2 equity points are available — no
/// stats are well-defined in that case.
pub fn compute_stats(daily_equity: &[(NaiveDate, f64)]) -> Option<PortfolioStats> {
    if daily_equity.len() < 2 {
        return None;
    }
    let n = daily_equity.len();
    let e0 = daily_equity[0].1;
    let e_last = daily_equity[n - 1].1;
    if e0 <= 0.0 || !e0.is_finite() {
        return None;
    }
    let cum_return = e_last / e0 - 1.0;

    let returns: Vec<f64> = daily_equity
        .windows(2)
        .map(|w| (w[1].1 - w[0].1) / w[0].1)
        .collect();
    let daily_mean = returns.iter().sum::<f64>() / returns.len() as f64;
    let var = if returns.len() > 1 {
        returns
            .iter()
            .map(|r| (r - daily_mean).powi(2))
            .sum::<f64>()
            / (returns.len() - 1) as f64
    } else {
        0.0
    };
    let daily_std = var.sqrt();
    let sharpe = if daily_std > 0.0 {
        daily_mean / daily_std * (252.0_f64).sqrt()
    } else {
        0.0
    };
    let ann_return = daily_mean * 252.0;

    // Max drawdown over the equity curve.
    let mut peak = e0;
    let mut max_dd = 0.0_f64;
    for (_, e) in daily_equity {
        if *e > peak {
            peak = *e;
        }
        if peak > 0.0 {
            let dd = (peak - e) / peak;
            if dd > max_dd {
                max_dd = dd;
            }
        }
    }

    Some(PortfolioStats {
        cum_return,
        ann_return,
        sharpe,
        max_drawdown: max_dd,
        n_days: n,
        daily_mean,
        daily_std,
    })
}

/// Write the parity TSV to `path`. Format matches the legacy
/// `basket_runner::write_tsv` output: header comment line with summary
/// stats, then per-session daily P&L rows (date, daily_pnl_dollar).
///
/// `daily_equity` is the output of `SimulatedBroker::daily_equity()`.
/// Per-basket-fit metrics are not currently emitted — the new replay
/// path does not expose per-basket P&L (#300 follow-up).
pub fn write_tsv(
    path: &Path,
    stats: &PortfolioStats,
    daily_equity: &[(NaiveDate, f64)],
) -> Result<(), String> {
    use std::io::Write;
    let mut f = fs::File::create(path).map_err(|e| format!("create {}: {e}", path.display()))?;
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
    writeln!(f, "# daily_portfolio_pnl").map_err(|e| e.to_string())?;
    writeln!(f, "date\tequity\tdaily_pnl_dollar").map_err(|e| e.to_string())?;
    let mut prev_eq: Option<f64> = None;
    for (d, e) in daily_equity {
        let pnl = match prev_eq {
            Some(p) => e - p,
            None => 0.0,
        };
        writeln!(f, "{}\t{:.4}\t{:.4}", d, e, pnl).map_err(|e| e.to_string())?;
        prev_eq = Some(*e);
    }
    Ok(())
}

/// Print a side-by-side parity comparison of replay stats against a
/// quant-lab baseline. Returns true when all monitored metrics fall
/// inside the documented tolerances (matching the legacy harness):
///
///   - cum_return: ±2 percentage points
///   - sharpe: within [2.70, 2.90]
///   - max_drawdown: ±1 percentage point
///
/// The tolerances were chosen by quant-lab (#256) when the original
/// parity check was written. Any change to them is a separate
/// research decision, not a refactor change.
pub fn print_parity_comparison(rust: &PortfolioStats, py: &BaselineStats) -> bool {
    println!("\n=== PARITY COMPARISON (Rust replay vs quant-lab) ===");
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

    println!("\n=== TOLERANCE CHECK ===");
    let cum_ok = (rust.cum_return - py.cum_return).abs() < 0.02;
    let sharpe_ok = (2.70..=2.90).contains(&rust.sharpe);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn dates(start: (i32, u32, u32), n: usize) -> Vec<NaiveDate> {
        let mut out = Vec::new();
        let mut d = NaiveDate::from_ymd_opt(start.0, start.1, start.2).unwrap();
        for _ in 0..n {
            out.push(d);
            d = d.succ_opt().unwrap();
        }
        out
    }

    #[test]
    fn compute_stats_flat_equity_returns_zero_metrics() {
        let ds = dates((2024, 7, 1), 5);
        let series: Vec<_> = ds.into_iter().map(|d| (d, 10_000.0)).collect();
        let s = compute_stats(&series).unwrap();
        assert!((s.cum_return - 0.0).abs() < 1e-9);
        assert!((s.daily_mean - 0.0).abs() < 1e-9);
        assert!((s.daily_std - 0.0).abs() < 1e-9);
        assert!((s.sharpe - 0.0).abs() < 1e-9);
        assert!((s.max_drawdown - 0.0).abs() < 1e-9);
        assert_eq!(s.n_days, 5);
    }

    #[test]
    fn compute_stats_monotonic_growth_has_zero_drawdown() {
        let ds = dates((2024, 7, 1), 4);
        let eqs = [10_000.0, 10_100.0, 10_300.0, 10_400.0];
        let series: Vec<_> = ds.into_iter().zip(eqs).collect();
        let s = compute_stats(&series).unwrap();
        assert!(s.cum_return > 0.039 && s.cum_return < 0.041);
        assert!((s.max_drawdown - 0.0).abs() < 1e-9);
    }

    #[test]
    fn compute_stats_drawdown_after_peak() {
        let ds = dates((2024, 7, 1), 4);
        // Up to 11k, then drop to 9.9k → max_dd = 0.1
        let eqs = [10_000.0, 11_000.0, 9_900.0, 10_500.0];
        let series: Vec<_> = ds.into_iter().zip(eqs).collect();
        let s = compute_stats(&series).unwrap();
        assert!(
            (s.max_drawdown - 0.1).abs() < 1e-9,
            "got {}",
            s.max_drawdown
        );
    }

    #[test]
    fn compute_stats_returns_none_for_empty_or_single() {
        assert!(compute_stats(&[]).is_none());
        let one = vec![(NaiveDate::from_ymd_opt(2024, 7, 1).unwrap(), 10_000.0)];
        assert!(compute_stats(&one).is_none());
    }

    #[test]
    fn write_tsv_roundtrips_format() {
        let ds = dates((2024, 7, 1), 3);
        let eqs = [10_000.0, 10_050.0, 10_010.0];
        let series: Vec<_> = ds.into_iter().zip(eqs).collect();
        let stats = compute_stats(&series).unwrap();
        let tmp = std::env::temp_dir().join("parity_test_oq.tsv");
        write_tsv(&tmp, &stats, &series).unwrap();
        let body = std::fs::read_to_string(&tmp).unwrap();
        assert!(body.contains("cum_return="));
        assert!(body.contains("daily_portfolio_pnl"));
        assert!(body.contains("date\tequity\tdaily_pnl_dollar"));
        // Three rows of data.
        let row_count = body.lines().filter(|l| l.starts_with("2024-07")).count();
        assert_eq!(row_count, 3);
        let _ = std::fs::remove_file(&tmp);
    }
}
