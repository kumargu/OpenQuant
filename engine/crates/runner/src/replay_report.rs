//! Replay report — portfolio stats + TSV writer.
//!
//! Computes summary statistics and an EOD time series from the
//! [`crate::simulated_broker::SimulatedBroker`]'s mark-to-market
//! daily-equity history. Pure mark-to-market on simulated fills.
//!
//! Conventions used here:
//!
//!   - `ann_return` is linearized (`daily_mean × 252`).
//!   - `max_drawdown` is a positive fraction `(peak − equity) / peak`.
//!   - The TSV emits one section: `date\tequity\tdaily_pnl_dollar`.

use std::fs;
use std::path::Path;

use chrono::NaiveDate;

/// Summary stats for a replay run.
#[derive(Debug, Clone)]
pub struct ReplayStats {
    pub cum_return: f64,
    /// Linearized annualized return: `daily_mean × 252`.
    pub ann_return: f64,
    pub sharpe: f64,
    /// Reported as a positive fraction `(peak − equity) / peak`.
    pub max_drawdown: f64,
    pub n_days: usize,
    pub daily_mean: f64,
    pub daily_std: f64,
}

/// Compute replay stats from a daily-equity time series. The series
/// is the output of `SimulatedBroker::daily_equity()` — already in
/// chronological order because it's a `BTreeMap`.
///
/// Definitions:
///
///   - daily return r_t = (E_t − E_{t-1}) / E_{t-1}, t ≥ 1
///   - cum_return     = E_n / E_0 − 1
///   - daily_mean     = mean(r_t)
///   - daily_std      = stdev(r_t, ddof=1)
///   - sharpe         = daily_mean / daily_std × √252
///   - ann_return     = daily_mean × 252
///   - max_drawdown   = max over t of (peak_so_far − E_t) / peak_so_far
///
/// Returns `None` when fewer than 2 equity points are available — no
/// stats are well-defined in that case.
pub fn compute_stats(daily_equity: &[(NaiveDate, f64)]) -> Option<ReplayStats> {
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

    Some(ReplayStats {
        cum_return,
        ann_return,
        sharpe,
        max_drawdown: max_dd,
        n_days: n,
        daily_mean,
        daily_std,
    })
}

/// Write the replay report to `path`.
///
/// Format:
///
///   - line 1: comment with summary stats
///     `# cum_return=...\tann_return=...\tsharpe=...\tmax_dd=...\t
///     n_days=...\tdaily_mean=...\tdaily_std=...`
///   - line 2: section marker `# daily_portfolio_pnl`
///   - line 3: header `date\tequity\tdaily_pnl_dollar`
///   - data rows: one per session, each `YYYY-MM-DD\t<equity>\t<daily_pnl>`
///
/// `daily_equity` is the output of `SimulatedBroker::daily_equity()`.
pub fn write_tsv(
    path: &Path,
    stats: &ReplayStats,
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
    fn flat_equity_returns_zero_metrics() {
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
    fn monotonic_growth_has_zero_drawdown() {
        let ds = dates((2024, 7, 1), 4);
        let eqs = [10_000.0, 10_100.0, 10_300.0, 10_400.0];
        let series: Vec<_> = ds.into_iter().zip(eqs).collect();
        let s = compute_stats(&series).unwrap();
        assert!(s.cum_return > 0.039 && s.cum_return < 0.041);
        assert!((s.max_drawdown - 0.0).abs() < 1e-9);
    }

    #[test]
    fn drawdown_after_peak_is_positive_fraction() {
        let ds = dates((2024, 7, 1), 4);
        let eqs = [10_000.0, 11_000.0, 9_900.0, 10_500.0];
        let series: Vec<_> = ds.into_iter().zip(eqs).collect();
        let s = compute_stats(&series).unwrap();
        assert!(s.max_drawdown > 0.0, "drawdown is positive in this format");
        assert!(
            (s.max_drawdown - 0.1).abs() < 1e-9,
            "got {}",
            s.max_drawdown
        );
    }

    #[test]
    fn ann_return_is_linearized_not_compounded() {
        // 1% per day for 4 days. daily_mean ≈ 0.01.
        // Linearized ann_return = 0.01 × 252 = 2.52.
        // Compounded would be (1.01)^252 - 1 ≈ 11.27. Big difference.
        let ds = dates((2024, 7, 1), 5);
        let eqs = [10_000.0, 10_100.0, 10_201.0, 10_303.01, 10_406.04];
        let series: Vec<_> = ds.into_iter().zip(eqs).collect();
        let s = compute_stats(&series).unwrap();
        assert!(
            (s.ann_return - 2.52).abs() < 0.01,
            "expected linearized ann_return ≈ 2.52, got {}",
            s.ann_return
        );
    }

    #[test]
    fn returns_none_for_empty_or_single() {
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
        let tmp = std::env::temp_dir().join("replay_report_test_oq.tsv");
        write_tsv(&tmp, &stats, &series).unwrap();
        let body = std::fs::read_to_string(&tmp).unwrap();
        assert!(body.contains("cum_return="));
        assert!(body.contains("daily_portfolio_pnl"));
        assert!(body.contains("date\tequity\tdaily_pnl_dollar"));
        let row_count = body.lines().filter(|l| l.starts_with("2024-07")).count();
        assert_eq!(row_count, 3);
        let _ = std::fs::remove_file(&tmp);
    }
}
