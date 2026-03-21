"""
Time-Travel Replay — feed historical bars through the engine as if trading live.

Unlike backtesting, the engine sees one bar at a time with no knowledge of
what comes next. This tests actual decision-making on unseen data.

A full trading day (~390 bars) replays in ~5 minutes by default.

Usage:
    python data/time_travel.py --date 2026-03-19
    python data/time_travel.py --date 2026-03-19 --speed 2.0    # slower
    python data/time_travel.py --date 2026-03-19 --speed 0.1    # near instant
    python data/time_travel.py --date 2026-03-19 --config openquant.toml
    python data/time_travel.py --date 2026-03-19 --quiet         # no per-bar output
"""

import argparse
import json
import os
import sys
import time
from collections import defaultdict
from datetime import datetime, timezone
from pathlib import Path
from zoneinfo import ZoneInfo

import toml

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from openquant import Engine

DATA_DIR = Path(__file__).parent
ET = ZoneInfo("America/New_York")

# ANSI colors
RESET = "\033[0m"
BOLD = "\033[1m"
DIM = "\033[2m"
GREEN = "\033[32m"
RED = "\033[31m"
CYAN = "\033[36m"
MAGENTA = "\033[35m"
YELLOW = "\033[33m"
WHITE = "\033[97m"
BG_GREEN = "\033[42m"
BG_RED = "\033[41m"


def load_bars(date_str: str) -> dict[str, list[dict]]:
    safe = date_str.replace("-", "")
    path = DATA_DIR / f"experiment_bars_{safe}.json"
    if not path.exists():
        raise FileNotFoundError(
            f"No data for {date_str}. Run: python data/backfill.py --date {date_str}"
        )
    with open(path) as f:
        return json.load(f)


def format_ts(ms: int) -> str:
    return datetime.fromtimestamp(ms / 1000, tz=timezone.utc).astimezone(ET).strftime("%H:%M")


def colored_pnl(pnl: float) -> str:
    if pnl > 0:
        return f"{GREEN}+${pnl:.2f}{RESET}"
    elif pnl < 0:
        return f"{RED}-${abs(pnl):.2f}{RESET}"
    return f"${pnl:.2f}"


