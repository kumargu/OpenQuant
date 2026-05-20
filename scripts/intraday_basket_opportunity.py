#!/usr/bin/env python3
from __future__ import annotations

import argparse
import json
import math
from collections import defaultdict
from dataclasses import dataclass
from datetime import date, datetime, time
from pathlib import Path
from typing import Dict, Iterable, List, Sequence
from zoneinfo import ZoneInfo

import pyarrow.parquet as pq


NY = ZoneInfo("America/New_York")
UTC = ZoneInfo("UTC")
SESSION_OPEN = time(9, 30)
SESSION_CLOSE_OPEN_TS = time(15, 59)


@dataclass
class BasketFit:
    sector: str
    target: str
    members: List[str]
    threshold_k: float
    mu: float
    sigma_eq: float


@dataclass
class TradeEvent:
    timestamp: datetime
    old_position: int
    new_position: int
    z_score: float
    spread: float


@dataclass
class DayResult:
    day: date
    pnl: float
    return_pct: float
    start_position: int
    end_position: int
    event_time: str
    event_action: str
    event_z: str


@dataclass
class ModeResult:
    name: str
    cumulative_return: float
    trade_count: int
    position_days: int
    daily: List[DayResult]
    trades: List[TradeEvent]


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Single-basket intraday opportunity experiment with one trade per day max."
    )
    parser.add_argument("--fit-artifact", required=True, help="Frozen fit artifact JSON path")
    parser.add_argument("--sector", required=True, help="Basket sector name")
    parser.add_argument("--target", required=True, help="Basket target symbol")
    parser.add_argument("--start", required=True, help="Start date (YYYY-MM-DD)")
    parser.add_argument("--end", required=True, help="End date (YYYY-MM-DD)")
    parser.add_argument(
        "--focus-start",
        help="Optional focus/report start date (YYYY-MM-DD). Simulation still starts at --start.",
    )
    parser.add_argument(
        "--focus-end",
        help="Optional focus/report end date (YYYY-MM-DD). Simulation still ends at --end.",
    )
    parser.add_argument(
        "--bars-dir",
        default="/Users/gulshan/quant-data/bars/v3_sp500_2024-2026_1min_adjusted",
        help="Directory containing per-symbol minute parquet bars",
    )
    parser.add_argument("--out", help="Optional markdown report output path")
    return parser.parse_args()


def load_fit(path: Path, sector: str, target: str) -> BasketFit:
    with path.open() as f:
        artifact = json.load(f)

    matches = []
    for fit in artifact["fits"]:
        if not fit.get("valid"):
            continue
        candidate = fit["candidate"]
        if candidate["sector"] == sector and candidate["target"] == target:
            ou = fit.get("ou")
            if not ou:
                continue
            matches.append(
                BasketFit(
                    sector=sector,
                    target=target,
                    members=list(candidate["members"]),
                    threshold_k=float(fit["threshold_k"]),
                    mu=float(ou["mu"]),
                    sigma_eq=float(ou["sigma_eq"]),
                )
            )

    if not matches:
        raise SystemExit(f"no valid basket fit found for sector={sector!r} target={target!r}")
    if len(matches) > 1:
        raise SystemExit(f"multiple valid basket fits found for sector={sector!r} target={target!r}")
    return matches[0]


def load_symbol_closes(
    symbol: str,
    bars_dir: Path,
    start_day: date,
    end_day: date,
) -> Dict[datetime, float]:
    table = pq.read_table(bars_dir / f"{symbol}.parquet", columns=["timestamp", "close"])
    data = table.to_pydict()
    out: Dict[datetime, float] = {}
    for ts, close in zip(data["timestamp"], data["close"]):
        if ts.tzinfo is None:
            ts = ts.replace(tzinfo=UTC)
        ts = ts.astimezone(NY)
        if ts.date() < start_day or ts.date() > end_day:
            continue
        if not (SESSION_OPEN <= ts.time() < time(16, 0)):
            continue
        px = float(close)
        if not math.isfinite(px) or px <= 0.0:
            continue
        out[ts] = px
    return out


def common_timestamps(symbol_maps: Sequence[Dict[datetime, float]]) -> List[datetime]:
    return sorted(set.intersection(*(set(symbol_map.keys()) for symbol_map in symbol_maps)))


