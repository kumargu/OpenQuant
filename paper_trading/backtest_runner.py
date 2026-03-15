"""
Backtest runner: fetches historical data from Alpaca, runs through Rust engine.

Usage:
  python -m paper_trading.backtest_runner --symbol BTC/USD --days 30
  python -m paper_trading.backtest_runner --symbol AAPL --days 90 --timeframe 5Min
"""

import argparse
import os
from datetime import datetime, timedelta, timezone

from dotenv import load_dotenv

load_dotenv()


def fetch_bars(symbol: str, days: int, timeframe: str = "1Min"):
    """Fetch historical bars from Alpaca."""
    from alpaca.data.timeframe import TimeFrame, TimeFrameUnit

    tf_map = {
        "1Min": TimeFrame.Minute,
        "5Min": TimeFrame(5, TimeFrameUnit.Minute),
        "15Min": TimeFrame(15, TimeFrameUnit.Minute),
        "1Hour": TimeFrame.Hour,
        "1Day": TimeFrame.Day,
    }
    tf = tf_map.get(timeframe, TimeFrame.Minute)

    end = datetime.now(timezone.utc)
    start = end - timedelta(days=days)

    is_crypto = "/" in symbol

    if is_crypto:
        from alpaca.data.historical import CryptoHistoricalDataClient
        from alpaca.data.requests import CryptoBarsRequest

        client = CryptoHistoricalDataClient()
        req = CryptoBarsRequest(
            symbol_or_symbols=symbol,
            timeframe=tf,
            start=start,
            end=end,
        )
        barset = client.get_crypto_bars(req)
    else:
        from alpaca.data.historical import StockHistoricalDataClient
        from alpaca.data.requests import StockBarsRequest

        client = StockHistoricalDataClient(
            os.environ["ALPACA_API_KEY"],
            os.environ["ALPACA_SECRET_KEY"],
        )
        req = StockBarsRequest(
            symbol_or_symbols=symbol,
            timeframe=tf,
            start=start,
            end=end,
        )
        barset = client.get_stock_bars(req)

    # Find the right key
    bar_key = symbol if symbol in barset.data else symbol.replace("/", "")
    if bar_key not in barset.data:
        print(f"No data for {symbol}")
        return []

    raw_bars = barset.data[bar_key]
    print(f"Fetched {len(raw_bars)} bars for {symbol} ({days} days, {timeframe})")

    # Convert to tuples for Rust: (symbol, timestamp, open, high, low, close, volume)
    bars = [
        (
            symbol.replace("/", ""),
            int(b.timestamp.timestamp() * 1000),
            float(b.open),
            float(b.high),
            float(b.low),
            float(b.close),
            float(b.volume),
        )
        for b in raw_bars
    ]

    # Quick validation: warn on high zero-volume ratio
    if bars:
        zero_vol = sum(1 for b in bars if b[6] == 0.0)
        pct = zero_vol / len(bars)
        if pct > 0.5:
            print(f"  WARNING: {pct:.0%} zero-volume bars — volume signals unreliable. "
                  f"Consider --timeframe 5Min or higher.")
        elif pct > 0.1:
            print(f"  NOTE: {pct:.0%} zero-volume bars detected.")

    return bars


