//! Durable SQLite audit trail for basket paper/live runs.
//!
//! Replay diagnostics TSVs are useful for research, but paper/live needs a
//! queryable record of what the runner intended, submitted, and observed after
//! reconciliation. Volume is low (one close cycle/day, tens of orders), so a
//! simple synchronous SQLite writer is sufficient and keeps post-mortems easy.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, NaiveDate, Utc};
use rusqlite::{params, Connection};

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS basket_runs (
    run_id TEXT PRIMARY KEY,
    started_at_utc TEXT NOT NULL,
    execution_mode TEXT NOT NULL,
    universe_path TEXT NOT NULL,
    fit_artifact_path TEXT,
    state_path TEXT NOT NULL,
    startup_phase TEXT NOT NULL,
    symbols INTEGER NOT NULL,
    baskets INTEGER NOT NULL,
    capital REAL NOT NULL,
    leverage REAL NOT NULL,
    n_active_baskets INTEGER NOT NULL,
    broker_positions INTEGER NOT NULL,
    last_processed_trading_day TEXT
);

CREATE TABLE IF NOT EXISTS basket_session_closes (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id TEXT NOT NULL,
    trading_day TEXT NOT NULL,
    status TEXT NOT NULL,
    closes_received INTEGER NOT NULL,
    symbols_expected INTEGER NOT NULL,
    active_baskets INTEGER NOT NULL,
    admitted_baskets INTEGER NOT NULL,
    excluded_baskets INTEGER NOT NULL,
    gross_long REAL NOT NULL,
    gross_short REAL NOT NULL,
    gross_notional REAL NOT NULL,
    gross_cap REAL NOT NULL,
    net_notional REAL NOT NULL,
    max_abs_leg REAL NOT NULL,
    median_abs_leg REAL NOT NULL,
    target_positions INTEGER NOT NULL,
    current_positions_before INTEGER NOT NULL,
    current_positions_after INTEGER NOT NULL,
    buy_orders INTEGER NOT NULL,
    sell_orders INTEGER NOT NULL,
    buy_notional REAL NOT NULL,
    sell_notional REAL NOT NULL,
    order_gross_notional REAL NOT NULL,
    order_max_notional REAL NOT NULL,
    accepted_orders INTEGER NOT NULL,
    failed_orders INTEGER NOT NULL,
    target_gross REAL,
    actual_gross REAL,
    divergence_pct REAL,
    selected_baskets_json TEXT NOT NULL,
    excluded_baskets_json TEXT NOT NULL,
    current_shares_before_json TEXT NOT NULL,
    target_shares_json TEXT NOT NULL,
    current_shares_after_json TEXT,
    error_text TEXT,
    created_at_utc TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS basket_order_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id TEXT NOT NULL,
    trading_day TEXT NOT NULL,
    seq INTEGER NOT NULL,
    symbol TEXT NOT NULL,
    side TEXT NOT NULL,
    requested_qty REAL NOT NULL,
    intended_notional REAL,
    reason TEXT NOT NULL,
    basket_id TEXT,
    broker_order_id TEXT,
    broker_status TEXT,
    submission_status TEXT NOT NULL,
    error_text TEXT,
    created_at_utc TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS basket_picker_decisions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id TEXT NOT NULL,
    trading_day TEXT NOT NULL,
    picker_id TEXT NOT NULL,
    mode TEXT NOT NULL,
    reason TEXT NOT NULL,
    active_sectors_json TEXT NOT NULL,
    active_symbols_json TEXT NOT NULL,
    leadership_short_conflict_ratio REAL NOT NULL,
    strategy_return_20d REAL NOT NULL,
    strategy_drawdown_20d REAL NOT NULL,
    basket_only_scale_if_sleeve REAL NOT NULL,
    sleeve_leverage_scale REAL NOT NULL DEFAULT 1.0,
    created_at_utc TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_basket_session_closes_run_day
    ON basket_session_closes(run_id, trading_day);
CREATE INDEX IF NOT EXISTS idx_basket_order_events_run_day
    ON basket_order_events(run_id, trading_day);
CREATE INDEX IF NOT EXISTS idx_basket_order_events_symbol
    ON basket_order_events(symbol);
CREATE INDEX IF NOT EXISTS idx_basket_picker_decisions_run_day
    ON basket_picker_decisions(run_id, trading_day);
";

#[derive(Clone)]
pub struct BasketJournal {
    conn: Arc<Mutex<Connection>>,
    legacy_picker_scale_column: bool,
}

pub struct BasketRunRecord<'a> {
    pub run_id: &'a str,
    pub started_at_utc: DateTime<Utc>,
    pub execution_mode: &'a str,
    pub universe_path: &'a str,
    pub fit_artifact_path: Option<&'a str>,
    pub state_path: &'a str,
    pub startup_phase: &'a str,
    pub symbols: usize,
    pub baskets: usize,
    pub capital: f64,
    pub leverage: f64,
    pub n_active_baskets: usize,
    pub broker_positions: usize,
    pub last_processed_trading_day: Option<NaiveDate>,
}

