"""
49-day Time-Travel Simulation — daily symbol selection + replay.

For each trading day, selects the top-12 symbols by a mean-reversion
tradability score computed from the PREVIOUS day's data (no lookahead),
then replays through the engine.

Usage:
    python data/multi_day_sim.py
    python data/multi_day_sim.py --top-n 15
    python data/multi_day_sim.py --post-issue 115
"""

import argparse
import json
import math
import os
import subprocess
import sys
import tempfile
from collections import defaultdict
from datetime import date, timedelta
from pathlib import Path

import numpy as np

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from data.time_travel import replay, load_bars

DATA_DIR = Path(__file__).parent
ROOT_DIR = DATA_DIR.parent


ALL_SYMBOLS = [
    "AAPL", "MSFT", "NVDA", "TSLA", "GOOGL", "META", "AMD", "AMZN", "AVGO", "ORCL",
    "JPM", "BAC", "GS", "MS", "C", "LLY", "UNH", "JNJ", "ABBV", "MRK", "PFE",
    "XOM", "CVX", "COP", "SLB", "WMT", "COST", "HD", "MCD", "NKE",
    "CAT", "BA", "GE", "RTX", "MU", "INTC", "QCOM", "TXN",
    "GLD", "SLV", "XLE", "XLF", "COIN", "PLTR", "SOFI",
]


def get_trading_days(end_date: date, count: int) -> list[date]:
    """Get `count` trading days ending on `end_date` (inclusive, weekdays only)."""
    days = []
    d = end_date
    while len(days) < count:
        if d.weekday() < 5:
            days.append(d)
        d -= timedelta(days=1)
    days.reverse()
    return days


def data_file_exists(d: date) -> bool:
    safe = d.strftime("%Y%m%d")
    return (DATA_DIR / f"experiment_bars_{safe}.json").exists()


def load_day_bars(d: date) -> dict[str, list[dict]] | None:
    """Load bars for a date, returning None if file doesn't exist or is empty."""
    safe = d.strftime("%Y-%m-%d")
    try:
        data = load_bars(safe)
        # Check it has meaningful data (not just empty lists)
        if not any(len(bars) > 0 for bars in data.values()):
            return None
        return data
    except FileNotFoundError:
        return None


def compute_symbol_scores(bars_by_symbol: dict[str, list[dict]]) -> dict[str, float]:
    """
    Score symbols for mean-reversion tradability.

    Score = -autocorrelation * 0.4 + relative_range * 0.3 + log(avg_volume) * 0.3

    Only symbols with > 200 bars are eligible.
    """
    scores = {}
    for sym, bars in bars_by_symbol.items():
        if len(bars) < 200:
            continue

        closes = np.array([b["close"] for b in bars])
        volumes = np.array([b["volume"] for b in bars])
        highs = np.array([b["high"] for b in bars])
        lows = np.array([b["low"] for b in bars])

        # 1-min returns
        returns = np.diff(closes) / closes[:-1]
        if len(returns) < 10:
            continue

        # Lag-1 autocorrelation (more negative = better for mean-reversion)
        if np.std(returns) < 1e-10:
            continue
        autocorr = np.corrcoef(returns[:-1], returns[1:])[0, 1]
        if np.isnan(autocorr):
            continue

        # Relative intraday range (high-low as fraction of close)
        avg_range = np.mean((highs - lows) / closes) if np.mean(closes) > 0 else 0

        # Average volume (log-scaled)
        avg_vol = np.mean(volumes)
        if avg_vol <= 0:
            continue
        log_vol = math.log(avg_vol)

        # Composite score
        score = (-autocorr) * 0.4 + avg_range * 100 * 0.3 + log_vol / 15.0 * 0.3
        scores[sym] = score

    return scores


