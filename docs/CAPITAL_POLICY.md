# Capital Allocation Policy — Pairs vs Stat-Arb Transition

## Overview

During the transition from pure pairs trading to multi-asset stat-arb, capital is allocated between the two strategies according to this policy.

## Initial Allocation (Paper Go-Live)

| Strategy | Risk Budget | Rationale |
|----------|-------------|-----------|
| Pairs Engine | 60% | Proven baseline, lower risk during transition |
| Stat-Arb Engine | 40% | New strategy, needs validation |

## Transition Schedule

| Milestone | Pairs | Stat-Arb | Trigger |
|-----------|-------|----------|---------|
| Paper go-live | 60% | 40% | Phase 2 complete, stat-arb engine functional |
| 3 months live | 40% | 60% | Stat-arb Sharpe ≥ 0.8 sustained, no major drawdown |
| 6 months live | 25% | 75% | Stat-arb outperforms pairs on risk-adjusted basis |
| Full transition | 0% | 100% | Pairs sleeve retired (optional, may keep for diversification) |

## Risk Budget Isolation

Each strategy has independent risk limits:

### Pairs Engine
- Max gross exposure: 2x NAV
- Max per-pair exposure: 10% of pairs budget
- Max sector concentration: 40% of pairs budget

### Stat-Arb Engine
- Max gross exposure: 2x NAV
- Max per-name exposure: 2% of stat-arb budget
- Max sector exposure: 25% of stat-arb budget
- Max net exposure drift: 5% (dollar-neutral target)

## Kill Switches

Both strategies share kill switches that trigger full flatten:

- Daily P&L < -3% of total NAV
- Realized volatility > 3x predicted
- PC-1 variance ratio > 80% (stat-arb specific)

## Review Cadence

- Weekly: exposure report, Sharpe comparison
- Monthly: allocation rebalance decision point
- Quarterly: full strategy review, potential allocation shift

## Exceptions

Human override required for:
- Any allocation change > 20% in a single decision
- Early transition (before 3-month milestone)
- Full retirement of pairs sleeve

---

*Approved by: [Pending human sign-off]*
*Effective date: [After Phase 2 paper go-live]*