pub struct BasketSessionCloseRecord<'a> {
    pub run_id: &'a str,
    pub trading_day: NaiveDate,
    pub status: &'a str,
    pub closes_received: usize,
    pub symbols_expected: usize,
    pub active_baskets: usize,
    pub admitted_baskets: usize,
    pub excluded_baskets: usize,
    pub gross_long: f64,
    pub gross_short: f64,
    pub gross_notional: f64,
    pub gross_cap: f64,
    pub net_notional: f64,
    pub max_abs_leg: f64,
    pub median_abs_leg: f64,
    pub target_positions: usize,
    pub current_positions_before: usize,
    pub current_positions_after: usize,
    pub buy_orders: usize,
    pub sell_orders: usize,
    pub buy_notional: f64,
    pub sell_notional: f64,
    pub order_gross_notional: f64,
    pub order_max_notional: f64,
    pub accepted_orders: usize,
    pub failed_orders: usize,
    pub target_gross: Option<f64>,
    pub actual_gross: Option<f64>,
    pub divergence_pct: Option<f64>,
    pub selected_baskets_json: String,
    pub excluded_baskets_json: String,
    pub current_shares_before_json: String,
    pub target_shares_json: String,
    pub current_shares_after_json: Option<String>,
    pub error_text: Option<String>,
}

pub struct BasketOrderEvent<'a> {
    pub run_id: &'a str,
    pub trading_day: NaiveDate,
    pub seq: usize,
    pub symbol: &'a str,
    pub side: &'a str,
    pub requested_qty: f64,
    pub intended_notional: Option<f64>,
    pub reason: &'a str,
    pub basket_id: Option<&'a str>,
    pub broker_order_id: Option<&'a str>,
    pub broker_status: Option<&'a str>,
    pub submission_status: &'a str,
    pub error_text: Option<&'a str>,
}

pub struct BasketPickerDecisionRecord<'a> {
    pub run_id: &'a str,
    pub trading_day: NaiveDate,
    pub picker_id: &'a str,
    pub mode: &'a str,
    pub reason: &'a str,
    pub active_sectors_json: String,
    pub active_symbols_json: String,
    pub leadership_short_conflict_ratio: f64,
    pub strategy_return_20d: f64,
    pub strategy_drawdown_20d: f64,
    pub basket_only_scale_if_sleeve: f64,
    pub sleeve_leverage_scale: f64,
}

