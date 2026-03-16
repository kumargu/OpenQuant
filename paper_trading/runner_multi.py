"""
Multi-symbol live runner: polls bars for a basket of stocks and feeds
them into a single Rust engine instance with shared risk state.

Usage:
  python -m paper_trading.runner_multi --symbols AAPL,MSFT,NVDA,TSLA,AMD
  python -m paper_trading.runner_multi --symbols AAPL,MSFT --interval 30
  python -m paper_trading.runner_multi --symbols AAPL --config openquant.toml --journal data/journal/multi.db
"""

import argparse
import logging
import signal
import sys
import time
from datetime import datetime, timezone

from openquant import Engine

from . import alpaca_client as alpaca
from .runner import _get_latest_bar

logging.basicConfig(
    level=logging.INFO,
    format="%(asctime)s %(levelname)s %(message)s",
    datefmt="%Y-%m-%d %H:%M:%S",
)
log = logging.getLogger("runner_multi")

_shutdown_requested = False


def _handle_signal(signum, _frame):
    global _shutdown_requested
    _shutdown_requested = True
    log.info("Shutdown requested (signal %d)", signum)


signal.signal(signal.SIGINT, _handle_signal)
signal.signal(signal.SIGTERM, _handle_signal)


def _is_market_open() -> bool:
    """Check if US equity markets are open (9:30-16:00 ET, weekdays)."""
    from zoneinfo import ZoneInfo

    now = datetime.now(ZoneInfo("America/New_York"))
    if now.weekday() >= 5:  # Sat=5, Sun=6
        return False
    market_open = now.replace(hour=9, minute=30, second=0, microsecond=0)
    market_close = now.replace(hour=16, minute=0, second=0, microsecond=0)
    return market_open <= now <= market_close


