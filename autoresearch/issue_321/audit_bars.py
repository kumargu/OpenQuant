#!/usr/bin/env python3
"""Bar completeness audit for the 3 walk-forward OOS blocks.

Checks how many RTH days have <380 minute bars for each mega-cap that the
no-mining v1 universe references. Per CLAUDE.md, RTH = 13:30-20:00 UTC = 390 minutes/day.
"""
import pandas as pd
from pathlib import Path

PARQUETS = Path("/Users/gulshan/quant-data/bars/v3_sp500_2024-2026_1min_adjusted")
SYMBOLS = ["META", "MSFT", "AAPL", "GOOGL", "AMZN", "NVDA"]
BLOCKS = [
    ("Test1", "2025-01-01", "2025-06-30"),
    ("Test2", "2025-07-01", "2025-12-31"),
    ("Test3", "2026-01-01", "2026-03-31"),
]
RTH_OPEN_HM = (13, 30)   # 13:30 UTC
RTH_CLOSE_HM = (20, 0)   # 20:00 UTC
EXPECTED = 390           # 6.5h * 60min


def audit_symbol(sym: str) -> pd.DataFrame:
    p = PARQUETS / f"{sym}.parquet"
    df = pd.read_parquet(p)
    ts = pd.to_datetime(df["timestamp"], utc=True)
    df = df.assign(t=ts)
    open_t = pd.Timestamp("1900-01-01").replace(hour=RTH_OPEN_HM[0], minute=RTH_OPEN_HM[1])
    close_t = pd.Timestamp("1900-01-01").replace(hour=RTH_CLOSE_HM[0], minute=RTH_CLOSE_HM[1])
    df["hm"] = df["t"].dt.hour * 60 + df["t"].dt.minute
    open_min = open_t.hour * 60 + open_t.minute
    close_min = close_t.hour * 60 + close_t.minute
    df = df[(df["hm"] >= open_min) & (df["hm"] < close_min)]
    df["date"] = df["t"].dt.date
    counts = df.groupby("date").size().reset_index(name="bars")
    return counts


def main():
    print("=" * 80)
    print(" BAR COMPLETENESS AUDIT — RTH 13:30-20:00 UTC, expected 390 bars/day")
    print("=" * 80)
    for sym in SYMBOLS:
        counts = audit_symbol(sym)
        print(f"\n{sym}")
        print(f"{'block':<8} {'days':>5} {'<380':>6} {'%<380':>7} {'<200':>5} {'min':>5} {'mean':>6} {'med':>5}")
        for name, start, end in BLOCKS:
            sd = pd.Timestamp(start).date()
            ed = pd.Timestamp(end).date()
            sub = counts[(counts["date"] >= sd) & (counts["date"] <= ed)]
            n = len(sub)
            n_under_380 = (sub["bars"] < 380).sum()
            n_under_200 = (sub["bars"] < 200).sum()
            pct = 100.0 * n_under_380 / n if n else 0.0
            mn = sub["bars"].min() if n else 0
            mean = sub["bars"].mean() if n else 0.0
            med = sub["bars"].median() if n else 0
            print(f"{name:<8} {n:>5} {n_under_380:>6} {pct:>6.0f}% {n_under_200:>5} {mn:>5} {mean:>6.0f} {int(med):>5}")


if __name__ == "__main__":
    main()
