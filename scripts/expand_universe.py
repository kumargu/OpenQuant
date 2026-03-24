#!/usr/bin/env python3
"""
Expand the pairs trading universe to S&P 500.

Pipeline:
1. Load S&P 500 symbols with sector tags
2. Fetch daily prices via Alpaca (if not cached)
3. Generate all same-sector pairs
4. Run pattern analysis (AC1, R², holding curves)
5. Select top diversified pairs (no symbol overlap)
6. Run walk-forward simulation
7. Output results + updated candidate files
"""

import json
import math
import sys
import time
from collections import Counter
from pathlib import Path

root = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(root / "scripts"))

# S&P 500 sectors — will be populated by researcher or loaded from file
SP500_SECTORS_FILE = root / "data" / "sp500_sectors.json"
PRICES_FILE = root / "data" / "pair_picker_prices.json"
EXPANDED_CANDIDATES = root / "trading" / "pair_candidates_sp500.json"
DIVERSIFIED_CANDIDATES = root / "trading" / "pair_candidates_diversified.json"


def load_sectors():
    """Load S&P 500 sector mapping."""
    if not SP500_SECTORS_FILE.exists():
        print(f"ERROR: {SP500_SECTORS_FILE} not found. Run the researcher first.")
        sys.exit(1)
    with open(SP500_SECTORS_FILE) as f:
        return json.load(f)


def fetch_prices(symbols, days=350):
    """Fetch prices for all symbols via Alpaca."""
    # Check what we already have
    existing = {}
    if PRICES_FILE.exists():
        with open(PRICES_FILE) as f:
            existing = json.load(f)

    need_fetch = [s for s in symbols if s not in existing or len(existing[s]) < 200]
    have = [s for s in symbols if s in existing and len(existing[s]) >= 200]

    print(f"Symbols: {len(symbols)} total, {len(have)} cached, {len(need_fetch)} to fetch")

    if not need_fetch:
        print("All symbols already cached!")
        return existing

    # Fetch in batches using our existing infrastructure
    import subprocess
    # Add new symbols to fetch list and call the fetcher
    all_symbols = list(set(list(existing.keys()) + symbols))

    # Write temporary symbol list
    sym_file = root / "data" / "fetch_symbols.json"
    with open(sym_file, 'w') as f:
        json.dump(all_symbols, f)

    print(f"Fetching {len(need_fetch)} new symbols via Alpaca...")
    result = subprocess.run(
        [sys.executable, "-m", "paper_trading.fetch_pair_prices",
         "--days", str(days), "--symbols"] + need_fetch,
        cwd=str(root), capture_output=True, text=True, timeout=600
    )
    print(result.stdout[-500:] if result.stdout else "")
    if result.returncode != 0:
        print(f"WARN: fetch returned {result.returncode}")
        print(result.stderr[-500:] if result.stderr else "")

    # Reload prices
    if PRICES_FILE.exists():
        with open(PRICES_FILE) as f:
            return json.load(f)
    return existing


def generate_pairs(sectors, prices):
    """Generate all same-sector pairs where both symbols have price data."""
    pairs = []
    for sector, symbols in sectors.items():
        available = sorted([s for s in symbols if s in prices and len(prices[s]) >= 200])
        for i in range(len(available)):
            for j in range(i + 1, len(available)):
                pairs.append({
                    "leg_a": available[i],
                    "leg_b": available[j],
                    "economic_rationale": f"S&P 500 same sector: {sector}"
                })
    return pairs


def run_pattern_analysis(pairs, prices):
    """Score all pairs by autocorrelation and quality metrics."""
    from pattern_analysis import analyze_pair
    total_bars = min(len(v) for v in prices.values())

    results = []
    total = len(pairs)
    for idx, p in enumerate(pairs):
        if idx % 100 == 0:
            print(f"  Analyzing pair {idx}/{total}...")
        r = analyze_pair(p['leg_a'], p['leg_b'], prices, total_bars)
        if r and r['autocorr_1'] < 0:  # only mean-reverting
            best_wr = max((r['hold_stats'].get(h, {}).get('win_rate', 0) for h in range(1, 8)), default=0)
            score = (-r['autocorr_1'] * 3) + (r['avg_r2'] * 1) + (best_wr * 2) - (r['beta_cv'] * 0.5)
            results.append({
                'leg_a': p['leg_a'], 'leg_b': p['leg_b'],
                'pair': r['pair'], 'score': score,
                'ac1': r['autocorr_1'], 'r2': r['avg_r2'],
                'beta_cv': r['beta_cv'], 'best_wr': best_wr,
                'sector': p['economic_rationale'],
            })

    results.sort(key=lambda x: x['score'], reverse=True)
    return results


