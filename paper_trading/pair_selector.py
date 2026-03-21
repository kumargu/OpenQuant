"""
Daily Pair Selector — finds the best cointegrated pairs for today's trading.

Runs pre-market using the last N days of daily closes. Screens all pairs
from the symbol universe for mean-reversion quality, ranks them, and
outputs configs for the PairsEngine.

Usage:
    python -m paper_trading.pair_selector                    # default: top 4 pairs
    python -m paper_trading.pair_selector --top 6            # top 6 pairs
    python -m paper_trading.pair_selector --days 30          # use 30 days history
    python -m paper_trading.pair_selector --json             # output as JSON
    python -m paper_trading.pair_selector --toml             # output as TOML snippet
"""

import argparse
import json
import math
import os
import sys
from dataclasses import dataclass
from datetime import datetime, timedelta, timezone
from zoneinfo import ZoneInfo

import numpy as np
from dotenv import load_dotenv

load_dotenv()

ET = ZoneInfo("America/New_York")

# Full universe — same as scanner
SYMBOLS = [
    "AAPL", "MSFT", "NVDA", "TSLA", "GOOGL", "META", "AMD", "AMZN", "AVGO", "ORCL",
    "JPM", "BAC", "GS", "MS", "C",
    "LLY", "UNH", "JNJ", "ABBV", "MRK", "PFE",
    "XOM", "CVX", "COP", "SLB",
    "WMT", "COST", "HD", "MCD", "NKE",
    "CAT", "BA", "GE", "RTX",
    "MU", "INTC", "QCOM", "TXN",
    "GLD", "SLV", "XLE", "XLF",
    "COIN", "PLTR", "SOFI",
]

# Sector groups for diversification — avoid picking 3 pairs from same sector
SECTORS = {
    "tech": ["AAPL", "MSFT", "NVDA", "GOOGL", "META", "AMZN", "ORCL"],
    "semis": ["AMD", "AVGO", "MU", "INTC", "QCOM", "TXN"],
    "financials": ["JPM", "BAC", "GS", "MS", "C", "XLF"],
    "energy": ["XOM", "CVX", "COP", "SLB", "XLE"],
    "healthcare": ["LLY", "UNH", "JNJ", "ABBV", "MRK", "PFE"],
    "consumer": ["WMT", "COST", "HD", "MCD", "NKE"],
    "industrials": ["CAT", "BA", "GE", "RTX"],
    "metals": ["GLD", "SLV"],
    "high_beta": ["TSLA", "COIN", "PLTR", "SOFI"],
}

SYMBOL_TO_SECTOR = {}
for sector, syms in SECTORS.items():
    for s in syms:
        SYMBOL_TO_SECTOR[s] = sector


@dataclass
class PairResult:
    """Screening result for a single pair."""
    leg_a: str
    leg_b: str
    beta: float
    correlation: float
    half_life: float
    autocorrelation: float
    z_range: float
    spread_std: float
    score: float
    sector_a: str
    sector_b: str

    @property
    def is_cross_sector(self) -> bool:
        return self.sector_a != self.sector_b

    def to_toml(self) -> str:
        return (
            f"[[pairs]]\n"
            f"leg_a = \"{self.leg_a}\"\n"
            f"leg_b = \"{self.leg_b}\"\n"
            f"beta = {self.beta:.4f}\n"
            f"entry_z = 2.0\n"
            f"exit_z = 0.5\n"
            f"stop_z = 4.0\n"
            f"lookback = 32\n"
            f"max_hold_bars = 150\n"
            f"notional_per_leg = 10000.0\n"
        )

    def to_dict(self) -> dict:
        return {
            "leg_a": self.leg_a,
            "leg_b": self.leg_b,
            "beta": round(self.beta, 4),
            "correlation": round(self.correlation, 3),
            "half_life": round(self.half_life, 1),
            "autocorrelation": round(self.autocorrelation, 3),
            "z_range": round(self.z_range, 1),
            "score": round(self.score, 3),
            "sector_a": self.sector_a,
            "sector_b": self.sector_b,
        }