impl BasketJournal {
    pub fn open(path: &Path) -> Result<Self, String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("create journal dir {}: {e}", parent.display()))?;
        }
        let conn =
            Connection::open(path).map_err(|e| format!("open journal {}: {e}", path.display()))?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")
            .map_err(|e| format!("set journal pragmas: {e}"))?;
        conn.execute_batch(SCHEMA)
            .map_err(|e| format!("init basket journal schema: {e}"))?;
        migrate_picker_decisions_schema(&conn)
            .map_err(|e| format!("migrate basket journal schema: {e}"))?;
        let legacy_picker_scale_column =
            table_has_column(&conn, "basket_picker_decisions", "baseline_scale_if_sleeve")
                .map_err(|e| format!("inspect basket journal schema: {e}"))?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            legacy_picker_scale_column,
        })
    }

    pub fn record_run(&self, rec: &BasketRunRecord<'_>) -> Result<(), String> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| "basket journal mutex poisoned".to_string())?;
        conn.execute(
            "INSERT OR REPLACE INTO basket_runs (
                run_id, started_at_utc, execution_mode, universe_path, fit_artifact_path,
                state_path, startup_phase, symbols, baskets, capital, leverage,
                n_active_baskets, broker_positions, last_processed_trading_day
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                rec.run_id,
                rec.started_at_utc.to_rfc3339(),
                rec.execution_mode,
                rec.universe_path,
                rec.fit_artifact_path,
                rec.state_path,
                rec.startup_phase,
                rec.symbols as i64,
                rec.baskets as i64,
                rec.capital,
                rec.leverage,
                rec.n_active_baskets as i64,
                rec.broker_positions as i64,
                rec.last_processed_trading_day.map(|d| d.to_string()),
            ],
        )
        .map_err(|e| format!("insert basket run: {e}"))?;
        Ok(())
    }

    pub fn record_session_close(&self, rec: &BasketSessionCloseRecord<'_>) -> Result<(), String> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| "basket journal mutex poisoned".to_string())?;
        conn.execute(
            "INSERT INTO basket_session_closes (
                run_id, trading_day, status, closes_received, symbols_expected,
                active_baskets, admitted_baskets, excluded_baskets,
                gross_long, gross_short, gross_notional, gross_cap, net_notional,
                max_abs_leg, median_abs_leg, target_positions,
                current_positions_before, current_positions_after,
                buy_orders, sell_orders, buy_notional, sell_notional,
                order_gross_notional, order_max_notional, accepted_orders, failed_orders,
                target_gross, actual_gross, divergence_pct,
                selected_baskets_json, excluded_baskets_json,
                current_shares_before_json, target_shares_json, current_shares_after_json,
                error_text, created_at_utc
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5,
                ?6, ?7, ?8,
                ?9, ?10, ?11, ?12, ?13,
                ?14, ?15, ?16,
                ?17, ?18,
                ?19, ?20, ?21, ?22,
                ?23, ?24, ?25, ?26,
                ?27, ?28, ?29,
                ?30, ?31, ?32, ?33, ?34,
                ?35, ?36
            )",
            params![
                rec.run_id,
                rec.trading_day.to_string(),
                rec.status,
                rec.closes_received as i64,
                rec.symbols_expected as i64,
                rec.active_baskets as i64,
                rec.admitted_baskets as i64,
                rec.excluded_baskets as i64,
                rec.gross_long,
                rec.gross_short,
                rec.gross_notional,
                rec.gross_cap,
                rec.net_notional,
                rec.max_abs_leg,
                rec.median_abs_leg,
                rec.target_positions as i64,
                rec.current_positions_before as i64,
                rec.current_positions_after as i64,
                rec.buy_orders as i64,
                rec.sell_orders as i64,
                rec.buy_notional,
                rec.sell_notional,
                rec.order_gross_notional,
                rec.order_max_notional,
                rec.accepted_orders as i64,
                rec.failed_orders as i64,
                rec.target_gross,
                rec.actual_gross,
                rec.divergence_pct,
                rec.selected_baskets_json,
                rec.excluded_baskets_json,
                rec.current_shares_before_json,
                rec.target_shares_json,
                rec.current_shares_after_json,
                rec.error_text,
                Utc::now().to_rfc3339(),
            ],
        )
        .map_err(|e| format!("insert basket session close: {e}"))?;
        Ok(())
    }

    pub fn record_order_event(&self, rec: &BasketOrderEvent<'_>) -> Result<(), String> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| "basket journal mutex poisoned".to_string())?;
        conn.execute(
            "INSERT INTO basket_order_events (
                run_id, trading_day, seq, symbol, side, requested_qty, intended_notional,
                reason, basket_id, broker_order_id, broker_status, submission_status,
                error_text, created_at_utc
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                rec.run_id,
                rec.trading_day.to_string(),
                rec.seq as i64,
                rec.symbol,
                rec.side,
                rec.requested_qty,
                rec.intended_notional,
                rec.reason,
                rec.basket_id,
                rec.broker_order_id,
                rec.broker_status,
                rec.submission_status,
                rec.error_text,
                Utc::now().to_rfc3339(),
            ],
        )
        .map_err(|e| format!("insert basket order event: {e}"))?;
        Ok(())
    }

    pub fn record_picker_decision(
        &self,
        rec: &BasketPickerDecisionRecord<'_>,
    ) -> Result<(), String> {
        let conn = self
            .conn
            .lock()
            .map_err(|_| "basket journal mutex poisoned".to_string())?;
        if self.legacy_picker_scale_column {
            conn.execute(
                "INSERT INTO basket_picker_decisions (
                    run_id, trading_day, picker_id, mode, reason,
                    active_sectors_json, active_symbols_json,
                    leadership_short_conflict_ratio, strategy_return_20d,
                    strategy_drawdown_20d, baseline_scale_if_sleeve,
                    basket_only_scale_if_sleeve, sleeve_leverage_scale, created_at_utc
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
                params![
                    rec.run_id,
                    rec.trading_day.to_string(),
                    rec.picker_id,
                    rec.mode,
                    rec.reason,
                    rec.active_sectors_json,
                    rec.active_symbols_json,
                    rec.leadership_short_conflict_ratio,
                    rec.strategy_return_20d,
                    rec.strategy_drawdown_20d,
                    rec.basket_only_scale_if_sleeve,
                    rec.basket_only_scale_if_sleeve,
                    rec.sleeve_leverage_scale,
                    Utc::now().to_rfc3339(),
                ],
            )
        } else {
            conn.execute(
                "INSERT INTO basket_picker_decisions (
                    run_id, trading_day, picker_id, mode, reason,
                    active_sectors_json, active_symbols_json,
                    leadership_short_conflict_ratio, strategy_return_20d,
                    strategy_drawdown_20d, basket_only_scale_if_sleeve,
                    sleeve_leverage_scale, created_at_utc
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                params![
                    rec.run_id,
                    rec.trading_day.to_string(),
                    rec.picker_id,
                    rec.mode,
                    rec.reason,
                    rec.active_sectors_json,
                    rec.active_symbols_json,
                    rec.leadership_short_conflict_ratio,
                    rec.strategy_return_20d,
                    rec.strategy_drawdown_20d,
                    rec.basket_only_scale_if_sleeve,
                    rec.sleeve_leverage_scale,
                    Utc::now().to_rfc3339(),
                ],
            )
        }
        .map_err(|e| format!("insert basket picker decision: {e}"))?;
        Ok(())
    }
}

