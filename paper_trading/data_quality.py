"""
Data quality audit: validates bar data against TradingView-grade expectations.

Checks:
  - OHLC consistency (high >= max(open,close), low <= min(open,close))
  - Timestamp ordering and duplicates
  - Gap detection with configurable threshold
  - Zero-volume bar detection and statistics
  - Expected bar count vs actual (completeness)

Usage:
  python -m paper_trading.data_quality --symbol BTC/USD --days 7
  python -m paper_trading.data_quality --symbol BTC/USD --days 30 --timeframe 5Min
  python -m paper_trading.data_quality --all                # audit all benchmark symbols
"""

import argparse
import sys
from datetime import datetime, timezone

from paper_trading.backtest_runner import fetch_bars


def expected_bar_count(days: int, timeframe: str, is_crypto: bool) -> int:
    """Estimate expected bar count for a symbol over N days.

    For stocks, IEX feed includes extended-hours bars (~10h/day: pre-market
    4:00-9:30 + regular 9:30-16:00 + post-market 16:00-18:00 ET), so we
    estimate ~10 hours of bars per trading day.
    """
    minutes_per_bar = {
        "1Min": 1,
        "5Min": 5,
        "15Min": 15,
        "1Hour": 60,
        "1Day": 1440,
    }
    mpb = minutes_per_bar.get(timeframe, 1)

    if is_crypto:
        # Crypto trades 24/7
        total_minutes = days * 24 * 60
    else:
        # US stocks: ~252 trading days/year, ~8h/day with extended hours (IEX)
        # IEX extended hours: ~8:00 ET pre-market to ~18:00 ET post-market, but
        # actual bar coverage is typically ~8h depending on liquidity.
        trading_days = max(1, int(days * 252 / 365))
        total_minutes = trading_days * 480  # 8 hours typical IEX coverage

    return max(1, total_minutes // mpb)


def _timeframe_minutes(timeframe: str) -> int:
    """Convert timeframe string to minutes."""
    tf_map = {"1Min": 1, "5Min": 5, "15Min": 15, "1Hour": 60, "1Day": 1440}
    return tf_map.get(timeframe, 1)


def audit_bars(symbol: str, days: int, timeframe: str = "1Min", feed: str = "auto") -> dict:
    """Fetch bars and run full quality audit."""
    from openquant import validate_bars

    bars = fetch_bars(symbol, days, timeframe, feed=feed)
    if not bars:
        return {"symbol": symbol, "error": "No data returned"}

    # Gap threshold = 3x the bar interval (to allow for minor delays)
    gap_threshold_min = _timeframe_minutes(timeframe) * 3
    report = validate_bars(bars, gap_threshold_minutes=gap_threshold_min)

    is_crypto = "/" in symbol
    expected = expected_bar_count(days, timeframe, is_crypto)

    # Timestamp range
    ts_first = bars[0][1]
    ts_last = bars[-1][1]
    t0 = datetime.fromtimestamp(ts_first / 1000, tz=timezone.utc)
    t1 = datetime.fromtimestamp(ts_last / 1000, tz=timezone.utc)
    actual_span_hours = (ts_last - ts_first) / (1000 * 3600)

    # Completeness: actual / expected
    completeness = len(bars) / expected if expected > 0 else 0

    return {
        "symbol": symbol,
        "days": days,
        "timeframe": timeframe,
        "total_bars": len(bars),
        "expected_bars": expected,
        "completeness": completeness,
        "time_range": f"{t0.strftime('%Y-%m-%d %H:%M')} -> {t1.strftime('%Y-%m-%d %H:%M')} UTC",
        "span_hours": round(actual_span_hours, 1),
        **report,
    }


def print_audit(result: dict):
    """Print audit results in markdown format."""
    if "error" in result:
        print(f"\n**{result['symbol']}**: {result['error']}")
        return

    sym = result["symbol"]
    print(f"\n## Data Quality: {sym}")
    print(f"Range: {result['time_range']}")
    print(f"Span: {result['span_hours']}h | Bars: {result['total_bars']:,} / {result['expected_bars']:,} expected")
    print(f"Completeness: {result['completeness']:.1%}")

    print(f"\n| Check | Result |")
    print(f"|-------|--------|")

    # Critical checks
    ohlc = result["ohlc_violations"]
    print(f"| OHLC consistency | {'PASS' if ohlc == 0 else f'FAIL ({ohlc} violations)'} |")

    neg = result["non_positive_prices"]
    print(f"| Price positivity | {'PASS' if neg == 0 else f'FAIL ({neg} bars)'} |")

    ts_back = result["timestamp_backwards"]
    print(f"| Timestamp ordering | {'PASS' if ts_back == 0 else f'FAIL ({ts_back} backwards)'} |")

    dups = result["duplicate_timestamps"]
    print(f"| No duplicates | {'PASS' if dups == 0 else f'FAIL ({dups} dupes)'} |")

    # Warnings
    zvp = result["zero_volume_pct"]
    zv = result["zero_volume_bars"]
    if zvp > 0.5:
        status = f"WARNING ({zv:,} bars, {zvp:.0%})"
    elif zvp > 0.1:
        status = f"NOTE ({zv:,} bars, {zvp:.0%})"
    elif zv == 0:
        status = "PASS"
    else:
        status = f"OK ({zv:,} bars, {zvp:.1%})"
    print(f"| Volume coverage | {status} |")

    gaps = result["gap_count"]
    is_crypto = "/" in result.get("symbol", "")
    gap_list = result.get("gaps", [])

    if not is_crypto and gap_list:
        # For stocks, separate overnight gaps (~14h = 840min) from intraday gaps
        intraday_gaps = [(i, g) for i, g in gap_list if g < 780 * 60000]  # < 13h
        overnight_gaps = [(i, g) for i, g in gap_list if g >= 780 * 60000]

        if intraday_gaps:
            print(f"| Intraday gaps | {len(intraday_gaps)} gaps |")
            largest = sorted(intraday_gaps, key=lambda g: g[1], reverse=True)[:5]
            for idx, gap_ms in largest:
                gap_min = gap_ms // 60000
                print(f"|   gap at bar {idx} | {gap_min} minutes |")
        else:
            print(f"| Intraday gaps | PASS (none) |")

        if overnight_gaps:
            print(f"| Overnight closures | {len(overnight_gaps)} (expected) |")
    elif gaps > 0:
        print(f"| Gaps (>{_timeframe_minutes(result.get('timeframe', '1Min')) * 3}min) | {gaps} gaps detected |")
        # Show top 5 largest
        if gap_list:
            largest = sorted(gap_list, key=lambda g: g[1], reverse=True)[:5]
            for idx, gap_ms in largest:
                gap_min = gap_ms // 60000
                print(f"|   gap at bar {idx} | {gap_min} minutes |")
    else:
        print(f"| Gap-free | PASS |")

    comp = result["completeness"]
    if comp < 0.9:
        print(f"| Completeness | WARNING ({comp:.0%}) |")
    else:
        print(f"| Completeness | PASS ({comp:.0%}) |")

    # Granularity recommendation
    tf = result.get("timeframe", "1Min")
    if zvp > 0.5 and tf == "1Min":
        print(f"\n**Granularity note:** At 1Min, {zvp:.0%} of bars have zero volume. "
              f"Recommend 5Min+ for volume-dependent signals (relative_volume, VWAP). "
              f"1Min is fine for price-only signals (z-score, SMA trend).")
    elif zvp > 0.3 and tf == "1Min":
        print(f"\n**Granularity note:** {zvp:.0%} zero-volume at 1Min. "
              f"Consider 5Min for more reliable volume data.")

    # Overall verdict
    critical = result["has_critical_issues"]
    if critical:
        print(f"\n**VERDICT: FAIL** — critical data issues found, do NOT use for backtesting")
    elif zvp > 0.5:
        print(f"\n**VERDICT: WARNING** — {zvp:.0%} zero-volume bars. "
              f"Volume-based signals (relative_volume filter) are unreliable. "
              f"Consider using a higher timeframe or filtering zero-vol bars.")
    elif comp < 0.9:
        print(f"\n**VERDICT: WARNING** — only {comp:.0%} of expected bars present. "
              f"Missing data may affect feature warm-up and signal accuracy.")
    else:
        print(f"\n**VERDICT: PASS** — data quality acceptable for backtesting")


def main():
    parser = argparse.ArgumentParser(description="OpenQuant Data Quality Audit")
    parser.add_argument("--symbol", "-s", default="BTC/USD")
    parser.add_argument("--days", "-d", type=int, default=7)
    parser.add_argument("--timeframe", "-t", default="1Min")
    parser.add_argument("--all", action="store_true", help="Audit all benchmark symbols")
    parser.add_argument("--feed", "-f", default="auto", choices=["auto", "iex", "sip"],
                        help="Stock data feed: iex (free), sip (paid), auto (SIP+IEX fallback)")
    args = parser.parse_args()

    if args.all:
        from paper_trading.benchmark import CATEGORIES
        results = []
        for cat, symbols in CATEGORIES.items():
            print(f"\n{'='*50}")
            print(f"Category: {cat.upper()}")
            print(f"{'='*50}")
            for sym in symbols:
                result = audit_bars(sym, args.days, args.timeframe, feed=args.feed)
                print_audit(result)
                results.append(result)

        # Summary
        print(f"\n{'='*50}")
        print(f"SUMMARY")
        print(f"{'='*50}")
        print(f"\n| Symbol | Bars | Completeness | Zero Vol | Gaps | Critical |")
        print(f"|--------|------|--------------|----------|------|----------|")
        for r in results:
            if "error" in r:
                print(f"| {r['symbol']} | ERROR | - | - | - | - |")
                continue
            print(f"| {r['symbol']} | {r['total_bars']:,} | "
                  f"{r['completeness']:.0%} | "
                  f"{r['zero_volume_pct']:.0%} | "
                  f"{r['gap_count']} | "
                  f"{'YES' if r['has_critical_issues'] else 'no'} |")
    else:
        result = audit_bars(args.symbol, args.days, args.timeframe, feed=args.feed)
        print_audit(result)


if __name__ == "__main__":
    main()