def fetch_daily_closes(symbols: list[str], days: int) -> dict[str, np.ndarray]:
    """Fetch daily close prices from Alpaca for the last N trading days."""
    from alpaca.data.historical import StockHistoricalDataClient
    from alpaca.data.requests import StockBarsRequest
    from alpaca.data.timeframe import TimeFrame
    from alpaca.data.enums import DataFeed

    client = StockHistoricalDataClient(
        os.environ["ALPACA_API_KEY"],
        os.environ["ALPACA_SECRET_KEY"],
    )

    end = datetime.now(timezone.utc)
    start = end - timedelta(days=days * 2)  # extra buffer for weekends/holidays

    req = StockBarsRequest(
        symbol_or_symbols=symbols,
        timeframe=TimeFrame.Day,
        start=start,
        end=end,
        feed=DataFeed.IEX,
    )

    barset = client.get_stock_bars(req)
    result = {}

    for sym in symbols:
        key = sym if sym in barset.data else sym.replace("/", "")
        if key not in barset.data:
            continue
        bars = barset.data[key]
        if len(bars) < days // 2:
            continue
        closes = np.array([float(b.close) for b in bars[-days:]])
        if len(closes) >= days // 2:
            result[sym] = closes

    return result


def compute_pair_score(
    prices_a: np.ndarray,
    prices_b: np.ndarray,
    sym_a: str,
    sym_b: str,
) -> PairResult | None:
    """Score a pair for mean-reversion tradability."""
    n = min(len(prices_a), len(prices_b))
    if n < 15:
        return None

    p_a = prices_a[-n:]
    p_b = prices_b[-n:]

    # Returns correlation
    r_a = np.diff(p_a) / p_a[:-1]
    r_b = np.diff(p_b) / p_b[:-1]

    if np.std(r_a) < 1e-10 or np.std(r_b) < 1e-10:
        return None

    corr = np.corrcoef(r_a, r_b)[0, 1]
    if np.isnan(corr):
        return None

    # Hedge ratio via OLS: beta = cov(r_a, r_b) / var(r_b)
    beta = np.cov(r_a, r_b)[0, 1] / np.var(r_b) if np.var(r_b) > 0 else 1.0

    # Log-spread
    spread = np.log(p_a) - beta * np.log(p_b)

    # Spread autocorrelation (lag-1 of spread changes)
    d_spread = np.diff(spread)
    if len(d_spread) < 5 or np.std(d_spread) < 1e-10:
        return None
    autocorr = np.corrcoef(d_spread[:-1], d_spread[1:])[0, 1]
    if np.isnan(autocorr):
        return None

    # Half-life from AR(1) on spread
    spread_lag = spread[:-1]
    spread_now = spread[1:]
    if np.std(spread_lag) < 1e-10:
        return None
    phi = np.corrcoef(spread_lag, spread_now)[0, 1]
    if np.isnan(phi) or phi >= 1.0 or phi <= 0:
        return None
    half_life = -math.log(2) / math.log(phi)

    # Z-score range (how wide the spread oscillates)
    spread_std = np.std(spread)
    z_range = (np.max(spread) - np.min(spread)) / spread_std if spread_std > 0 else 0

    # Composite score:
    #   - High correlation (pair moves together) — 25%
    #   - Negative autocorrelation (spread reverts) — 30%
    #   - Short half-life (reverts fast) — 25%
    #   - Wide z-range (more trading opportunities) — 20%
    score = (
        abs(corr) * 0.25
        + max(-autocorr, 0) * 0.30
        + (1.0 / max(half_life, 0.1)) * 0.25
        + min(z_range / 10.0, 1.0) * 0.20
    )

    return PairResult(
        leg_a=sym_a,
        leg_b=sym_b,
        beta=beta,
        correlation=corr,
        half_life=half_life,
        autocorrelation=autocorr,
        z_range=z_range,
        spread_std=spread_std,
        score=score,
        sector_a=SYMBOL_TO_SECTOR.get(sym_a, "other"),
        sector_b=SYMBOL_TO_SECTOR.get(sym_b, "other"),
    )


