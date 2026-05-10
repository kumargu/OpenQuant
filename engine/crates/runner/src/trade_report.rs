//! Per-trade replay diagnostics TSV writer (issue #325 Stage 1).
//!
//! Replay-only: emits one row per closed basket trade with the fields the
//! reviewer asked for in the issue (entry/exit dates, days held, entry_z,
//! exit_z, max adverse / favorable z, days_to_max_adverse, exit reason,
//! and the entry / exit spreads needed for downstream P&L attribution).
//!
//! Live and paper paths leave `BasketRunOptions::trade_tsv_path = None` and
//! never invoke this writer.

use std::fs;
use std::io::Write;
use std::path::Path;

use basket_engine::ClosedTrade;

/// Write closed trades as TSV. Header is fixed and machine-parseable; the
/// loader for downstream tabulation is plain `awk` / `cut` / a tiny pandas
/// read (see issue #325 Stage 1c). No per-trade P&L column — the reviewer
/// asked for spread movement and the spread fields let the consumer compute
/// it without the engine needing to attribute portfolio dollars.
pub fn write_trade_tsv(path: &Path, trades: &[ClosedTrade]) -> Result<(), String> {
    let mut f = fs::File::create(path).map_err(|e| format!("create {}: {e}", path.display()))?;
    writeln!(
        f,
        "basket_id\tposition\tentry_date\texit_date\tdays_held\tbars_held\tentry_z\texit_z\tmax_adverse_z\tmax_adverse_date\tdays_to_max_adverse\tmax_favorable_z\tentry_spread\texit_spread\tspread_move\texit_reason"
    )
    .map_err(|e| e.to_string())?;
    for t in trades {
        let calendar_days_held = (t.exit_date - t.entry_date).num_days();
        let days_to_max_adverse = (t.max_adverse_date - t.entry_date).num_days();
        // Signed spread move in the position's direction. Positive = favorable.
        let spread_move = (t.position as f64) * (t.exit_spread - t.entry_spread);
        writeln!(
            f,
            "{}\t{}\t{}\t{}\t{}\t{}\t{:.6}\t{:.6}\t{:.6}\t{}\t{}\t{:.6}\t{:.8}\t{:.8}\t{:.8}\t{}",
            t.basket_id,
            t.position,
            t.entry_date,
            t.exit_date,
            calendar_days_held,
            t.bars_held,
            t.entry_z,
            t.exit_z,
            t.max_adverse_z,
            t.max_adverse_date,
            days_to_max_adverse,
            t.max_favorable_z,
            t.entry_spread,
            t.exit_spread,
            spread_move,
            t.exit_reason.as_str(),
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use basket_engine::ExitReason;
    use chrono::NaiveDate;

    #[test]
    fn writes_header_and_records() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("trades.tsv");
        let entry = NaiveDate::from_ymd_opt(2026, 4, 1).unwrap();
        let exit = NaiveDate::from_ymd_opt(2026, 4, 15).unwrap();
        let max_adv = NaiveDate::from_ymd_opt(2026, 4, 12).unwrap();
        let trades = vec![ClosedTrade {
            basket_id: "chips:AMD".to_string(),
            position: -1,
            entry_date: entry,
            exit_date: exit,
            entry_z: 1.6,
            exit_z: 0.1,
            entry_spread: 0.08,
            exit_spread: 0.02,
            max_adverse_z: 0.5,
            max_adverse_date: max_adv,
            max_favorable_z: 1.5,
            bars_held: 14,
            exit_reason: ExitReason::WindowEnd,
        }];
        write_trade_tsv(&path, &trades).unwrap();
        let body = fs::read_to_string(&path).unwrap();
        assert!(body.starts_with("basket_id\tposition\tentry_date\t"));
        assert!(body.contains("chips:AMD\t-1\t2026-04-01\t2026-04-15\t14\t14\t"));
        assert!(body.contains("\twindow_end\n"));
        // spread_move = position * (exit - entry) = -1 * (0.02 - 0.08) = +0.06
        assert!(body.contains("\t0.06000000\twindow_end\n"));
        // days_to_max_adverse = 11
        assert!(body.contains("\t2026-04-12\t11\t"));
    }
}
