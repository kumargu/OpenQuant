"""
Diversified benchmark suite — runs backtests across multiple asset categories
and compares against a saved baseline from main branch.

Symbol universe:
  Tech:    AAPL, MSFT, NVDA
  Oil/Gas: XOM, CVX
  Energy:  NEE, ENPH
  Metals:  GLD, SLV
  Pharma:  JNJ, PFE, ABBV
  Crypto:  BTC/USD, ETH/USD

Usage:
  python -m paper_trading.benchmark                    # run full benchmark
  python -m paper_trading.benchmark --save-baseline    # save current as baseline
  python -m paper_trading.benchmark --compare          # compare against baseline
  python -m paper_trading.benchmark --category tech    # run single category
"""

import argparse
import json
import os
import sys
from datetime import datetime, timezone
from pathlib import Path

from dotenv import load_dotenv

load_dotenv()

# ---------------------------------------------------------------------------
# Symbol Universe
# ---------------------------------------------------------------------------

CATEGORIES = {
    "tech": ["AAPL", "MSFT", "NVDA"],
    "oil_gas": ["XOM", "CVX"],
    "energy": ["NEE", "ENPH"],
    "metals": ["GLD", "SLV"],
    "pharma": ["JNJ", "PFE", "ABBV"],
    "crypto": ["BTC/USD", "ETH/USD"],
}

ALL_SYMBOLS = [s for symbols in CATEGORIES.values() for s in symbols]

BASELINE_DIR = Path(__file__).parent.parent / "data" / "baseline"
CACHE_DIR = Path(__file__).parent.parent / "data" / "bar_cache"

# Default backtest parameters
DEFAULT_PARAMS = {
    "max_position_notional": 10_000.0,
    "max_daily_loss": 500.0,
    "buy_z_threshold": -2.2,
    "sell_z_threshold": 2.0,
    "min_relative_volume": 1.2,
    "stop_loss_pct": 0.02,
    "max_hold_bars": 100,
    "take_profit_pct": 0.0,
}

# Key metrics to compare
METRICS_KEYS = [
    "total_trades",
    "win_rate",
    "total_pnl",
    "profit_factor",
    "max_drawdown",
    "expectancy",
    "sharpe_approx",
    "winning_trades",
    "losing_trades",
]


def _get_git_sha() -> str:
    """Get current git SHA."""
    import subprocess
    try:
        return subprocess.check_output(
            ["git", "rev-parse", "--short", "HEAD"],
            stderr=subprocess.DEVNULL,
        ).decode().strip()
    except Exception:
        return "unknown"


# ---------------------------------------------------------------------------
# Bar Data Fetching + Caching
# ---------------------------------------------------------------------------

def _cache_path(symbol: str, days: int, timeframe: str, feed: str = "auto") -> Path:
    """Path for cached bar data. Includes feed to avoid mixing IEX/SIP data."""
    safe_symbol = symbol.replace("/", "_")
    feed_suffix = f"_{feed}" if feed != "auto" else ""
    return CACHE_DIR / f"{safe_symbol}_{days}d_{timeframe}{feed_suffix}.json"


def fetch_bars(symbol: str, days: int, timeframe: str = "1Min", feed: str = "auto") -> list:
    """Fetch bars, using cache if available and fresh (< 24h old)."""
    cache_file = _cache_path(symbol, days, timeframe, feed)

    # Use cache if exists and < 24 hours old
    if cache_file.exists():
        age_hours = (datetime.now().timestamp() - cache_file.stat().st_mtime) / 3600
        if age_hours < 24:
            with open(cache_file) as f:
                cached = json.load(f)
            print(f"  {symbol}: {len(cached)} bars (cached)")
            return [tuple(b) for b in cached]

    # Fetch from Alpaca
    from paper_trading.backtest_runner import fetch_bars as alpaca_fetch
    bars = alpaca_fetch(symbol, days, timeframe, feed=feed)

    if bars:
        CACHE_DIR.mkdir(parents=True, exist_ok=True)
        with open(cache_file, "w") as f:
            json.dump(bars, f)

    return bars


