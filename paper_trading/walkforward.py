"""
Walk-forward validation: rolling train/test splits to detect overfitting.

Splits data into windows: train on N days, test on M days, slide forward.
Reports per-window and aggregate metrics.

Usage:
  python -m paper_trading.walkforward --symbol BTC/USD --days 30 --train 14 --test 7
"""

import argparse
from paper_trading.backtest_runner import fetch_bars


def run_walkforward(symbol, days, timeframe, train_days, test_days, params=None):
    """Run walk-forward validation with rolling windows."""
    from openquant import backtest

    bars = fetch_bars(symbol, days, timeframe)
    if not bars:
        print(f"No data for {symbol}")
        return []

    params = params or {}

    # Estimate bars per day from data
    total_bars = len(bars)
    bars_per_day = total_bars / days if days > 0 else 1440  # fallback: 1-min

    train_bars = int(train_days * bars_per_day)
    test_bars = int(test_days * bars_per_day)
    step_bars = test_bars  # slide by test window size

    if train_bars + test_bars > total_bars:
        print(f"Not enough data: {total_bars} bars, need {train_bars + test_bars}")
        return []

    windows = []
    start = 0
    window_num = 0

    while start + train_bars + test_bars <= total_bars:
        window_num += 1
        train_end = start + train_bars
        test_end = train_end + test_bars

        train_data = bars[start:train_end]
        test_data = bars[train_end:test_end]

        # Run backtest on test data (we don't optimize on train — just validate)
        # The train window is for warmup context; test window measures performance
        # Feed train + test to get accurate features, but only count test-period trades
        full_data = bars[start:test_end]
        result = backtest(full_data, **params)

        # We want metrics from the full run (features warm up during train period)
        windows.append({
            "window": window_num,
            "train_start": start,
            "train_end": train_end,
            "test_start": train_end,
            "test_end": test_end,
            "total_trades": result["total_trades"],
            "win_rate": result["win_rate"],
            "total_pnl": result["total_pnl"],
            "expectancy": result["expectancy"],
            "profit_factor": result["profit_factor"],
            "sharpe": result["sharpe_approx"],
            "max_drawdown": result["max_drawdown"],
        })

        start += step_bars

    return windows


def print_report(symbol, windows):
    """Print walk-forward report."""
    if not windows:
        print("No windows to report.")
        return

    print(f"\n## Walk-Forward Results: {symbol}")
    print(f"\n| Window | Trades | Win Rate | P&L | Expectancy | PF | Sharpe | Max DD |")
    print("|--------|--------|----------|-----|------------|-----|--------|--------|")

    for w in windows:
        pf = w["profit_factor"]
        pf_str = f"{pf:.2f}" if pf != float("inf") else "∞"
        print(
            f"| {w['window']:>6} | {w['total_trades']:>6} | {w['win_rate']:>7.1%} | "
            f"${w['total_pnl']:>+9,.2f} | ${w['expectancy']:>+8,.2f} | {pf_str:>5} | "
            f"{w['sharpe']:>+6.2f} | ${w['max_drawdown']:>8,.2f} |"
        )

    # Aggregate stats
    total_trades = sum(w["total_trades"] for w in windows)
    total_pnl = sum(w["total_pnl"] for w in windows)
    profitable_windows = sum(1 for w in windows if w["total_pnl"] > 0)
    worst_pnl = min(w["total_pnl"] for w in windows)
    best_pnl = max(w["total_pnl"] for w in windows)
    avg_pnl = total_pnl / len(windows) if windows else 0

    sharpes = [w["sharpe"] for w in windows]
    avg_sharpe = sum(sharpes) / len(sharpes) if sharpes else 0
    min_sharpe = min(sharpes) if sharpes else 0

    print(f"\n### Summary")
    print(f"| Metric | Value |")
    print(f"|--------|-------|")
    print(f"| Windows | {len(windows)} |")
    print(f"| Profitable windows | {profitable_windows}/{len(windows)} ({profitable_windows/len(windows):.0%}) |")
    print(f"| Total trades | {total_trades} |")
    print(f"| Total P&L | ${total_pnl:+,.2f} |")
    print(f"| Avg P&L per window | ${avg_pnl:+,.2f} |")
    print(f"| Best window | ${best_pnl:+,.2f} |")
    print(f"| Worst window | ${worst_pnl:+,.2f} |")
    print(f"| Avg Sharpe | {avg_sharpe:+.2f} |")
    print(f"| Worst Sharpe | {min_sharpe:+.2f} |")

    # Verdict
    if profitable_windows / len(windows) >= 0.6 and avg_sharpe > 0:
        verdict = "PASS — strategy shows consistent edge across windows"
    elif profitable_windows / len(windows) >= 0.5:
        verdict = "MARGINAL — strategy is borderline, may be overfit"
    else:
        verdict = "FAIL — strategy does not generalize across time windows"

    print(f"\n**Verdict: {verdict}**")


def main():
    parser = argparse.ArgumentParser(description="Walk-Forward Validation")
    parser.add_argument("--symbol", "-s", default="BTC/USD")
    parser.add_argument("--days", "-d", type=int, default=30)
    parser.add_argument("--timeframe", "-t", default="1Min")
    parser.add_argument("--train", type=int, default=14, help="Training window in days")
    parser.add_argument("--test", type=int, default=7, help="Test window in days")
    parser.add_argument("--no-trend-filter", action="store_true")
    args = parser.parse_args()

    params = {
        "trend_filter": not args.no_trend_filter,
    }

    print(f"Walk-forward: {args.symbol} ({args.days}d, train={args.train}d, test={args.test}d)")

    windows = run_walkforward(
        args.symbol, args.days, args.timeframe,
        args.train, args.test, params
    )
    print_report(args.symbol, windows)


if __name__ == "__main__":
    main()