def replay(
    date_str: str,
    config_path: str = "openquant.toml",
    config_overrides: dict | None = None,
    speed: float = 0.75,
    warmup_bars: int = 64,
    quiet: bool = False,
    label: str = "",
) -> dict:
    """
    Replay a full day of bars through the engine.

    Args:
        speed: seconds between bars (0.75 = ~5 min for full day)
        quiet: if True, only print trades, not every bar

    Returns:
        dict with full results (same format as replay.py)
    """
    # Load config
    cfg = toml.load(config_path)
    if config_overrides:
        for section, values in config_overrides.items():
            if section not in cfg:
                cfg[section] = {}
            if isinstance(values, dict):
                cfg[section].update(values)
            else:
                cfg[section] = values

    # Disable stale bar check for replay
    cfg["data"]["max_bar_age_seconds"] = 0

    tmp_path = f"/tmp/time_travel_{label or 'replay'}.toml"
    with open(tmp_path, "w") as f:
        toml.dump(cfg, f)

    engine = Engine.from_toml(tmp_path, warmup_bars=warmup_bars)
    all_bars = load_bars(date_str)

    # Merge and sort all bars by timestamp
    merged = []
    for sym, bars in all_bars.items():
        for bar in bars:
            merged.append((sym, bar))
    merged.sort(key=lambda x: x[1]["timestamp"])

    symbols = sorted(all_bars.keys())
    total_bars = len(merged)

    if not quiet:
        print(f"\n{BOLD}{'═' * 80}{RESET}")
        print(f"{BOLD}  ⏰ Time-Travel Replay — {date_str}   "
              f"{DIM}({total_bars} bars, ~{total_bars * speed / 60:.0f} min){RESET}")
        if label:
            print(f"{BOLD}  Strategy: {label}{RESET}")
        print(f"{BOLD}  Symbols: {', '.join(symbols)}{RESET}")
        print(f"{BOLD}{'═' * 80}{RESET}\n")

    # Split warmup
    warmup_counts = defaultdict(int)
    warmup_end = 0
    for i, (sym, bar) in enumerate(merged):
        if all(warmup_counts[s] >= warmup_bars for s in all_bars.keys()):
            warmup_end = i
            break
        warmup_counts[sym] += 1

    # Feed warmup bars (instant, no delay)
    engine.set_warmup_mode(True)
    for sym, bar in merged[:warmup_end]:
        engine.on_bar(sym, bar["timestamp"], bar["open"], bar["high"],
                      bar["low"], bar["close"], bar["volume"])
    engine.set_warmup_mode(False)

    if not quiet:
        print(f"{DIM}  Warmup complete: {warmup_end} bars consumed{RESET}\n")

    # Replay trading bars with time delay
    trades = []
    positions = {}
    order_count = 0
    signals_fired = 0
    signals_rejected = 0

    for idx, (sym, bar) in enumerate(merged[warmup_end:], 1):
        bar_time = format_ts(bar["timestamp"])

        intents = engine.on_bar(
            sym, bar["timestamp"], bar["open"], bar["high"],
            bar["low"], bar["close"], bar["volume"],
        )

        if intents:
            for intent in intents:
                side, qty = intent["side"], intent["qty"]
                price = bar["close"]
                score = intent["score"]
                reason = intent["reason"]
                votes = intent.get("votes", "")
                order_count += 1

                if side == "buy":
                    positions[sym] = {
                        "qty": qty, "entry": price,
                        "time": bar["timestamp"], "reason": reason,
                    }
                    engine.on_fill(sym, side, qty, price)
                    notional = qty * price

                    if not quiet:
                        print(f"  {CYAN}▲ BUY {RESET} {BOLD}{sym:5s}{RESET} "
                              f"{bar_time} │ qty={qty:.0f} @ ${price:.2f} "
                              f"(${notional:,.0f}) │ score={score:.2f}")
                        print(f"         {YELLOW}{reason}{RESET}")
                        if votes:
                            print(f"         {DIM}votes=[{votes}]{RESET}")
                        print()
                else:
                    engine.on_fill(sym, side, qty, price)
                    if sym in positions:
                        entry = positions[sym]
                        pnl = (price - entry["entry"]) * qty
                        hold_ms = bar["timestamp"] - entry["time"]
                        hold_min = hold_ms / 60_000
                        verdict = "HIT" if pnl > 0 else "MISS"

                        trades.append({
                            "symbol": sym,
                            "entry_price": entry["entry"],
                            "exit_price": price,
                            "qty": qty,
                            "pnl": pnl,
                            "hold_min": hold_min,
                            "entry_reason": entry["reason"],
                            "exit_reason": reason,
                            "votes": votes,
                        })
                        del positions[sym]

                        if not quiet:
                            badge_color = BG_GREEN if pnl > 0 else BG_RED
                            print(f"  {MAGENTA}▼ SELL{RESET} {BOLD}{sym:5s}{RESET} "
                                  f"{bar_time} │ qty={qty:.0f} @ ${price:.2f} │ "
                                  f"{badge_color}{BOLD} {verdict} {RESET} "
                                  f"{colored_pnl(pnl)} │ held {hold_min:.0f}min")
                            print(f"         {YELLOW}{reason}{RESET}")
                            print(f"         {DIM}entry=${entry['entry']:.2f} "
                                  f"→ exit=${price:.2f}{RESET}")
                            print()

        # Progress indicator every 50 bars (quiet mode) or show bar (normal)
        if not quiet and not intents:
            # Only print bar line every 10th bar to reduce noise
            if idx % 10 == 0:
                progress = idx / (total_bars - warmup_end) * 100
                running_pnl = sum(t["pnl"] for t in trades)
                open_count = len(positions)
                print(f"  {DIM}{bar_time} │ {sym:5s} ${bar['close']:.2f} │ "
                      f"bar {idx}/{total_bars - warmup_end} ({progress:.0f}%) │ "
                      f"P&L: {colored_pnl(running_pnl)} │ "
                      f"{open_count} open{RESET}", end="\r")

        # Time delay between bars
        if speed > 0:
            time.sleep(speed)

    # Final summary
    pnls = [t["pnl"] for t in trades]
    wins = [t for t in trades if t["pnl"] > 0]
    losses = [t for t in trades if t["pnl"] < 0]
    total_pnl = sum(pnls) if pnls else 0
    win_rate = len(wins) / len(trades) if trades else 0

    # Unrealized P&L from open positions
    open_pnl = 0
    last_prices = {}
    for sym, bar in merged[-len(all_bars):]:
        last_prices[sym] = bar["close"]
    for sym, pos in positions.items():
        if sym in last_prices:
            open_pnl += (last_prices[sym] - pos["entry"]) * pos["qty"]

    if not quiet:
        print(f"\n{'':>80}\n")  # Clear progress line
        print(f"{BOLD}{'═' * 80}{RESET}")
        print(f"{BOLD}  📊 END OF DAY SUMMARY — {date_str}{RESET}")
        if label:
            print(f"{BOLD}  Strategy: {label}{RESET}")
        print(f"{BOLD}{'═' * 80}{RESET}\n")

        print(f"  Completed trades: {len(trades)}")
        print(f"  Wins: {GREEN}{len(wins)}{RESET}  │  "
              f"Losses: {RED}{len(losses)}{RESET}  │  "
              f"Win Rate: {BOLD}{win_rate:.0%}{RESET}")
        print(f"  Realized P&L: {colored_pnl(total_pnl)}")

        if positions:
            print(f"  Open positions: {len(positions)} "
                  f"(unrealized: {colored_pnl(open_pnl)})")
            for sym, pos in positions.items():
                last = last_prices.get(sym, pos["entry"])
                upnl = (last - pos["entry"]) * pos["qty"]
                print(f"    {sym}: {pos['qty']:.0f} @ ${pos['entry']:.2f} "
                      f"→ ${last:.2f} ({colored_pnl(upnl)})")

        print(f"\n  {BOLD}Total P&L (realized + unrealized): "
              f"{colored_pnl(total_pnl + open_pnl)}{RESET}")

        if trades:
            avg_win = sum(t["pnl"] for t in wins) / len(wins) if wins else 0
            avg_loss = sum(t["pnl"] for t in losses) / len(losses) if losses else 0
            avg_hold = sum(t["hold_min"] for t in trades) / len(trades)
            print(f"\n  Avg win: {colored_pnl(avg_win)}  │  "
                  f"Avg loss: {colored_pnl(avg_loss)}  │  "
                  f"Avg hold: {avg_hold:.0f} min")

        # Trade log
        if trades:
            print(f"\n{BOLD}  ┌─ Trade Log ───────────────────────────────────────────────────┐{RESET}")
            for t in trades:
                badge = f"{BG_GREEN}{BOLD} HIT  {RESET}" if t["pnl"] > 0 else f"{BG_RED}{BOLD} MISS {RESET}"
                print(f"  {badge} {BOLD}{t['symbol']:5s}{RESET} "
                      f"${t['entry_price']:.2f}→${t['exit_price']:.2f} "
                      f"qty={t['qty']:.0f} │ {colored_pnl(t['pnl'])} │ "
                      f"{t['hold_min']:.0f}min")
            print(f"{BOLD}  └─────────────────────────────────────────────────────────────────┘{RESET}")

    # Build per-symbol stats
    by_sym = defaultdict(list)
    for t in trades:
        by_sym[t["symbol"]].append(t)

    per_symbol = {}
    for sym in sorted(by_sym.keys()):
        st = by_sym[sym]
        sw = [t for t in st if t["pnl"] > 0]
        per_symbol[sym] = {
            "trades": len(st),
            "win_rate": len(sw) / len(st) if st else 0,
            "pnl": sum(t["pnl"] for t in st),
            "avg_hold_min": sum(t["hold_min"] for t in st) / len(st) if st else 0,
        }

    return {
        "label": label or "time_travel",
        "date": date_str,
        "config_overrides": config_overrides or {},
        "total_bars": total_bars,
        "warmup_bars": warmup_end,
        "trading_bars": total_bars - warmup_end,
        "total_orders": order_count,
        "round_trips": len(trades),
        "win_rate": win_rate,
        "total_pnl": total_pnl,
        "open_positions": len(positions),
        "unrealized_pnl": open_pnl,
        "total_pnl_incl_open": total_pnl + open_pnl,
        "avg_pnl": sum(pnls) / len(pnls) if pnls else 0,
        "avg_hold_min": sum(t["hold_min"] for t in trades) / len(trades) if trades else 0,
        "per_symbol": per_symbol,
        "trades": trades,
    }