def select_diversified(results, max_pairs=40):
    """Greedy selection: best pair first, then next best with no symbol overlap."""
    used = set()
    selected = []
    for r in results:
        if r['leg_a'] not in used and r['leg_b'] not in used:
            selected.append(r)
            used.add(r['leg_a'])
            used.add(r['leg_b'])
        if len(selected) >= max_pairs:
            break
    return selected


def main():
    print("=" * 60)
    print("UNIVERSE EXPANSION — S&P 500")
    print("=" * 60)

    # Step 1: Load sectors
    sectors = load_sectors()
    all_symbols = []
    for syms in sectors.values():
        all_symbols.extend(syms)
    all_symbols = sorted(set(all_symbols))
    print(f"\n1. Loaded {len(all_symbols)} S&P 500 symbols across {len(sectors)} sectors")
    for sector, syms in sorted(sectors.items(), key=lambda x: -len(x[1])):
        print(f"   {sector}: {len(syms)} symbols")

    # Step 2: Fetch prices
    print(f"\n2. Fetching prices...")
    prices = fetch_prices(all_symbols)
    available = [s for s in all_symbols if s in prices and len(prices[s]) >= 200]
    print(f"   {len(available)}/{len(all_symbols)} symbols have sufficient price data")

    # Step 3: Generate pairs
    print(f"\n3. Generating same-sector pairs...")
    pairs = generate_pairs(sectors, prices)
    print(f"   {len(pairs)} same-sector pairs generated")

    # Save full pair list
    with open(EXPANDED_CANDIDATES, 'w') as f:
        json.dump({"pairs": pairs}, f, indent=2)

    # Step 4: Pattern analysis
    print(f"\n4. Running pattern analysis on {len(pairs)} pairs...")
    t0 = time.time()
    results = run_pattern_analysis(pairs, prices)
    elapsed = time.time() - t0
    print(f"   {len(results)} mean-reverting pairs found in {elapsed:.0f}s")
    print(f"   Top 10 by score:")
    for r in results[:10]:
        print(f"     {r['pair']:<15} score={r['score']:.2f} AC1={r['ac1']:+.3f} R²={r['r2']:.3f} WR={r['best_wr']*100:.0f}%")

    # Step 5: Select diversified
    print(f"\n5. Selecting diversified pairs (no symbol overlap)...")
    selected = select_diversified(results, max_pairs=40)
    print(f"   {len(selected)} diversified pairs, {len(selected)*2} unique symbols")
    for i, r in enumerate(selected):
        print(f"   {i+1:>2}. {r['pair']:<15} score={r['score']:.2f} AC1={r['ac1']:+.3f}")

    # Save diversified list
    div_pairs = [{"leg_a": r['leg_a'], "leg_b": r['leg_b'],
                  "economic_rationale": r['sector']} for r in selected]
    with open(DIVERSIFIED_CANDIDATES, 'w') as f:
        json.dump({"pairs": div_pairs}, f, indent=2)

    # Step 6: Quick walk-forward test
    print(f"\n6. Running walk-forward simulation...")
    import daily_walkforward_dashboard as d
    d.CAPITAL_PER_LEG = 500
    d.MAX_PAIRS = len(selected)
    d.ENTRY_Z = 1.0
    d.ENTRY_Z_CAP = 2.5
    d.MIN_R2_ENTRY = 0.70
    d.EXIT_Z = 0.3

    cands = [(r['leg_a'], r['leg_b']) for r in selected]
    days, trades = d.run_simulation(prices, cands)

    total_bars = min(len(v) for v in prices.values())
    last14 = total_bars - 10  # ~2 weeks
    recent = [t for t in trades if t.exit_day >= last14]
    r_pnl = sum(t.pnl_usd for t in recent)
    r_wins = sum(1 for t in recent if t.pnl_usd > 0)
    slots = [day.n_open for day in days if day.day_idx >= last14]

    print(f"\n{'='*60}")
    print(f"RESULTS — {len(selected)} diversified S&P 500 pairs")
    print(f"{'='*60}")
    print(f"ALL TIME: {len(trades)} trades, ${sum(t.pnl_usd for t in trades):+,.0f}")
    print(f"LAST 2 WEEKS: {len(recent)} trades, {r_wins} wins, ${r_pnl:+,.0f}")
    if slots:
        print(f"Avg slots: {sum(slots)/len(slots):.1f}, Max: {max(slots)}, "
              f"Idle: {sum(1 for s in slots if s==0)/len(slots)*100:.0f}%")


if __name__ == "__main__":
    main()
