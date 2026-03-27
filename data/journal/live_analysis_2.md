# Live Trade Analysis — Day 2 (2026-03-27)

## Results
- COIN/PYPL LONG: -$21 (still open, z barely moved)
- PNC/USB SHORT: -$580 (PNC gapped +12% overnight — NEWS EVENT)
- HD/LOW LONG: +$151 (REVERTED! But we missed exit — runner was stopped)
- **NET: -$450**

## What the pair picker got right
- 13 out of 18 signals would have been winners (72% hit rate)
- HD/LOW correctly identified as LONG and reverted (+$151)
- Priority scoring ranked PNC/USB #2 — correct by the formula

## What went wrong

### 1. PNC/USB: News event destroyed the short (-$580)
PNC gapped +12% overnight. Our earnings calendar doesn't cover PNC.
The S&P 500 expansion added pairs from symbols NOT in the calendar.
FIX: Extend earnings calendar to all S&P 500 symbols, or add a
"no overnight hold for uncovered symbols" rule.

### 2. We missed the HD/LOW exit (+$151 unrealized)
z reverted from -1.70 to +0.37 (past exit threshold of ±0.2).
The runner was stopped overnight so no exit was triggered.
FIX: Either keep runner running, or place bracket orders at entry.

### 3. Priority scoring picked PNC (#2) over better alternatives
The scoring ranked PNC/USB at priority=131.9, but CRM/PLTR (prio=18.5)
would have made +$355. NOC/WM (prio=19.1) would have made +$122.
The priority formula favors high R² + high kappa over high |z|.
PNC had R²=0.979 (excellent stats) but was vulnerable to news.

### 4. Optimal picks (hindsight): CRM/PLTR + LRCX/MU + NOC/WM = +$699
vs what we picked: COIN/PYPL + PNC/USB + HD/LOW = -$30 (excl overnight gap)

## Pair Picker Improvements

1. **Earnings coverage**: Must cover ALL traded symbols, not just 52
2. **News/event risk**: Add overnight gap risk filter — pairs with
   upcoming catalysts should get lower priority or be excluded
3. **Diversification**: We picked 2 financial pairs (COIN/PYPL, PNC/USB).
   Should enforce sector diversity — max 1 pair per sector
4. **Keep runner live**: The HD/LOW exit was the right trade — we just
   weren't running to capture it
5. **Priority score lacks news risk**: High R² doesn't protect against
   earnings gaps. Consider adding a "news risk" penalty