# ---------------------------------------------------------------------------
# Benchmark Runner
# ---------------------------------------------------------------------------

def run_benchmark(
    symbols: list[str],
    days: int = 30,
    timeframe: str = "1Min",
    params: dict | None = None,
) -> dict:
    """Run backtest across all symbols. Returns per-symbol and aggregated results."""
    from openquant import backtest, validate_bars

    params = params or DEFAULT_PARAMS
    results = {}

    for symbol in symbols:
        bars = fetch_bars(symbol, days, timeframe)
        if not bars:
            print(f"  {symbol}: NO DATA — skipping")
            continue

        # Data quality gate
        quality = validate_bars(bars)
        if quality["has_critical_issues"]:
            print(f"  {symbol}: SKIPPED — critical data quality issues "
                  f"(ohlc={quality['ohlc_violations']}, ts_back={quality['timestamp_backwards']})")
            continue

        zvp = quality["zero_volume_pct"]
        quality_note = ""
        if zvp > 0.5:
            quality_note = f" [!vol:{zvp:.0%}]"

        result = backtest(bars, **params)
        results[symbol] = {k: result[k] for k in METRICS_KEYS if k in result}
        results[symbol]["total_bars"] = result["total_bars"]
        results[symbol]["zero_volume_pct"] = zvp

        pnl = result["total_pnl"]
        wr = result["win_rate"]
        trades = result["total_trades"]
        print(f"  {symbol}: {trades} trades, {wr:.0%} win rate, ${pnl:+,.2f} P&L{quality_note}")

    return results


def aggregate_results(results: dict) -> dict:
    """Aggregate per-symbol results into summary metrics."""
    if not results:
        return {}

    total_trades = sum(r.get("total_trades", 0) for r in results.values())
    total_pnl = sum(r.get("total_pnl", 0) for r in results.values())
    total_wins = sum(r.get("winning_trades", 0) for r in results.values())
    total_losses = sum(r.get("losing_trades", 0) for r in results.values())

    win_rate = total_wins / total_trades if total_trades > 0 else 0
    expectancy = total_pnl / total_trades if total_trades > 0 else 0

    # Weighted average of per-symbol metrics (by trade count)
    weighted_sharpe = 0
    weighted_pf = 0
    max_dd = 0

    for r in results.values():
        n = r.get("total_trades", 0)
        if n > 0 and total_trades > 0:
            w = n / total_trades
            weighted_sharpe += r.get("sharpe_approx", 0) * w
            weighted_pf += r.get("profit_factor", 0) * w
        max_dd = max(max_dd, r.get("max_drawdown", 0))

    return {
        "total_trades": total_trades,
        "winning_trades": total_wins,
        "losing_trades": total_losses,
        "win_rate": win_rate,
        "total_pnl": total_pnl,
        "expectancy": expectancy,
        "profit_factor": weighted_pf,
        "sharpe_approx": weighted_sharpe,
        "max_drawdown": max_dd,
    }


def run_by_category(
    days: int = 30,
    timeframe: str = "1Min",
    params: dict | None = None,
    categories: list[str] | None = None,
) -> dict:
    """Run benchmark per category. Returns category results + aggregated."""
    cats = categories or list(CATEGORIES.keys())
    category_results = {}
    all_symbol_results = {}

    for cat in cats:
        symbols = CATEGORIES.get(cat, [])
        if not symbols:
            print(f"Unknown category: {cat}")
            continue

        print(f"\n{'='*50}")
        print(f"Category: {cat.upper()}")
        print(f"{'='*50}")

        results = run_benchmark(symbols, days, timeframe, params)
        category_results[cat] = {
            "symbols": results,
            "aggregated": aggregate_results(results),
        }
        all_symbol_results.update(results)

    overall = aggregate_results(all_symbol_results)

    return {
        "timestamp": datetime.now(timezone.utc).isoformat(),
        "engine_version": _get_git_sha(),
        "days": days,
        "timeframe": timeframe,
        "params": params or DEFAULT_PARAMS,
        "categories": category_results,
        "aggregated": overall,
    }