def select_symbols(prev_day_bars: dict[str, list[dict]] | None, top_n: int) -> list[str]:
    """
    Select top_n symbols based on previous day's tradability score.
    If no previous day data, return all eligible symbols.
    """
    if prev_day_bars is None:
        return ALL_SYMBOLS

    scores = compute_symbol_scores(prev_day_bars)
    if not scores:
        return ALL_SYMBOLS

    ranked = sorted(scores.items(), key=lambda x: x[1], reverse=True)
    return [sym for sym, _ in ranked[:top_n]]


def create_filtered_data_file(d: date, symbols: list[str]) -> str | None:
    """
    Create a temporary data file containing only the selected symbols.
    Returns the temp file path, or None if no data.
    """
    all_bars = load_day_bars(d)
    if all_bars is None:
        return None

    filtered = {sym: all_bars[sym] for sym in symbols if sym in all_bars and len(all_bars[sym]) > 0}
    if not filtered:
        return None

    # Write to temp location with same naming convention
    safe = d.strftime("%Y%m%d")
    tmp_path = f"/tmp/sim_bars_{safe}.json"
    with open(tmp_path, "w") as f:
        json.dump(filtered, f)
    return tmp_path


def replay_day(d: date, symbols: list[str], config_path: str) -> dict | None:
    """Run time-travel replay for selected symbols on a given day."""
    date_str = d.strftime("%Y-%m-%d")

    # Create filtered data file
    tmp_data = create_filtered_data_file(d, symbols)
    if tmp_data is None:
        return None

    # Monkey-patch the data loading to use our filtered file
    safe = d.strftime("%Y%m%d")
    original_path = DATA_DIR / f"experiment_bars_{safe}.json"
    tmp_backup = None

    # Strategy: temporarily symlink the filtered file
    # Simpler: just call replay with the filtered data directly
    # We'll override load_bars by swapping the file temporarily
    import data.time_travel as tt
    original_load = tt.load_bars

    def patched_load(date_str_arg):
        with open(tmp_data) as f:
            return json.load(f)

    tt.load_bars = patched_load
    try:
        result = replay(
            date_str=date_str,
            config_path=config_path,
            speed=0,
            quiet=True,
            label=f"sim_{safe}",
        )
        return result
    except Exception as e:
        print(f"  ERROR replaying {date_str}: {e}")
        return None
    finally:
        tt.load_bars = original_load
        # Clean up temp file
        if os.path.exists(tmp_data):
            os.unlink(tmp_data)


def format_daily_table(daily_results: list[dict]) -> str:
    """Format daily P&L table as markdown."""
    lines = [
        "| # | Date | Symbols | Trades | WR | P&L | Cumulative |",
        "|---|------|---------|--------|-----|-----|-----------|",
    ]
    cumulative = 0.0
    for i, r in enumerate(daily_results, 1):
        pnl = r["total_pnl_incl_open"]
        cumulative += pnl
        symbols_str = ", ".join(r.get("selected_symbols", [])[:5])
        if len(r.get("selected_symbols", [])) > 5:
            symbols_str += f" +{len(r['selected_symbols']) - 5}"
        wr = f"{r['win_rate']:.0%}" if r['round_trips'] > 0 else "n/a"
        lines.append(
            f"| {i} | {r['date']} | {symbols_str} | "
            f"{r['round_trips']} | {wr} | ${pnl:+,.2f} | ${cumulative:+,.2f} |"
        )
    return "\n".join(lines)


def format_symbol_frequency(daily_results: list[dict]) -> str:
    """Format symbol frequency and P&L table."""
    sym_stats = defaultdict(lambda: {"picks": 0, "trades": 0, "pnl": 0.0, "wins": 0})

    for r in daily_results:
        for sym in r.get("selected_symbols", []):
            sym_stats[sym]["picks"] += 1
        for sym, data in r.get("per_symbol", {}).items():
            sym_stats[sym]["trades"] += data["trades"]
            sym_stats[sym]["pnl"] += data["pnl"]
            sym_stats[sym]["wins"] += int(data["win_rate"] * data["trades"])

    lines = [
        "| Symbol | Picked | Trades | Wins | WR | Total P&L |",
        "|--------|--------|--------|------|-----|-----------|",
    ]
    for sym in sorted(sym_stats.keys(), key=lambda s: sym_stats[s]["pnl"], reverse=True):
        s = sym_stats[sym]
        wr = f"{s['wins']/s['trades']:.0%}" if s['trades'] > 0 else "n/a"
        lines.append(
            f"| {sym} | {s['picks']} | {s['trades']} | {s['wins']} | {wr} | ${s['pnl']:+,.2f} |"
        )
    return "\n".join(lines)


