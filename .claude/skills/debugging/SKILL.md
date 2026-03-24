---
name: debugging
description: Use when debugging strategy performance, analyzing trade logs, investigating why trades lose money, or diagnosing unexpected behavior in the trading system. Covers log-driven debugging, pattern analysis, and the iterative fix-commit-rerun cycle.
---

# Strategy Debugging

## Trigger

Activate when investigating trade losses, unexpected strategy behavior, performance regressions, or when asked to "figure out what's going wrong" with any trading component.

## Core Principle

**The numbers tell you WHAT is wrong. The logs tell you WHY.**

Always start with logs, not aggregate stats. Read individual trades before drawing conclusions.

## Debugging Workflow

### 1. Add logging if it doesn't exist
Every decision point needs a log line:
- Scan: what passed each filter, what was rejected and why
- Entry: z-score, beta, R², HL, ADF, prices, frozen stats
- Hold: per-bar fixed_z, rolling_z, drift, unrealized P&L
- Exit: reason, P&L, cost breakdown, leg returns

All logs go to `data/journal/walkforward.log` (append mode, persists across runs).

### 2. Deep dive a single pair first
```bash
python3 scripts/pair_deep_dive.py "FDX/UPS"
```
Read the per-bar evolution. Look for:
- fixed_z getting **worse** over the hold → spread is trending, not reverting
- Nonsensical parameters (negative beta, tiny spread_std, extreme z)
- Every trade hitting max_hold → reversion signal doesn't work for this pair
- Rolling z ≠ fixed z → drift is in play (the #182 bug pattern)

### 3. Pattern analysis across all pairs
```bash
python3 scripts/pattern_analysis.py
```
Opens `dashboards/patterns_dashboard.html` with:
- AC(1) per pair — negative = mean-reverting, positive = trending
- Day-of-week effects — which days have the biggest moves
- Holding period curves — win rate by hold duration
- Spread change distributions — fat tails = danger

### 4. Statistical analysis on the logs
After reading individual trades, compute aggregates:
- **Group by exit reason**: reversion exits vs max_hold vs stop_loss — what's the P&L split?
- **Group by direction**: SHORT vs LONG win rates
- **Group by entry |z|**: do higher z entries perform better or worse?
- **Group by pair**: which pairs are consistently profitable?
- **Group by time period**: did the strategy work in Q1 but fail in Q2?

### 5. Fix → Commit → Rerun → Compare
Each fix gets its own commit with before/after numbers in the message:
```
ab27884: Fix beta/std bugs — P&L improves -$5,164 → -$2,362
ebb68d1: Add quality gate — P&L improves -$2,362 → +$3,397
```
The commit history IS the experiment log.

## Common Bugs Found Through Logs

| Bug | Log Symptom | Fix |
|-----|-------------|-----|
| Negative beta | Entry log shows `beta=-0.46` | Guard `beta > 0.1` |
| Spread std too small | Entry z = -9.46, can never cross exit | Guard `spread_std > 0.005` |
| Beta unstable | Same pair shows beta 0.95 then 0.17 | Guard `beta_change < 30%` |
| Fake reversion (rolling z drift) | rolling_z decays while fixed_z stays extreme | Use frozen entry-time stats for exit |
| Earnings blowout | One leg moves 5-10% in a single bar | Earnings calendar blackout ±5 days |
| All trades hit max_hold | 0 reversion exits | The pair doesn't actually mean-revert |

## Key Learnings (see `docs/debugging_learnings.md`)

- **48/58 trades exiting via max_hold** was the first clue the strategy didn't work as designed
- **Negative beta** was invisible in aggregate stats but obvious in per-trade logs
- **Parameter sweeps should be on experiment branches**, not main
- **Pattern analysis (AC1) is more predictive than ADF** for identifying tradeable pairs
- **Stops hurt** in our strategy — they crystallize losses that would have recovered

## Log File Locations

| Log | Purpose |
|-----|---------|
| `data/journal/walkforward.log` | Walk-forward simulation + deep dive trades |
| `data/journal/patterns.log` | Pattern analysis runs |
| `data/journal/engine.log` | Live Rust engine logs |

## Dashboards

| Dashboard | Script |
|-----------|--------|
| `dashboards/walkforward_dashboard.html` | `scripts/daily_walkforward_dashboard.py` |
| `dashboards/patterns_dashboard.html` | `scripts/pattern_analysis.py` |
