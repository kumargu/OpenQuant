#!/usr/bin/env python3
"""Stream 1-min bars from Alpaca to stdout as JSON lines.

Minimal script — only fetches bars. All trading logic is in Rust.

Outputs one JSON line per bar:
  {"symbol":"GLD","timestamp":123,"open":1,"high":2,"low":0.5,"close":1.5,"volume":100}
"""

import json
import os
import sys
import time
from datetime import datetime, timedelta, timezone
from pathlib import Path
from zoneinfo import ZoneInfo

from dotenv import load_dotenv

load_dotenv(Path(__file__).resolve().parent.parent / ".env")

ET = ZoneInfo("America/New_York")
SYMBOLS = sys.argv[1:] if len(sys.argv) > 1 else ["GLD", "SLV"]
INTERVAL = int(os.environ.get("BAR_INTERVAL", "60"))


def fetch_bars(symbols, minutes=5):
    from alpaca.data.historical import StockHistoricalDataClient
    from alpaca.data.requests import StockBarsRequest
    from alpaca.data.timeframe import TimeFrame
    from alpaca.data.enums import DataFeed

    client = StockHistoricalDataClient(
        os.environ["ALPACA_API_KEY"], os.environ["ALPACA_SECRET_KEY"],
    )
    now = datetime.now(timezone.utc)
    request = StockBarsRequest(
        symbol_or_symbols=symbols,
        timeframe=TimeFrame.Minute,
        start=now - timedelta(minutes=minutes),
        end=now,
        feed=DataFeed.IEX,
    )
    barset = client.get_stock_bars(request)
    bars = []
    for sym in symbols:
        for b in barset.data.get(sym, []):
            bars.append({
                "symbol": sym,
                "timestamp": int(b.timestamp.timestamp() * 1000),
                "open": float(b.open), "high": float(b.high),
                "low": float(b.low), "close": float(b.close),
                "volume": float(b.volume),
            })
    bars.sort(key=lambda b: (b["timestamp"], b["symbol"]))
    return bars


seen = set()
sys.stderr.write(f"Streaming bars: {SYMBOLS}, interval={INTERVAL}s\n")

while True:
    now_et = datetime.now(ET)
    if now_et.weekday() >= 5 or not (
        now_et.replace(hour=9, minute=30) <= now_et <= now_et.replace(hour=16, minute=0)
    ):
        time.sleep(30)
        continue

    try:
        bars = fetch_bars(SYMBOLS, minutes=INTERVAL // 60 + 2)
        for b in bars:
            key = (b["symbol"], b["timestamp"])
            if key not in seen:
                seen.add(key)
                sys.stdout.write(json.dumps(b) + "\n")
                sys.stdout.flush()
    except Exception as e:
        sys.stderr.write(f"Error: {e}\n")

    time.sleep(INTERVAL)