def compute_sharpe(daily_pnls: list[float]) -> float:
    """Annualized Sharpe ratio from daily P&L."""
    if len(daily_pnls) < 2 or np.std(daily_pnls) == 0:
        return 0.0
    return (np.mean(daily_pnls) / np.std(daily_pnls)) * math.sqrt(252)


def run_simulation(top_n: int = 12, config_path: str = "openquant.toml") -> list[dict]:
    """Run the full multi-day simulation."""
    end_date = date(2026, 3, 20)
    trading_days = get_trading_days(end_date, 49)

    print(f"\n{'='*80}")
    print(f"  49-Day Time-Travel Simulation")
    print(f"  Period: {trading_days[0]} to {trading_days[-1]}")
    print(f"  Top-N symbols: {top_n}")
    print(f"  Config: {config_path}")
    print(f"{'='*80}\n")

    daily_results = []
    prev_day_bars = None
    skipped_holidays = []

    for i, d in enumerate(trading_days):
        date_str = d.strftime("%Y-%m-%d")

        # Check if data exists
        if not data_file_exists(d):
            print(f"  [{i+1:2d}/49] {date_str} — NO DATA (holiday/missing), skipping")
            skipped_holidays.append(date_str)
            continue

        # Load current day to check it has real data
        current_bars = load_day_bars(d)
        if current_bars is None:
            print(f"  [{i+1:2d}/49] {date_str} — empty data file, skipping")
            skipped_holidays.append(date_str)
            continue

        # Select symbols from previous day's data
        selected = select_symbols(prev_day_bars, top_n)

        # Filter to symbols that actually exist in today's data
        available = [s for s in selected if s in current_bars and len(current_bars[s]) > 0]
        if not available:
            print(f"  [{i+1:2d}/49] {date_str} — no eligible symbols in data, skipping")
            prev_day_bars = current_bars
            continue

        avail_str = ', '.join(available[:8])
        suffix = '...' if len(available) > 8 else ''
        print(f"  [{i+1:2d}/49] {date_str} — {len(available)} symbols: {avail_str}{suffix}", flush=True)

        # Run replay
        result = replay_day(d, available, config_path)
        if result is None:
            print(f"           -> replay failed, skipping")
            prev_day_bars = current_bars
            continue

        result["selected_symbols"] = available
        daily_results.append(result)

        pnl = result["total_pnl_incl_open"]
        trades = result["round_trips"]
        wr = f"{result['win_rate']:.0%}" if trades > 0 else "n/a"
        cum = sum(r["total_pnl_incl_open"] for r in daily_results)
        print(f"           -> {trades} trades, WR={wr}, P&L=${pnl:+,.2f}, cum=${cum:+,.2f}")

        # Update prev day bars for next iteration
        prev_day_bars = current_bars

    # Print summary
    print(f"\n{'='*80}")
    print(f"  SIMULATION COMPLETE")
    print(f"{'='*80}\n")

    if skipped_holidays:
        print(f"  Skipped dates (holidays/no data): {', '.join(skipped_holidays)}")

    if not daily_results:
        print("  No results to report!")
        return daily_results

    daily_pnls = [r["total_pnl_incl_open"] for r in daily_results]
    total_pnl = sum(daily_pnls)
    total_trades = sum(r["round_trips"] for r in daily_results)
    total_wins = sum(
        sum(1 for t in r["trades"] if t["pnl"] > 0)
        for r in daily_results
    )
    overall_wr = total_wins / total_trades if total_trades > 0 else 0
    profitable_days = sum(1 for p in daily_pnls if p > 0)
    sharpe = compute_sharpe(daily_pnls)

    best_day = max(daily_results, key=lambda r: r["total_pnl_incl_open"])
    worst_day = min(daily_results, key=lambda r: r["total_pnl_incl_open"])

    print(f"  Trading days: {len(daily_results)}")
    print(f"  Total P&L: ${total_pnl:+,.2f}")
    print(f"  Total trades: {total_trades}")
    print(f"  Overall win rate: {overall_wr:.0%}")
    print(f"  Profitable days: {profitable_days}/{len(daily_results)} ({profitable_days/len(daily_results)*100:.0f}%)")
    print(f"  Sharpe (annualized): {sharpe:.2f}")
    print(f"  Avg daily P&L: ${np.mean(daily_pnls):+,.2f}")
    print(f"  Std daily P&L: ${np.std(daily_pnls):,.2f}")
    print(f"  Best day: {best_day['date']} (${best_day['total_pnl_incl_open']:+,.2f})")
    print(f"  Worst day: {worst_day['date']} (${worst_day['total_pnl_incl_open']:+,.2f})")

    print(f"\n  Daily P&L Table:")
    print(format_daily_table(daily_results))

    print(f"\n  Symbol Frequency & P&L:")
    print(format_symbol_frequency(daily_results))

    return daily_results


