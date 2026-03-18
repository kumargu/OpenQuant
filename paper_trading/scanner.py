"""
Pre-market scanner — picks today's trading universe based on Alpaca snapshots.

Scores stocks on liquidity, volatility, mean-reversion suitability, and spread
tightness. Outputs a ranked symbol list for runner_multi.py.

Usage:
    python -m paper_trading.scanner                      # scan default watchlist
    python -m paper_trading.scanner --top 15             # pick top 15
    python -m paper_trading.scanner --watchlist sp100    # use S&P 100 base
    python -m paper_trading.scanner --json               # output as JSON
"""

import argparse
import os
import sys
from dataclasses import dataclass
from datetime import datetime
from zoneinfo import ZoneInfo

from dotenv import load_dotenv

load_dotenv()

ET = ZoneInfo("America/New_York")

# Base watchlists — broad enough to find good candidates, narrow enough to be fast
WATCHLISTS = {
    "default": [
        # Tech
        "AAPL", "MSFT", "NVDA", "TSLA", "GOOGL", "META", "AMD", "AMZN", "AVGO", "ORCL",
        # Financials
        "JPM", "BAC", "GS", "MS", "C",
        # Healthcare
        "LLY", "UNH", "JNJ", "ABBV", "MRK", "PFE",
        # Energy
        "XOM", "CVX", "COP", "SLB",
        # Consumer
        "WMT", "COST", "HD", "MCD", "NKE",
        # Industrials
        "CAT", "BA", "GE", "RTX",
        # Semiconductors
        "MU", "INTC", "QCOM", "TXN",
        # ETFs / Commodities
        "GLD", "SLV", "XLE", "XLF",
        # Volatile / high-beta
        "COIN", "MARA", "RIOT", "PLTR", "SOFI",
    ],
    "sp100": [
        "AAPL", "ABBV", "ABT", "ACN", "ADBE", "AIG", "AMD", "AMGN", "AMZN", "AVGO",
        "AXP", "BA", "BAC", "BK", "BKNG", "BLK", "BMY", "C", "CAT", "CHTR",
        "CL", "CMCSA", "COF", "COP", "COST", "CRM", "CSCO", "CVS", "CVX", "DE",
        "DHR", "DIS", "DOW", "DUK", "EMR", "EXC", "F", "FDX", "GD", "GE",
        "GILD", "GM", "GOOGL", "GS", "HD", "HON", "IBM", "INTC", "JNJ", "JPM",
        "KHC", "KO", "LIN", "LLY", "LMT", "LOW", "MA", "MCD", "MDLZ", "MDT",
        "MET", "META", "MMM", "MO", "MRK", "MS", "MSFT", "NEE", "NFLX", "NKE",
        "NVDA", "ORCL", "PEP", "PFE", "PG", "PM", "PYPL", "QCOM", "RTX", "SBUX",
        "SCHW", "SO", "SPG", "T", "TGT", "TMO", "TMUS", "TSLA", "TXN", "UNH",
        "UNP", "UPS", "USB", "V", "VZ", "WBA", "WFC", "WMT", "XOM",
    ],
    "current": [
        "AAPL", "NVDA", "TSLA", "MU", "GOOGL", "CVX", "GLD", "XOM", "AMZN", "JPM", "LLY", "INTC",
    ],
}


@dataclass
class StockScore:
    symbol: str
    price: float
    volume: int
    prev_volume: int
    relative_volume: float  # today's vol / yesterday's vol
    gap_pct: float          # overnight gap %
    daily_range_pct: float  # (high - low) / close — intraday volatility
    prev_range_pct: float   # yesterday's range — ATR proxy
    spread_pct: float       # (ask - bid) / mid — liquidity
    score: float = 0.0      # composite score (higher = better for MR)
    reasons: str = ""


def fetch_snapshots(symbols: list[str]) -> dict:
    """Fetch snapshots from Alpaca for all symbols. Returns dict of symbol → snapshot."""
    from alpaca.data.historical import StockHistoricalDataClient
    from alpaca.data.requests import StockSnapshotRequest

    client = StockHistoricalDataClient(
        os.environ["ALPACA_API_KEY"],
        os.environ["ALPACA_SECRET_KEY"],
    )

    # Alpaca accepts up to ~200 symbols per request
    snapshots = {}
    batch_size = 100
    for i in range(0, len(symbols), batch_size):
        batch = symbols[i : i + batch_size]
        try:
            result = client.get_stock_snapshot(
                StockSnapshotRequest(symbol_or_symbols=batch)
            )
            snapshots.update(result)
        except Exception as e:
            print(f"  Warning: batch {i}-{i+len(batch)} failed: {e}", file=sys.stderr)

    return snapshots


