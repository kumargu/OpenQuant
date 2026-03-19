"""
Live Trade Monitor — Terminal UI for analyzing GLD/SLV trades in real-time.

Polls the journal SQLite database and displays:
- Live trade feed with color-coded BUY/SELL signals
- Hit/miss analysis (profitable vs unprofitable trades)
- Signal triggers and feature context
- Running statistics

Usage:
  python -m paper_trading.trade_monitor
  python -m paper_trading.trade_monitor --db data/journal/metals-2026-03-19.db
"""

import argparse
import os
import sqlite3
import sys
import time
from datetime import datetime, timezone
from pathlib import Path

# ── ANSI colors ──────────────────────────────────────────────────────────────
RESET = "\033[0m"
BOLD = "\033[1m"
DIM = "\033[2m"

GREEN = "\033[32m"
RED = "\033[31m"
YELLOW = "\033[33m"
CYAN = "\033[36m"
MAGENTA = "\033[35m"
WHITE = "\033[97m"
BLUE = "\033[34m"

BG_GREEN = "\033[42m"
BG_RED = "\033[41m"
BG_YELLOW = "\033[43m"
BG_BLUE = "\033[44m"

HIT_COLOR = f"{BOLD}{GREEN}"
MISS_COLOR = f"{BOLD}{RED}"
BUY_COLOR = f"{BOLD}{CYAN}"
SELL_COLOR = f"{BOLD}{MAGENTA}"
SIGNAL_COLOR = f"{YELLOW}"
HEADER_COLOR = f"{BOLD}{WHITE}"


def clear_screen():
    os.system("clear" if os.name != "nt" else "cls")


def colored_side(side: str) -> str:
    if side and side.lower() == "buy":
        return f"{BUY_COLOR}▲ BUY {RESET}"
    return f"{SELL_COLOR}▼ SELL{RESET}"


def colored_pnl(pnl: float) -> str:
    if pnl > 0:
        return f"{HIT_COLOR}+${pnl:.2f}{RESET}"
    elif pnl < 0:
        return f"{MISS_COLOR}-${abs(pnl):.2f}{RESET}"
    return f"${pnl:.2f}"


def colored_return(ret_pct: float) -> str:
    if ret_pct is None:
        return f"{DIM}--{RESET}"
    pct = ret_pct * 100
    if pct > 0:
        return f"{HIT_COLOR}+{pct:.3f}%{RESET}"
    elif pct < 0:
        return f"{MISS_COLOR}{pct:.3f}%{RESET}"
    return f"{pct:.3f}%"


def hit_miss_badge(pnl: float) -> str:
    if pnl > 0:
        return f"{BG_GREEN}{BOLD} HIT  {RESET}"
    elif pnl < 0:
        return f"{BG_RED}{BOLD} MISS {RESET}"
    return f"{BG_YELLOW}{BOLD} FLAT {RESET}"


def format_ts(ms: int) -> str:
    return datetime.fromtimestamp(ms / 1000, tz=timezone.utc).strftime("%H:%M:%S")


def table_has_rows(conn: sqlite3.Connection, table: str) -> bool:
    try:
        cur = conn.execute(f"SELECT 1 FROM {table} LIMIT 1")
        return cur.fetchone() is not None
    except sqlite3.OperationalError:
        return False


def get_completed_trades(conn: sqlite3.Connection, seen_ids: set):
    """Get round-trip trades from the trades table."""
    try:
        rows = conn.execute("""
            SELECT t.id, t.symbol, t.pnl, t.return_pct, t.bars_held, t.exit_reason,
                   t.entry_z_score, t.entry_rel_volume,
                   ef.fill_price as entry_price, xf.fill_price as exit_price,
                   ef.side as entry_side,
                   eb.timestamp as entry_ts, xb.timestamp as exit_ts,
                   d.signal_reason, d.signal_score
            FROM trades t
            JOIN fills ef ON t.entry_fill_id = ef.id
            JOIN fills xf ON t.exit_fill_id = xf.id
            JOIN bars eb ON ef.bar_id = eb.id
            JOIN bars xb ON xf.bar_id = xb.id
            LEFT JOIN decisions d ON ef.bar_id = d.bar_id
            WHERE t.id NOT IN ({})
            ORDER BY t.id ASC
        """.format(",".join("?" * len(seen_ids)) if seen_ids else "NULL"),
            list(seen_ids) if seen_ids else [],
        ).fetchall()
    except sqlite3.OperationalError:
        return []
    return rows