def build_issue_comment(daily_results: list[dict], skipped: list[str]) -> str:
    """Build the full markdown comment for the GH issue."""
    if not daily_results:
        return "## 49-Day Simulation\n\nNo results — all dates skipped."

    daily_pnls = [r["total_pnl_incl_open"] for r in daily_results]
    total_pnl = sum(daily_pnls)
    total_trades = sum(r["round_trips"] for r in daily_results)
    total_wins = sum(
        sum(1 for t in r["trades"] if t["pnl"] > 0)
        for r in daily_results
    )
    overall_wr = total_wins / total_trades if total_trades > 0 else 0
    profitable_days = sum(1 for p in daily_pnls if p > 0)
    sharpe = compute_sharpe(daily_pnls)

    best_day = max(daily_results, key=lambda r: r["total_pnl_incl_open"])
    worst_day = min(daily_results, key=lambda r: r["total_pnl_incl_open"])

    # Equity curve data points
    cum = 0.0
    equity_points = []
    for r in daily_results:
        cum += r["total_pnl_incl_open"]
        equity_points.append((r["date"], cum))

    # Max drawdown
    peak = 0.0
    max_dd = 0.0
    for _, eq in equity_points:
        peak = max(peak, eq)
        dd = eq - peak
        max_dd = min(max_dd, dd)

    emoji = "+" if total_pnl >= 0 else "-"

    lines = [
        f"## 49-Day Time-Travel Simulation Results",
        "",
        f"**Period:** {daily_results[0]['date']} to {daily_results[-1]['date']} "
        f"({len(daily_results)} active trading days)",
        f"**Symbol selection:** Top-12 by mean-reversion tradability score (previous day, no lookahead)",
        f"**Config:** production `openquant.toml` (min_net_score=0.5, linear sizing, $10k max position)",
        "",
        "### Overall Statistics",
        "",
        "| Metric | Value |",
        "|--------|-------|",
        f"| **Total P&L** | **${total_pnl:+,.2f}** |",
        f"| Total trades | {total_trades} |",
        f"| Overall win rate | {overall_wr:.0%} |",
        f"| Profitable days | {profitable_days}/{len(daily_results)} ({profitable_days/len(daily_results)*100:.0f}%) |",
        f"| Sharpe (annualized) | {sharpe:.2f} |",
        f"| Avg daily P&L | ${np.mean(daily_pnls):+,.2f} |",
        f"| Std daily P&L | ${np.std(daily_pnls):,.2f} |",
        f"| Max drawdown | ${max_dd:+,.2f} |",
        f"| Best day | {best_day['date']} (${best_day['total_pnl_incl_open']:+,.2f}) |",
        f"| Worst day | {worst_day['date']} (${worst_day['total_pnl_incl_open']:+,.2f}) |",
        "",
        "### Daily P&L",
        "",
        format_daily_table(daily_results),
        "",
        "### Equity Curve",
        "",
        "```",
    ]

    # ASCII equity curve
    if equity_points:
        min_eq = min(eq for _, eq in equity_points)
        max_eq = max(eq for _, eq in equity_points)
        eq_range = max_eq - min_eq if max_eq != min_eq else 1.0
        width = 40
        for dt, eq in equity_points:
            bar_len = int((eq - min_eq) / eq_range * width)
            bar = "#" * max(bar_len, 0)
            lines.append(f"  {dt} | {bar} ${eq:+,.2f}")
    lines.extend(["```", ""])

    lines.extend([
        "### Symbol Selection Analysis",
        "",
        format_symbol_frequency(daily_results),
        "",
    ])

    if skipped:
        lines.extend([
            f"### Skipped Dates ({len(skipped)})",
            "",
            f"Holidays/no data: {', '.join(skipped)}",
            "",
        ])

    # Verdict
    lines.extend([
        "### Verdict",
        "",
        "**Is daily symbol selection better than static?**",
        "",
    ])

    if total_pnl > 0 and sharpe > 0.5:
        lines.append(
            "The dynamic symbol selection shows positive results with a reasonable "
            "Sharpe ratio. The tradability score successfully identifies symbols with "
            "stronger mean-reversion characteristics on a daily basis."
        )
    elif total_pnl > 0:
        lines.append(
            "The simulation is profitable but with modest risk-adjusted returns. "
            "The daily selection helps avoid poor-quality symbols but the edge is thin."
        )
    else:
        lines.append(
            "The simulation shows negative P&L over the test period. The dynamic "
            "symbol selection does not overcome the strategy's challenges in this "
            "market environment. Consider tuning the tradability score or the "
            "underlying strategy parameters."
        )

    return "\n".join(lines)


