//! SQLite journal schema — append-only audit trail of every engine decision.
//!
//! Tables:
//!   bars     — raw OHLCV data as received
//!   features — computed feature snapshot at each bar
//!   decisions — signal + risk gate outcome per bar
//!   fills    — executed trades with prices and slippage
//!   trades   — round-trip trade records (entry->exit linked)

use rusqlite::Connection;

/// SQL statements to create the journal schema.
pub const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS bars (
    id        INTEGER PRIMARY KEY AUTOINCREMENT,
    symbol    TEXT NOT NULL,
    timestamp INTEGER NOT NULL,
    open      REAL NOT NULL,
    high      REAL NOT NULL,
    low       REAL NOT NULL,
    close     REAL NOT NULL,
    volume    REAL NOT NULL
);

CREATE TABLE IF NOT EXISTS features (
    bar_id          INTEGER NOT NULL REFERENCES bars(id),
    return_1        REAL NOT NULL,
    return_5        REAL NOT NULL,
    return_20       REAL NOT NULL,
    sma_20          REAL NOT NULL,
    sma_50          REAL NOT NULL,
    atr             REAL NOT NULL,
    return_std_20   REAL NOT NULL,
    return_z_score  REAL NOT NULL,
    relative_volume REAL NOT NULL,
    bar_range       REAL NOT NULL,
    close_location  REAL NOT NULL,
    trend_up        INTEGER NOT NULL,
    warmed_up       INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS decisions (
    bar_id          INTEGER NOT NULL REFERENCES bars(id),
    signal_fired    INTEGER NOT NULL,
    signal_side     TEXT,
    signal_score    REAL,
    signal_reason   TEXT,
    risk_passed     INTEGER,
    risk_rejection  TEXT,
    qty_approved    REAL,
    engine_version  TEXT
);

CREATE TABLE IF NOT EXISTS fills (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    bar_id          INTEGER REFERENCES bars(id),
    symbol          TEXT NOT NULL,
    side            TEXT NOT NULL,
    qty             REAL NOT NULL,
    fill_price      REAL NOT NULL,
    slippage        REAL NOT NULL DEFAULT 0.0,
    entry_bar_id    INTEGER,
    engine_version  TEXT
);

CREATE TABLE IF NOT EXISTS trades (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    symbol          TEXT NOT NULL,
    entry_fill_id   INTEGER NOT NULL REFERENCES fills(id),
    exit_fill_id    INTEGER NOT NULL REFERENCES fills(id),
    pnl             REAL NOT NULL,
    return_pct      REAL NOT NULL,
    bars_held       INTEGER NOT NULL,
    entry_z_score   REAL,
    entry_rel_volume REAL,
    exit_reason     TEXT,
    engine_version  TEXT
);

CREATE INDEX IF NOT EXISTS idx_bars_symbol_ts ON bars(symbol, timestamp);
CREATE INDEX IF NOT EXISTS idx_decisions_signal ON decisions(signal_fired);
CREATE INDEX IF NOT EXISTS idx_fills_symbol ON fills(symbol);
CREATE INDEX IF NOT EXISTS idx_trades_symbol ON trades(symbol);
";

/// Initialize journal database with schema.
pub fn init(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(SCHEMA)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_creates_tables() {
        let conn = Connection::open_in_memory().unwrap();
        init(&conn).unwrap();

        // Verify tables exist
        let tables: Vec<String> = conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();

        assert!(tables.contains(&"bars".to_string()));
        assert!(tables.contains(&"features".to_string()));
        assert!(tables.contains(&"decisions".to_string()));
        assert!(tables.contains(&"fills".to_string()));
        assert!(tables.contains(&"trades".to_string()));
    }

    #[test]
    fn schema_is_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        init(&conn).unwrap();
        init(&conn).unwrap(); // second call should not fail
    }
}