def score_stock(symbol: str, snap) -> StockScore | None:
    """Score a single stock from its snapshot data."""
    try:
        if not snap.daily_bar or not snap.previous_daily_bar or not snap.latest_quote:
            return None

        db = snap.daily_bar
        pb = snap.previous_daily_bar
        quote = snap.latest_quote

        price = db.close
        if price <= 0:
            return None

        # Volume
        volume = int(db.volume)
        prev_volume = int(pb.volume)
        relative_volume = volume / prev_volume if prev_volume > 0 else 0

        # Gap: how much did it gap from yesterday's close?
        gap_pct = (db.open - pb.close) / pb.close if pb.close > 0 else 0

        # Intraday range (volatility today)
        daily_range_pct = (db.high - db.low) / price if price > 0 else 0

        # Yesterday's range (ATR proxy)
        prev_range_pct = (pb.high - pb.low) / pb.close if pb.close > 0 else 0

        # Spread tightness (use price as fallback if bid/ask is stale or crossed)
        bid = quote.bid_price if quote.bid_price > 0 else price
        ask = quote.ask_price if quote.ask_price > 0 else price
        if ask < bid:
            bid, ask = price, price  # stale/crossed quotes
        mid = (ask + bid) / 2
        spread_pct = (ask - bid) / mid if mid > 0 else 0.01

        return StockScore(
            symbol=symbol,
            price=price,
            volume=volume,
            prev_volume=prev_volume,
            relative_volume=relative_volume,
            gap_pct=gap_pct,
            daily_range_pct=daily_range_pct,
            prev_range_pct=prev_range_pct,
            spread_pct=spread_pct,
        )
    except Exception as e:
        print(f"  Warning: {symbol} scoring failed: {e}", file=sys.stderr)
        return None


def rank_for_mean_reversion(stocks: list[StockScore]) -> list[StockScore]:
    """
    Score and rank stocks for intraday mean-reversion trading.

    Good MR candidates:
    - High liquidity (volume, tight spread)
    - Moderate volatility (not dead, not trending wildly)
    - Small gap (large gaps tend to trend, not revert)
    - Active today (relative volume > 0.5)
    """
    for s in stocks:
        score = 0.0
        reasons = []

        # 1. Liquidity: prefer high volume + tight spread
        #    Volume score: log-scaled, cap at 5 points
        if s.volume > 0:
            import math
            vol_score = min(math.log10(s.volume) - 3, 5)  # 10k vol = 1pt, 1M = 3pt, 10M = 4pt
            if vol_score > 0:
                score += vol_score
                if vol_score > 3:
                    reasons.append("high_vol")
        else:
            score -= 5  # no volume = skip
            reasons.append("no_vol")

        # 2. Spread: tighter is better (penalty for wide spreads)
        if s.spread_pct < 0.001:      # < 10 bps — excellent
            score += 2
            reasons.append("tight_spread")
        elif s.spread_pct < 0.005:    # < 50 bps — OK
            score += 1
        elif s.spread_pct > 0.02:     # > 200 bps — too wide
            score -= 3
            reasons.append("wide_spread")

        # 3. Volatility: sweet spot is 0.5% - 3% daily range
        if 0.005 < s.prev_range_pct < 0.03:
            score += 2
            reasons.append("good_vol")
        elif s.prev_range_pct < 0.003:
            score -= 1
            reasons.append("low_vol")
        elif s.prev_range_pct > 0.05:
            score -= 2
            reasons.append("too_volatile")

        # 4. Gap: small gaps are better for MR (large gaps trend)
        abs_gap = abs(s.gap_pct)
        if abs_gap < 0.01:            # < 1% gap — ideal
            score += 2
            reasons.append("small_gap")
        elif abs_gap < 0.02:          # 1-2% — OK
            score += 1
        elif abs_gap > 0.05:          # > 5% — likely trending
            score -= 3
            reasons.append("big_gap")

        # 5. Relative volume: active today
        if s.relative_volume > 1.5:
            score += 2
            reasons.append("active_today")
        elif s.relative_volume > 0.8:
            score += 1
        elif s.relative_volume < 0.3:
            score -= 2
            reasons.append("quiet_today")

        # 6. Price filter: prefer $20-$500 range (good for $10k position sizing)
        if 20 < s.price < 500:
            score += 1
        elif s.price < 5 or s.price > 2000:
            score -= 2
            reasons.append("price_outlier")

        s.score = score
        s.reasons = ",".join(reasons) if reasons else "baseline"

    # Sort by score descending
    stocks.sort(key=lambda s: s.score, reverse=True)
    return stocks