def get_open_signals(conn: sqlite3.Connection, last_bar_id: int):
    """Get recent signals (decisions where signal fired)."""
    try:
        rows = conn.execute("""
            SELECT b.id, b.symbol, b.timestamp, b.close, b.volume,
                   d.signal_side, d.signal_score, d.signal_reason,
                   d.risk_passed, d.risk_rejection, d.qty_approved,
                   f.return_z_score, f.relative_volume, f.market_regime,
                   f.atr, f.adx, f.garch_vol_percentile, f.trend_up
            FROM bars b
            JOIN decisions d ON b.id = d.bar_id
            LEFT JOIN features f ON b.id = f.bar_id
            WHERE b.id > ? AND d.signal_fired = 1
            ORDER BY b.id ASC
        """, (last_bar_id,)).fetchall()
    except sqlite3.OperationalError:
        return []
    return rows


def get_recent_fills(conn: sqlite3.Connection, last_fill_id: int):
    """Get new fills since last check."""
    try:
        rows = conn.execute("""
            SELECT fl.id, fl.symbol, fl.side, fl.qty, fl.fill_price, fl.slippage,
                   b.timestamp, b.close,
                   d.signal_reason, d.signal_score,
                   f.return_z_score, f.relative_volume, f.market_regime,
                   f.atr, f.trend_up
            FROM fills fl
            JOIN bars b ON fl.bar_id = b.id
            LEFT JOIN decisions d ON fl.bar_id = d.bar_id
            LEFT JOIN features f ON fl.bar_id = f.bar_id
            WHERE fl.id > ?
            ORDER BY fl.id ASC
        """, (last_fill_id,)).fetchall()
    except sqlite3.OperationalError:
        return []
    return rows


def get_recent_signal_history(conn: sqlite3.Connection):
    """Get last 5 signals (fired=1) for display."""
    try:
        return conn.execute("""
            SELECT b.id, b.symbol, b.timestamp, b.close, b.volume,
                   d.signal_side, d.signal_score, d.signal_reason,
                   d.risk_passed, d.risk_rejection, d.qty_approved,
                   f.return_z_score, f.relative_volume, f.market_regime,
                   f.atr, f.adx, f.garch_vol_percentile, f.trend_up
            FROM bars b
            JOIN decisions d ON b.id = d.bar_id
            LEFT JOIN features f ON b.id = f.bar_id
            WHERE d.signal_fired = 1
            ORDER BY b.id DESC LIMIT 5
        """).fetchall()
    except sqlite3.OperationalError:
        return []


def get_bar_stats(conn: sqlite3.Connection):
    """Get overall bar counts per symbol."""
    try:
        rows = conn.execute("""
            SELECT symbol, COUNT(*) as cnt,
                   MIN(timestamp) as first_ts, MAX(timestamp) as last_ts
            FROM bars GROUP BY symbol
        """).fetchall()
    except sqlite3.OperationalError:
        return []
    return rows


def get_signal_stats(conn: sqlite3.Connection):
    """Get signal fire counts."""
    try:
        rows = conn.execute("""
            SELECT b.symbol, d.signal_side,
                   COUNT(*) as cnt,
                   SUM(CASE WHEN d.risk_passed = 1 THEN 1 ELSE 0 END) as passed,
                   SUM(CASE WHEN d.risk_passed = 0 THEN 1 ELSE 0 END) as rejected
            FROM decisions d
            JOIN bars b ON d.bar_id = b.id
            WHERE d.signal_fired = 1
            GROUP BY b.symbol, d.signal_side
        """).fetchall()
    except sqlite3.OperationalError:
        return []
    return rows


# ── Log file for hits/misses ────────────────────────────────────────────────
LOG_PATH = Path("data/journal/trade_analysis.log")


def log_event(event_type: str, msg: str):
    """Append any event to the analysis log."""
    LOG_PATH.parent.mkdir(parents=True, exist_ok=True)
    ts = datetime.now(timezone.utc).strftime("%Y-%m-%d %H:%M:%S")
    with open(LOG_PATH, "a") as f:
        f.write(f"{ts} | {event_type:8s} | {msg}\n")


def log_signal(sig_row):
    """Log a signal event."""
    bar_id, symbol, ts, close, volume, side, score, reason, \
        risk_passed, risk_rejection, qty, z_score, rvol, regime, \
        atr, adx, garch_pct, trend_up = sig_row
    t = format_ts(ts)
    status = "PASSED" if risk_passed else f"REJECTED({risk_rejection})"
    log_event("SIGNAL",
        f"{symbol} {side} @ ${close:.2f} | {t} | score={score or 0:.2f} | "
        f"z={z_score or 0:.2f} rvol={rvol or 0:.2f} regime={regime} | "
        f"{status} | {reason}")