pub fn serialize_string_vec(values: &[String]) -> String {
    serde_json::to_string(values).unwrap_or_else(|_| "[]".to_string())
}

pub fn serialize_shares_map(values: &HashMap<String, f64>) -> String {
    serde_json::to_string(values).unwrap_or_else(|_| "{}".to_string())
}

fn table_has_column(conn: &Connection, table: &str, column: &str) -> Result<bool, rusqlite::Error> {
    let pragma = format!("PRAGMA table_info({table})");
    let mut stmt = conn.prepare(&pragma)?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let name: String = row.get(1)?;
        if name == column {
            return Ok(true);
        }
    }
    Ok(false)
}

fn migrate_picker_decisions_schema(conn: &Connection) -> Result<(), rusqlite::Error> {
    let has_new = table_has_column(
        conn,
        "basket_picker_decisions",
        "basket_only_scale_if_sleeve",
    )?;
    if has_new {
        return Ok(());
    }

    let has_old = table_has_column(conn, "basket_picker_decisions", "baseline_scale_if_sleeve")?;
    if !has_old {
        return Ok(());
    }

    conn.execute(
        "ALTER TABLE basket_picker_decisions
         ADD COLUMN basket_only_scale_if_sleeve REAL NOT NULL DEFAULT 1.0",
        [],
    )?;
    conn.execute(
        "UPDATE basket_picker_decisions
            SET basket_only_scale_if_sleeve = baseline_scale_if_sleeve",
        [],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn basket_journal_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let db = tmp.path().join("basket.db");
        let journal = BasketJournal::open(&db).unwrap();
        journal
            .record_run(&BasketRunRecord {
                run_id: "run-1",
                started_at_utc: Utc::now(),
                execution_mode: "PAPER",
                universe_path: "config/basket_universe_v1.toml",
                fit_artifact_path: Some("config/basket_universe_v1.fits.json"),
                state_path: "data/state.json",
                startup_phase: "intraday",
                symbols: 42,
                baskets: 10,
                capital: 10_000.0,
                leverage: 4.0,
                n_active_baskets: 15,
                broker_positions: 0,
                last_processed_trading_day: None,
            })
            .unwrap();
        journal
            .record_order_event(&BasketOrderEvent {
                run_id: "run-1",
                trading_day: NaiveDate::from_ymd_opt(2026, 4, 28).unwrap(),
                seq: 1,
                symbol: "AAPL",
                side: "buy",
                requested_qty: 3.0,
                intended_notional: Some(600.0),
                reason: "aggregated",
                basket_id: None,
                broker_order_id: Some("oid"),
                broker_status: Some("accepted"),
                submission_status: "accepted",
                error_text: None,
            })
            .unwrap();
        journal
            .record_picker_decision(&BasketPickerDecisionRecord {
                run_id: "run-1",
                trading_day: NaiveDate::from_ymd_opt(2026, 4, 28).unwrap(),
                picker_id: "rule_v1",
                mode: "basket_only",
                reason: "no_leadership",
                active_sectors_json: "[]".to_string(),
                active_symbols_json: "[]".to_string(),
                leadership_short_conflict_ratio: 0.0,
                strategy_return_20d: 0.0,
                strategy_drawdown_20d: 0.0,
                basket_only_scale_if_sleeve: 1.0,
                sleeve_leverage_scale: 1.0,
            })
            .unwrap();
        journal
            .record_session_close(&BasketSessionCloseRecord {
                run_id: "run-1",
                trading_day: NaiveDate::from_ymd_opt(2026, 4, 28).unwrap(),
                status: "ok",
                closes_received: 42,
                symbols_expected: 42,
                active_baskets: 10,
                admitted_baskets: 10,
                excluded_baskets: 0,
                gross_long: 20_000.0,
                gross_short: -20_000.0,
                gross_notional: 40_000.0,
                gross_cap: 40_000.0,
                net_notional: 0.0,
                max_abs_leg: 2_000.0,
                median_abs_leg: 900.0,
                target_positions: 20,
                current_positions_before: 0,
                current_positions_after: 20,
                buy_orders: 10,
                sell_orders: 10,
                buy_notional: 10_000.0,
                sell_notional: 10_000.0,
                order_gross_notional: 20_000.0,
                order_max_notional: 2_000.0,
                accepted_orders: 20,
                failed_orders: 0,
                target_gross: Some(40_000.0),
                actual_gross: Some(39_500.0),
                divergence_pct: Some(1.25),
                selected_baskets_json: "[\"a\"]".to_string(),
                excluded_baskets_json: "[]".to_string(),
                current_shares_before_json: "{}".to_string(),
                target_shares_json: "{\"AAPL\":3}".to_string(),
                current_shares_after_json: Some("{\"AAPL\":3}".to_string()),
                error_text: None,
            })
            .unwrap();

        let conn = Connection::open(db).unwrap();
        let runs: i64 = conn
            .query_row("SELECT COUNT(*) FROM basket_runs", [], |r| r.get(0))
            .unwrap();
        let sessions: i64 = conn
            .query_row("SELECT COUNT(*) FROM basket_session_closes", [], |r| {
                r.get(0)
            })
            .unwrap();
        let orders: i64 = conn
            .query_row("SELECT COUNT(*) FROM basket_order_events", [], |r| r.get(0))
            .unwrap();
        let decisions: i64 = conn
            .query_row("SELECT COUNT(*) FROM basket_picker_decisions", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(runs, 1);
        assert_eq!(sessions, 1);
        assert_eq!(orders, 1);
        assert_eq!(decisions, 1);
    }

    #[test]
    fn basket_journal_migrates_legacy_picker_column() {
        let dir = tempdir().unwrap();
        let db = dir.path().join("legacy.sqlite3");
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "
CREATE TABLE basket_picker_decisions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    run_id TEXT NOT NULL,
    trading_day TEXT NOT NULL,
    picker_id TEXT NOT NULL,
    mode TEXT NOT NULL,
    reason TEXT NOT NULL,
    active_sectors_json TEXT NOT NULL,
    active_symbols_json TEXT NOT NULL,
    leadership_short_conflict_ratio REAL NOT NULL,
    strategy_return_20d REAL NOT NULL,
    strategy_drawdown_20d REAL NOT NULL,
    baseline_scale_if_sleeve REAL NOT NULL,
    sleeve_leverage_scale REAL NOT NULL DEFAULT 1.0,
    created_at_utc TEXT NOT NULL
);",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO basket_picker_decisions (
                run_id, trading_day, picker_id, mode, reason, active_sectors_json,
                active_symbols_json, leadership_short_conflict_ratio,
                strategy_return_20d, strategy_drawdown_20d, baseline_scale_if_sleeve,
                sleeve_leverage_scale, created_at_utc
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                "legacy-run",
                "2026-04-28",
                "rule_v1",
                "baseline",
                "legacy",
                "[]",
                "[]",
                0.0_f64,
                0.01_f64,
                0.02_f64,
                0.75_f64,
                1.0_f64,
                Utc::now().to_rfc3339(),
            ],
        )
        .unwrap();
        drop(conn);

        let journal = BasketJournal::open(&db).unwrap();
        journal
            .record_picker_decision(&BasketPickerDecisionRecord {
                run_id: "new-run",
                trading_day: NaiveDate::from_ymd_opt(2026, 4, 29).unwrap(),
                picker_id: "rule_v1",
                mode: "basket_only",
                reason: "post_migration",
                active_sectors_json: "[]".to_string(),
                active_symbols_json: "[]".to_string(),
                leadership_short_conflict_ratio: 0.0,
                strategy_return_20d: 0.0,
                strategy_drawdown_20d: 0.0,
                basket_only_scale_if_sleeve: 0.9,
                sleeve_leverage_scale: 1.0,
            })
            .unwrap();

        let conn = journal.conn.lock().unwrap();
        let migrated: f64 = conn
            .query_row(
                "SELECT basket_only_scale_if_sleeve
                   FROM basket_picker_decisions
                  WHERE run_id = 'legacy-run'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!((migrated - 0.75).abs() < 1e-9);
    }
}
