# Priority Queue & Capital Allocation Research

Filed as GitHub issue #193. Key implementation references below.

## Core Architecture (from j0shk0/pairs-trading-system)

```
SCORING:   priority = |z_score| * spread_vol / half_life
SLOTS:     fixed N slots, equal capital per slot
ROTATION:  opportunity cost — replace profitable-but-stale trades with stronger signals
SIZING:    equal weight per slot → graduate to quarter-Kelly when enough data
OVERLAP:   no symbol appears in 2 active pairs simultaneously
```

## Opportunity Cost Rotation Logic

```python
for each active trade:
    unrealized = PnL / capital_allocated
    remaining  = expected_return - unrealized
    best_queue = top_queued_signal.expected_return

    if unrealized >= target:           # hit target
        CLOSE
    elif unrealized > 0 and remaining < best_queue - 2*cost:
        CLOSE and REPLACE              # better use of capital
    elif held > 2 * half_life:
        CLOSE                          # stale, reversion unlikely
```

## Scoring Approaches (by sophistication)

1. **Simple**: `abs(z_score)` descending
2. **Better**: `abs(z) * spread_vol / half_life` (speed-adjusted expected return)
3. **Best**: Avellaneda-Lee s-score: `-(m_bar / sigma_eq)` with kappa filtering

## Key GitHub Repos

- `j0shk0/pairs-trading-system` — opportunity-cost rotation
- `sebisto68/Kryptokraken` — SignalPriorityQueue + CapitalAllocator
- `aaravjj2/Apex-Terminal` — quality scoring + uncorrelated selection
- `kingwongf/statsArb` — Avellaneda-Lee s-score implementation
- `hudson-and-thames/arbitragelab` — production-grade (commercial)

## Position Sizing

- Start: equal weight per slot
- Quarter-Kelly: `f = 0.25 * (p*b - q) / b` (need 30+ trades per pair first)
- Vol-targeted: `size = target_vol / spread_vol * total_capital`

## Our Key Data Points

- Reversion exits: +1.51% avg, 100% win rate
- Max hold exits: -0.39% avg, 36% win rate
- Dynamic max_hold = 2 × half_life cuts losers faster
- Trade frequency: 1.32/day across 41 pairs