def print_result(result: dict):
    """Pretty-print backtest results."""
    print("\n" + "=" * 60)
    print("BACKTEST RESULTS")
    print("=" * 60)

    print(f"\nBars processed:    {result['total_bars']}")
    print(f"Signals generated: {result['signals_generated']}")
    print(f"Trades completed:  {result['total_trades']}")

    if result["total_trades"] == 0:
        print("\nNo trades taken. Strategy did not fire or all signals were rejected.")
        return

    print(f"\n--- P&L ---")
    print(f"Total P&L:         ${result['total_pnl']:,.2f}")
    print(f"Expectancy/trade:  ${result['expectancy']:,.2f}")

    print(f"\n--- Win/Loss ---")
    print(f"Win rate:          {result['win_rate']:.1%}")
    print(f"Winners:           {result['winning_trades']}")
    print(f"Losers:            {result['losing_trades']}")
    print(f"Avg win:           ${result['avg_win']:,.2f}")
    print(f"Avg loss:          ${result['avg_loss']:,.2f}")
    print(f"Profit factor:     {result['profit_factor']:.2f}")

    print(f"\n--- Risk ---")
    print(f"Max drawdown:      ${result['max_drawdown']:,.2f}")
    if result['max_drawdown_pct'] > 0:
        print(f"Max drawdown %:    {result['max_drawdown_pct']:.1%}")
    print(f"Sharpe (approx):   {result['sharpe_approx']:.2f}")

    if result["trades"]:
        print(f"\n--- Trades ---")
        print(f"{'Entry':<12} {'Exit':<12} {'Qty':<8} {'P&L':<12} {'Return':<10} {'Bars':<6} {'Exit Reason'}")
        print("-" * 85)
        for t in result["trades"]:
            print(
                f"${t['entry_price']:<11,.2f} ${t['exit_price']:<11,.2f} "
                f"{t['qty']:<8.1f} ${t['pnl']:<11,.2f} {t['return_pct']:>8.2%}  "
                f"{t['bars_held']:<6} {t['exit_reason'][:35]}"
            )


def main():
    from paper_trading.config import engine_kwargs, merge_cli_overrides

    # Load TOML defaults first
    toml_kw = engine_kwargs()

    parser = argparse.ArgumentParser(description="OpenQuant Backtester")
    parser.add_argument("--symbol", "-s", default="BTC/USD")
    parser.add_argument("--days", "-d", type=int, default=7)
    parser.add_argument("--timeframe", "-t", default="1Min")
    parser.add_argument("--config", default=None, help="Path to openquant.toml (default: repo root)")
    parser.add_argument("--max-position", type=float, default=None)
    parser.add_argument("--max-daily-loss", type=float, default=None)
    parser.add_argument("--buy-z", type=float, default=None)
    parser.add_argument("--sell-z", type=float, default=None)
    parser.add_argument("--min-vol", type=float, default=None)
    parser.add_argument("--stop-loss", type=float, default=None, help="Fixed stop loss pct (0 = disabled, use ATR)")
    parser.add_argument("--stop-loss-atr", type=float, default=None, help="ATR multiplier for dynamic stop (0 = disabled)")
    parser.add_argument("--max-hold", type=int, default=None, help="Max bars to hold (0 = disabled)")
    parser.add_argument("--take-profit", type=float, default=None, help="Take profit pct (0 = disabled)")
    parser.add_argument("--no-trend-filter", action="store_true", help="Disable SMA-50 trend filter")
    args = parser.parse_args()

    # Reload TOML if custom path specified
    if args.config:
        toml_kw = engine_kwargs(args.config)

    # CLI flags override TOML values
    kw = merge_cli_overrides(toml_kw, args)

    bars = fetch_bars(args.symbol, args.days, args.timeframe)
    if not bars:
        return

    from openquant import backtest

    # Build backtest kwargs: start from TOML+CLI merged values, apply hardcoded defaults
    # for anything not specified in either source
    result = backtest(
        bars,
        max_position_notional=kw.get("max_position_notional", 10_000.0),
        max_daily_loss=kw.get("max_daily_loss", 500.0),
        buy_z_threshold=kw.get("buy_z_threshold", -2.2),
        sell_z_threshold=kw.get("sell_z_threshold", 2.0),
        min_relative_volume=kw.get("min_relative_volume", 1.2),
        stop_loss_pct=kw.get("stop_loss_pct", 0.0),
        max_hold_bars=kw.get("max_hold_bars", 100),
        take_profit_pct=kw.get("take_profit_pct", 0.0),
        trend_filter=kw.get("trend_filter", True),
        stop_loss_atr_mult=kw.get("stop_loss_atr_mult", 2.5),
    )

    print_result(result)


if __name__ == "__main__":
    main()
