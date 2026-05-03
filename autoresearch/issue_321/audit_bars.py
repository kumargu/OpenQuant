#!/usr/bin/env python3
"""Bar completeness audit for the 4 walk-forward OOS blocks.

Per CLAUDE.md, RTH = 13:30-20:00 UTC = 390 minutes/day.

Reports two metrics per block:
  full-day: how many trading days have <380 RTH bars (sparse pre-/post-market noise)
  decision-bar: how many trading days are MISSING the 19:55-20:00 UTC close bar
                (the bar that drives basket decisions when
                 [runner].decision_offset_minutes_before_close = 0)

The "decision-bar coverage" is the metric that matters for whether a walk-
forward result is distorted by missing data. Full-day counts can be sparse
without affecting basket decisions if the close bar is present.
"""
import pandas as pd
from pathlib import Path

PARQUETS = Path("/Users/gulshan/quant-data/bars/v3_sp500_2024-2026_1min_adjusted")
SYMBOLS = ["META", "MSFT", "AAPL", "GOOGL", "AMZN", "NVDA"]
BLOCKS = [
    ("Test0", "2024-07-01", "2024-12-31"),
    ("Test1", "2025-01-01", "2025-06-30"),
    ("Test2", "2025-07-01", "2025-12-31"),
    ("Test3", "2026-01-01", "2026-03-31"),
]
RTH_OPEN_MIN = 13 * 60 + 30   # 13:30 UTC
RTH_CLOSE_MIN = 20 * 60       # 20:00 UTC
DECISION_BAR_OPEN_MIN = 19 * 60 + 55   # 19:55 UTC
DECISION_BAR_CLOSE_MIN = 20 * 60       # 20:00 UTC
EXPECTED = RTH_CLOSE_MIN - RTH_OPEN_MIN  # 390 bars


def audit_symbol(sym: str) -> tuple[pd.DataFrame, set]:
    """Return (full-day RTH counts, set of dates with a 19:55-20:00 UTC bar)."""
    p = PARQUETS / f"{sym}.parquet"
    df = pd.read_parquet(p)
    ts = pd.to_datetime(df["timestamp"], utc=True)
    df = df.assign(t=ts)
    df["hm"] = df["t"].dt.hour * 60 + df["t"].dt.minute
    df["date"] = df["t"].dt.date

    rth = df[(df["hm"] >= RTH_OPEN_MIN) & (df["hm"] < RTH_CLOSE_MIN)]
    counts = rth.groupby("date").size().reset_index(name="bars")

    decision = df[
        (df["hm"] >= DECISION_BAR_OPEN_MIN) & (df["hm"] < DECISION_BAR_CLOSE_MIN)
    ]
    decision_dates = set(decision["date"].unique())
    return counts, decision_dates


def main():
    print("=" * 90)
    print(" BAR COMPLETENESS AUDIT")
    print(f"  full-day window: {RTH_OPEN_MIN // 60}:{RTH_OPEN_MIN % 60:02d}-"
          f"{RTH_CLOSE_MIN // 60}:{RTH_CLOSE_MIN % 60:02d} UTC, expected {EXPECTED}/day")
    print(f"  decision bar:    {DECISION_BAR_OPEN_MIN // 60}:{DECISION_BAR_OPEN_MIN % 60:02d}-"
          f"{DECISION_BAR_CLOSE_MIN // 60}:{DECISION_BAR_CLOSE_MIN % 60:02d} UTC")
    print("=" * 90)
    for sym in SYMBOLS:
        counts, decision_dates = audit_symbol(sym)
        print(f"\n{sym}")
        print(f"{'block':<8} {'days':>5} {'<380':>6} {'%<380':>7} {'<200':>5}"
              f" {'min':>5} {'mean':>6} {'med':>5} {'dec_cov':>9}")
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
            decision_in_block = sum(1 for d in sub["date"] if d in decision_dates)
            dec_pct = 100.0 * decision_in_block / n if n else 0.0
            print(f"{name:<8} {n:>5} {n_under_380:>6} {pct:>6.0f}% {n_under_200:>5}"
                  f" {mn:>5} {mean:>6.0f} {int(med):>5} {dec_pct:>7.1f}%")


if __name__ == "__main__":
    main()
