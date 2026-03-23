#!/usr/bin/env python3
"""
OpenQuant Paper Trading — live intraday pairs trading via Alpaca.

Fetches 1-min bars, runs the Rust engine, executes order intents.
Designed for GLD/SLV pairs trading with regime gate.

Usage:
    python3 paper_trade.py                 # run with defaults
    python3 paper_trade.py --dry-run       # log signals, don't place orders
    python3 paper_trade.py --interval 60   # poll every 60 seconds
"""

import argparse
import json
import logging
import os
import signal
import subprocess
import sys
import time
from datetime import datetime, timezone
from pathlib import Path
from zoneinfo import ZoneInfo

from dotenv import load_dotenv

load_dotenv()

logging.basicConfig(
    level=logging.INFO,
    format="%(asctime)s %(levelname)s %(name)s: %(message)s",
)
log = logging.getLogger("paper_trade")

ROOT = Path(__file__).resolve().parent
DATA_DIR = ROOT / "data"
TRADING_DIR = ROOT / "trading"
CONFIG = ROOT / "config" / "pairs.toml"
BINARY = ROOT / "engine" / "target" / "release" / "openquant-runner"

ET = ZoneInfo("America/New_York")


def is_market_open() -> bool:
    """Check if US equity market is currently open (9:30-16:00 ET, Mon-Fri)."""
    now = datetime.now(ET)
    if now.weekday() >= 5:  # weekend
        return False
    market_open = now.replace(hour=9, minute=30, second=0, microsecond=0)
    market_close = now.replace(hour=16, minute=0, second=0, microsecond=0)
    return market_open <= now <= market_close


def fetch_latest_bars(symbols: list[str]) -> dict:
    """Fetch the most recent 1-min bars from Alpaca."""
    from alpaca.data.historical import StockHistoricalDataClient
    from alpaca.data.requests import StockBarsRequest
    from alpaca.data.timeframe import TimeFrame
    from alpaca.data.enums import DataFeed

    client = StockHistoricalDataClient(
        os.environ.get("ALPACA_API_KEY"),
        os.environ.get("ALPACA_SECRET_KEY"),
    )

    now = datetime.now(timezone.utc)
    # Fetch last 60 minutes of bars
    start = now - __import__("datetime").timedelta(minutes=60)

    request = StockBarsRequest(
        symbol_or_symbols=symbols,
        timeframe=TimeFrame.Minute,
        start=start,
        end=now,
        feed=DataFeed.IEX,
    )

    barset = client.get_stock_bars(request)
    result = {}
    for sym in symbols:
        bars = barset.data.get(sym, [])
        result[sym] = [
            {
                "timestamp": int(b.timestamp.timestamp() * 1000),
                "open": float(b.open),
                "high": float(b.high),
                "low": float(b.low),
                "close": float(b.close),
                "volume": float(b.volume),
            }
            for b in bars
        ]
    return result


def run_engine(bars_path: Path) -> list[dict]:
    """Run the Rust engine on the bars and return order intents."""
    output_dir = DATA_DIR / "live"
    output_dir.mkdir(exist_ok=True)

    intents_path = output_dir / "order_intents.json"
    intents_path.unlink(missing_ok=True)

    result = subprocess.run(
        [
            str(BINARY), "backtest",
            "--config", str(CONFIG),
            "--data-dir", str(bars_path.parent),
            "--trading-dir", str(TRADING_DIR),
            "--output-dir", str(output_dir),
            "--warmup-bars", "0",
        ],
        capture_output=True, text=True,
        env={**os.environ, "RUST_LOG": "warn"},
    )

    if result.returncode != 0:
        log.error("Engine failed: %s", result.stderr[-500:])
        return []

    if intents_path.exists():
        with open(intents_path) as f:
            return json.load(f)
    return []


def execute_intents(intents: list[dict], dry_run: bool = False):
    """Execute order intents via Alpaca."""
    from paper_trading.alpaca_client import buy, sell, get_positions

    if not intents:
        return

    # Get current positions to avoid duplicates
    current = {p["symbol"]: p for p in get_positions()}

    for intent in intents:
        symbol = intent.get("symbol", "")
        side = intent.get("side", "")
        qty = intent.get("qty", 0)
        pair_id = intent.get("pair_id", "")
        reason = intent.get("reason", "")

        if qty <= 0:
            continue

        action = f"{side} {qty} {symbol} (pair={pair_id}, reason={reason})"

        if dry_run:
            log.info("DRY RUN: %s", action)
            continue

        try:
            if side == "buy":
                result = buy(symbol, qty)
            elif side == "sell":
                result = sell(symbol, qty)
            else:
                log.warning("Unknown side: %s", side)
                continue

            log.info("EXECUTED: %s → %s", action, result["status"])
        except Exception as e:
            log.error("FAILED: %s → %s", action, e)


