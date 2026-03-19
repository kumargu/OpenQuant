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

/// Initialize journal database with schema, then apply migrations for upgrades.
pub fn init(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(SCHEMA)?;
    migrate(conn)?;
    Ok(())
}

/// Apply forward-only migrations for columns added after the initial schema.
/// Each ALTER TABLE uses IF NOT EXISTS–style guards (SQLite errors on duplicate
/// column adds, so we catch and ignore "duplicate column name" errors).
fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    let new_columns = [
        // V1 migrations
        ("features", "sma_50", "REAL NOT NULL DEFAULT 0.0"),
        ("features", "atr", "REAL NOT NULL DEFAULT 0.0"),
        ("features", "trend_up", "INTEGER NOT NULL DEFAULT 0"),
        // V2 migrations: momentum features
        ("features", "ema_fast", "REAL NOT NULL DEFAULT 0.0"),
        ("features", "ema_slow", "REAL NOT NULL DEFAULT 0.0"),
        (
            "features",
            "ema_fast_above_slow",
            "INTEGER NOT NULL DEFAULT 0",
        ),
        ("features", "adx", "REAL NOT NULL DEFAULT 0.0"),
        ("features", "plus_di", "REAL NOT NULL DEFAULT 0.0"),
        ("features", "minus_di", "REAL NOT NULL DEFAULT 0.0"),
        ("features", "bollinger_upper", "REAL NOT NULL DEFAULT 0.0"),
        ("features", "bollinger_lower", "REAL NOT NULL DEFAULT 0.0"),
        ("features", "bollinger_pct_b", "REAL NOT NULL DEFAULT 0.0"),
        (
            "features",
            "bollinger_bandwidth",
            "REAL NOT NULL DEFAULT 0.0",
        ),
        // V5 migrations: GARCH volatility
        ("features", "garch_vol", "REAL NOT NULL DEFAULT 0.0"),
    ];

    for (table, column, col_type) in &new_columns {
        let sql = format!("ALTER TABLE {table} ADD COLUMN {column} {col_type}");
        match conn.execute_batch(&sql) {
            Ok(()) => {}
            Err(e) if e.to_string().contains("duplicate column name") => {
                // Column already exists (fresh schema or previously migrated)
            }
            Err(e) => return Err(e),
        }
    }

    Ok(())
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

    #[test]
    fn migrate_v2_adds_momentum_columns() {
        let conn = Connection::open_in_memory().unwrap();

        // Simulate V1 schema (has sma_50, atr, trend_up but not V2 columns)
        conn.execute_batch(
            "
            CREATE TABLE features (
                bar_id          INTEGER NOT NULL,
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
        ",
        )
        .unwrap();

        // Migration should add V2 columns
        migrate(&conn).unwrap();

        // Verify we can insert with the new V2 columns
        conn.execute(
            "INSERT INTO features (bar_id, return_1, return_5, return_20, sma_20, sma_50, atr,
             return_std_20, return_z_score, relative_volume, bar_range, close_location,
             trend_up, warmed_up, ema_fast, ema_slow, ema_fast_above_slow, adx, plus_di, minus_di,
             bollinger_upper, bollinger_lower, bollinger_pct_b, bollinger_bandwidth)
             VALUES (1, 0.0, 0.0, 0.0, 100.0, 99.5, 1.5, 0.01, 0.5, 1.2, 2.0, 0.75,
                     1, 1, 100.5, 99.8, 1, 25.0, 30.0, 15.0, 102.0, 98.0, 0.6, 0.04)",
            [],
        )
        .unwrap();
    }

    #[test]
    fn migrate_adds_columns_to_old_schema() {
        let conn = Connection::open_in_memory().unwrap();

        // Simulate the old schema without sma_50, atr, trend_up
        conn.execute_batch(
            "
            CREATE TABLE features (
                bar_id          INTEGER NOT NULL,
                return_1        REAL NOT NULL,
                return_5        REAL NOT NULL,
                return_20       REAL NOT NULL,
                sma_20          REAL NOT NULL,
                return_std_20   REAL NOT NULL,
                return_z_score  REAL NOT NULL,
                relative_volume REAL NOT NULL,
                bar_range       REAL NOT NULL,
                close_location  REAL NOT NULL,
                warmed_up       INTEGER NOT NULL
            );
        ",
        )
        .unwrap();

        // Migration should add the 3 missing columns
        migrate(&conn).unwrap();

        // Verify we can insert with the new columns
        conn.execute(
            "INSERT INTO features (bar_id, return_1, return_5, return_20, sma_20,
             return_std_20, return_z_score, relative_volume, bar_range, close_location,
             warmed_up, sma_50, atr, trend_up)
             VALUES (1, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1, 99.5, 1.5, 1)",
            [],
        )
        .unwrap();
    }
}
