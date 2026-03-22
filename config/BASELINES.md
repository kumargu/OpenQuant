# Trading Baselines

## Pairs Trading — Pre-fix Audit (2026-03-22)

**Commit**: main at 6a8d79a
**Config**: config/pairs.toml
**Pairs**: GLD/SLV (beta=0.37), GS/MS (beta=0.91), AMD/INTC (beta=0.65)
**Params**: entry_z=2.0, exit_z=0.25, stop_z=5.0, min_hold=20, max_hold=150, cost=3bps/leg

### In-sample (45 days: Jan 13 – Mar 20, 2026)

| Metric | Value |
|--------|-------|
| Trades | 733 |
| Win rate | 49.5% |
| Total P&L | $20,087 |
| Per day | **$446/day** |
| GLD/SLV | +$328/day (51.6% win, 26.8 avg bps) |
| GS/MS | +$36/day (46.3% win, 4.3 avg bps) |
| AMD/INTC | +$83/day (49.6% win, 6.9 avg bps) |

### Out-of-sample (181 days: Apr 2025 – Mar 2026)

| Metric | Value |
|--------|-------|
| Trades | 2,333 |
| Win rate | 38.3% |
| Total P&L | $5,156 |
| Per day | **$28/day** |
| GLD/SLV | +$33/day (33.7% win, 3.8 avg bps) — DEGRADED 10x |
| GS/MS | +$42/day (46.6% win, 8.4 avg bps) — CONSISTENT |
| AMD/INTC | **-$46/day** (38.1% win, -3.7 avg bps) — FLIPPED |

### Diagnosis

The $446/day was in-sample overfitting:
- AMD/INTC beta shifted sign over 2025 (beta_cv=1.037). Hardcoded beta=0.65 is wrong.
- GLD/SLV edge shrinks 10x on OOS data (beta=0.37 hardcoded vs 0.50 real)
- Only GS/MS is stable across both windows

### Pending fixes

- #161: DST timezone bug (entries/exits off by 1 hour during EDT)
- #162: Beta-weighted sizing (equal-dollar != beta-neutral)
- #160: Kelly clamp (forces trades at zero edge)
- Drop AMD/INTC, refresh beta via pair-picker daily

---

## Single-Symbol Trading (config/single.toml)

**Status**: NOT YET BASELINED (P&L tracker only covers pairs)