def close_all_positions():
    """Close all open positions (EOD cleanup)."""
    from paper_trading.alpaca_client import get_positions, sell

    positions = get_positions()
    for p in positions:
        try:
            sell(p["symbol"], abs(p["qty"]))
            log.info("CLOSED: %s %s", p["symbol"], p["qty"])
        except Exception as e:
            log.error("CLOSE FAILED: %s → %s", p["symbol"], e)


def main():
    parser = argparse.ArgumentParser(description="OpenQuant Paper Trading")
    parser.add_argument("--dry-run", action="store_true", help="Log signals without placing orders")
    parser.add_argument("--interval", type=int, default=120, help="Poll interval in seconds (default: 120)")
    parser.add_argument("--once", action="store_true", help="Run once and exit (don't loop)")
    args = parser.parse_args()

    # Load pair config
    with open(TRADING_DIR / "active_pairs.json") as f:
        pairs = json.load(f)

    symbols = set()
    for p in pairs["pairs"]:
        symbols.add(p["leg_a"])
        symbols.add(p["leg_b"])
    symbols = sorted(symbols)

    log.info("OpenQuant Paper Trading starting")
    log.info("  Pairs: %s", [(p["leg_a"] + "/" + p["leg_b"]) for p in pairs["pairs"]])
    log.info("  Symbols: %s", symbols)
    log.info("  Interval: %ds", args.interval)
    log.info("  Dry run: %s", args.dry_run)

    if not BINARY.exists():
        log.error("Binary not found: %s — run: cd engine && cargo build -p openquant-runner --release", BINARY)
        sys.exit(1)

    # Graceful shutdown
    running = True
    def handle_signal(sig, frame):
        nonlocal running
        log.info("Shutting down...")
        running = False
    signal.signal(signal.SIGINT, handle_signal)
    signal.signal(signal.SIGTERM, handle_signal)

    # Accumulate bars across the session for the engine
    session_bars = {sym: [] for sym in symbols}
    bars_dir = DATA_DIR / "live"
    bars_dir.mkdir(exist_ok=True)

    while running:
        now_et = datetime.now(ET)

        if not is_market_open():
            if now_et.hour >= 16 and session_bars.get(symbols[0]):
                # Market just closed — close all positions
                log.info("Market closed — closing all positions")
                if not args.dry_run:
                    close_all_positions()
                session_bars = {sym: [] for sym in symbols}

            log.info("Market closed (%s ET). Waiting...", now_et.strftime("%H:%M"))
            if args.once:
                break
            time.sleep(60)
            continue

        # Fetch latest bars
        try:
            new_bars = fetch_latest_bars(symbols)
            for sym in symbols:
                existing_ts = {b["timestamp"] for b in session_bars[sym]}
                for bar in new_bars.get(sym, []):
                    if bar["timestamp"] not in existing_ts:
                        session_bars[sym].append(bar)

            total_bars = sum(len(v) for v in session_bars.values())
            log.info("Bars: %d total (%s)", total_bars,
                     ", ".join(f"{s}={len(session_bars[s])}" for s in symbols))
        except Exception as e:
            log.error("Bar fetch failed: %s", e)
            time.sleep(args.interval)
            continue

        # Write bars for engine
        today = now_et.strftime("%Y%m%d")
        bars_file = bars_dir / "experiment_bars.json"
        with open(bars_file, "w") as f:
            json.dump([{"date": today, "symbols": session_bars}], f)

        # Run engine
        intents = run_engine(bars_file)
        if intents:
            log.info("Engine produced %d intents", len(intents))
            # Only execute the most recent intents (last 2 = one pair trade)
            recent = intents[-2:] if len(intents) >= 2 else intents
            execute_intents(recent, dry_run=args.dry_run)
        else:
            log.info("No intents (warmup or no signals)")

        if args.once:
            break

        time.sleep(args.interval)

    log.info("Paper trading stopped")


if __name__ == "__main__":
    main()
