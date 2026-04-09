
### PHASE 1A RESULTS: November destroys January's best pair

**GOOG/GOOGL:**
- January: 372 trades, +2416 bps, 97% win — BEST PAIR
- November: 232 trades, -974 bps, 8% win — WORST PAIR

The spread reverts in both months. But in November the reversions
are tiny (3-5 bps gross). After 10 bps costs, every trade LOSES.
In January the reversions are 15-25 bps gross — enough to profit.

**ROOT CAUSE: GOOG/GOOGL spread volatility is regime-dependent.**
In volatile months (Nov), GOOG and GOOGL move in perfect lockstep
(they're the same company). The spread barely moves = no edge.
In calmer months (Jan), slight dislocations create tradeable spreads.

**IMPLICATION: We need a spread volatility filter.**
Before entering any pair, check: is the spread volatile enough to
overcome costs? If rolling spread_std is too small (< cost_bps equivalent),
skip the pair for that period.

Formula: only enter if expected_profit > 2 × cost_bps
  expected_profit ≈ entry_z × spread_std × 10000
  If entry_z=2.0 and spread_std=0.0003 → expected = 6 bps
  With cost=10 bps → NET = -4 bps → SKIP

This also explains why HBAN/KEY, CMS/LNT lost in Nov — the spread
std shrank during the volatile period, making costs > profits.

**PAIRS THAT MIGHT SURVIVE ALL MONTHS:**
- Pairs with CONSISTENTLY high spread volatility
- Not near-arbitrage pairs (GOOG/GOOGL, GLD/SGOL)
- Need: different enough to have 20+ bps spread moves
- But similar enough to mean-revert

This is a tension: too similar = tiny spread, costs kill.
Too different = spread trends, stops kill.
The sweet spot is pairs with 15-30 bps average spread deviation.
