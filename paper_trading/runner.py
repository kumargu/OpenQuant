"""
Live runner: feeds Alpaca market data bars into the Rust engine,
places paper trades when the engine emits order intents.

Designed for multi-day unattended operation with:
- Automatic crash recovery and reconnection
- SQLite journal for full audit trail
- Daily risk state reset at midnight UTC
- Heartbeat logging every N bars

Usage:
  python -m paper_trading.runner --symbol BTC/USD --interval 60
  python -m paper_trading.runner --symbol BTC/USD --interval 60 --journal data/journal/live.db
"""

import argparse
import logging
import os
import signal
import sys
import time
from datetime import datetime, timezone

from openquant import Engine

from . import alpaca_client as alpaca

logging.basicConfig(
    level=logging.INFO,
    format="%(asctime)s %(levelname)s %(message)s",
    datefmt="%Y-%m-%d %H:%M:%S",
)
log = logging.getLogger("runner")

# Graceful shutdown
_shutdown_requested = False


def _handle_signal(signum, frame):
    global _shutdown_requested
    _shutdown_requested = True
    log.info("Shutdown requested (signal %d)", signum)


signal.signal(signal.SIGINT, _handle_signal)
signal.signal(signal.SIGTERM, _handle_signal)


def run(symbol: str, interval_seconds: int, engine: Engine, max_retries: int = 10):
    """Poll bars and feed into engine. Auto-recovers from transient errors."""
    log.info("Starting engine on %s, polling every %ds", symbol, interval_seconds)

    last_bar_time = None
    consecutive_errors = 0
    bars_processed = 0
    trades_placed = 0
    current_day = datetime.now(timezone.utc).date()
    heartbeat_interval = 100  # log heartbeat every N bars

    while not _shutdown_requested:
        try:
            # Daily reset at midnight UTC
            now = datetime.now(timezone.utc)
            if now.date() != current_day:
                log.info("New day: resetting daily risk state")
                engine.reset_daily()
                current_day = now.date()

            bars = _get_latest_bar(symbol)
            if bars is None:
                log.debug("No bar data for %s, waiting...", symbol)
                time.sleep(interval_seconds)
                continue

            bar_time = bars["timestamp"]

            # Skip if we already processed this bar
            if bar_time == last_bar_time:
                time.sleep(interval_seconds)
                continue

            last_bar_time = bar_time
            consecutive_errors = 0  # reset on success
            bars_processed += 1

            # Feed bar into Rust engine
            engine_symbol = symbol.replace("/", "")
            intents = engine.on_bar(
                engine_symbol,
                int(bar_time),
                bars["open"],
                bars["high"],
                bars["low"],
                bars["close"],
                bars["volume"],
            )

            # Log bar
            ts = datetime.fromtimestamp(bar_time / 1000, tz=timezone.utc).strftime("%H:%M:%S")
            if intents:
                log.info(
                    "[%s] %s C=%.2f V=%.0f -> %d signal(s)",
                    ts, symbol, bars["close"], bars["volume"], len(intents),
                )
            elif bars_processed % heartbeat_interval == 0:
                dropped = engine.journal_dropped()
                stale = engine.stale_bars_skipped()
                stale_total = sum(stale.values()) if stale else 0
                log.info(
                    "Heartbeat: %d bars processed, %d trades, %d journal drops, %d stale skipped",
                    bars_processed, trades_placed, dropped, stale_total,
                )

            # Execute intents
            for intent in intents:
                log.info(
                    "SIGNAL: %s %s %s (score=%.2f, reason=%s)",
                    intent["side"].upper(), intent["qty"], symbol,
                    intent["score"], intent["reason"],
                )

                try:
                    if intent["side"] == "buy":
                        result = alpaca.buy(symbol, intent["qty"])
                    else:
                        result = alpaca.sell(symbol, intent["qty"])

                    log.info("ORDER: %s (id=%s)", result["status"], result["id"][:12])
                    trades_placed += 1

                    # Notify engine of fill
                    engine.on_fill(
                        engine_symbol,
                        intent["side"],
                        intent["qty"],
                        bars["close"],
                    )
                except Exception as e:
                    log.error("ORDER FAILED: %s", e)

        except KeyboardInterrupt:
            break
        except Exception as e:
            consecutive_errors += 1
            backoff = min(interval_seconds * (2 ** consecutive_errors), 300)
            log.error(
                "Error (attempt %d/%d): %s. Retrying in %ds",
                consecutive_errors, max_retries, e, backoff,
            )
            if consecutive_errors >= max_retries:
                log.critical("Max retries exceeded, shutting down")
                break
            time.sleep(backoff)
            continue

        time.sleep(interval_seconds)

    # Graceful shutdown
    log.info("Shutting down: %d bars processed, %d trades placed", bars_processed, trades_placed)
    engine.shutdown_journal()
    log.info("Journal flushed, goodbye.")


def _get_latest_bar(symbol: str):
    """Get the most recent bar from Alpaca."""
    from alpaca.data.historical import CryptoHistoricalDataClient, StockHistoricalDataClient
    from alpaca.data.requests import CryptoBarsRequest, StockBarsRequest
    from alpaca.data.timeframe import TimeFrame

    is_crypto = "/" in symbol

    if is_crypto:
        client = CryptoHistoricalDataClient()
        req = CryptoBarsRequest(symbol_or_symbols=symbol, timeframe=TimeFrame.Minute, limit=1)
        bars = client.get_crypto_bars(req)
    else:
        from dotenv import load_dotenv
        load_dotenv()
        client = StockHistoricalDataClient(
            os.environ["ALPACA_API_KEY"],
            os.environ["ALPACA_SECRET_KEY"],
        )
        req = StockBarsRequest(symbol_or_symbols=symbol, timeframe=TimeFrame.Minute, limit=1)
        bars = client.get_stock_bars(req)

    # Extract the bar
    bar_key = symbol if symbol in bars.data else symbol.replace("/", "")
    if bar_key not in bars.data or len(bars.data[bar_key]) == 0:
        return None

    bar = bars.data[bar_key][0]
    return {
        "timestamp": int(bar.timestamp.timestamp() * 1000),
        "open": float(bar.open),
        "high": float(bar.high),
        "low": float(bar.low),
        "close": float(bar.close),
        "volume": float(bar.volume),
    }


def main():
    parser = argparse.ArgumentParser(description="OpenQuant Live Runner")
    parser.add_argument("--symbol", "-s", default="BTC/USD", help="Symbol to trade")
    parser.add_argument("--interval", "-i", type=int, default=60, help="Poll interval in seconds")
    parser.add_argument("--max-position", type=float, default=10_000.0)
    parser.add_argument("--max-daily-loss", type=float, default=500.0)
    parser.add_argument("--journal", type=str, default=None, help="SQLite journal path")
    parser.add_argument("--no-trend-filter", action="store_true")
    parser.add_argument("--max-retries", type=int, default=10, help="Max consecutive errors before exit")
    parser.add_argument("--max-bar-age", type=int, default=300,
                        help="Max bar age in seconds before skipping signals (0=disabled, default=300)")
    args = parser.parse_args()

    engine = Engine(
        max_position_notional=args.max_position,
        max_daily_loss=args.max_daily_loss,
        trend_filter=not args.no_trend_filter,
        journal_path=args.journal,
        max_bar_age_seconds=args.max_bar_age,
    )

    run(args.symbol, args.interval, engine, max_retries=args.max_retries)


if __name__ == "__main__":
    main()