def main():
    parser = argparse.ArgumentParser(description="49-day multi-day time-travel simulation")
    parser.add_argument("--top-n", type=int, default=12, help="Top N symbols to select per day")
    parser.add_argument("--config", default="openquant.toml", help="Config file path")
    parser.add_argument("--post-issue", type=int, help="Post results to GH issue number")
    args = parser.parse_args()

    os.chdir(ROOT_DIR)
    results = run_simulation(top_n=args.top_n, config_path=args.config)

    # Collect skipped dates
    end_date = date(2026, 3, 20)
    trading_days = get_trading_days(end_date, 49)
    active_dates = {r["date"] for r in results}
    skipped = [d.strftime("%Y-%m-%d") for d in trading_days if d.strftime("%Y-%m-%d") not in active_dates]

    if args.post_issue and results:
        comment = build_issue_comment(results, skipped)
        # Write comment to temp file and post via gh
        with tempfile.NamedTemporaryFile(mode="w", suffix=".md", delete=False) as f:
            f.write(comment)
            tmp_comment = f.name

        try:
            subprocess.run(
                ["gh", "issue", "comment", str(args.post_issue), "--body-file", tmp_comment],
                check=True,
                cwd=str(ROOT_DIR),
            )
            print(f"\n  Posted results to issue #{args.post_issue}")
        except subprocess.CalledProcessError as e:
            print(f"\n  Failed to post to issue: {e}")
            print(f"  Comment saved to: {tmp_comment}")
        finally:
            if os.path.exists(tmp_comment):
                os.unlink(tmp_comment)

    # Also save raw results
    out_path = DATA_DIR / "sim_49day_results.json"
    with open(out_path, "w") as f:
        json.dump(results, f, indent=2, default=str)
    print(f"\n  Raw results saved to {out_path}")


if __name__ == "__main__":
    main()