def run(
    symbols: list[str],
    interval_seconds: int,
    engine: Engine,
    max_retries: int = 10,
    require_market_hours: bool = True,
):
    """Poll bars for all symbols and feed into a single engine."""
    log.info(
        "Starting multi-symbol runner: %s, polling every %ds",
        ", ".join(symbols),
        interval_seconds,
    )

    last_bar_times: dict[str, int | None] = {s: None for s in symbols}
    consecutive_errors = 0
    bars_processed: dict[str, int] = {s: 0 for s in symbols}
    trades_placed = 0
    current_day = datetime.now(timezone.utc).date()
    heartbeat_interval = 100

    while not _shutdown_requested:
        try:
            now = datetime.now(timezone.utc)

            # Daily reset at midnight UTC
            if now.date() != current_day:
                log.info("New day: resetting daily risk state")
                engine.reset_daily()
                current_day = now.date()

            # Market hours gate for equities
            if require_market_hours and not _is_market_open():
                time.sleep(interval_seconds)
                continue

            for symbol in symbols:
                if _shutdown_requested:
                    break

                bar = _get_latest_bar(symbol)
                if bar is None:
                    continue

                bar_time = bar["timestamp"]

                # Skip duplicate bars
                if bar_time == last_bar_times[symbol]:
                    continue

                last_bar_times[symbol] = bar_time
                consecutive_errors = 0
                bars_processed[symbol] = bars_processed.get(symbol, 0) + 1

                engine_symbol = symbol.replace("/", "")
                intents = engine.on_bar(
                    engine_symbol,
                    int(bar_time),
                    bar["open"],
                    bar["high"],
                    bar["low"],
                    bar["close"],
                    bar["volume"],
                )

                ts = datetime.fromtimestamp(
                    bar_time / 1000, tz=timezone.utc
                ).strftime("%H:%M:%S")

                if intents:
                    log.info(
                        "[%s] %s C=%.2f V=%.0f -> %d signal(s)",
                        ts,
                        symbol,
                        bar["close"],
                        bar["volume"],
                        len(intents),
                    )

                total_bars = sum(bars_processed.values())
                if total_bars % heartbeat_interval == 0 and total_bars > 0:
                    dropped = engine.journal_dropped()
                    stale = engine.stale_bars_skipped()
                    stale_total = sum(stale.values()) if stale else 0
                    log.info(
                        "Heartbeat: %s, %d trades, %d journal drops, %d stale skipped",
                        " ".join(f"{s}={bars_processed[s]}" for s in symbols),
                        trades_placed,
                        dropped,
                        stale_total,
                    )

                # Execute intents
                for intent in intents:
                    log.info(
                        "SIGNAL: %s %s %s (score=%.2f, reason=%s)",
                        intent["side"].upper(),
                        intent["qty"],
                        symbol,
                        intent["score"],
                        intent["reason"],
                    )

                    try:
                        if intent["side"] == "buy":
                            result = alpaca.buy(symbol, intent["qty"])
                        else:
                            result = alpaca.sell(symbol, intent["qty"])

                        log.info(
                            "ORDER: %s (id=%s)", result["status"], result["id"][:12]
                        )
                        trades_placed += 1

                        engine.on_fill(
                            engine_symbol,
                            intent["side"],
                            intent["qty"],
                            bar["close"],
                        )
                    except Exception as e:
                        log.error("ORDER FAILED for %s: %s", symbol, e)

        except KeyboardInterrupt:
            break
        except Exception as e:
            consecutive_errors += 1
            backoff = min(interval_seconds * (2**consecutive_errors), 300)
            log.error(
                "Error (attempt %d/%d): %s. Retrying in %ds",
                consecutive_errors,
                max_retries,
                e,
                backoff,
            )
            if consecutive_errors >= max_retries:
                log.critical("Max retries exceeded, shutting down")
                break
            time.sleep(backoff)
            continue

        time.sleep(interval_seconds)

    # Shutdown summary
    total = sum(bars_processed.values())
    log.info(
        "Shutting down: %d total bars (%s), %d trades",
        total,
        ", ".join(f"{s}={bars_processed[s]}" for s in symbols),
        trades_placed,
    )

    # Log open positions
    try:
        positions = engine.positions()
        if positions:
            log.info("Open positions at shutdown:")
            for p in positions:
                log.info(
                    "  %s: qty=%.4f entry=%.2f pnl=%.2f",
                    p["symbol"],
                    p["qty"],
                    p["avg_entry_price"],
                    p["unrealized_pnl"],
                )
    except Exception as e:
        log.warning("Could not read positions at shutdown: %s", e)

    engine.shutdown_journal()
    log.info("Journal flushed, goodbye.")


def main():
    parser = argparse.ArgumentParser(
        description="OpenQuant Multi-Symbol Live Runner"
    )
    parser.add_argument(
        "--symbols",
        "-s",
        required=True,
        help="Comma-separated symbols (e.g. AAPL,MSFT,NVDA)",
    )
    parser.add_argument(
        "--interval", "-i", type=int, default=60, help="Poll interval in seconds"
    )
    parser.add_argument(
        "--config",
        "-c",
        type=str,
        default="openquant.toml",
        help="Path to openquant.toml config",
    )
    parser.add_argument(
        "--journal", type=str, default=None, help="SQLite journal path"
    )
    parser.add_argument(
        "--no-market-hours",
        action="store_true",
        help="Run outside market hours (useful for crypto or testing)",
    )
    parser.add_argument(
        "--max-retries",
        type=int,
        default=10,
        help="Max consecutive errors before exit",
    )
    args = parser.parse_args()

    symbols = [s.strip() for s in args.symbols.split(",") if s.strip()]
    if not symbols:
        print("Error: no symbols provided", file=sys.stderr)
        sys.exit(1)

    log.info("Loading config from %s", args.config)
    engine = Engine.from_toml(args.config, journal_path=args.journal)

    run(
        symbols=symbols,
        interval_seconds=args.interval,
        engine=engine,
        max_retries=args.max_retries,
        require_market_hours=not args.no_market_hours,
    )


if __name__ == "__main__":
    main()
