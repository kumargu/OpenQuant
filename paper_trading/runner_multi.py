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
from zoneinfo import ZoneInfo

from openquant import Engine

from . import alpaca_client as alpaca
from .runner import _get_latest_bar

_log_fmt = "%(asctime)s %(levelname)s %(message)s"
_log_datefmt = "%Y-%m-%d %H:%M:%S"

# Log to both console and file
logging.basicConfig(level=logging.INFO, format=_log_fmt, datefmt=_log_datefmt)

_file_handler = logging.FileHandler("data/journal/runner.log", mode="a")
_file_handler.setLevel(logging.INFO)
_file_handler.setFormatter(logging.Formatter(_log_fmt, datefmt=_log_datefmt))
logging.getLogger().addHandler(_file_handler)

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


def _warmup(symbols: list[str], engine: Engine, num_bars: int = 60):
    """Pre-load historical bars to warm up indicators instantly."""
    import os
    from datetime import timedelta
    from dotenv import load_dotenv
    from alpaca.data.historical import CryptoHistoricalDataClient, StockHistoricalDataClient
    from alpaca.data.requests import CryptoBarsRequest, StockBarsRequest
    from alpaca.data.timeframe import TimeFrame
    from alpaca.data.enums import DataFeed

    load_dotenv()
    start = datetime.now(timezone.utc) - timedelta(minutes=num_bars + 5)

    # Bypass stale-data gate during warmup — historical bars are always "old"
    engine.set_warmup_mode(True)

    for symbol in symbols:
        is_crypto = "/" in symbol
        if is_crypto:
            client = CryptoHistoricalDataClient()
            req = CryptoBarsRequest(
                symbol_or_symbols=symbol, timeframe=TimeFrame.Minute, start=start,
            )
            bars = client.get_crypto_bars(req)
        else:
            client = StockHistoricalDataClient(
                os.environ["ALPACA_API_KEY"], os.environ["ALPACA_SECRET_KEY"],
            )
            req = StockBarsRequest(
                symbol_or_symbols=symbol, timeframe=TimeFrame.Minute, start=start, feed=DataFeed.IEX,
            )
            bars = client.get_stock_bars(req)

        bar_key = symbol if symbol in bars.data else symbol.replace("/", "")
        bar_list = bars.data.get(bar_key, [])
        engine_symbol = symbol.replace("/", "")
        for bar in bar_list:
            engine.on_bar(
                engine_symbol,
                int(bar.timestamp.timestamp() * 1000),
                float(bar.open),
                float(bar.high),
                float(bar.low),
                float(bar.close),
                float(bar.volume),
            )
        log.info("Warmup: fed %d historical bars for %s", len(bar_list), symbol)

    # Switch back to live mode — re-enable stale-data gate
    engine.set_warmup_mode(False)
    log.info("Warmup complete — indicators hot, ready to trade")


def run(
    symbols: list[str],
    interval_seconds: int,
    engine: Engine,
    max_retries: int = 10,
    require_market_hours: bool = True,
    warmup_bars: int = 60,
):
    """Poll bars for all symbols and feed into a single engine."""
    log.info(
        "Starting multi-symbol runner: %s, polling every %ds",
        ", ".join(symbols),
        interval_seconds,
    )

    if warmup_bars > 0:
        _warmup(symbols, engine, warmup_bars)

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
                ).astimezone(ZoneInfo("America/New_York")).strftime("%H:%M:%S ET")

                age = (datetime.now(timezone.utc) - datetime.fromtimestamp(bar_time / 1000, tz=timezone.utc)).total_seconds()
                log.info(
                    "[%s] %s C=%.2f V=%.4f age=%ds -> %d signal(s)",
                    ts,
                    symbol,
                    bar["close"],
                    bar["volume"],
                    int(age),
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
                    side = intent["side"].upper()
                    qty = intent["qty"]
                    price = bar["close"]
                    notional = qty * price
                    votes = intent.get("votes", "")
                    vote_str = f" votes=[{votes}]" if votes else ""
                    log.info(
                        ">>> %s %s qty=%s @ $%.2f ($%.0f) | score=%.2f reason=%s%s",
                        side,
                        symbol,
                        qty,
                        price,
                        notional,
                        intent["score"],
                        intent["reason"],
                        vote_str,
                    )

                    try:
                        if intent["side"] == "buy":
                            result = alpaca.buy(symbol, qty)
                        else:
                            result = alpaca.sell(symbol, qty)

                        log.info(
                            "<<< FILLED %s %s qty=%s | order=%s status=%s",
                            side,
                            symbol,
                            qty,
                            result["id"][:12],
                            result["status"],
                        )
                        trades_placed += 1

                        engine.on_fill(
                            engine_symbol,
                            intent["side"],
                            qty,
                            price,
                        )
                    except Exception as e:
                        log.error("!!! ORDER FAILED %s %s: %s", side, symbol, e)

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
    parser.add_argument(
        "--warmup-bars",
        type=int,
        default=60,
        help="Historical bars to pre-load for indicator warmup (0=skip)",
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
        warmup_bars=args.warmup_bars,
    )


if __name__ == "__main__":
    main()
