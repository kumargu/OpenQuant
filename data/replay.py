"""
Replay harness for experiment runs. Feeds saved bars through the engine
with a given config and returns structured results.

Usage:
    from data.replay import run_experiment
    results = run_experiment(config_overrides={...}, label="my_experiment")
"""

import json
import os
import statistics
import sys
from collections import defaultdict
from datetime import datetime, timezone
from pathlib import Path
from zoneinfo import ZoneInfo

import toml

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from openquant import Engine

DATA_DIR = Path(__file__).parent
DEFAULT_DATE = "20260317"
BASE_CONFIG = Path(__file__).parent.parent / "openquant.toml"


def load_bars(bars_date: str = DEFAULT_DATE) -> dict[str, list[dict]]:
    path = DATA_DIR / f"experiment_bars_{bars_date}.json"
    if not path.exists():
        raise FileNotFoundError(f"No data for date {bars_date}. Run: python data/backfill.py --date {bars_date[:4]}-{bars_date[4:6]}-{bars_date[6:]}")
    with open(path) as f:
        return json.load(f)


def available_dates() -> list[str]:
    """Return list of available experiment dates (YYYYMMDD format)."""
    return sorted(
        p.stem.replace("experiment_bars_", "")
        for p in DATA_DIR.glob("experiment_bars_*.json")
    )


def run_experiment(
    config_overrides: dict | None = None,
    label: str = "experiment",
    warmup_bars: int = 60,
    verbose: bool = False,
    bars_date: str = DEFAULT_DATE,
) -> dict:
    """
    Run a full-day replay with the given config overrides.

    Returns a dict with:
        label, config_overrides, total_orders, round_trips, win_rate,
        total_pnl, avg_pnl, avg_hold_min, per_symbol, trades
    """
    # Load and patch config
    cfg = toml.load(str(BASE_CONFIG))
    if config_overrides:
        for section, values in config_overrides.items():
            if section not in cfg:
                cfg[section] = {}
            cfg[section].update(values)

    # Disable stale bar check for replay
    cfg["data"]["max_bar_age_seconds"] = 0

    tmp_path = f"/tmp/replay_{label}.toml"
    with open(tmp_path, "w") as f:
        toml.dump(cfg, f)

    engine = Engine.from_toml(tmp_path)
    all_bars = load_bars(bars_date)

    # Merge and sort all bars by timestamp
    merged = []
    for sym, bars in all_bars.items():
        for bar in bars:
            merged.append((sym, bar))
    merged.sort(key=lambda x: x[1]["timestamp"])

    # Split warmup and trading bars
    warmup_counts = defaultdict(int)
    trading_start_idx = 0
    for i, (sym, bar) in enumerate(merged):
        if all(warmup_counts[s] >= warmup_bars for s in all_bars.keys()):
            trading_start_idx = i
            break
        warmup_counts[sym] += 1

    # Feed warmup bars (no trading)
    for sym, bar in merged[:trading_start_idx]:
        engine.on_bar(sym, bar["timestamp"], bar["open"], bar["high"], bar["low"], bar["close"], bar["volume"])

    # Replay trading bars
    trades = []
    positions = {}
    order_count = 0

    for sym, bar in merged[trading_start_idx:]:
        intents = engine.on_bar(sym, bar["timestamp"], bar["open"], bar["high"], bar["low"], bar["close"], bar["volume"])
        bar_time = datetime.fromtimestamp(bar["timestamp"] / 1000, tz=timezone.utc).astimezone(ZoneInfo("America/New_York"))

        for intent in intents:
            side, qty = intent["side"], intent["qty"]
            c = bar["close"]
            votes = intent.get("votes", "")
            order_count += 1

            if side == "buy":
                positions[sym] = {"qty": qty, "entry": c, "time": bar_time}
                engine.on_fill(sym, side, qty, c)
                if verbose:
                    print(f"{bar_time.strftime('%H:%M')} BUY  {sym:6s} qty={qty:>5.0f} @ ${c:>8.2f} votes=[{votes}]")
            else:
                engine.on_fill(sym, side, qty, c)
                if sym in positions:
                    entry = positions[sym]["entry"]
                    pnl = (c - entry) * qty
                    hold_min = (bar_time - positions[sym]["time"]).total_seconds() / 60
                    trades.append({"symbol": sym, "pnl": pnl, "hold_min": hold_min, "votes": votes})
                    if verbose:
                        print(f"{bar_time.strftime('%H:%M')} SELL {sym:6s} qty={qty:>5.0f} @ ${c:>8.2f} P&L ${pnl:>+8.2f} held {hold_min:.0f}min")
                    del positions[sym]

    # Compute results
    pnls = [t["pnl"] for t in trades]
    holds = [t["hold_min"] for t in trades]
    wins = [p for p in pnls if p > 0]

    by_sym = defaultdict(list)
    sym_holds = defaultdict(list)
    for t in trades:
        by_sym[t["symbol"]].append(t["pnl"])
        sym_holds[t["symbol"]].append(t["hold_min"])

    per_symbol = {}
    for sym in sorted(by_sym.keys()):
        sp = by_sym[sym]
        sw = len([p for p in sp if p > 0])
        per_symbol[sym] = {
            "trades": len(sp),
            "win_rate": sw / len(sp) if sp else 0,
            "pnl": sum(sp),
            "avg_hold_min": statistics.mean(sym_holds[sym]) if sym_holds[sym] else 0,
        }

    return {
        "label": label,
        "config_overrides": config_overrides or {},
        "total_orders": order_count,
        "round_trips": len(trades),
        "win_rate": len(wins) / len(trades) if trades else 0,
        "total_pnl": sum(pnls) if pnls else 0,
        "avg_pnl": statistics.mean(pnls) if pnls else 0,
        "avg_hold_min": statistics.mean(holds) if holds else 0,
        "per_symbol": per_symbol,
        "open_positions": len(positions),
        "trades": trades,
    }


def format_results(r: dict) -> str:
    """Format results as a markdown table for GH issue comments."""
    lines = [
        f"## Experiment: {r['label']}",
        "",
        f"**Config changes:** `{json.dumps(r['config_overrides'], indent=None)}`",
        "",
        "| Metric | Value |",
        "|--------|-------|",
        f"| Orders | {r['total_orders']} |",
        f"| Round trips | {r['round_trips']} |",
        f"| Win rate | {r['win_rate']:.1%} |",
        f"| Total P&L | ${r['total_pnl']:+,.2f} |",
        f"| Avg P&L/trade | ${r['avg_pnl']:+,.2f} |",
        f"| Avg hold time | {r['avg_hold_min']:.1f} min |",
        f"| Open positions at end | {r['open_positions']} |",
        "",
        "### Per-symbol breakdown",
        "",
        "| Symbol | Trades | WR | P&L | Avg Hold |",
        "|--------|--------|-----|-----|----------|",
    ]
    for sym, data in sorted(r["per_symbol"].items()):
        lines.append(
            f"| {sym} | {data['trades']} | {data['win_rate']:.0%} | ${data['pnl']:+,.2f} | {data['avg_hold_min']:.0f}m |"
        )
    return "\n".join(lines)
