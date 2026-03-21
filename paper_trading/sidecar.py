"""
Python sidecar — data plumbing for the Rust trading engine.

Responsibilities:
1. Fetch bars from Alpaca, write to data/bars/
2. Read data/order_intents.json, submit orders to Alpaca
3. Run on a timer alongside the Rust openquant-runner binary

The interface is JSON files — this script can be replaced with any language.
"""

import json
import logging
import sys
import time
from pathlib import Path

from paper_trading.alpaca_client import AlpacaClient

logging.basicConfig(
    level=logging.INFO,
    format="%(asctime)s %(levelname)s %(name)s: %(message)s",
)
log = logging.getLogger("sidecar")

DATA_DIR = Path("data")
ORDER_INTENTS_PATH = DATA_DIR / "order_intents.json"


def fetch_bars(client: AlpacaClient, symbols: list[str], timeframe: str = "1Min"):
    """Fetch latest bars from Alpaca and write to data/ as experiment_bars format."""
    from datetime import datetime

    today = datetime.now().strftime("%Y%m%d")
    output_path = DATA_DIR / f"experiment_bars_{today}.json"

    if output_path.exists():
        log.info(f"bars already fetched for today: {output_path}")
        return

    log.info(f"fetching bars for {len(symbols)} symbols...")
    all_bars = {}
    for symbol in symbols:
        try:
            bars = client.get_bars(symbol, timeframe=timeframe, limit=390)
            all_bars[symbol] = [
                {
                    "timestamp": int(b.timestamp.timestamp() * 1000),
                    "open": b.open,
                    "high": b.high,
                    "low": b.low,
                    "close": b.close,
                    "volume": b.volume,
                }
                for b in bars
            ]
            log.info(f"  {symbol}: {len(all_bars[symbol])} bars")
        except Exception as e:
            log.warning(f"  {symbol}: failed to fetch bars: {e}")

    output_path.write_text(json.dumps(all_bars, indent=2))
    log.info(f"wrote {output_path}")


def submit_orders(client: AlpacaClient):
    """Read order_intents.json and submit to Alpaca."""
    if not ORDER_INTENTS_PATH.exists():
        log.info("no order_intents.json — nothing to submit")
        return

    intents = json.loads(ORDER_INTENTS_PATH.read_text())
    if not intents:
        log.info("order_intents.json is empty — nothing to submit")
        return

    log.info(f"submitting {len(intents)} order intents...")
    for intent in intents:
        symbol = intent["symbol"]
        side = intent["side"]
        qty = int(intent["qty"])
        reason = intent.get("reason", "unknown")
        pair_id = intent.get("pair_id", "")

        if qty <= 0:
            log.warning(f"skipping {symbol} {side}: qty={qty}")
            continue

        try:
            order = client.submit_order(
                symbol=symbol,
                qty=qty,
                side=side,
                type="market",
                time_in_force="day",
            )
            log.info(
                f"  {side} {qty} {symbol} (pair={pair_id}, reason={reason}): "
                f"order_id={order.id}"
            )
        except Exception as e:
            log.error(f"  {side} {qty} {symbol}: failed: {e}")

    # Clear intents after submission
    ORDER_INTENTS_PATH.write_text("[]")
    log.info("cleared order_intents.json")


def main():
    """Run one cycle: fetch bars, submit pending orders."""
    client = AlpacaClient()

    # Symbols to fetch — read from active_pairs.json if available
    symbols = set()
    active_pairs_path = DATA_DIR / "active_pairs.json"
    if active_pairs_path.exists():
        data = json.loads(active_pairs_path.read_text())
        for pair in data.get("pairs", []):
            symbols.add(pair["leg_a"])
            symbols.add(pair["leg_b"])

    if symbols:
        fetch_bars(client, sorted(symbols))
    else:
        log.warning("no symbols found in active_pairs.json — skipping bar fetch")

    submit_orders(client)


if __name__ == "__main__":
    main()
