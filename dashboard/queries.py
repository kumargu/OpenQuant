"""
OpenQuant data queries — reusable SQL + pandas layer for journal analysis.

This module is UI-agnostic: it returns DataFrames from SQLite journals.
Use it from Streamlit, Grafana (via JSON API), CLI scripts, or notebooks.

To switch to Grafana later:
  1. Point Grafana's SQLite datasource at data/journal/*.db
  2. Copy the SQL from each function's docstring into a Grafana panel
  3. The pandas post-processing (regime classification, forward returns, etc.)
     can be replicated with Grafana transformations or kept as a thin API layer
"""

import sqlite3
from datetime import datetime, timezone

import numpy as np
import pandas as pd


# ---------------------------------------------------------------------------
# Connection helper
# ---------------------------------------------------------------------------

def connect(db_path: str) -> sqlite3.Connection:
    """Open a read-only SQLite connection."""
    return sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)


# ---------------------------------------------------------------------------
# Raw table loaders
# ---------------------------------------------------------------------------

# SQL: SELECT id, symbol, timestamp, open, high, low, close, volume FROM bars ORDER BY timestamp
def load_bars(conn: sqlite3.Connection) -> pd.DataFrame:
    """Load all OHLCV bars from journal."""
    df = pd.read_sql_query(
        "SELECT id, symbol, timestamp, open, high, low, close, volume FROM bars ORDER BY timestamp",
        conn,
    )
    df["datetime"] = df["timestamp"].apply(_ts_to_dt)
    return df


# SQL: SELECT f.*, b.symbol, b.timestamp, b.close FROM features f JOIN bars b ON f.bar_id = b.id ORDER BY b.timestamp
def load_features(conn: sqlite3.Connection) -> pd.DataFrame:
    """Load computed features joined with bar metadata."""
    df = pd.read_sql_query(
        """SELECT f.*, b.symbol, b.timestamp, b.close
        FROM features f JOIN bars b ON f.bar_id = b.id
        ORDER BY b.timestamp""",
        conn,
    )
    df["datetime"] = df["timestamp"].apply(_ts_to_dt)
    return df


# SQL: SELECT d.*, b.symbol, b.timestamp, b.close, b.volume FROM decisions d JOIN bars b ON d.bar_id = b.id ORDER BY b.timestamp
def load_decisions(conn: sqlite3.Connection) -> pd.DataFrame:
    """Load signal decisions joined with bar metadata."""
    df = pd.read_sql_query(
        """SELECT d.*, b.symbol, b.timestamp, b.close, b.volume
        FROM decisions d JOIN bars b ON d.bar_id = b.id
        ORDER BY b.timestamp""",
        conn,
    )
    df["datetime"] = df["timestamp"].apply(_ts_to_dt)
    return df


# SQL: SELECT t.*, ef.symbol, ef.fill_price AS entry_price, xf.fill_price AS exit_price, ef.side AS entry_side
#      FROM trades t JOIN fills ef ON t.entry_fill_id = ef.id JOIN fills xf ON t.exit_fill_id = xf.id ORDER BY t.id
def load_trades(conn: sqlite3.Connection) -> pd.DataFrame:
    """Load round-trip trades with entry/exit prices."""
    return pd.read_sql_query(
        """SELECT t.*, ef.symbol, ef.fill_price AS entry_price, xf.fill_price AS exit_price,
                  ef.side AS entry_side
        FROM trades t
        JOIN fills ef ON t.entry_fill_id = ef.id
        JOIN fills xf ON t.exit_fill_id = xf.id
        ORDER BY t.id""",
        conn,
    )


# SQL: SELECT f.*, b.timestamp, b.close AS bar_close FROM fills f LEFT JOIN bars b ON f.bar_id = b.id ORDER BY f.id
def load_fills(conn: sqlite3.Connection) -> pd.DataFrame:
    """Load order fills with bar context."""
    return pd.read_sql_query(
        """SELECT f.*, b.timestamp, b.close AS bar_close
        FROM fills f LEFT JOIN bars b ON f.bar_id = b.id
        ORDER BY f.id""",
        conn,
    )


# ---------------------------------------------------------------------------
# Derived queries
# ---------------------------------------------------------------------------

def signals(conn: sqlite3.Connection, symbols: list[str] | None = None) -> pd.DataFrame:
    """
    Load only rows where a signal fired, with strategy name extracted.

    SQL (base):
        SELECT d.*, b.symbol, b.timestamp, b.close, b.volume
        FROM decisions d JOIN bars b ON d.bar_id = b.id
        WHERE d.signal_fired = 1
        ORDER BY b.timestamp
    """
    df = load_decisions(conn)
    df = df[df["signal_fired"] == 1].copy()
    if symbols:
        df = df[df["symbol"].isin(symbols)]
    df["strategy"] = df["signal_reason"].apply(extract_strategy)
    return df


