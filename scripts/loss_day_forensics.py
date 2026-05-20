#!/usr/bin/env python3
from __future__ import annotations

import argparse
import re
from collections import defaultdict
from dataclasses import dataclass
from datetime import date, datetime, time, timedelta
from pathlib import Path
from typing import Dict, Iterable, List, Tuple

import pyarrow.parquet as pq
from zoneinfo import ZoneInfo


NY = ZoneInfo("America/New_York")
UTC = ZoneInfo("UTC")

SESSION_RE = re.compile(r"session close firing date=(\d{4}-\d{2}-\d{2})")
PICKER_RE = re.compile(
    r'basket overlay picker decision date=(\d{4}-\d{2}-\d{2}).*?picker_mode="([^"]+)".*?picker_reason="([^"]+)"'
)
TARGET_RE = re.compile(
    r'target notionals summary date=(\d{4}-\d{2}-\d{2}).*?leadership_mode="([^"]+)".*?gross_notional=([0-9-]+).*?net_notional=([0-9-]+).*?top_abs_legs=\[(.*)\]'
)
ORDER_RE = re.compile(
    r'BASKET_ORDER mode="PAPER" symbol="([^"]+)" qty=([0-9]+) side="([^"]+)"'
)
FAIL_RE = re.compile(
    r"incremental exposure .* exceeds Alpaca buying power .* on (\d{4}-\d{2}-\d{2})"
)
AFFORDABILITY_FAIL_RE = re.compile(
    r"(affordability failed even though target gross did not increase|affordability failed while expanding exposure).*date=(\d{4}-\d{2}-\d{2}).*gross_delta=([+-]?[0-9.]+).*incremental_exposure_notional=([0-9.]+).*buying_power=([0-9.]+).*opens=([0-9]+).*closes=([0-9]+).*increases=([0-9]+).*reductions=([0-9]+).*flips=([0-9]+).*top_share_deltas=\[(.*)\]"
)


@dataclass
class DayStats:
    day: date
    equity: float
    pnl: float


@dataclass
class DayConfig:
    picker_mode: str | None = None
    picker_reason: str | None = None
    leadership_mode: str | None = None
    gross_notional: float | None = None
    net_notional: float | None = None
    top_abs_legs: List[str] | None = None
    orders: List[Tuple[str, int, str]] | None = None


@dataclass
class AffordabilityFailure:
    day: date
    kind: str
    gross_delta: float
    incremental_exposure: float
    buying_power: float
    opens: int
    closes: int
    increases: int
    reductions: int
    flips: int
    top_share_deltas: List[str]


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description="Forensic breakdown of worst basket loss days.")
    parser.add_argument("replay_dirs", nargs="+", help="Replay output directories containing report.tsv/replay.log")
    parser.add_argument("--top", type=int, default=8, help="Number of worst days to report")
    parser.add_argument(
        "--bars-dir",
        default="/Users/gulshan/quant-data/bars/v3_sp500_2024-2026_1min_adjusted",
        help="Directory containing per-symbol parquet bars",
    )
    return parser.parse_args()


def parse_report(path: Path) -> List[DayStats]:
    if not path.exists():
        return []
    rows: List[DayStats] = []
    with path.open() as f:
        for line in f:
            if line.startswith("#") or line.startswith("date") or not line.strip():
                continue
            raw_day, raw_equity, raw_pnl = line.strip().split("\t")
            rows.append(
                DayStats(
                    day=date.fromisoformat(raw_day),
                    equity=float(raw_equity),
                    pnl=float(raw_pnl),
                )
            )
    return rows


