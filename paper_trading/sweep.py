"""
Parameter sweep tool: grid search over strategy parameters for a symbol.

Usage:
  python -m paper_trading.sweep --symbol BTC/USD --days 7
  python -m paper_trading.sweep --symbol ETH/USD --days 30 --param buy_z --range -3.0:-1.5:0.1
  python -m paper_trading.sweep --symbol BTC/USD --days 7 --param stop_loss_atr --range 1.0:4.0:0.5
"""

import argparse
import itertools
from paper_trading.backtest_runner import fetch_bars


# Parameter definitions: name -> (backtest kwarg, values)
PARAM_GRIDS = {
    "buy_z": {
        "kwarg": "buy_z_threshold",
        "default_range": (-3.0, -1.5, 0.1),
    },
    "sell_z": {
        "kwarg": "sell_z_threshold",
        "default_range": (1.5, 3.0, 0.25),
    },
    "min_vol": {
        "kwarg": "min_relative_volume",
        "default_range": (0.8, 2.0, 0.2),
    },
    "stop_loss_atr": {
        "kwarg": "stop_loss_atr_mult",
        "default_range": (1.0, 4.0, 0.5),
    },
    "max_hold": {
        "kwarg": "max_hold_bars",
        "default_range": (50, 200, 25),
        "int": True,
    },
}


def frange(start, stop, step):
    """Float range generator."""
    vals = []
    v = start
    while v <= stop + 1e-9:
        vals.append(round(v, 4))
        v += step
    return vals


def parse_range(range_str, is_int=False):
    """Parse 'start:stop:step' into list of values."""
    parts = range_str.split(":")
    if len(parts) != 3:
        raise ValueError(f"Range must be start:stop:step, got '{range_str}'")
    start, stop, step = float(parts[0]), float(parts[1]), float(parts[2])
    vals = frange(start, stop, step)
    if is_int:
        vals = [int(v) for v in vals]
    return vals


def run_sweep(symbol, days, timeframe, param_name, values, base_params):
    """Run backtest for each parameter value, return results."""
    from openquant import backtest

    bars = fetch_bars(symbol, days, timeframe)
    if not bars:
        print(f"No data for {symbol}")
        return []

    results = []
    param_kwarg = PARAM_GRIDS[param_name]["kwarg"]

    for val in values:
        params = dict(base_params)
        params[param_kwarg] = val
        result = backtest(bars, **params)
        results.append({
            "value": val,
            "trades": result["total_trades"],
            "win_rate": result["win_rate"],
            "total_pnl": result["total_pnl"],
            "expectancy": result["expectancy"],
            "profit_factor": result["profit_factor"],
            "sharpe": result["sharpe_approx"],
            "max_dd": result["max_drawdown"],
        })

    return results


def run_multi_sweep(symbol, days, timeframe, param_names, param_values_list, base_params):
    """Run grid search over multiple parameters."""
    from openquant import backtest

    bars = fetch_bars(symbol, days, timeframe)
    if not bars:
        print(f"No data for {symbol}")
        return []

    param_kwargs = [PARAM_GRIDS[p]["kwarg"] for p in param_names]
    combos = list(itertools.product(*param_values_list))

    results = []
    for combo in combos:
        params = dict(base_params)
        for kwarg, val in zip(param_kwargs, combo):
            params[kwarg] = val
        result = backtest(bars, **params)
        results.append({
            "params": dict(zip(param_names, combo)),
            "trades": result["total_trades"],
            "win_rate": result["win_rate"],
            "total_pnl": result["total_pnl"],
            "expectancy": result["expectancy"],
            "profit_factor": result["profit_factor"],
            "sharpe": result["sharpe_approx"],
            "max_dd": result["max_drawdown"],
        })

    return results


