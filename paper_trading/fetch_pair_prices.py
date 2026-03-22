"""
Fetch daily close prices for pair-picker validation.

Pulls 200+ trading days of daily bars from Alpaca API and writes
`data/pair_picker_prices.json` in the format the pair-picker binary expects:

    {"GS": [185.2, 186.1, ...], "MS": [92.4, 93.1, ...], ...}

Usage:
    python -m paper_trading.fetch_pair_prices
    python -m paper_trading.fetch_pair_prices --days 300
    python -m paper_trading.fetch_pair_prices --symbols GS MS JPM
"""

from __future__ import annotations

import argparse
import json
import logging
import os
from datetime import datetime, timedelta, timezone
from pathlib import Path

from dotenv import load_dotenv

load_dotenv(Path(__file__).resolve().parent.parent / ".env")

logger = logging.getLogger(__name__)

# Default symbols: union of pair_candidates.json legs + relationship graph nodes
DEFAULT_SYMBOLS: list[str] = [
    "AAPL", "MSFT", "NVDA", "GOOGL", "META", "AMZN", "ORCL", "CRM", "ADBE", "NOW",
    "AMD", "AVGO", "INTC", "QCOM", "TXN", "MU", "MRVL", "LRCX", "AMAT", "KLAC", "ON", "TSM",
    "JPM", "BAC", "GS", "MS", "C", "WFC", "USB", "PNC", "SCHW", "BLK", "AXP",
    "XOM", "CVX", "COP", "SLB", "EOG", "PSX", "VLO", "MPC", "HAL",
    "WMT", "COST", "HD", "LOW", "MCD", "SBUX", "NKE", "TGT", "YUM", "DG",
    "V", "MA", "PYPL", "SQ",
    "UBER", "LYFT",
    "DAL", "UAL", "LUV", "AAL",
    "T", "VZ", "TMUS",
    "DIS", "NFLX",
    "FDX", "UPS",
    "GLD", "SLV",
    "COIN", "PLTR",
    # Boring duopoly candidates (stable relationships)
    "KO", "PEP",       # beverages
    "PG", "CL",        # household products
    "WM", "RSG",       # waste management
    "O", "NNN",        # net-lease REITs
    "LMT", "NOC",      # defense
    "DUK", "SO",       # regulated utilities
    "MCO", "SPGI",     # credit ratings
    "ABT", "MDT",      # medical devices
    "ELV", "CI",       # health insurance
]

MIN_BARS = 200


def fetch_daily_closes(
    symbols: list[str],
    days: int = 250,
) -> dict[str, list[float]]:
    """Fetch daily close prices from Alpaca API.

    Returns {symbol: [close_day1, close_day2, ...]} ordered oldest-to-newest.
    """
    from alpaca.data.historical import StockHistoricalDataClient
    from alpaca.data.requests import StockBarsRequest
    from alpaca.data.timeframe import TimeFrame
    from alpaca.data.enums import DataFeed

    client = StockHistoricalDataClient(
        os.environ.get("ALPACA_API_KEY"),
        os.environ.get("ALPACA_SECRET_KEY"),
    )

    end = datetime.now(timezone.utc)
    # Request extra days to account for weekends/holidays
    start = end - timedelta(days=int(days * 1.5))

    prices: dict[str, list[float]] = {}
    batch_size = 20  # Alpaca allows batching

    for i in range(0, len(symbols), batch_size):
        batch = symbols[i : i + batch_size]
        logger.info("Fetching daily bars for %d symbols: %s...", len(batch), batch[:5])

        try:
            # Try SIP first (consolidated, matches live trading feed),
            # fall back to IEX for free-tier accounts.
            try:
                request = StockBarsRequest(
                    symbol_or_symbols=batch,
                    timeframe=TimeFrame.Day,
                    start=start,
                    end=end,
                    feed=DataFeed.SIP,
                )
                bars = client.get_stock_bars(request)
            except Exception:
                request = StockBarsRequest(
                    symbol_or_symbols=batch,
                    timeframe=TimeFrame.Day,
                    start=start,
                    end=end,
                    feed=DataFeed.IEX,
                )
                bars = client.get_stock_bars(request)

            for symbol in batch:
                symbol_bars = bars.data.get(symbol, [])
                closes = [bar.close for bar in symbol_bars]
                if len(closes) >= MIN_BARS:
                    prices[symbol] = closes
                    logger.info("  %s: %d bars", symbol, len(closes))
                else:
                    logger.warning(
                        "  %s: only %d bars (need %d), skipping",
                        symbol, len(closes), MIN_BARS,
                    )
        except Exception as e:
            logger.error("Failed to fetch batch %s: %s", batch[:3], e)

    return prices


def write_prices(prices: dict[str, list[float]], output_path: Path) -> None:
    """Write prices to JSON file."""
    output_path.parent.mkdir(parents=True, exist_ok=True)
    with open(output_path, "w") as f:
        json.dump(prices, f)
    logger.info(
        "Wrote %d symbols (%d total bars) to %s",
        len(prices),
        sum(len(v) for v in prices.values()),
        output_path,
    )


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Fetch daily close prices for pair-picker"
    )
    parser.add_argument(
        "--days", type=int, default=250,
        help="Number of trading days to fetch (default: 250)",
    )
    parser.add_argument(
        "--output", type=Path,
        default=Path(__file__).resolve().parent.parent / "data" / "pair_picker_prices.json",
        help="Output path",
    )
    parser.add_argument(
        "--symbols", nargs="+", default=None,
        help="Symbols to fetch (default: all pair candidates)",
    )
    args = parser.parse_args()

    logging.basicConfig(
        level=logging.INFO,
        format="%(asctime)s %(levelname)s %(name)s: %(message)s",
    )

    symbols = args.symbols or DEFAULT_SYMBOLS
    logger.info("Fetching %d days of daily bars for %d symbols", args.days, len(symbols))

    prices = fetch_daily_closes(symbols, days=args.days)

    if prices:
        write_prices(prices, args.output)
        print(f"Fetched {len(prices)} symbols, wrote to {args.output}")
    else:
        print("No prices fetched — check API key and network")


if __name__ == "__main__":
    main()
