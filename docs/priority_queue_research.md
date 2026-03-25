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

## Academic & Book References (the actual authority)

### Key Finding
No single book/paper solves the complete portfolio-of-pairs rotation problem.
The literature is pair-level focused. Portfolio-level is an open question (Krauss 2017).

### Priority Scoring Formula (synthesized from literature)
```
priority_i = |z_score_i| × sqrt(kappa_i) / sigma_spread_i
```
Sources: Avellaneda-Lee (z), Lee-Leung-Ning 2023 (kappa), risk parity (1/sigma)

### Opportunity Cost (Leung & Li 2015)
The discount rate r in OU optimal stopping = expected return of best alternative.
Higher r → pickier entry, faster exit. The marginal pair in the queue sets r.

### Allocation (Lee, Leung & Ning 2023 — MOST RELEVANT PAPER)
"A Diversification Framework for Multiple Pairs Trading Strategies"
Mean Reversion Budgeting (MRB): weight ∝ kappa × sigma × log-likelihood
Outperforms equal-weight in both returns and Sharpe.

### Kelly for Portfolio (Chan 2013, Ch. 6-8)
F = C^{-1} × M (multivariate Kelly = tangency portfolio × leverage)
Practically: quarter-Kelly, assume zero cross-pair correlation, 500-day window.
20x more sensitive to mean return errors than covariance errors (Chopra & Ziemba).

### Books Consulted
- Isichenko (2021) "Quantitative Portfolio Management" — full stat-arb pipeline
- Chan (2013) "Algorithmic Trading" Ch. 4, 8 — Kelly, portfolio sizing
- Chan (2008) "Quantitative Trading" Ch. 6 — Kelly criterion
- Vidyamurthy (2004) — pair-level only, no portfolio chapter
- Pole (2007) — "Popcorn Process" mental model, catastrophe detection
- Whistler (2004) — retail-oriented, light on math

### Papers Consulted
- Lee, Leung & Ning (2023) — MRB/MRR for multiple pairs
- Avellaneda & Lee (2010) — s-score, kappa>8.4 filter
- Leung & Li (2015) — optimal stopping with opportunity cost
- Gatev et al. (2006) — Return_committed = Return_employed × Utilization
- Grinold & Kahn (1999) — IR = IC × sqrt(BR) × TC
- Krauss (2017) — survey, identifies portfolio-level as open question
- Chopra & Ziemba (1993) — sensitivity of optimization to estimation errors