def print_single_sweep(param_name, results):
    """Print markdown table for single-param sweep."""
    print(f"\n| {param_name} | Trades | Win Rate | P&L | Expectancy | PF | Sharpe | Max DD |")
    print("|--------|--------|----------|-----|------------|-----|--------|--------|")
    for r in results:
        pnl = r["total_pnl"]
        pf = r["profit_factor"]
        pf_str = f"{pf:.2f}" if pf != float("inf") else "∞"
        print(
            f"| {r['value']:>8} | {r['trades']:>6} | {r['win_rate']:>7.1%} | "
            f"${pnl:>+9,.2f} | ${r['expectancy']:>+8,.2f} | {pf_str:>5} | "
            f"{r['sharpe']:>+6.2f} | ${r['max_dd']:>8,.2f} |"
        )

    # Highlight best by P&L
    if results:
        best = max(results, key=lambda r: r["total_pnl"])
        print(f"\n**Best by P&L**: {param_name}={best['value']} → "
              f"${best['total_pnl']:+,.2f} ({best['trades']} trades, "
              f"{best['win_rate']:.0%} WR)")


def print_multi_sweep(param_names, results):
    """Print top results from multi-param sweep."""
    # Sort by P&L descending
    results.sort(key=lambda r: r["total_pnl"], reverse=True)

    header_params = " | ".join(param_names)
    print(f"\n| {header_params} | Trades | Win Rate | P&L | PF | Sharpe |")
    sep = " | ".join(["--------"] * len(param_names))
    print(f"| {sep} | ------ | -------- | --- | -- | ------ |")

    for r in results[:20]:  # top 20
        param_vals = " | ".join(f"{r['params'][p]:>8}" for p in param_names)
        pf = r["profit_factor"]
        pf_str = f"{pf:.2f}" if pf != float("inf") else "∞"
        print(
            f"| {param_vals} | {r['trades']:>6} | {r['win_rate']:>7.1%} | "
            f"${r['total_pnl']:>+9,.2f} | {pf_str:>5} | {r['sharpe']:>+6.2f} |"
        )

    if results:
        best = results[0]
        print(f"\n**Best**: {best['params']} → ${best['total_pnl']:+,.2f}")


def main():
    parser = argparse.ArgumentParser(description="OpenQuant Parameter Sweep")
    parser.add_argument("--symbol", "-s", default="BTC/USD")
    parser.add_argument("--days", "-d", type=int, default=7)
    parser.add_argument("--timeframe", "-t", default="1Min")
    parser.add_argument("--param", "-p", action="append",
                        help="Parameter to sweep (can specify multiple). Options: "
                             + ", ".join(PARAM_GRIDS.keys()))
    parser.add_argument("--range", "-r", action="append",
                        help="Range as start:stop:step (one per --param)")
    parser.add_argument("--no-trend-filter", action="store_true")
    args = parser.parse_args()

    # Base params
    base_params = {
        "trend_filter": not args.no_trend_filter,
    }

    param_names = args.param or ["buy_z"]

    # Validate params
    for p in param_names:
        if p not in PARAM_GRIDS:
            print(f"Unknown param '{p}'. Options: {', '.join(PARAM_GRIDS.keys())}")
            return

    # Build value lists
    param_values_list = []
    for i, p in enumerate(param_names):
        grid = PARAM_GRIDS[p]
        is_int = grid.get("int", False)

        if args.range and i < len(args.range):
            values = parse_range(args.range[i], is_int)
        else:
            start, stop, step = grid["default_range"]
            values = frange(start, stop, step)
            if is_int:
                values = [int(v) for v in values]

        param_values_list.append(values)

    total_combos = 1
    for v in param_values_list:
        total_combos *= len(v)

    print(f"Sweeping {' × '.join(param_names)} on {args.symbol} ({args.days}d)")
    print(f"  {total_combos} combinations to test")

    if len(param_names) == 1:
        results = run_sweep(
            args.symbol, args.days, args.timeframe,
            param_names[0], param_values_list[0], base_params
        )
        print_single_sweep(param_names[0], results)
    else:
        results = run_multi_sweep(
            args.symbol, args.days, args.timeframe,
            param_names, param_values_list, base_params
        )
        print_multi_sweep(param_names, results)


if __name__ == "__main__":
    main()