# ---------------------------------------------------------------------------
# Baseline Management
# ---------------------------------------------------------------------------

def save_baseline(report: dict):
    """Save benchmark report as the baseline."""
    BASELINE_DIR.mkdir(parents=True, exist_ok=True)
    baseline_path = BASELINE_DIR / "benchmark.json"
    with open(baseline_path, "w") as f:
        json.dump(report, f, indent=2)
    print(f"\nBaseline saved to {baseline_path}")
    print(f"Engine version: {report['engine_version']}")


def load_baseline() -> dict | None:
    """Load the saved baseline."""
    baseline_path = BASELINE_DIR / "benchmark.json"
    if not baseline_path.exists():
        return None
    with open(baseline_path) as f:
        return json.load(f)


# ---------------------------------------------------------------------------
# Comparison Report
# ---------------------------------------------------------------------------

def compare_reports(baseline: dict, candidate: dict) -> str:
    """Generate a markdown comparison table."""
    lines = []
    lines.append(f"## Backtest Comparison")
    lines.append(f"")
    lines.append(f"Baseline: `{baseline.get('engine_version', '?')}` | "
                 f"Candidate: `{candidate.get('engine_version', '?')}` | "
                 f"Data: {baseline.get('days', '?')}d {baseline.get('timeframe', '?')}")
    lines.append("")

    # Per-category comparison
    for cat in CATEGORIES:
        base_cat = baseline.get("categories", {}).get(cat, {}).get("aggregated", {})
        cand_cat = candidate.get("categories", {}).get(cat, {}).get("aggregated", {})

        if not base_cat and not cand_cat:
            continue

        lines.append(f"### {cat.replace('_', '/').upper()}")
        lines.append("")
        lines.append(_metrics_table(base_cat, cand_cat))
        lines.append("")

    # Aggregated comparison
    base_agg = baseline.get("aggregated", {})
    cand_agg = candidate.get("aggregated", {})

    lines.append(f"### AGGREGATED (ALL SYMBOLS)")
    lines.append("")
    lines.append(_metrics_table(base_agg, cand_agg))
    lines.append("")

    # Verdict
    verdict = _verdict(base_agg, cand_agg)
    lines.append(verdict)

    return "\n".join(lines)


def _metrics_table(base: dict, cand: dict) -> str:
    """Generate a markdown metrics table."""
    rows = [
        ("Trades", "total_trades", "{:.0f}", False),
        ("Win Rate", "win_rate", "{:.1%}", True),
        ("Total P&L", "total_pnl", "${:,.2f}", True),
        ("Expectancy", "expectancy", "${:,.2f}", True),
        ("Profit Factor", "profit_factor", "{:.2f}", True),
        ("Sharpe", "sharpe_approx", "{:.2f}", True),
        ("Max Drawdown", "max_drawdown", "${:,.2f}", False),  # lower is better
    ]

    lines = []
    lines.append("| Metric | Baseline | Candidate | Delta |")
    lines.append("|--------|----------|-----------|-------|")

    for label, key, fmt, higher_is_better in rows:
        b = base.get(key, 0)
        c = cand.get(key, 0)
        delta = c - b

        b_str = fmt.format(b)
        c_str = fmt.format(c)

        if key == "max_drawdown":
            # For drawdown, negative delta is good
            if delta < -0.01:
                d_str = f"-{fmt.format(abs(delta))}"
            elif delta > 0.01:
                d_str = f"+{fmt.format(delta)}"
            else:
                d_str = "---"
        elif abs(delta) < 0.001:
            d_str = "---"
        elif higher_is_better:
            prefix = "+" if delta > 0 else ""
            d_str = f"{prefix}{fmt.format(delta)}"
        else:
            d_str = fmt.format(delta)

        lines.append(f"| {label} | {b_str} | {c_str} | {d_str} |")

    return "\n".join(lines)


