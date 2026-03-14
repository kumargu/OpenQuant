"""
Live runner: feeds Alpaca market data bars into the Rust engine,
places paper trades when the engine emits order intents.

Usage:
  python -m paper_trading.runner --symbol BTC/USD --interval 1m
  python -m paper_trading.runner --symbol AAPL --interval 5m
"""

import argparse
import time
from datetime import datetime, timezone

from openquant import Engine

from . import alpaca_client as alpaca


def run(symbol: str, interval_seconds: int, engine: Engine):
    """Poll bars and feed into engine. Place trades on signals."""
    print(f"Running engine on {symbol}, polling every {interval_seconds}s")
    print(f"Ctrl+C to stop\n")

    last_bar_time = None

    while True:
        try:
            bars = _get_latest_bar(symbol)
            if bars is None:
                print(f"[{_now()}] No bar data for {symbol}, waiting...")
                time.sleep(interval_seconds)
                continue

            bar_time = bars["timestamp"]

            # Skip if we already processed this bar
            if bar_time == last_bar_time:
                time.sleep(interval_seconds)
                continue

            last_bar_time = bar_time

            # Feed bar into Rust engine
            intents = engine.on_bar(
                symbol.replace("/", ""),  # Alpaca uses BTCUSD not BTC/USD internally
                int(bar_time),
                bars["open"],
                bars["high"],
                bars["low"],
                bars["close"],
                bars["volume"],
            )

            # Log bar
            ts = datetime.fromtimestamp(bar_time / 1000, tz=timezone.utc).strftime("%H:%M:%S")
            print(
                f"[{ts}] {symbol} O={bars['open']:.2f} H={bars['high']:.2f} "
                f"L={bars['low']:.2f} C={bars['close']:.2f} V={bars['volume']:.0f}"
                + (f"  -> {len(intents)} signal(s)" if intents else "")
            )

            # Execute intents
            for intent in intents:
                print(f"  SIGNAL: {intent['side'].upper()} {intent['qty']} {symbol} "
                      f"(score={intent['score']:.2f}, reason={intent['reason']})")

                try:
                    if intent["side"] == "buy":
                        result = alpaca.buy(symbol, intent["qty"])
                    else:
                        result = alpaca.sell(symbol, intent["qty"])

                    print(f"  ORDER: {result['status']} (id={result['id'][:12]})")

                    # Notify engine of fill (use close as approximate fill price)
                    engine.on_fill(
                        symbol.replace("/", ""),
                        intent["side"],
                        intent["qty"],
                        bars["close"],
                    )
                except Exception as e:
                    print(f"  ORDER FAILED: {e}")

        except KeyboardInterrupt:
            print("\nStopping.")
            break
        except Exception as e:
            print(f"[{_now()}] Error: {e}")

        time.sleep(interval_seconds)


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
        import os
        from dotenv import load_dotenv
        load_dotenv()
        client = StockHistoricalDataClient(
            os.environ["ALPACA_API_KEY"],
            os.environ["ALPACA_SECRET_KEY"],
        )
        req = StockBarsRequest(symbol_or_symbols=symbol, timeframe=TimeFrame.Minute, limit=1)
        bars = client.get_stock_bars(req)

    # Extract the bar — key might be "BTC/USD" or "BTCUSD" depending on asset type
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


def _now() -> str:
    return datetime.now(timezone.utc).strftime("%H:%M:%S")


def main():
    parser = argparse.ArgumentParser(description="OpenQuant Live Runner")
    parser.add_argument("--symbol", "-s", default="BTC/USD", help="Symbol to trade")
    parser.add_argument("--interval", "-i", type=int, default=60, help="Poll interval in seconds")
    parser.add_argument("--max-position", type=float, default=10_000.0, help="Max position notional (USD)")
    parser.add_argument("--max-daily-loss", type=float, default=500.0, help="Max daily loss before kill switch (USD)")
    args = parser.parse_args()

    engine = Engine(
        max_position_notional=args.max_position,
        max_daily_loss=args.max_daily_loss,
    )

    run(args.symbol, args.interval, engine)


if __name__ == "__main__":
    main()