def log_fill(fill_row):
    """Log a fill event."""
    fid, symbol, side, qty, price, slippage, ts, bar_close, \
        signal_reason, score, z_score, rvol, regime, atr, trend_up = fill_row
    t = format_ts(ts) if ts else "?"
    log_event("FILL",
        f"{symbol} {side} qty={qty} @ ${price:.2f} | {t} | "
        f"z={z_score or 0:.2f} rvol={rvol or 0:.2f} regime={regime or '?'} | "
        f"{signal_reason or '?'}")


def log_roundtrip(symbol, entry_price, exit_price, entry_side, pnl, verdict,
                  entry_reason, exit_reason, entry_z, entry_rvol):
    """Log a completed round-trip trade."""
    log_event(verdict,
        f"{symbol} {entry_side} ${entry_price:.2f} -> ${exit_price:.2f} | "
        f"pnl={pnl:+.2f} ({pnl/entry_price*100:+.3f}%) | "
        f"z={entry_z or 0:.2f} rvol={entry_rvol or 0:.2f} | "
        f"entry={entry_reason or '?'} exit={exit_reason or '?'}")


# ── Main monitor loop ───────────────────────────────────────────────────────

def print_header():
    print(f"\n{BOLD}{'═' * 90}{RESET}")
    print(f"{BOLD}  ⚡ OpenQuant Trade Monitor — GLD / SLV   {DIM}(Ctrl+C to exit){RESET}")
    print(f"{BOLD}{'═' * 90}{RESET}\n")


def print_stats_panel(conn: sqlite3.Connection, completed_trades: list):
    """Print summary statistics."""
    bar_stats = get_bar_stats(conn)
    sig_stats = get_signal_stats(conn)

    # Bar counts
    print(f"{HEADER_COLOR}┌─ Market Data ────────────────────────────────────────────────────────────────┐{RESET}")
    for symbol, cnt, first_ts, last_ts in bar_stats:
        first = format_ts(first_ts)
        last = format_ts(last_ts)
        print(f"  {BOLD}{symbol:5s}{RESET}  {cnt:>5d} bars  │  {first} → {last}")
    print(f"{HEADER_COLOR}└──────────────────────────────────────────────────────────────────────────────┘{RESET}")

    # Signal stats
    if sig_stats:
        print(f"\n{HEADER_COLOR}┌─ Signals ────────────────────────────────────────────────────────────────────┐{RESET}")
        for symbol, side, cnt, passed, rejected in sig_stats:
            side_str = colored_side(side)
            print(f"  {BOLD}{symbol:5s}{RESET}  {side_str} ×{cnt}  │  "
                  f"{GREEN}passed: {passed or 0}{RESET}  {RED}rejected: {rejected or 0}{RESET}")
        print(f"{HEADER_COLOR}└──────────────────────────────────────────────────────────────────────────────┘{RESET}")

    # Trade stats (round-trips: index 4 = pnl)
    if completed_trades:
        hits = [t for t in completed_trades if t[4] > 0]
        misses = [t for t in completed_trades if t[4] < 0]
        flats = [t for t in completed_trades if t[4] == 0]
        total_pnl = sum(t[4] for t in completed_trades)
        avg_win = sum(t[4] for t in hits) / len(hits) if hits else 0
        avg_loss = sum(t[4] for t in misses) / len(misses) if misses else 0

        print(f"\n{HEADER_COLOR}┌─ Trade Summary ──────────────────────────────────────────────────────────────┐{RESET}")
        print(f"  Total: {BOLD}{len(completed_trades)}{RESET}  │  "
              f"{HIT_COLOR}Hits: {len(hits)}{RESET}  │  "
              f"{MISS_COLOR}Misses: {len(misses)}{RESET}  │  "
              f"Flat: {len(flats)}")
        wr = len(hits) / len(completed_trades) * 100 if completed_trades else 0
        wr_color = HIT_COLOR if wr >= 50 else MISS_COLOR
        print(f"  Win Rate: {wr_color}{wr:.1f}%{RESET}  │  "
              f"Total P&L: {colored_pnl(total_pnl)}  │  "
              f"Avg Win: {colored_pnl(avg_win)}  │  Avg Loss: {colored_pnl(avg_loss)}")
        print(f"{HEADER_COLOR}└──────────────────────────────────────────────────────────────────────────────┘{RESET}")


