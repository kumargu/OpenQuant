#!/usr/bin/env python3
"""
OpenQuant Live Paper Trading — single process, Rust engine via pybridge.

One script, one process:
  1. Fetch bars from Alpaca (Python)
  2. Feed to PairsEngine.on_bar() (Rust via pybridge)
  3. Execute intents via Alpaca (Python)

Usage:
    python3 run_live.py              # live paper trading
    python3 run_live.py --dry-run    # log signals, no orders
    python3 run_live.py --interval 60
"""

import argparse
import json
import logging
import os
import signal
import sys
import time
from datetime import datetime, timedelta, timezone
from pathlib import Path
from zoneinfo import ZoneInfo

from dotenv import load_dotenv
load_dotenv()

logging.basicConfig(
    level=logging.INFO,
    format="%(asctime)s %(levelname)s %(message)s",
    handlers=[
        logging.StreamHandler(),
        logging.FileHandler("data/journal/engine.log", mode="a"),
    ],
)
log = logging.getLogger("live")

ET = ZoneInfo("America/New_York")
ROOT = Path(__file__).resolve().parent


def is_market_open():
    now = datetime.now(ET)
    if now.weekday() >= 5:
        return False
    return now.replace(hour=9, minute=30, second=0) <= now <= now.replace(hour=16, minute=0, second=0)


def fetch_bars(client, symbols, minutes=5):
    from alpaca.data.requests import StockBarsRequest
    from alpaca.data.timeframe import TimeFrame
    from alpaca.data.enums import DataFeed

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
                "close": float(b.close),
            })
    bars.sort(key=lambda b: (b["timestamp"], b["symbol"]))
    return bars


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--dry-run", action="store_true")
    parser.add_argument("--interval", type=int, default=60)
    args = parser.parse_args()

    # Ensure journal dir exists
    (ROOT / "data" / "journal").mkdir(parents=True, exist_ok=True)

    # Load pairs config
    with open(ROOT / "trading" / "active_pairs.json") as f:
        pairs_cfg = json.load(f)
    symbols = sorted({p["leg_a"] for p in pairs_cfg["pairs"]} | {p["leg_b"] for p in pairs_cfg["pairs"]})

    if not symbols:
        log.error("No pairs in trading/active_pairs.json")
        sys.exit(1)

    # Init Rust engine via pybridge
    import openquant
    engine = openquant.PairsEngine.from_active_pairs(
        str(ROOT / "trading" / "active_pairs.json"),
        str(ROOT / "trading" / "pair_trading_history.json"),
        str(ROOT / "config" / "pairs.toml"),
    )

    # Init Alpaca client
    from alpaca.data.historical import StockHistoricalDataClient
    data_client = StockHistoricalDataClient(
        os.environ["ALPACA_API_KEY"], os.environ["ALPACA_SECRET_KEY"],
    )

    log.info("=" * 50)
    log.info("OPENQUANT LIVE — single process")
    log.info("  Pairs: %s", [f"{p['leg_a']}/{p['leg_b']}" for p in pairs_cfg["pairs"]])
    log.info("  Symbols: %s", symbols)
    log.info("  Dry run: %s", args.dry_run)
    log.info("  Interval: %ds", args.interval)
    log.info("=" * 50)

    # Graceful shutdown
    running = True
    def stop(sig, frame):
        nonlocal running
        running = False
        log.info("Shutting down...")
    signal.signal(signal.SIGINT, stop)
    signal.signal(signal.SIGTERM, stop)

    seen_ts = set()
    bar_count = 0
    intent_count = 0

    while running:
        now_et = datetime.now(ET)

        if not is_market_open():
            if not running:
                break
            log.info("Market closed (%s ET)", now_et.strftime("%H:%M"))
            time.sleep(30)
            continue

        # Fetch bars
        try:
            bars = fetch_bars(data_client, symbols, minutes=args.interval // 60 + 2)
            new_bars = [b for b in bars if (b["symbol"], b["timestamp"]) not in seen_ts]

            for b in new_bars:
                seen_ts.add((b["symbol"], b["timestamp"]))

                # Feed to Rust engine
                intents = engine.on_bar(b["symbol"], b["timestamp"], b["close"])
                bar_count += 1

                for intent in intents:
                    intent_count += 1
                    side = intent["side"]
                    sym = intent["symbol"]
                    qty = intent["qty"]
                    pair = intent["pair_id"]
                    z = intent["z_score"]

                    log.info("INTENT: %s %.0f %s (pair=%s z=%.2f) %s",
                             side.upper(), qty, sym, pair, z, intent["reason"])

                    if args.dry_run:
                        continue

                    # Execute via Alpaca
                    try:
                        from paper_trading.alpaca_client import buy, sell
                        result = buy(sym, qty) if side == "buy" else sell(sym, qty)
                        log.info("FILL: %s %.0f %s → %s", side.upper(), qty, sym, result["status"])
                    except Exception as e:
                        log.error("ORDER FAILED: %s %.0f %s → %s", side.upper(), qty, sym, e)

            if new_bars:
                log.info("Fed %d bars (%d total, %d intents)", len(new_bars), bar_count, intent_count)

        except Exception as e:
            log.error("Error: %s", e)

        time.sleep(args.interval)

    log.info("LIVE SESSION END — %d bars, %d intents", bar_count, intent_count)


if __name__ == "__main__":
    main()
