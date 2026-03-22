"""
Backfill historical 1-min bars from Alpaca for experiment replay.

Fetches a single trading day's worth of bars for all 12 experiment symbols
and saves in the format expected by data/replay.py.

Usage:
    python data/backfill.py --date 2026-03-14
    python data/backfill.py --date 2026-03-18 --symbols AAPL,NVDA
    python data/backfill.py --list  # show available data files
"""

import argparse
import json
import os
import sys
from datetime import datetime, timedelta, timezone
from pathlib import Path
from zoneinfo import ZoneInfo

from dotenv import load_dotenv

load_dotenv()

DATA_DIR = Path(__file__).parent
SYMBOLS = [
    "AAPL", "MSFT", "NVDA", "GOOGL", "META", "AMZN", "ORCL", "TSLA",
    "AMD", "AVGO", "INTC", "QCOM", "TXN", "MU",
    "JPM", "BAC", "GS", "MS", "C",
    "XOM", "CVX", "COP", "SLB",
    "WMT", "COST", "HD", "MCD", "NKE",
    "JNJ", "LLY", "PFE", "ABBV", "MRK", "UNH",
    "BA", "CAT", "GE", "RTX",
    "GLD", "SLV", "XLE", "XLF",
    "COIN", "PLTR", "SOFI",
]

ET = ZoneInfo("America/New_York")
MARKET_OPEN_HOUR, MARKET_OPEN_MIN = 9, 30
MARKET_CLOSE_HOUR, MARKET_CLOSE_MIN = 16, 0


def fetch_day_bars(symbol: str, date_str: str, feed: str = "iex") -> list[dict]:
    """Fetch 1-min bars for a single symbol on a single day."""
    from alpaca.data.historical import StockHistoricalDataClient
    from alpaca.data.requests import StockBarsRequest
    from alpaca.data.timeframe import TimeFrame
    from alpaca.data.enums import DataFeed

    dt = datetime.strptime(date_str, "%Y-%m-%d").replace(tzinfo=ET)
    start = dt.replace(hour=MARKET_OPEN_HOUR, minute=MARKET_OPEN_MIN, second=0)
    end = dt.replace(hour=MARKET_CLOSE_HOUR, minute=MARKET_CLOSE_MIN, second=0)

    client = StockHistoricalDataClient(
        os.environ["ALPACA_API_KEY"],
        os.environ["ALPACA_SECRET_KEY"],
    )

    feed_enum = DataFeed.IEX if feed == "iex" else DataFeed.SIP
    req = StockBarsRequest(
        symbol_or_symbols=symbol,
        timeframe=TimeFrame.Minute,
        start=start.astimezone(timezone.utc),
        end=end.astimezone(timezone.utc),
        feed=feed_enum,
    )

    try:
        barset = client.get_stock_bars(req)
    except Exception as e:
        if feed == "iex":
            raise
        print(f"  {symbol}: SIP failed, falling back to IEX")
        req = StockBarsRequest(
            symbol_or_symbols=symbol,
            timeframe=TimeFrame.Minute,
            start=start.astimezone(timezone.utc),
            end=end.astimezone(timezone.utc),
            feed=DataFeed.IEX,
        )
        barset = client.get_stock_bars(req)

    bar_key = symbol if symbol in barset.data else symbol.replace("/", "")
    if bar_key not in barset.data:
        return []

    raw = barset.data[bar_key]
    return [
        {
            "timestamp": int(b.timestamp.timestamp() * 1000),
            "open": float(b.open),
            "high": float(b.high),
            "low": float(b.low),
            "close": float(b.close),
            "volume": float(b.volume),
        }
        for b in raw
    ]


def validate_bars(symbol: str, bars: list[dict], date_str: str) -> list[str]:
    """Validate bar quality. Returns list of warnings."""
    warnings = []
    if not bars:
        warnings.append(f"{symbol}: NO DATA")
        return warnings

    # Check bar count (expect ~390 for full day, IEX typically 300-390)
    if len(bars) < 200:
        warnings.append(f"{symbol}: only {len(bars)} bars (expected ~350+)")

    # Check for gaps > 5 min during market hours
    for i in range(1, len(bars)):
        gap_ms = bars[i]["timestamp"] - bars[i - 1]["timestamp"]
        if gap_ms > 5 * 60 * 1000:
            gap_min = gap_ms / 60_000
            bar_time = datetime.fromtimestamp(bars[i]["timestamp"] / 1000, tz=timezone.utc)
            bar_et = bar_time.astimezone(ET)
            warnings.append(f"{symbol}: {gap_min:.0f}min gap at {bar_et.strftime('%H:%M')} ET")

    # Check zero-volume ratio
    zero_vol = sum(1 for b in bars if b["volume"] == 0)
    if zero_vol > 0:
        pct = zero_vol / len(bars)
        if pct > 0.3:
            warnings.append(f"{symbol}: {pct:.0%} zero-volume bars")

    return warnings