def print_trade_row(trade_row):
    """Print a single completed trade."""
    tid, symbol, pnl, ret_pct, bars_held, exit_reason, entry_z, entry_rvol, \
        entry_price, exit_price, entry_side, entry_ts, exit_ts, signal_reason, score = trade_row

    badge = hit_miss_badge(pnl)
    side_str = colored_side(entry_side)
    entry_t = format_ts(entry_ts) if entry_ts else "?"
    exit_t = format_ts(exit_ts) if exit_ts else "?"

    print(f"  {badge} {side_str} {BOLD}{symbol:4s}{RESET} │ "
          f"${entry_price:.2f} → ${exit_price:.2f} │ "
          f"{colored_pnl(pnl)} ({colored_return(ret_pct)}) │ "
          f"{bars_held or 0} bars │ {entry_t}→{exit_t}")

    # Trigger details
    trigger = signal_reason or "unknown"
    z_str = f"z={entry_z:.2f}" if entry_z else ""
    rvol_str = f"rvol={entry_rvol:.2f}" if entry_rvol else ""
    score_str = f"score={score:.2f}" if score else ""
    exit_str = exit_reason or "?"

    details = "  ·  ".join(filter(None, [z_str, rvol_str, score_str]))
    print(f"         {SIGNAL_COLOR}trigger: {trigger}{RESET}")
    print(f"         {DIM}exit: {exit_str}  │  {details}{RESET}")
    print()


def print_fill_row(fill_row):
    """Print a new fill (entry or exit)."""
    fid, symbol, side, qty, price, slippage, ts, bar_close, \
        signal_reason, score, z_score, rvol, regime, atr, trend_up = fill_row

    side_str = colored_side(side)
    t = format_ts(ts) if ts else "?"
    regime_str = regime or "?"
    trend_icon = f"{GREEN}↑{RESET}" if trend_up else f"{RED}↓{RESET}"

    print(f"  {BG_BLUE}{BOLD} FILL {RESET} {side_str} {BOLD}{symbol:4s}{RESET} │ "
          f"qty={qty:.2f} @ ${price:.2f} │ {t}")

    trigger = signal_reason or "—"
    parts = []
    if z_score is not None:
        parts.append(f"z={z_score:.2f}")
    if rvol is not None:
        parts.append(f"rvol={rvol:.2f}")
    if score is not None:
        parts.append(f"score={score:.2f}")
    parts.append(f"regime={regime_str}")
    parts.append(f"trend={trend_icon}")
    if atr is not None:
        parts.append(f"atr={atr:.4f}")

    print(f"         {SIGNAL_COLOR}trigger: {trigger}{RESET}")
    print(f"         {DIM}{'  ·  '.join(parts)}{RESET}")
    if slippage and slippage != 0:
        print(f"         {DIM}slippage: ${slippage:.4f}{RESET}")
    print()


def print_signal_row(sig_row):
    """Print a signal event (may or may not result in fill)."""
    bar_id, symbol, ts, close, volume, side, score, reason, \
        risk_passed, risk_rejection, qty, z_score, rvol, regime, \
        atr, adx, garch_pct, trend_up = sig_row

    side_str = colored_side(side)
    t = format_ts(ts)
    trend_icon = f"{GREEN}↑{RESET}" if trend_up else f"{RED}↓{RESET}"

    if risk_passed:
        status = f"{GREEN}✓ PASSED{RESET}"
    else:
        status = f"{RED}✗ REJECTED{RESET}"

    print(f"  {SIGNAL_COLOR}⚡ SIGNAL{RESET} {side_str} {BOLD}{symbol:4s}{RESET} │ "
          f"${close:.2f} │ {t} │ {status}")

    parts = []
    if z_score is not None:
        parts.append(f"z={z_score:.2f}")
    if rvol is not None:
        parts.append(f"rvol={rvol:.2f}")
    if score is not None:
        parts.append(f"score={score:.2f}")
    if adx is not None:
        parts.append(f"adx={adx:.1f}")
    parts.append(f"regime={regime or '?'}")
    parts.append(f"trend={trend_icon}")

    print(f"         {DIM}{reason or '—'}{RESET}")
    print(f"         {DIM}{'  ·  '.join(parts)}{RESET}")
    if not risk_passed and risk_rejection:
        print(f"         {RED}rejection: {risk_rejection}{RESET}")
    if qty and qty > 0:
        print(f"         {DIM}qty_approved: {qty:.2f}{RESET}")
    print()


