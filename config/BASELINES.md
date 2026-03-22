# Trading Baselines

## Pairs Trading (config/pairs.toml)

**Date**: 2026-03-22
**Commit**: 1f36d7e
**Data**: 48 bar files, 45 symbols, Jan 13 - Mar 20 2026

### Config
- Pairs: GLD/SLV (beta=0.37), GS/MS (beta=0.91), AMD/INTC (beta=0.65)
- entry_z=2.0, exit_z=0.25, stop_z=5.0
- min_hold_bars=20, max_hold_bars=150
- last_entry_hour=14 (no entries after 14:00 ET)
- force_close_minute=930 (close at 15:30 ET)
- cost=3 bps/leg (12 bps round-trip)
- notional=$10K/leg

### Results
| Metric | Value |
|--------|-------|
| Trades | 733 |
| Win rate | 49.5% |
| Total P&L | $20,087 |
| Per day | **$446/day** |
| Trading days | 45 |
| Per pair | GLD/SLV +$307, GS/MS +$34, AMD/INTC +$78 |

### Key invariants
- Deterministic: 5 runs produce identical results
- Zero overnight trades (entry cutoff works)
- 3-4 stop losses total (rare)
- Reversion exits are net positive

---

## Single-Symbol Trading (config/single.toml)

**Date**: 2026-03-22
**Commit**: 1f36d7e
**Status**: NOT YET BASELINED (P&L tracker only covers pairs)

### Config
- mode=single, max_bar_age=0 (backtesting)
- 45 symbols, mean-reversion + momentum + VWAP combiner
- See openquant.toml [signal], [momentum], [combiner] sections

### Results
- 33,990 intents generated across 45 symbols
- P&L not tracked (needs single-symbol P&L tracker)
- Previous estimate: ~$22/day (from sim_49day_results.json, unvalidated)

### TODO
- Add single-symbol P&L tracking to runner
- Validate against known good results