def parse_log(path: Path) -> tuple[Dict[date, DayConfig], List[date], List[AffordabilityFailure]]:
    configs: Dict[date, DayConfig] = defaultdict(DayConfig)
    failures: List[date] = []
    affordability_failures: List[AffordabilityFailure] = []
    current_order_day: date | None = None

    with path.open() as f:
        for line in f:
            if match := SESSION_RE.search(line):
                current_order_day = date.fromisoformat(match.group(1))

            if match := PICKER_RE.search(line):
                day = date.fromisoformat(match.group(1))
                cfg = configs[day]
                cfg.picker_mode = match.group(2)
                cfg.picker_reason = match.group(3)

            if match := TARGET_RE.search(line):
                day = date.fromisoformat(match.group(1))
                cfg = configs[day]
                cfg.leadership_mode = match.group(2)
                cfg.gross_notional = float(match.group(3))
                cfg.net_notional = float(match.group(4))
                raw_legs = match.group(5).strip()
                cfg.top_abs_legs = [part.strip().strip('"') for part in raw_legs.split(",") if part.strip()]

            if match := ORDER_RE.search(line):
                if current_order_day is None:
                    continue
                cfg = configs[current_order_day]
                if cfg.orders is None:
                    cfg.orders = []
                cfg.orders.append((match.group(1), int(match.group(2)), match.group(3)))

            if match := FAIL_RE.search(line):
                failures.append(date.fromisoformat(match.group(1)))

            if match := AFFORDABILITY_FAIL_RE.search(line):
                raw_top_deltas = match.group(11).strip()
                affordability_failures.append(
                    AffordabilityFailure(
                        day=date.fromisoformat(match.group(2)),
                        kind=match.group(1),
                        gross_delta=float(match.group(3)),
                        incremental_exposure=float(match.group(4)),
                        buying_power=float(match.group(5)),
                        opens=int(match.group(6)),
                        closes=int(match.group(7)),
                        increases=int(match.group(8)),
                        reductions=int(match.group(9)),
                        flips=int(match.group(10)),
                        top_share_deltas=re.findall(r'"([^"]+)"', raw_top_deltas),
                    )
                )

    return configs, failures, affordability_failures


def build_eod_positions(rows: Iterable[DayStats], configs: Dict[date, DayConfig]) -> Dict[date, Dict[str, int]]:
    positions: Dict[str, int] = {}
    out: Dict[date, Dict[str, int]] = {}
    for row in rows:
        cfg = configs.get(row.day)
        if cfg and cfg.orders:
            for symbol, qty, side in cfg.orders:
                signed_qty = qty if side == "buy" else -qty
                positions[symbol] = positions.get(symbol, 0) + signed_qty
                if positions[symbol] == 0:
                    positions.pop(symbol)
        out[row.day] = dict(positions)
    return out


def load_symbol_bars(symbol: str, bars_dir: Path, cache: Dict[str, Dict[date, dict]]) -> Dict[date, dict]:
    if symbol in cache:
        return cache[symbol]

    table = pq.read_table(bars_dir / f"{symbol}.parquet", columns=["timestamp", "open", "close"])
    data = table.to_pydict()
    per_day: Dict[date, dict] = {}
    rows_by_day: Dict[date, List[tuple[datetime, float, float]]] = defaultdict(list)

    for ts, opn, cls in zip(data["timestamp"], data["open"], data["close"]):
        if ts.tzinfo is None:
            ts = ts.replace(tzinfo=UTC)
        ts_ny = ts.astimezone(NY)
        rows_by_day[ts_ny.date()].append((ts_ny, float(opn), float(cls)))

    for day, rows in rows_by_day.items():
        rows.sort(key=lambda x: x[0])
        session_rows = [row for row in rows if time(9, 30) <= row[0].time() <= time(16, 0)]
        if not session_rows:
            continue
        open_px = session_rows[0][1]
        close_px = session_rows[-1][2]
        first_hour_rows = [row for row in session_rows if row[0].time() < time(10, 30)]
        first_hour_close = first_hour_rows[-1][2] if first_hour_rows else close_px
        per_day[day] = {
            "open": open_px,
            "close": close_px,
            "first_hour_close": first_hour_close,
        }

    cache[symbol] = per_day
    return per_day


def signed_position_sides(positions: Dict[str, int]) -> tuple[int, int]:
    longs = sum(1 for qty in positions.values() if qty > 0)
    shorts = sum(1 for qty in positions.values() if qty < 0)
    return longs, shorts


def leg_contributors(
    positions: Dict[str, int],
    prev_day: date,
    day: date,
    bars_dir: Path,
    cache: Dict[str, Dict[date, dict]],
) -> tuple[float, float, float, List[Tuple[float, str, int]]]:
    overnight = 0.0
    first_hour = 0.0
    rest_day = 0.0
    contributions: List[Tuple[float, str, int]] = []

    for symbol, qty in positions.items():
        symbol_days = load_symbol_bars(symbol, bars_dir, cache)
        prev = symbol_days.get(prev_day)
        curr = symbol_days.get(day)
        if not prev or not curr:
            continue

        overnight_part = qty * (curr["open"] - prev["close"])
        first_hour_part = qty * (curr["first_hour_close"] - curr["open"])
        rest_day_part = qty * (curr["close"] - curr["first_hour_close"])
        total = overnight_part + first_hour_part + rest_day_part

        overnight += overnight_part
        first_hour += first_hour_part
        rest_day += rest_day_part
        contributions.append((total, symbol, qty))

    contributions.sort()
    return overnight, first_hour, rest_day, contributions