def basket_return(position: int, prev_prices: Dict[str, float], curr_prices: Dict[str, float], peers: Sequence[str], target: str) -> float:
    if position == 0:
        return 0.0
    target_ret = curr_prices[target] / prev_prices[target] - 1.0
    peer_ret = sum(curr_prices[peer] / prev_prices[peer] - 1.0 for peer in peers) / len(peers)
    return position * (0.5 * target_ret - 0.5 * peer_ret)


def transition_position(position: int, z_score: float, threshold_k: float) -> int:
    if position == 0:
        if z_score < -threshold_k:
            return 1
        if z_score > threshold_k:
            return -1
        return 0
    if position == 1 and z_score > threshold_k:
        return -1
    if position == -1 and z_score < -threshold_k:
        return 1
    return position


def simulate_mode(
    mode_name: str,
    fit: BasketFit,
    timestamps: Iterable[datetime],
    prices_by_symbol: Dict[str, Dict[datetime, float]],
) -> ModeResult:
    position = 0
    equity = 1.0
    trades: List[TradeEvent] = []
    daily: List[DayResult] = []
    prev_prices: Dict[str, float] | None = None
    current_day: date | None = None
    day_start_equity = 1.0
    day_start_position = 0
    day_traded = False
    day_event: TradeEvent | None = None

    target = fit.target
    peers = fit.members

    for ts in timestamps:
        prices = {symbol: prices_by_symbol[symbol][ts] for symbol in [target, *peers]}
        spread = math.log(prices[target]) - sum(math.log(prices[peer]) for peer in peers) / len(peers)
        z_score = (spread - fit.mu) / fit.sigma_eq
        day = ts.date()

        if current_day is None or day != current_day:
            if current_day is not None:
                daily.append(
                    DayResult(
                        day=current_day,
                        pnl=equity - day_start_equity,
                        return_pct=(equity / day_start_equity - 1.0) * 100.0 if day_start_equity else 0.0,
                        start_position=day_start_position,
                        end_position=position,
                        event_time=day_event.timestamp.strftime("%H:%M") if day_event else "-",
                        event_action=(
                            f"{day_event.old_position}->{day_event.new_position}" if day_event else "-"
                        ),
                        event_z=f"{day_event.z_score:.3f}" if day_event else "-",
                    )
                )
            current_day = day
            day_start_equity = equity
            day_start_position = position
            day_traded = False
            day_event = None

        if prev_prices is not None:
            equity *= 1.0 + basket_return(position, prev_prices, prices, peers, target)

        should_evaluate = False
        if mode_name == "close":
            should_evaluate = ts.time() == SESSION_CLOSE_OPEN_TS
        elif mode_name == "intraday_first_breach":
            should_evaluate = not day_traded
        else:
            raise ValueError(f"unknown mode {mode_name}")

        if should_evaluate:
            new_position = transition_position(position, z_score, fit.threshold_k)
            if new_position != position:
                event = TradeEvent(
                    timestamp=ts,
                    old_position=position,
                    new_position=new_position,
                    z_score=z_score,
                    spread=spread,
                )
                trades.append(event)
                day_event = event
                position = new_position
                if mode_name == "intraday_first_breach":
                    day_traded = True

        prev_prices = prices

    if current_day is not None:
        daily.append(
            DayResult(
                day=current_day,
                pnl=equity - day_start_equity,
                return_pct=(equity / day_start_equity - 1.0) * 100.0 if day_start_equity else 0.0,
                start_position=day_start_position,
                end_position=position,
                event_time=day_event.timestamp.strftime("%H:%M") if day_event else "-",
                event_action=(f"{day_event.old_position}->{day_event.new_position}" if day_event else "-"),
                event_z=f"{day_event.z_score:.3f}" if day_event else "-",
            )
        )

    return ModeResult(
        name=mode_name,
        cumulative_return=(equity - 1.0) * 100.0,
        trade_count=len(trades),
        position_days=sum(1 for row in daily if row.start_position != 0 or row.end_position != 0),
        daily=daily,
        trades=trades,
    )