def monitor(db_path: str, poll_interval: int = 5):
    """Main monitor loop."""
    print(f"\n{DIM}Watching: {db_path}{RESET}")
    print(f"{DIM}Polling every {poll_interval}s — waiting for engine to start...{RESET}\n")

    seen_trade_ids: set = set()
    last_bar_id = 0
    last_fill_id = 0
    all_trades: list = []
    open_positions: dict = {}  # symbol -> entry fill row
    refresh_count = 0

    while True:
        try:
            if not os.path.exists(db_path):
                print(f"{DIM}[{datetime.now().strftime('%H:%M:%S')}] Waiting for journal DB...{RESET}", end="\r")
                time.sleep(poll_interval)
                continue

            conn = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
            conn.execute("PRAGMA journal_mode=wal")

            # Check if tables exist
            if not table_has_rows(conn, "bars"):
                conn.close()
                print(f"{DIM}[{datetime.now().strftime('%H:%M:%S')}] DB exists but no bars yet...{RESET}", end="\r")
                time.sleep(poll_interval)
                continue

            # ── New signals ──
            signals = get_open_signals(conn, last_bar_id)
            for sig in signals:
                bar_id = sig[0]
                if bar_id > last_bar_id:
                    last_bar_id = bar_id
                print_signal_row(sig)
                log_signal(sig)

            # ── New fills — build round-trips from buy/sell pairs ──
            fills = get_recent_fills(conn, last_fill_id)
            for fill in fills:
                fid = fill[0]
                if fid > last_fill_id:
                    last_fill_id = fid
                print_fill_row(fill)
                log_fill(fill)

                sym, side = fill[1], fill[2]
                if side == "buy":
                    open_positions[sym] = fill  # track entry
                elif side == "sell" and sym in open_positions:
                    entry = open_positions.pop(sym)
                    entry_price = entry[4]
                    exit_price = fill[4]
                    pnl = (exit_price - entry_price) * entry[3]  # qty
                    verdict = "HIT" if pnl > 0 else ("MISS" if pnl < 0 else "FLAT")
                    roundtrip = (sym, entry_price, exit_price, "buy", pnl, verdict,
                                 entry[8], fill[8], entry[10], entry[11])
                    all_trades.append(roundtrip)
                    log_roundtrip(*roundtrip)

            # ── Full refresh every poll ──
            clear_screen()
            print_header()
            print_stats_panel(conn, all_trades)

            if all_trades:
                print(f"\n{HEADER_COLOR}┌─ Completed Trades ───────────────────────────────────────────────────────────┐{RESET}")
                for t in all_trades[-10:]:  # Last 10
                    sym, ep, xp, side, pnl, verdict, entry_r, exit_r, ez, ervol = t
                    badge = hit_miss_badge(pnl)
                    side_str = colored_side(side)
                    print(f"  {badge} {side_str} {BOLD}{sym:4s}{RESET} │ "
                          f"${ep:.2f} → ${xp:.2f} │ {colored_pnl(pnl)} │ "
                          f"{DIM}z={ez or 0:.2f} rvol={ervol or 0:.2f}{RESET}")
                    print(f"         {SIGNAL_COLOR}entry: {entry_r or '?'}{RESET}  {DIM}exit: {exit_r or '?'}{RESET}")
                print(f"{HEADER_COLOR}└──────────────────────────────────────────────────────────────────────────────┘{RESET}")

            # Show recent signals (last 5) even if not new
            recent_sigs = get_recent_signal_history(conn)
            if recent_sigs:
                print(f"\n{HEADER_COLOR}┌─ Recent Signals ─────────────────────────────────────────────────────────────┐{RESET}")
                for sig in recent_sigs:
                    print_signal_row(sig)
                print(f"{HEADER_COLOR}└──────────────────────────────────────────────────────────────────────────────┘{RESET}")

            print(f"\n{DIM}Last poll: {datetime.now().strftime('%H:%M:%S')} │ "
                  f"Next in {poll_interval}s │ Log: {LOG_PATH}{RESET}\n")

            conn.close()

        except KeyboardInterrupt:
            print(f"\n\n{BOLD}Final Summary{RESET}")
            if all_trades:
                try:
                    conn = sqlite3.connect(f"file:{db_path}?mode=ro", uri=True)
                    print_stats_panel(conn, all_trades)
                    conn.close()
                except Exception:
                    pass
            print(f"\n{DIM}Trade log saved to: {LOG_PATH}{RESET}")
            break
        except Exception as e:
            print(f"{RED}Error: {e}{RESET}")
            time.sleep(poll_interval)
            continue

        time.sleep(poll_interval)


def main():
    parser = argparse.ArgumentParser(description="OpenQuant Live Trade Monitor")
    parser.add_argument("--db", default="data/journal/metals-2026-03-19.db",
                        help="Path to journal SQLite database")
    parser.add_argument("--poll", type=int, default=30, help="Poll interval in seconds")
    args = parser.parse_args()

    clear_screen()
    print_header()
    monitor(args.db, args.poll)


if __name__ == "__main__":
    main()
