#!/usr/bin/env python3
from __future__ import annotations

import argparse
import json
import math
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
    basket_id: str
    sector: str
    target: str
    members: List[str]
    threshold_k: float
    mu: float
    sigma_eq: float


@dataclass
class BasketState:
    position: int = 0
    last_z: float = 0.0
    traded_today: bool = False


@dataclass
class DayResult:
    day: date
    equity: float
    pnl: float


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Replay-only event-driven intraday portfolio experiment for basket core."
    )
    parser.add_argument("--fit-artifact", required=True)
    parser.add_argument("--start", required=True, help="Simulation start date (YYYY-MM-DD)")
    parser.add_argument("--end", required=True, help="Simulation end date (YYYY-MM-DD)")
    parser.add_argument("--focus-start", help="Optional report start date (YYYY-MM-DD)")
    parser.add_argument("--focus-end", help="Optional report end date (YYYY-MM-DD)")
    parser.add_argument(
        "--bars-dir",
        default="/Users/gulshan/quant-data/bars/v3_sp500_2024-2026_1min_adjusted",
    )
    parser.add_argument("--capital", type=float, default=10000.0)
    parser.add_argument("--leverage", type=float, default=4.0)
    parser.add_argument("--n-active-baskets", type=int, default=5)
    parser.add_argument("--out")
    return parser.parse_args()


def canonical_basket_id(candidate: dict) -> str:
    # Match the Rust `DefaultHasher`-based ID indirectly by using the stored
    # basket_id from the replay-era fit file when present; otherwise rebuild it
    # in the simple visible form.
    members = ",".join(sorted(candidate["members"]))
    return f'{candidate["sector"]}:{candidate["target"]}:{candidate["fit_date"]}:{members}'


def load_fits(path: Path) -> List[BasketFit]:
    with path.open() as f:
        artifact = json.load(f)
    fits: List[BasketFit] = []
    for fit in artifact["fits"]:
        if not fit.get("valid"):
            continue
        candidate = fit["candidate"]
        ou = fit.get("ou")
        if not ou:
            continue
        fits.append(
            BasketFit(
                basket_id=canonical_basket_id(candidate),
                sector=candidate["sector"],
                target=candidate["target"],
                members=list(candidate["members"]),
                threshold_k=float(fit["threshold_k"]),
                mu=float(ou["mu"]),
                sigma_eq=float(ou["sigma_eq"]),
            )
        )
    return fits


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


def basket_legs(fit: BasketFit, position: int, notional: float) -> Dict[str, float]:
    if position == 0:
        return {}
    sign = float(position)
    peer_weight = -sign * notional * 0.5 / len(fit.members)
    out = {fit.target: sign * notional * 0.5}
    for peer in fit.members:
        out[peer] = out.get(peer, 0.0) + peer_weight
    return out


def plan_portfolio(
    fits: Sequence[BasketFit],
    states: Dict[str, BasketState],
    capital: float,
    leverage: float,
    n_active_baskets: int,
) -> tuple[Dict[str, float], List[str]]:
    active = []
    for fit in fits:
        state = states[fit.basket_id]
        if state.position == 0:
            continue
        active.append((fit.basket_id, abs(state.last_z)))
    active.sort(key=lambda item: (-item[1], item[0]))
    selected_ids = [basket_id for basket_id, _ in active[:n_active_baskets]]
    notional_per_basket = capital * leverage / n_active_baskets
    selected = set(selected_ids)
    notionals: Dict[str, float] = {}
    for fit in fits:
        if fit.basket_id not in selected:
            continue
        legs = basket_legs(fit, states[fit.basket_id].position, notional_per_basket)
        for symbol, notional in legs.items():
            notionals[symbol] = notionals.get(symbol, 0.0) + notional
    return notionals, selected_ids


def target_shares_from_notionals(
    notionals: Dict[str, float], prices: Dict[str, float]
) -> Dict[str, int]:
    shares: Dict[str, int] = {}
    for symbol, notional in notionals.items():
        px = prices.get(symbol)
        if px is None or px <= 0.0 or not math.isfinite(px):
            continue
        qty = math.trunc(notional / px)
        if qty != 0:
            shares[symbol] = qty
    return shares


def market_value(shares: Dict[str, int], prices: Dict[str, float]) -> float:
    return sum(qty * prices[symbol] for symbol, qty in shares.items() if symbol in prices)