def format_results_markdown(r: dict) -> str:
    """Format results as markdown for GH issue comments."""
    lines = [
        f"## {'🟢' if r['total_pnl'] >= 0 else '🔴'} {r['label']} — {r['date']}",
        "",
    ]
    if r["config_overrides"]:
        lines.append(f"**Config overrides:** `{json.dumps(r['config_overrides'])}`")
        lines.append("")

    lines.extend([
        "| Metric | Value |",
        "|--------|-------|",
        f"| Round trips | {r['round_trips']} |",
        f"| Win rate | {r['win_rate']:.0%} |",
        f"| Realized P&L | ${r['total_pnl']:+,.2f} |",
        f"| Open positions | {r['open_positions']} (${r['unrealized_pnl']:+,.2f} unrealized) |",
        f"| **Total P&L** | **${r['total_pnl_incl_open']:+,.2f}** |",
        f"| Avg P&L/trade | ${r['avg_pnl']:+,.2f} |",
        f"| Avg hold time | {r['avg_hold_min']:.0f} min |",
        "",
    ])

    if r["per_symbol"]:
        lines.extend([
            "### Per-symbol",
            "",
            "| Symbol | Trades | WR | P&L | Avg Hold |",
            "|--------|--------|-----|-----|----------|",
        ])
        for sym, d in sorted(r["per_symbol"].items()):
            lines.append(
                f"| {sym} | {d['trades']} | {d['win_rate']:.0%} | "
                f"${d['pnl']:+,.2f} | {d['avg_hold_min']:.0f}m |"
            )
        lines.append("")

    if r["trades"]:
        lines.extend([
            "<details><summary>Trade log</summary>",
            "",
            "| # | Symbol | Entry | Exit | Qty | P&L | Hold | Entry Reason | Exit Reason |",
            "|---|--------|-------|------|-----|-----|------|-------------|-------------|",
        ])
        for i, t in enumerate(r["trades"], 1):
            verdict = "✅" if t["pnl"] > 0 else "❌"
            lines.append(
                f"| {i} | {t['symbol']} | ${t['entry_price']:.2f} | "
                f"${t['exit_price']:.2f} | {t['qty']:.0f} | "
                f"{verdict} ${t['pnl']:+,.2f} | {t['hold_min']:.0f}m | "
                f"{t['entry_reason'][:40]} | {t['exit_reason'][:40]} |"
            )
        lines.extend(["", "</details>"])

    return "\n".join(lines)


def main():
    parser = argparse.ArgumentParser(description="Time-Travel Replay")
    parser.add_argument("--date", "-d", required=True, help="Date to replay (YYYY-MM-DD)")
    parser.add_argument("--config", "-c", default="openquant.toml", help="Config file")
    parser.add_argument("--speed", "-s", type=float, default=0.75,
                        help="Seconds between bars (0.75 = ~5 min/day, 0 = instant)")
    parser.add_argument("--warmup", type=int, default=64, help="Warmup bars")
    parser.add_argument("--quiet", "-q", action="store_true", help="Minimal output")
    parser.add_argument("--label", "-l", default="", help="Strategy label")
    args = parser.parse_args()

    replay(
        date_str=args.date,
        config_path=args.config,
        speed=args.speed,
        warmup_bars=args.warmup,
        quiet=args.quiet,
        label=args.label,
    )


if __name__ == "__main__":
    main()