def scan(watchlist: str = "default", top: int = 12) -> list[StockScore]:
    """Run the full scanner pipeline."""
    symbols = WATCHLISTS.get(watchlist, WATCHLISTS["default"])
    print(f"Scanning {len(symbols)} symbols from '{watchlist}' watchlist...")

    snapshots = fetch_snapshots(symbols)
    print(f"Got snapshots for {len(snapshots)}/{len(symbols)} symbols")

    # Score each stock
    stocks = []
    for sym in symbols:
        if sym in snapshots:
            scored = score_stock(sym, snapshots[sym])
            if scored:
                stocks.append(scored)

    # Rank for mean-reversion
    ranked = rank_for_mean_reversion(stocks)

    return ranked[:top]


def print_results(ranked: list[StockScore], top: int):
    """Print scanner results."""
    now = datetime.now(ET)
    print(f"\n{'='*80}")
    print(f"PRE-MARKET SCAN — {now.strftime('%Y-%m-%d %H:%M ET')}")
    print(f"{'='*80}")
    print()
    print(f"{'#':>3} {'Symbol':>7} {'Price':>8} {'Volume':>10} {'RelVol':>7} {'Gap%':>7} {'Range%':>7} {'Spread':>8} {'Score':>6} Reasons")
    print(f"{'—'*3} {'—'*7} {'—'*8} {'—'*10} {'—'*7} {'—'*7} {'—'*7} {'—'*8} {'—'*6} {'—'*20}")

    for i, s in enumerate(ranked):
        print(
            f"{i+1:>3} {s.symbol:>7} ${s.price:>7.2f} {s.volume:>10,} "
            f"{s.relative_volume:>6.1f}x {s.gap_pct:>+6.2%} {s.daily_range_pct:>6.2%} "
            f"{s.spread_pct:>7.3%} {s.score:>6.1f} {s.reasons}"
        )

    symbols = [s.symbol for s in ranked[:top]]
    print(f"\n{'='*80}")
    print(f"TODAY'S UNIVERSE ({len(symbols)} symbols):")
    print(f"  {','.join(symbols)}")
    print(f"\nRun with:")
    print(f"  python -m paper_trading.runner_multi --symbols {','.join(symbols)}")
    print(f"{'='*80}")


def main():
    parser = argparse.ArgumentParser(description="Pre-market scanner for OpenQuant")
    parser.add_argument("--watchlist", "-w", default="default",
                        choices=list(WATCHLISTS.keys()),
                        help="Base watchlist to scan")
    parser.add_argument("--top", "-t", type=int, default=12,
                        help="Number of symbols to pick (default: 12)")
    parser.add_argument("--json", action="store_true",
                        help="Output as JSON (for piping to runner)")
    args = parser.parse_args()

    ranked = scan(watchlist=args.watchlist, top=args.top)

    if args.json:
        import json
        output = {
            "timestamp": datetime.now(ET).isoformat(),
            "symbols": [s.symbol for s in ranked],
            "details": [
                {
                    "symbol": s.symbol,
                    "price": s.price,
                    "volume": s.volume,
                    "relative_volume": round(s.relative_volume, 2),
                    "gap_pct": round(s.gap_pct, 4),
                    "range_pct": round(s.daily_range_pct, 4),
                    "spread_pct": round(s.spread_pct, 4),
                    "score": round(s.score, 1),
                    "reasons": s.reasons,
                }
                for s in ranked
            ],
        }
        print(json.dumps(output, indent=2))
    else:
        print_results(ranked, args.top)


if __name__ == "__main__":
    main()