def strategy_summary(conn: sqlite3.Connection, symbols: list[str] | None = None) -> pd.DataFrame:
    """
    Per-strategy signal count, buy/sell split, avg score, risk pass rate.

    Grafana equivalent: GROUP BY on decisions table with signal_reason parsing.
    """
    sigs = signals(conn, symbols)
    if sigs.empty:
        return pd.DataFrame()
    rows = []
    for strat, grp in sigs.groupby("strategy"):
        rows.append({
            "strategy": strat,
            "signals": len(grp),
            "buys": len(grp[grp["signal_side"] == "buy"]),
            "sells": len(grp[grp["signal_side"] == "sell"]),
            "avg_score": round(grp["signal_score"].mean(), 3),
            "risk_pass_pct": round(grp["risk_passed"].mean() * 100, 1),
        })
    return pd.DataFrame(rows).sort_values("signals", ascending=False)


def trade_summary(conn: sqlite3.Connection) -> pd.DataFrame:
    """
    Per-strategy round-trip trade metrics: win rate, P&L, Sharpe, profit factor.

    Grafana equivalent: GROUP BY on trades table with exit_reason parsing.
    """
    trades = load_trades(conn)
    if trades.empty:
        return pd.DataFrame()
    trades["strategy"] = trades["exit_reason"].apply(extract_strategy)
    rows = []
    for strat, grp in trades.groupby("strategy"):
        wins = grp[grp["pnl"] > 0]
        losses = grp[grp["pnl"] <= 0]
        gross_profit = wins["pnl"].sum() if not wins.empty else 0
        gross_loss = abs(losses["pnl"].sum()) if not losses.empty else 0
        rets = grp["return_pct"]
        sharpe = (rets.mean() / rets.std() * np.sqrt(252 * 390)) if rets.std() > 0 else 0
        rows.append({
            "strategy": strat,
            "trades": len(grp),
            "win_rate": len(wins) / len(grp) if len(grp) > 0 else 0,
            "total_pnl": grp["pnl"].sum(),
            "avg_pnl": grp["pnl"].mean(),
            "profit_factor": gross_profit / gross_loss if gross_loss > 0 else float("inf"),
            "sharpe": sharpe,
            "avg_bars_held": grp["bars_held"].mean(),
        })
    return pd.DataFrame(rows).sort_values("trades", ascending=False)


def signal_decay(
    conn: sqlite3.Connection, symbols: list[str] | None = None, forward_bars: int = 10,
) -> pd.DataFrame:
    """
    For each buy signal, compute the forward return after N bars.
    Used to test whether higher-score signals produce better returns.

    Returns DataFrame with columns: score, strategy, symbol, forward_return_pct
    """
    sigs = signals(conn, symbols)
    buys = sigs[sigs["signal_side"] == "buy"]
    bars = load_bars(conn)
    if buys.empty:
        return pd.DataFrame()

    rows = []
    for _, sig in buys.iterrows():
        after = bars[(bars["symbol"] == sig["symbol"]) & (bars["timestamp"] > sig["timestamp"])].head(forward_bars)
        if len(after) >= forward_bars:
            fwd_ret = (after.iloc[-1]["close"] - sig["close"]) / sig["close"] * 100
            rows.append({
                "score": sig["signal_score"],
                "strategy": sig["strategy"],
                "symbol": sig["symbol"],
                "forward_return_pct": fwd_ret,
            })
    return pd.DataFrame(rows)


def signals_by_hour(conn: sqlite3.Connection, symbols: list[str] | None = None) -> pd.DataFrame:
    """
    Signal count and avg score grouped by hour (UTC).

    SQL (Grafana):
        SELECT CAST(strftime('%H', datetime(timestamp/1000, 'unixepoch')) AS INTEGER) AS hour,
               COUNT(*) AS count, AVG(signal_score) AS avg_score
        FROM decisions d JOIN bars b ON d.bar_id = b.id
        WHERE d.signal_fired = 1
        GROUP BY hour ORDER BY hour
    """
    sigs = signals(conn, symbols)
    if sigs.empty:
        return pd.DataFrame()
    sigs["hour"] = sigs["datetime"].dt.hour
    return sigs.groupby("hour").agg(
        count=("signal_score", "count"),
        avg_score=("signal_score", "mean"),
    ).reset_index()


def signals_by_hour_strategy(conn: sqlite3.Connection, symbols: list[str] | None = None) -> pd.DataFrame:
    """Pivot: hour x strategy -> signal count. For heatmap display."""
    sigs = signals(conn, symbols)
    if sigs.empty:
        return pd.DataFrame()
    sigs["hour"] = sigs["datetime"].dt.hour
    return sigs.groupby(["hour", "strategy"]).size().unstack(fill_value=0)


def regime_classification(conn: sqlite3.Connection, symbol: str) -> pd.DataFrame:
    """
    Classify each bar into a market regime based on ADX.

    Regime thresholds:
      ADX < 15  -> ranging
      15 <= ADX <= 25 -> ranging (weak)
      ADX > 25  -> trending
      ADX > 40  -> strong-trend
    """
    feats = load_features(conn)
    feats = feats[feats["symbol"] == symbol].copy()
    if feats.empty or "adx" not in feats.columns:
        return pd.DataFrame()
    feats = feats.set_index("datetime").sort_index()
    feats["regime"] = "ranging"
    feats.loc[feats["adx"] > 25, "regime"] = "trending"
    feats.loc[feats["adx"] > 40, "regime"] = "strong-trend"
    return feats