def simulate(
    mode: str,
    fits: Sequence[BasketFit],
    timestamps: Sequence[datetime],
    prices_by_symbol: Dict[str, Dict[datetime, float]],
    capital: float,
    leverage: float,
    n_active_baskets: int,
) -> tuple[List[DayResult], Dict[str, int], float]:
    states = {fit.basket_id: BasketState() for fit in fits}
    current_shares: Dict[str, int] = {}
    cash = capital
    current_day: date | None = None
    daily: List[DayResult] = []
    day_start_equity = capital
    last_prices: Dict[str, float] = {}

    for ts in timestamps:
        day = ts.date()
        prices = {
            symbol: price_map[ts]
            for symbol, price_map in prices_by_symbol.items()
            if ts in price_map
        }

        if current_day is None or day != current_day:
            if current_day is not None:
                equity = cash + market_value(current_shares, last_prices)
                daily.append(
                    DayResult(
                        day=current_day,
                        equity=equity,
                        pnl=equity - day_start_equity,
                    )
                )
                day_start_equity = equity
            current_day = day
            for state in states.values():
                state.traded_today = False

        transitions = False
        for fit in fits:
            symbols = [fit.target, *fit.members]
            if any(symbol not in prices for symbol in symbols):
                continue
            spread = math.log(prices[fit.target]) - sum(
                math.log(prices[peer]) for peer in fit.members
            ) / len(fit.members)
            z_score = (spread - fit.mu) / fit.sigma_eq
            state = states[fit.basket_id]
            state.last_z = z_score

            should_evaluate = False
            if mode == "close":
                should_evaluate = ts.time() == SESSION_CLOSE_OPEN_TS
            elif mode == "event_driven":
                should_evaluate = not state.traded_today
            else:
                raise ValueError(mode)

            if not should_evaluate:
                continue

            new_position = transition_position(state.position, z_score, fit.threshold_k)
            if new_position != state.position:
                state.position = new_position
                state.traded_today = True
                transitions = True

        if transitions:
            notionals, _selected = plan_portfolio(
                fits, states, capital, leverage, n_active_baskets
            )
            target_shares = target_shares_from_notionals(notionals, prices)
            # Rebalance immediately at current minute prices.
            all_symbols = set(current_shares) | set(target_shares)
            for symbol in sorted(all_symbols):
                old_qty = current_shares.get(symbol, 0)
                new_qty = target_shares.get(symbol, 0)
                delta = new_qty - old_qty
                if delta:
                    cash -= delta * prices[symbol]
            current_shares = target_shares

        last_prices = prices

    if current_day is not None:
        equity = cash + market_value(current_shares, last_prices)
        daily.append(DayResult(day=current_day, equity=equity, pnl=equity - day_start_equity))
        final_equity = equity
    else:
        final_equity = capital

    return daily, current_shares, final_equity


def focus_metrics(daily: Sequence[DayResult], focus_start: date, focus_end: date) -> tuple[float, int]:
    window = [row for row in daily if focus_start <= row.day <= focus_end]
    if not window:
        return 0.0, 0
    start_equity = window[0].equity - window[0].pnl
    end_equity = window[-1].equity
    return (end_equity / start_equity - 1.0) * 100.0, len(window)


def render_report(
    start_day: date,
    end_day: date,
    focus_start: date,
    focus_end: date,
    close_daily: Sequence[DayResult],
    event_daily: Sequence[DayResult],
) -> str:
    close_return, close_days = focus_metrics(close_daily, focus_start, focus_end)
    event_return, event_days = focus_metrics(event_daily, focus_start, focus_end)
    close_map = {row.day: row for row in close_daily if focus_start <= row.day <= focus_end}
    event_map = {row.day: row for row in event_daily if focus_start <= row.day <= focus_end}

    lines = [
        "# Intraday Basket Portfolio Experiment",
        "",
        f"- Simulation Window: `{start_day}` to `{end_day}`",
        f"- Report Window: `{focus_start}` to `{focus_end}`",
        "- Strategy shape: basket core only, all valid baskets, active-basket cap enforced, one transition per basket per day max in event-driven mode.",
        "",
        "## Summary",
        "",
        "| Mode | Return | Days |",
        "|---|---:|---:|",
        f"| close | {close_return:+.2f}% | {close_days} |",
        f"| event_driven | {event_return:+.2f}% | {event_days} |",
        "",
        "## Daily Comparison",
        "",
        "| Date | Close PnL | Event-Driven PnL | Delta |",
        "|---|---:|---:|---:|",
    ]
    for day in sorted(set(close_map) | set(event_map)):
        close_row = close_map[day]
        event_row = event_map[day]
        delta = event_row.pnl - close_row.pnl
        lines.append(
            f"| {day} | {close_row.pnl:+.2f} | {event_row.pnl:+.2f} | {delta:+.2f} |"
        )
    lines.append("")
    return "\n".join(lines)


def main() -> None:
    args = parse_args()
    fit_artifact = Path(args.fit_artifact)
    bars_dir = Path(args.bars_dir)
    start_day = date.fromisoformat(args.start)
    end_day = date.fromisoformat(args.end)
    focus_start = date.fromisoformat(args.focus_start) if args.focus_start else start_day
    focus_end = date.fromisoformat(args.focus_end) if args.focus_end else end_day

    fits = load_fits(fit_artifact)
    symbols = sorted({symbol for fit in fits for symbol in [fit.target, *fit.members]})
    prices_by_symbol = {
        symbol: load_symbol_closes(symbol, bars_dir, start_day, end_day) for symbol in symbols
    }
    timestamps = common_timestamps(list(prices_by_symbol.values()))
    close_daily, _, _ = simulate(
        "close",
        fits,
        timestamps,
        prices_by_symbol,
        args.capital,
        args.leverage,
        args.n_active_baskets,
    )
    event_daily, _, _ = simulate(
        "event_driven",
        fits,
        timestamps,
        prices_by_symbol,
        args.capital,
        args.leverage,
        args.n_active_baskets,
    )
    report = render_report(
        start_day, end_day, focus_start, focus_end, close_daily, event_daily
    )
    print(report)
    if args.out:
        out_path = Path(args.out)
        out_path.parent.mkdir(parents=True, exist_ok=True)
        out_path.write_text(report)


if __name__ == "__main__":
    main()