def _verdict(base: dict, cand: dict) -> str:
    """Simple pass/fail verdict based on key metrics."""
    improvements = 0
    regressions = 0

    # Primary metrics (higher is better)
    for key in ["expectancy", "profit_factor", "sharpe_approx"]:
        b = base.get(key, 0)
        c = cand.get(key, 0)
        if c > b + 0.01:
            improvements += 1
        elif c < b - 0.01:
            regressions += 1

    # Max drawdown (lower is better)
    b_dd = base.get("max_drawdown", 0)
    c_dd = cand.get("max_drawdown", 0)
    if c_dd < b_dd - 0.01:
        improvements += 1
    elif c_dd > b_dd + 0.01:
        regressions += 1

    if regressions == 0 and improvements > 0:
        return "**VERDICT: PASS** --- All primary metrics improved or neutral"
    elif regressions == 0:
        return "**VERDICT: NEUTRAL** --- No improvements or regressions detected"
    elif improvements > regressions:
        return f"**VERDICT: MIXED** --- {improvements} improvements, {regressions} regressions. Review carefully."
    else:
        return f"**VERDICT: FAIL** --- {regressions} regressions detected. Do not merge without review."


# ---------------------------------------------------------------------------
# Pretty Print
# ---------------------------------------------------------------------------

def print_report(report: dict):
    """Print benchmark report to console."""
    print(f"\n{'='*60}")
    print(f"BENCHMARK REPORT")
    print(f"Engine: {report['engine_version']} | "
          f"Data: {report['days']}d {report['timeframe']}")
    print(f"{'='*60}")

    for cat, data in report.get("categories", {}).items():
        agg = data.get("aggregated", {})
        if not agg:
            continue
        trades = agg.get("total_trades", 0)
        pnl = agg.get("total_pnl", 0)
        wr = agg.get("win_rate", 0)
        print(f"\n  {cat.upper():12s}  {trades:3d} trades  {wr:5.1%} WR  ${pnl:>+10,.2f} P&L")

        for sym, r in data.get("symbols", {}).items():
            t = r.get("total_trades", 0)
            p = r.get("total_pnl", 0)
            w = r.get("win_rate", 0)
            print(f"    {sym:12s}  {t:3d} trades  {w:5.1%} WR  ${p:>+10,.2f}")

    agg = report.get("aggregated", {})
    print(f"\n{'---'*20}")
    print(f"  {'TOTAL':12s}  {agg.get('total_trades', 0):3d} trades  "
          f"{agg.get('win_rate', 0):5.1%} WR  ${agg.get('total_pnl', 0):>+10,.2f} P&L")
    print(f"  Expectancy: ${agg.get('expectancy', 0):,.2f} | "
          f"PF: {agg.get('profit_factor', 0):.2f} | "
          f"Sharpe: {agg.get('sharpe_approx', 0):.2f} | "
          f"MaxDD: ${agg.get('max_drawdown', 0):,.2f}")


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------

def main():
    parser = argparse.ArgumentParser(description="OpenQuant Diversified Benchmark Suite")
    parser.add_argument("--days", "-d", type=int, default=30, help="Days of history")
    parser.add_argument("--timeframe", "-t", default="1Min", help="Bar timeframe")
    parser.add_argument("--category", "-c", help="Run single category")
    parser.add_argument("--save-baseline", action="store_true", help="Save result as baseline")
    parser.add_argument("--compare", action="store_true", help="Compare against baseline")
    args = parser.parse_args()

    categories = [args.category] if args.category else None

    print("Running diversified benchmark suite...")
    report = run_by_category(args.days, args.timeframe, categories=categories)

    print_report(report)

    if args.save_baseline:
        save_baseline(report)

    if args.compare:
        baseline = load_baseline()
        if baseline is None:
            print("\nNo baseline found. Run with --save-baseline first.")
            sys.exit(1)

        comparison = compare_reports(baseline, report)
        print(f"\n{'='*60}")
        print("COMPARISON vs BASELINE")
        print(f"{'='*60}")
        print(comparison)


if __name__ == "__main__":
    main()