def main() -> None:
    args = parse_args()
    bars_dir = Path(args.bars_dir)
    all_rows: List[Tuple[str, DayStats]] = []
    replay_rows: Dict[str, List[DayStats]] = {}
    replay_cfgs: Dict[str, Dict[date, DayConfig]] = {}
    replay_positions: Dict[str, Dict[date, Dict[str, int]]] = {}
    failures: Dict[str, List[date]] = {}
    affordability_failures: Dict[str, List[AffordabilityFailure]] = {}

    for replay_dir_str in args.replay_dirs:
        replay_dir = Path(replay_dir_str)
        report_path = replay_dir / "report.tsv"
        log_path = replay_dir / "replay.log"
        label = replay_dir.name
        rows = parse_report(report_path)
        cfgs, fail_days, aff_failures = parse_log(log_path)
        replay_rows[label] = rows
        replay_cfgs[label] = cfgs
        replay_positions[label] = build_eod_positions(rows, cfgs)
        failures[label] = fail_days
        affordability_failures[label] = aff_failures
        all_rows.extend((label, row) for row in rows)

    worst = sorted(all_rows, key=lambda item: item[1].pnl)[: args.top]
    bars_cache: Dict[str, Dict[date, dict]] = {}

    print("# Loss-Day Forensics")
    print()
    print("## Worst Days")
    print()
    print("| Replay | Date | PnL | Equity | Held Mode | Overnight | First Hour | Rest of Day | Longs | Shorts | Top Detractors |")
    print("|---|---:|---:|---:|---|---:|---:|---:|---:|---:|---|")

    for label, row in worst:
        rows = replay_rows[label]
        idx = next(i for i, candidate in enumerate(rows) if candidate.day == row.day)
        if idx == 0:
            prev_day = row.day
            held_positions: Dict[str, int] = {}
            held_cfg = DayConfig(picker_mode="none", picker_reason="initial_day")
        else:
            prev_day = rows[idx - 1].day
            held_positions = replay_positions[label][prev_day]
            held_cfg = replay_cfgs[label].get(prev_day, DayConfig())

        longs, shorts = signed_position_sides(held_positions)
        overnight, first_hour, rest_day, contributions = leg_contributors(
            held_positions, prev_day, row.day, bars_dir, bars_cache
        )
        top_detractors = ", ".join(
            f"{sym}:{qty} ({pnl:.0f})" for pnl, sym, qty in contributions[:3]
        )
        print(
            f"| {label} | {row.day.isoformat()} | {row.pnl:.2f} | {row.equity:.2f} | "
            f"{held_cfg.leadership_mode or held_cfg.picker_mode or 'unknown'} | "
            f"{overnight:.2f} | {first_hour:.2f} | {rest_day:.2f} | {longs} | {shorts} | {top_detractors} |"
        )

    failure_rows = []
    for label, fail_days in failures.items():
        if fail_days:
            fail_list = ", ".join(day.isoformat() for day in fail_days)
            failure_rows.append(f"- `{label}` hit affordability failures on: {fail_list}")

    if failure_rows:
        print()
        print("## Replay Integrity Notes")
        print()
        for row in failure_rows:
            print(row)

    detailed_affordability_rows: List[str] = []
    for label, entries in affordability_failures.items():
        for entry in entries:
            top_delta_sample = ", ".join(entry.top_share_deltas[:3])
            detailed_affordability_rows.append(
                f"- `{label}` {entry.day.isoformat()}: {entry.kind}; gross_delta={entry.gross_delta:+.0f}, "
                f"incremental_exposure={entry.incremental_exposure:.0f}, buying_power={entry.buying_power:.0f}, "
                f"opens={entry.opens}, closes={entry.closes}, increases={entry.increases}, "
                f"reductions={entry.reductions}, flips={entry.flips}; top deltas: {top_delta_sample}"
            )
    if detailed_affordability_rows:
        print()
        print("## Affordability Failure Details")
        print()
        for row in detailed_affordability_rows:
            print(row)


if __name__ == "__main__":
    main()
