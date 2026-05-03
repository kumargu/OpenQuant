#!/usr/bin/env python3
"""Quarterly Sharpe per variant from the daily P&L in report.tsv.

The walk-forward report.tsv files store one daily-pnl row per trading day.
This script reads them, splits each block by calendar quarter, computes
Sharpe (annualized, sqrt(252)) per quarter per variant, and prints a flat
table. Used to verify that gate-driven Sharpe is positive across all
quarters covered by the walk-forward window — not just on the half-year
aggregates the main analyzer prints.
"""
import math
from pathlib import Path
from typing import Optional

import pandas as pd

ROOT = Path("autoresearch/issue_321/walkforward")
VARIANTS = ["baseline", "dom050", "dom060", "nomegacaps", "nomegacaps_dom050"]
BLOCKS = ["test0", "test1", "test2", "test3"]


def load_pnl(path: Path) -> Optional[pd.DataFrame]:
    if not path.exists():
        return None
    df = pd.read_csv(path, sep="\t", skiprows=2)
    df["date"] = pd.to_datetime(df["date"])
    df["ret"] = df["daily_pnl_dollar"] / df["equity"].shift(1)
    return df.dropna(subset=["ret"])


def quarterly_sharpe(df: pd.DataFrame, min_days: int = 20):
    df = df.copy()
    df["q"] = df["date"].dt.to_period("Q").astype(str)
    out = []
    for q, g in df.groupby("q"):
        if len(g) < min_days:
            continue
        mu = g["ret"].mean()
        sd = g["ret"].std(ddof=1)
        sharpe = math.sqrt(252) * mu / sd if sd > 0 else float("nan")
        cum = (1 + g["ret"]).prod() - 1
        out.append((q, len(g), cum, sharpe))
    return out


def main():
    all_q: dict[str, dict[str, tuple[float, float, int]]] = {}
    for v in VARIANTS:
        all_q[v] = {}
        for b in BLOCKS:
            df = load_pnl(ROOT / v / b / "report.tsv")
            if df is None:
                continue
            for q, n, cum, sh in quarterly_sharpe(df):
                all_q[v][q] = (cum, sh, n)

    quarters = sorted({q for v in all_q.values() for q in v.keys()})
    print(f"{'quarter':<10} {'days':>5}", end="")
    for v in VARIANTS:
        print(f" {v[:18]:>18}", end="")
    print()
    print("-" * (16 + 19 * len(VARIANTS)))
    for q in quarters:
        days = max((all_q[v].get(q, (None, None, 0))[2] for v in VARIANTS), default=0)
        print(f"{q:<10} {days:>5}", end="")
        for v in VARIANTS:
            sh = all_q[v].get(q, (None, None, 0))[1]
            print(f" {sh:>+18.3f}" if sh is not None else f" {'---':>18}", end="")
        print()


if __name__ == "__main__":
    main()
