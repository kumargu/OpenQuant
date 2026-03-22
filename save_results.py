#!/usr/bin/env python3
"""Append current backtest results to data/backtest_history.json.

Each entry is indexed by git commit + timestamp. Run after every backtest
to build a history of results across code changes.

Usage:
    python3 save_results.py
    python3 save_results.py --note "dropped AMD/INTC, added C/JPM"
"""
import argparse
import json
import os
import subprocess
from collections import defaultdict
from datetime import datetime, timezone


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--note", default="", help="Optional note for this run")
    parser.add_argument("--results", default="data/trade_results.json")
    parser.add_argument("--history", default="data/backtest_history.json")
    args = parser.parse_args()

    commit = subprocess.check_output(
        ["git", "rev-parse", "--short", "HEAD"]
    ).decode().strip()
    ts = datetime.utcnow().strftime("%Y%m%d-%H%M%S")
    run_id = f"{commit}-{ts}"

    with open(args.results) as f:
        trades = json.load(f)

    # Per-pair summary
    by_pair = defaultdict(lambda: {"trades": 0, "total_bps": 0.0, "wins": 0})
    for t in trades:
        p = by_pair[t["id"]]
        p["trades"] += 1
        p["total_bps"] += t["return_bps"]
        if t["return_bps"] > 0:
            p["wins"] += 1

    pairs_summary = {}
    for pair, d in sorted(by_pair.items()):
        dollar = d["total_bps"] * 2
        pairs_summary[pair] = {
            "trades": d["trades"],
            "total_bps": round(d["total_bps"], 1),
            "dollar_pnl": round(dollar, 2),
            "dollar_per_day": 0.0,  # filled below
            "win_rate": round(d["wins"] / d["trades"] * 100, 1) if d["trades"] else 0,
        }

    total_bps = sum(d["total_bps"] for d in by_pair.values())
    total_trades = sum(d["trades"] for d in by_pair.values())
    total_wins = sum(d["wins"] for d in by_pair.values())
    trading_days = len(set(
        datetime.fromtimestamp(t["exit_ts"] / 1000, tz=timezone.utc).strftime("%Y-%m-%d")
        for t in trades
    ))

    for ps in pairs_summary.values():
        ps["dollar_per_day"] = round(ps["dollar_pnl"] / trading_days, 2) if trading_days else 0

    # Per-pair daily data (for charts on historical runs)
    daily_by_pair = defaultdict(lambda: defaultdict(
        lambda: {"pnl_bps": 0.0, "trades": 0, "wins": 0}
    ))
    for t in trades:
        dt = datetime.fromtimestamp(t["exit_ts"] / 1000, tz=timezone.utc)
        day = dt.strftime("%Y-%m-%d")
        pair = t["id"]
        daily_by_pair[pair][day]["pnl_bps"] += t["return_bps"]
        daily_by_pair[pair][day]["trades"] += 1
        if t["return_bps"] > 0:
            daily_by_pair[pair][day]["wins"] += 1

    daily_data = {}
    for pair_name in daily_by_pair:
        pair_days = []
        for day in sorted(daily_by_pair[pair_name].keys()):
            d = daily_by_pair[pair_name][day]
            dollar = d["pnl_bps"] * 2
            wr = d["wins"] / d["trades"] * 100 if d["trades"] else 0
            pair_days.append({
                "date": day,
                "pnl_bps": round(d["pnl_bps"], 1),
                "dollar_pnl": round(dollar, 2),
                "trades": d["trades"],
                "win_rate": round(wr, 1),
            })
        daily_data[pair_name] = pair_days

    # Combined ALL view
    all_days = defaultdict(lambda: {"pnl_bps": 0.0, "trades": 0, "wins": 0, "dollar_pnl": 0.0})
    for pair_name, days in daily_data.items():
        for d in days:
            all_days[d["date"]]["dollar_pnl"] += d["dollar_pnl"]
            all_days[d["date"]]["pnl_bps"] += d["pnl_bps"]
            all_days[d["date"]]["trades"] += d["trades"]
            all_days[d["date"]]["wins"] += round(d["win_rate"] * d["trades"] / 100)
    combined = []
    for day in sorted(all_days.keys()):
        d = all_days[day]
        wr = d["wins"] / d["trades"] * 100 if d["trades"] else 0
        combined.append({
            "date": day, "pnl_bps": round(d["pnl_bps"], 1),
            "dollar_pnl": round(d["dollar_pnl"], 2),
            "trades": d["trades"], "win_rate": round(wr, 1),
        })
    daily_data["ALL"] = combined

    entry = {
        "run_id": run_id,
        "commit": commit,
        "timestamp": datetime.utcnow().isoformat() + "Z",
        "note": args.note,
        "trading_days": trading_days,
        "total_trades": total_trades,
        "total_pnl_bps": round(total_bps, 1),
        "dollar_pnl": round(total_bps * 2, 2),
        "dollar_per_day": round(total_bps * 2 / trading_days, 2) if trading_days else 0,
        "win_rate": round(total_wins / total_trades * 100, 1) if total_trades else 0,
        "pairs": pairs_summary,
        "daily": daily_data,
    }

    if os.path.exists(args.history):
        with open(args.history) as f:
            history = json.load(f)
    else:
        history = []

    history.append(entry)
    with open(args.history, "w") as f:
        json.dump(history, f, indent=2)

    # Print summary
    print(f"Run: {run_id}")
    print(f"  Days: {trading_days} | Trades: {total_trades} | "
          f"P&L: ${total_bps*2:.0f} | $/day: ${total_bps*2/trading_days:.0f} | "
          f"Win: {total_wins/total_trades*100:.0f}%")
    for pair, ps in pairs_summary.items():
        print(f"  {pair:<12} ${ps['dollar_per_day']:>7.0f}/day  {ps['win_rate']:>5.1f}%  {ps['trades']:>4} trades")
    if args.note:
        print(f"  Note: {args.note}")
    print(f"History: {len(history)} runs")


if __name__ == "__main__":
    main()