def feature_vs_forward_return(
    conn: sqlite3.Connection, symbol: str, feature_name: str, forward_bars: int = 10,
) -> pd.DataFrame:
    """
    Join a feature column with the forward N-bar return for predictive analysis.

    Returns DataFrame with columns: feature_value, forward_return_pct
    """
    feats = load_features(conn)
    feats = feats[feats["symbol"] == symbol]
    bars = load_bars(conn)
    bars = bars[bars["symbol"] == symbol].sort_values("timestamp")

    if feats.empty or feature_name not in feats.columns:
        return pd.DataFrame()

    bars["fwd_return_pct"] = (bars["close"].shift(-forward_bars) / bars["close"] - 1) * 100
    merged = pd.merge(
        feats[["bar_id", feature_name]],
        bars[["id", "fwd_return_pct"]].rename(columns={"id": "bar_id"}),
        on="bar_id",
    ).dropna()
    return merged.rename(columns={feature_name: "feature_value"})


def drawdown_series(conn: sqlite3.Connection) -> pd.DataFrame:
    """
    Compute cumulative P&L, peak, and drawdown from round-trip trades.

    Returns DataFrame with columns: cum_pnl, peak, drawdown, strategy
    """
    trades = load_trades(conn)
    if trades.empty:
        return pd.DataFrame()
    trades["strategy"] = trades["exit_reason"].apply(extract_strategy)
    trades["cum_pnl"] = trades["pnl"].cumsum()
    trades["peak"] = trades["cum_pnl"].cummax()
    trades["drawdown"] = trades["cum_pnl"] - trades["peak"]
    return trades


def strategy_pnl_breakdown(conn: sqlite3.Connection) -> pd.DataFrame:
    """Total P&L grouped by exit strategy. Positive = making money, negative = losing."""
    trades = load_trades(conn)
    if trades.empty:
        return pd.DataFrame()
    trades["strategy"] = trades["exit_reason"].apply(extract_strategy)
    return trades.groupby("strategy")["pnl"].sum().sort_values().reset_index()


def slippage_analysis(conn: sqlite3.Connection) -> pd.DataFrame:
    """
    Compute slippage (fill price vs bar close) for each fill.

    SQL (Grafana):
        SELECT f.symbol, f.fill_price, b.close AS bar_close,
               (f.fill_price - b.close) / b.close * 10000 AS slippage_bps
        FROM fills f JOIN bars b ON f.bar_id = b.id
    """
    fills = load_fills(conn)
    if fills.empty or "bar_close" not in fills.columns or "fill_price" not in fills.columns:
        return pd.DataFrame()
    fills["slippage_bps"] = (fills["fill_price"] - fills["bar_close"]) / fills["bar_close"] * 10000
    if "timestamp" in fills.columns:
        fills["datetime"] = fills["timestamp"].apply(_ts_to_dt)
    return fills


def conflict_analysis(conn: sqlite3.Connection, symbols: list[str] | None = None) -> dict:
    """
    Analyze strategy switching and side flips between consecutive signals.

    Returns dict with keys: signals_df, strategy_switches, side_flips, transition_matrix
    """
    sigs = signals(conn, symbols)
    if sigs.empty:
        return {"signals_df": sigs, "strategy_switches": 0, "side_flips": 0, "transition_matrix": pd.DataFrame()}

    sigs = sigs.sort_values(["symbol", "timestamp"])
    sigs["prev_strategy"] = sigs.groupby("symbol")["strategy"].shift(1)
    sigs["prev_side"] = sigs.groupby("symbol")["signal_side"].shift(1)
    sigs["is_switch"] = (sigs["strategy"] != sigs["prev_strategy"]) & sigs["prev_strategy"].notna()
    sigs["is_flip"] = (sigs["signal_side"] != sigs["prev_side"]) & sigs["prev_side"].notna()

    switches = sigs[sigs["is_switch"]]
    transition = switches.groupby(["prev_strategy", "strategy"]).size().unstack(fill_value=0) if not switches.empty else pd.DataFrame()

    return {
        "signals_df": sigs,
        "strategy_switches": int(sigs["is_switch"].sum()),
        "side_flips": int(sigs["is_flip"].sum()),
        "transition_matrix": transition,
    }


# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------

def _ts_to_dt(ts_ms: int) -> datetime:
    return datetime.fromtimestamp(ts_ms / 1000, tz=timezone.utc)


def extract_strategy(reason: str) -> str:
    """Parse the strategy name from a signal_reason or exit_reason string."""
    if pd.isna(reason):
        return "unknown"
    r = reason.lower()
    if "mean-reversion" in r or "mean_reversion" in r:
        return "mean-reversion"
    if "momentum" in r:
        return "momentum"
    if "vwap" in r:
        return "vwap-reversion"
    if "breakout" in r:
        return "breakout"
    if "stop loss" in r:
        return "stop-loss"
    if "take profit" in r:
        return "take-profit"
    if "max hold" in r:
        return "max-hold"
    return reason.split(":")[0].strip()
