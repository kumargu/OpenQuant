#!/usr/bin/env python3
"""
OpenQuant Paper Trading Sidecar — thin Python wrapper for Rust engine.

Python handles ONLY:
  1. Fetching bars from Alpaca (stdin → Rust)
  2. Executing order intents from Rust (stdout → Alpaca)

All trading logic (z-scores, regime gate, entry/exit) lives in Rust.

Architecture:
  [Alpaca API] → bars → [this script] → stdin pipe → [Rust runner live] → stdout pipe → [this script] → [Alpaca API]

Usage:
    python3 paper_trade.py              # live paper trading
    python3 paper_trade.py --dry-run    # log intents, don't place orders
"""

import argparse
import json
import logging
import os
import subprocess
import sys
import time
import threading
from datetime import datetime, timedelta, timezone
from pathlib import Path
from zoneinfo import ZoneInfo

from dotenv import load_dotenv

load_dotenv()

logging.basicConfig(
    level=logging.INFO,
    format="%(asctime)s %(levelname)s %(name)s: %(message)s",
)
log = logging.getLogger("sidecar")

ROOT = Path(__file__).resolve().parent
TRADING_DIR = ROOT / "trading"
CONFIG = ROOT / "config" / "pairs.toml"
BINARY = ROOT / "engine" / "target" / "release" / "openquant-runner"
ET = ZoneInfo("America/New_York")


def is_market_open() -> bool:
    now = datetime.now(ET)
    if now.weekday() >= 5:
        return False
    return now.replace(hour=9, minute=30, second=0) <= now <= now.replace(hour=16, minute=0, second=0)


def get_symbols() -> list[str]:
    with open(TRADING_DIR / "active_pairs.json") as f:
        pairs = json.load(f)
    symbols = set()
    for p in pairs["pairs"]:
        symbols.add(p["leg_a"])
        symbols.add(p["leg_b"])
    return sorted(symbols)


def fetch_bars(symbols: list[str], lookback_minutes: int = 5) -> list[dict]:
    """Fetch recent 1-min bars from Alpaca, return as list of bar dicts."""
    from alpaca.data.historical import StockHistoricalDataClient
    from alpaca.data.requests import StockBarsRequest
    from alpaca.data.timeframe import TimeFrame
    from alpaca.data.enums import DataFeed

    client = StockHistoricalDataClient(
        os.environ.get("ALPACA_API_KEY"),
        os.environ.get("ALPACA_SECRET_KEY"),
    )

    now = datetime.now(timezone.utc)
    start = now - timedelta(minutes=lookback_minutes)

    request = StockBarsRequest(
        symbol_or_symbols=symbols,
        timeframe=TimeFrame.Minute,
        start=start,
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
                "open": float(b.open),
                "high": float(b.high),
                "low": float(b.low),
                "close": float(b.close),
                "volume": float(b.volume),
            })

    # Sort by timestamp then symbol for deterministic ordering
    bars.sort(key=lambda b: (b["timestamp"], b["symbol"]))
    return bars


def execute_intent(intent: dict, dry_run: bool = False):
    """Execute a single order intent via Alpaca."""
    from paper_trading.alpaca_client import buy, sell

    symbol = intent["symbol"]
    side = intent["side"]
    qty = intent["qty"]
    pair_id = intent.get("pair_id", "")
    z = intent.get("z_score", 0)

    action = f"{side.upper()} {qty:.0f} {symbol} (pair={pair_id}, z={z:.2f})"

    if dry_run:
        log.info("DRY RUN: %s", action)
        return

    try:
        result = buy(symbol, qty) if side == "buy" else sell(symbol, qty)
        log.info("EXECUTED: %s → %s", action, result["status"])
    except Exception as e:
        log.error("FAILED: %s → %s", action, e)


def main():
    parser = argparse.ArgumentParser(description="OpenQuant Paper Trading Sidecar")
    parser.add_argument("--dry-run", action="store_true", help="Log intents without placing orders")
    parser.add_argument("--interval", type=int, default=60, help="Bar fetch interval (seconds)")
    args = parser.parse_args()

    if not BINARY.exists():
        log.error("Build the runner first: cd engine && cargo build -p openquant-runner --release")
        sys.exit(1)

    symbols = get_symbols()
    log.info("Sidecar starting — symbols: %s, interval: %ds, dry_run: %s",
             symbols, args.interval, args.dry_run)

    # Start the Rust engine in live mode
    proc = subprocess.Popen(
        [str(BINARY), "live", "--config", str(CONFIG), "--trading-dir", str(TRADING_DIR)],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=sys.stderr,  # engine logs go to stderr
        text=True,
        bufsize=1,  # line-buffered
    )

    log.info("Rust engine started (pid=%d)", proc.pid)

    # Read intents from engine stdout in a background thread
    def read_intents():
        for line in proc.stdout:
            line = line.strip()
            if not line:
                continue
            try:
                intent = json.loads(line)
                log.info("INTENT: %s %s %.0f %s z=%.2f",
                         intent["side"], intent["symbol"], intent["qty"],
                         intent["pair_id"], intent["z_score"])
                execute_intent(intent, dry_run=args.dry_run)
            except json.JSONDecodeError:
                log.warning("Non-JSON from engine: %s", line[:100])

    intent_thread = threading.Thread(target=read_intents, daemon=True)
    intent_thread.start()

    # Track seen timestamps to avoid feeding duplicates
    seen_ts = set()

    try:
        while proc.poll() is None:
            if not is_market_open():
                now_et = datetime.now(ET)
                log.info("Market closed (%s ET)", now_et.strftime("%H:%M"))
                time.sleep(60)
                continue

            # Fetch bars
            try:
                bars = fetch_bars(symbols, lookback_minutes=args.interval // 60 + 2)
                new_bars = [b for b in bars if (b["symbol"], b["timestamp"]) not in seen_ts]

                for b in new_bars:
                    seen_ts.add((b["symbol"], b["timestamp"]))
                    # Pipe to Rust engine
                    proc.stdin.write(json.dumps(b) + "\n")
                    proc.stdin.flush()

                if new_bars:
                    log.info("Fed %d new bars to engine (%d total seen)", len(new_bars), len(seen_ts))
            except Exception as e:
                log.error("Bar fetch error: %s", e)

            time.sleep(args.interval)

    except KeyboardInterrupt:
        log.info("Shutting down...")

    # Close engine
    proc.stdin.close()
    proc.wait(timeout=5)
    log.info("Engine stopped. Sidecar done.")


if __name__ == "__main__":
    main()