def backfill(date_str: str, symbols: list[str], feed: str = "iex") -> Path:
    """Fetch and save bars for a given date."""
    safe_date = date_str.replace("-", "")
    out_path = DATA_DIR / f"experiment_bars_{safe_date}.json"

    if out_path.exists():
        print(f"File already exists: {out_path}")
        with open(out_path) as f:
            existing = json.load(f)
        missing = [s for s in symbols if s not in existing]
        if not missing:
            print(f"All {len(symbols)} symbols present. Use --force to re-fetch.")
            return out_path
        print(f"Missing symbols: {missing}. Fetching...")
        data = existing
    else:
        data = {}

    all_warnings = []
    for sym in symbols:
        if sym in data:
            print(f"  {sym}: {len(data[sym])} bars (cached)")
            continue
        print(f"  {sym}: fetching...", end=" ", flush=True)
        bars = fetch_day_bars(sym, date_str, feed=feed)
        print(f"{len(bars)} bars")
        data[sym] = bars

        warnings = validate_bars(sym, bars, date_str)
        all_warnings.extend(warnings)

    # Append to merged experiment_bars.json
    merged_path = DATA_DIR / "experiment_bars.json"
    if merged_path.exists():
        with open(merged_path) as f:
            merged = json.load(f)
        # Check if this date already exists
        existing_dates = {d["date"] for d in merged}
        if safe_date not in existing_dates:
            merged.append({"date": safe_date, "symbols": data})
            merged.sort(key=lambda d: d["date"])
            with open(merged_path, "w") as f:
                json.dump(merged, f)
            print(f"\nAppended to {merged_path} ({len(merged)} days)")
        else:
            print(f"\nDate {safe_date} already in {merged_path}, skipped append")
    else:
        # Create new merged file
        with open(merged_path, "w") as f:
            json.dump([{"date": safe_date, "symbols": data}], f)
        print(f"\nCreated {merged_path}")

    # Also save individual file (backup)
    with open(out_path, "w") as f:
        json.dump(data, f)
    print(f"Saved to {out_path}")

    # Summary
    print(f"\n{'='*50}")
    print(f"Backfill summary for {date_str}")
    print(f"{'='*50}")
    total_bars = 0
    for sym in sorted(data.keys()):
        n = len(data[sym])
        total_bars += n
        status = "OK" if n >= 200 else "LOW"
        print(f"  {sym:6s}: {n:4d} bars  [{status}]")
    print(f"  {'TOTAL':6s}: {total_bars:4d} bars across {len(data)} symbols")

    if all_warnings:
        print(f"\nWarnings ({len(all_warnings)}):")
        for w in all_warnings:
            print(f"  ⚠ {w}")
    else:
        print(f"\nNo warnings — data looks clean.")

    return out_path


def list_available():
    """List available experiment bar files."""
    files = sorted(DATA_DIR.glob("experiment_bars_*.json"))
    if not files:
        print("No experiment bar files found.")
        return
    print("Available experiment data:")
    for f in files:
        date = f.stem.replace("experiment_bars_", "")
        with open(f) as fh:
            data = json.load(fh)
        syms = sorted(data.keys())
        total = sum(len(v) for v in data.values())
        print(f"  {date}: {len(syms)} symbols, {total} total bars — {', '.join(syms)}")


def main():
    parser = argparse.ArgumentParser(description="Backfill experiment bars from Alpaca")
    parser.add_argument("--date", "-d", help="Trading date (YYYY-MM-DD)")
    parser.add_argument("--symbols", "-s", help="Comma-separated symbols (default: all 12)")
    parser.add_argument("--feed", default="iex", choices=["iex", "sip"], help="Data feed")
    parser.add_argument("--list", "-l", action="store_true", help="List available data files")
    parser.add_argument("--force", action="store_true", help="Re-fetch even if file exists")
    args = parser.parse_args()

    if args.list:
        list_available()
        return

    if not args.date:
        parser.error("--date is required (e.g. --date 2026-03-14)")

    symbols = args.symbols.split(",") if args.symbols else SYMBOLS

    if args.force:
        out_path = DATA_DIR / f"experiment_bars_{args.date.replace('-', '')}.json"
        if out_path.exists():
            out_path.unlink()

    backfill(args.date, symbols, feed=args.feed)


if __name__ == "__main__":
    main()