def select_pairs(
    daily_closes: dict[str, np.ndarray],
    top_n: int = 4,
    max_per_sector_pair: int = 2,
) -> list[PairResult]:
    """Screen all pairs and select top N with sector diversification."""
    symbols = sorted(daily_closes.keys())
    results = []

    for i, sym_a in enumerate(symbols):
        for sym_b in symbols[i + 1:]:
            pair = compute_pair_score(
                daily_closes[sym_a], daily_closes[sym_b], sym_a, sym_b
            )
            if pair is not None and pair.half_life < 30 and pair.correlation > 0.2:
                results.append(pair)

    # Sort by score descending
    results.sort(key=lambda p: p.score, reverse=True)

    # Greedy selection with sector diversification
    selected = []
    sector_pair_count: dict[tuple[str, str], int] = {}

    for pair in results:
        if len(selected) >= top_n:
            break

        # Sector pair key (sorted for consistency)
        sp = tuple(sorted([pair.sector_a, pair.sector_b]))

        # Limit same-sector pairs
        if sector_pair_count.get(sp, 0) >= max_per_sector_pair:
            continue

        # Don't pick a symbol that's already in 2 pairs
        sym_count_a = sum(1 for s in selected if pair.leg_a in (s.leg_a, s.leg_b))
        sym_count_b = sum(1 for s in selected if pair.leg_b in (s.leg_a, s.leg_b))
        if sym_count_a >= 2 or sym_count_b >= 2:
            continue

        selected.append(pair)
        sector_pair_count[sp] = sector_pair_count.get(sp, 0) + 1

    return selected


def print_results(pairs: list[PairResult], verbose: bool = True):
    """Print pair selection results."""
    print(f"\n{'='*80}")
    print(f"  DAILY PAIR SELECTION — {datetime.now(ET).strftime('%Y-%m-%d %H:%M ET')}")
    print(f"  {len(pairs)} pairs selected")
    print(f"{'='*80}\n")

    print(f"  {'#':>2s}  {'Pair':<12s} {'Beta':>6s} {'Corr':>5s} {'AutoC':>6s} "
          f"{'HLife':>5s} {'ZRng':>5s} {'Score':>6s} {'Sectors'}")
    print(f"  {'—'*75}")

    for i, p in enumerate(pairs, 1):
        cross = "✓" if p.is_cross_sector else " "
        print(f"  {i:>2d}  {p.leg_a}/{p.leg_b:<7s} {p.beta:>6.2f} {p.correlation:>5.2f} "
              f"{p.autocorrelation:>+5.3f} {p.half_life:>5.1f}d {p.z_range:>5.1f} "
              f"{p.score:>6.3f} {p.sector_a}/{p.sector_b} {cross}")

    if verbose:
        print(f"\n  Symbols involved: {sorted(set(s for p in pairs for s in [p.leg_a, p.leg_b]))}")
        print(f"  Sectors covered: {sorted(set(s for p in pairs for s in [p.sector_a, p.sector_b]))}")


def main():
    parser = argparse.ArgumentParser(description="Daily Pair Selector")
    parser.add_argument("--top", type=int, default=4, help="Number of pairs to select")
    parser.add_argument("--days", type=int, default=30, help="Days of history for screening")
    parser.add_argument("--json", action="store_true", help="Output as JSON")
    parser.add_argument("--toml", action="store_true", help="Output as TOML snippet")
    parser.add_argument("--symbols", type=str, default=None,
                        help="Comma-separated symbols (default: full universe)")
    args = parser.parse_args()

    symbols = args.symbols.split(",") if args.symbols else SYMBOLS

    print(f"Fetching {args.days} days of daily closes for {len(symbols)} symbols...")
    closes = fetch_daily_closes(symbols, args.days)
    print(f"Got data for {len(closes)} symbols")

    n_pairs = len(closes) * (len(closes) - 1) // 2
    print(f"Screening {n_pairs} pairs...")

    pairs = select_pairs(closes, top_n=args.top)

    if args.json:
        print(json.dumps([p.to_dict() for p in pairs], indent=2))
    elif args.toml:
        for p in pairs:
            print(p.to_toml())
    else:
        print_results(pairs)
        print(f"\n  Run with --toml to get config snippet for openquant.toml")
        print(f"  Run with --json for machine-readable output")


if __name__ == "__main__":
    main()
