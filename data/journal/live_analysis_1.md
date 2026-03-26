# Live Crypto Analysis — Session 1 (2026-03-26)

## Session Summary
- Duration: 8+ hours (10:16 — 18:25)
- BTC: $70,830 → $69,378 (-2.05%)
- ETH: $2,152 → $2,082 (-3.24%)
- Trades: 1 (SHORT at z=+0.81, exited at z=-0.08)
- P&L: $-0.09 (essentially zero)

## Problems Found

### 1. Stale data feed
Only 3 unique BTC prices and 2 unique ETH prices in 8 hours.
The dedup logic prevents adding bars with the same timestamp, but
the polling interval (30s) often returns the same minute bar.
The z-score is computed on stale data most of the time.

### 2. Fake reversion
The trade entered SHORT when z=+0.81 (BTC outperforming ETH).
Over 199 minutes, z dropped to -0.08 — looks like reversion.
But P&L was $-0.09 because:
- The z "reverted" because the ROLLING MEAN adapted, not because
  the spread actually returned to its original level
- This is the same fake reversion bug from issue #182
- The crypto trader uses rolling z-score (compute_spread_and_z),
  NOT frozen entry-time stats like the stock sim does

### 3. Market-wide move, not spread move  
Both BTC and ETH dropped together (-2% and -3%). The "signal" was
just noise from different drop speeds, not a real pair divergence.

## Fixes Needed
1. Use frozen entry-time mean/std for exit z (like ExitContext in Rust engine)
2. Fetch more historical bars per poll (5 min window, not 60s)
3. Consider using 5-min or 15-min bars instead of 1-min for less noise
4. Add data freshness check — warn if price hasn't changed in 5+ minutes