def render_report(
    fit: BasketFit,
    start_day: date,
    end_day: date,
    focus_start_day: date,
    focus_end_day: date,
    close_mode: ModeResult,
    intraday_mode: ModeResult,
) -> str:
    def fmt_pct(x: float) -> str:
        return f"{x:+.2f}%"

    lines: List[str] = []
    lines.append("# Single-Basket Intraday Opportunity Experiment")
    lines.append("")
    lines.append(
        f"- Basket: `{fit.sector}:{fit.target}` against `{', '.join(fit.members)}`"
    )
    lines.append(f"- Simulation Window: `{start_day}` to `{end_day}`")
    lines.append(f"- Report Window: `{focus_start_day}` to `{focus_end_day}`")
    lines.append(f"- Threshold k: `{fit.threshold_k:.4f}`")
    lines.append(f"- OU mu: `{fit.mu:.6f}`")
    lines.append(f"- OU sigma_eq: `{fit.sigma_eq:.6f}`")
    lines.append("")
    lines.append("## Summary")
    lines.append("")
    lines.append("| Mode | Cumulative Return | Trades | Days With Exposure |")
    lines.append("|---|---:|---:|---:|")
    for result in [close_mode, intraday_mode]:
        focus_daily = [
            row
            for row in result.daily
            if focus_start_day <= row.day <= focus_end_day
        ]
        focus_return = (
            math.prod(1.0 + row.return_pct / 100.0 for row in focus_daily) - 1.0
            if focus_daily
            else 0.0
        ) * 100.0
        lines.append(
            f"| {result.name} | {fmt_pct(focus_return)} | {result.trade_count} | "
            f"{sum(1 for row in focus_daily if row.start_position != 0 or row.end_position != 0)} |"
        )
    lines.append("")

    close_daily = {
        row.day: row
        for row in close_mode.daily
        if focus_start_day <= row.day <= focus_end_day
    }
    intraday_daily = {
        row.day: row
        for row in intraday_mode.daily
        if focus_start_day <= row.day <= focus_end_day
    }
    all_days = sorted(set(close_daily) | set(intraday_daily))
    lines.append("## Daily Comparison")
    lines.append("")
    lines.append(
        "| Date | Close PnL% | Intraday PnL% | Delta | Close Event | Intraday Event |"
    )
    lines.append("|---|---:|---:|---:|---|---|")
    for day in all_days:
        close_row = close_daily[day]
        intraday_row = intraday_daily[day]
        delta = intraday_row.return_pct - close_row.return_pct
        lines.append(
            f"| {day} | {fmt_pct(close_row.return_pct)} | {fmt_pct(intraday_row.return_pct)} | "
            f"{fmt_pct(delta)} | {close_row.event_time} {close_row.event_action} ({close_row.event_z}) | "
            f"{intraday_row.event_time} {intraday_row.event_action} ({intraday_row.event_z}) |"
        )
    lines.append("")

    lines.append("## Intraday Trade Events")
    lines.append("")
    focus_trades = [
        event
        for event in intraday_mode.trades
        if focus_start_day <= event.timestamp.date() <= focus_end_day
    ]
    if not focus_trades:
        lines.append("- none")
    else:
        for event in focus_trades:
            lines.append(
                f"- `{event.timestamp.isoformat()}` `{event.old_position}->{event.new_position}` "
                f"`z={event.z_score:.3f}` `spread={event.spread:.6f}`"
            )
    lines.append("")

    return "\n".join(lines)


def main() -> None:
    args = parse_args()
    fit_artifact = Path(args.fit_artifact)
    bars_dir = Path(args.bars_dir)
    start_day = date.fromisoformat(args.start)
    end_day = date.fromisoformat(args.end)
    focus_start_day = date.fromisoformat(args.focus_start) if args.focus_start else start_day
    focus_end_day = date.fromisoformat(args.focus_end) if args.focus_end else end_day

    fit = load_fit(fit_artifact, args.sector, args.target)
    symbols = [fit.target, *fit.members]
    prices_by_symbol = {
        symbol: load_symbol_closes(symbol, bars_dir, start_day, end_day) for symbol in symbols
    }
    timestamps = common_timestamps(list(prices_by_symbol.values()))
    if not timestamps:
        raise SystemExit("no common timestamps found for basket symbols in the requested window")

    close_mode = simulate_mode("close", fit, timestamps, prices_by_symbol)
    intraday_mode = simulate_mode("intraday_first_breach", fit, timestamps, prices_by_symbol)
    report = render_report(
        fit,
        start_day,
        end_day,
        focus_start_day,
        focus_end_day,
        close_mode,
        intraday_mode,
    )
    print(report)

    if args.out:
        out_path = Path(args.out)
        out_path.parent.mkdir(parents=True, exist_ok=True)
        out_path.write_text(report)


if __name__ == "__main__":
    main()
