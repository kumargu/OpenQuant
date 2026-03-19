"""
Generate quantstats tearsheets from backtest results.

Converts the Rust engine's backtest output (equity curve + trade records)
into a pandas returns series and generates a full HTML tearsheet.

Usage:
  python -m paper_trading.tearsheet                          # all symbols, 30d
  python -m paper_trading.tearsheet --days 90                # all symbols, 90d
  python -m paper_trading.tearsheet --category crypto        # single category
  python -m paper_trading.tearsheet --output reports/my.html # custom output path
"""

from __future__ import annotations

import argparse
from datetime import datetime, timezone
from pathlib import Path

import pandas as pd
import quantstats as qs

from paper_trading.benchmark import (
    CATEGORIES,
    fetch_bars,
)

REPORTS_DIR = Path(__file__).parent.parent / "reports"


def equity_curve_to_returns(
    equity_curve: list[float],
    timestamps: list[int],
    starting_capital: float = 100_000.0,
) -> pd.Series:
    """Convert cumulative P&L equity curve to a percentage returns series.

    The backtest equity curve is cumulative P&L (starting at 0).
    We convert to a returns series by:
      1. Adding starting_capital to get absolute equity
      2. Computing percentage change (pct_change)
      3. Indexing by timestamps
    """
    if not equity_curve or not timestamps:
        return pd.Series(dtype=float)

    # Build datetime index from millisecond timestamps
    index = pd.DatetimeIndex(
        [pd.Timestamp(ts, unit="ms", tz="UTC") for ts in timestamps]
    )

    # Convert cumulative P&L to absolute equity, then to returns
    equity = pd.Series(
        [starting_capital + pnl for pnl in equity_curve],
        index=index,
    )
    returns = equity.pct_change().fillna(0.0)

    # Drop duplicate timestamps (can happen with multi-symbol aggregation)
    returns = returns[~returns.index.duplicated(keep="first")]

    return returns


def trades_to_returns(
    trades: list[dict],
    timestamps: list[int],
    starting_capital: float = 100_000.0,
) -> pd.Series:
    """Build a returns series from trade records and bar timestamps.

    For each bar, the return is the P&L of any trade that closed on that bar,
    divided by the starting capital. Bars with no trade closings have 0 return.
    """
    if not trades or not timestamps:
        return pd.Series(dtype=float)

    index = pd.DatetimeIndex(
        [pd.Timestamp(ts, unit="ms", tz="UTC") for ts in timestamps]
    )
    returns = pd.Series(0.0, index=index)

    # Map exit_time to P&L
    for trade in trades:
        exit_ts = pd.Timestamp(trade["exit_time"], unit="ms", tz="UTC")
        if exit_ts in returns.index:
            returns.loc[exit_ts] += trade["pnl"] / starting_capital

    returns = returns[~returns.index.duplicated(keep="first")]
    return returns


def run_backtest_for_tearsheet(
    symbols: list[str],
    days: int = 30,
    timeframe: str = "1Min",
) -> tuple[pd.Series, dict]:
    """Run backtests across symbols and aggregate into a single returns series.

    Returns (returns_series, summary_stats).
    """
    from openquant import backtest, validate_bars
    from paper_trading.config import engine_kwargs

    params = engine_kwargs()

    all_returns = []
    total_trades = 0
    total_pnl = 0.0

    for symbol in symbols:
        bars = fetch_bars(symbol, days, timeframe)
        if not bars:
            continue

        quality = validate_bars(bars)
        if quality["has_critical_issues"]:
            print(f"  {symbol}: SKIPPED — critical data quality issues")
            continue

        result = backtest(bars, **params)

        timestamps = [bar[1] for bar in bars]
        returns = equity_curve_to_returns(
            result["equity_curve"], timestamps
        )

        if not returns.empty:
            all_returns.append(returns)
            total_trades += result["total_trades"]
            total_pnl += result["total_pnl"]
            print(
                f"  {symbol}: {result['total_trades']} trades, "
                f"${result['total_pnl']:+,.2f} P&L"
            )

    if not all_returns:
        return pd.Series(dtype=float), {}

    # Aggregate: align all series by timestamp, sum returns per bar.
    # Note: summing percentage returns assumes equal capital allocation
    # across symbols. At equal notional sizing this is a close approximation.
    combined = pd.concat(all_returns, axis=1).fillna(0.0).sum(axis=1)
    combined = combined.sort_index()

    summary = {
        "symbols": len(all_returns),
        "total_trades": total_trades,
        "total_pnl": total_pnl,
        "start": combined.index[0].isoformat(),
        "end": combined.index[-1].isoformat(),
    }

    return combined, summary


def generate_tearsheet(
    returns: pd.Series,
    output_path: str | Path = "reports/tearsheet.html",
    title: str = "OpenQuant Strategy",
    benchmark: str | None = "SPY",
) -> Path:
    """Generate a full quantstats HTML tearsheet.

    Args:
        benchmark: Ticker for benchmark comparison. Use "SPY" for equity
            categories, None for crypto-only runs.

    Returns the path to the generated HTML file.
    """
    output = Path(output_path)
    output.parent.mkdir(parents=True, exist_ok=True)

    qs.reports.html(
        returns,
        benchmark=benchmark,
        output=str(output),
        title=title,
        download_filename=output.stem,
    )

    return output


def main():
    parser = argparse.ArgumentParser(
        description="Generate quantstats tearsheet from backtest results"
    )
    parser.add_argument("--days", "-d", type=int, default=30)
    parser.add_argument("--timeframe", "-t", default="1Min")
    parser.add_argument("--category", "-c", help="Single category to test")
    parser.add_argument(
        "--output", "-o", default=None, help="Output HTML path"
    )
    parser.add_argument(
        "--baseline",
        action="store_true",
        help="Save as baseline tearsheet",
    )
    args = parser.parse_args()

    # Build symbol list
    if args.category:
        symbols = CATEGORIES.get(args.category, [])
        if not symbols:
            print(f"Unknown category: {args.category}")
            return
        cat_label = args.category
    else:
        symbols = [s for syms in CATEGORIES.values() for s in syms]
        cat_label = "all"

    print(f"Running backtests for tearsheet ({len(symbols)} symbols, {args.days}d)...")
    returns, summary = run_backtest_for_tearsheet(
        symbols, args.days, args.timeframe
    )

    if returns.empty:
        print("No returns data — cannot generate tearsheet.")
        return

    # Print headline metrics so you don't have to open the HTML
    sharpe = qs.stats.sharpe(returns)
    max_dd = qs.stats.max_drawdown(returns)
    print(f"\nAggregated: {summary['total_trades']} trades, "
          f"${summary['total_pnl']:+,.2f} P&L")
    print(f"Sharpe: {sharpe:.2f}, Max DD: {max_dd:.1%}")

    # Determine output path
    if args.output:
        output_path = Path(args.output)
    elif args.baseline:
        output_path = REPORTS_DIR / "baseline_tearsheet.html"
    else:
        timestamp = datetime.now(timezone.utc).strftime("%Y%m%d_%H%M%S")
        output_path = REPORTS_DIR / f"tearsheet_{cat_label}_{timestamp}.html"

    # Use SPY benchmark for equity categories, None for crypto
    benchmark = None if cat_label == "crypto" else "SPY"
    title = f"OpenQuant — {cat_label} ({args.days}d {args.timeframe})"
    path = generate_tearsheet(returns, output_path, title, benchmark=benchmark)
    print(f"\nTearsheet saved to: {path}")


if __name__ == "__main__":
    main()
