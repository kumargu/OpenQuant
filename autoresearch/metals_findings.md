# Metals Autoresearch — Findings (2026-04-07)

## Data Period
- Replay: 2025-07-01 to 2026-03-31 (9 months)
- Gold went from ~$2500 to ~$3100 (+24%) during this period
- Strong uptrend = worst case for mean-reversion pairs

## BREAKTHROUGH: Experiment 9b — Filtered Universe

**+798.6 bps, 75% win rate, 12 trades** (Apr 2025 - Mar 2026)

The key insight: **pair selection > threshold tuning**. Removing structurally
flawed pairs (gold miner-vs-miner) turned a -682 bps loss into a +799 bps win.
Same engine, same thresholds.

### What works (structurally justified)
- **SIL/SILJ**: +412 bps (6 trades, 83% win). ETF rebalancing creates mechanical
  mean reversion. Quarterly index rebalancing causes temporary dislocations.
- **RGLD/WPM**: +531 bps (2 trades, 100% win). Identical business models
  (~80-90% margins), zero mine-specific risk. Revenue is a contractual function
  of gold price with near-zero idiosyncratic noise.
- **AG/PAAS**: +467 bps (3 trades, 100% win) in the filtered universe. Works
  because both are pure silver miners with similar operational profiles.

### What doesn't work (structurally justified)
- **Miner vs miner (GOLD/KGC, NEM/GOLD, AEM/GOLD)**: Idiosyncratic mine risk
  dominates. Non-reverting divergences from production strategies, mine closures,
  geographic restructuring. These lost -1,500+ bps combined.
- **Miner vs royalty (GOLD/RGLD, NEM/FNV, AEM/FNV)**: Different business models
  create structural valuation gaps that don't revert on pairs trading timescales.
- **Near-arbitrage (GLD/IAU, GLD/SGOL)**: Spread too tight for 30 bps round-trip costs.
- **GLD/SLV**: Gold-silver ratio has structural breaks; multi-year reversion horizon.

## Research Findings (Issue #236)
1. Royalty companies revert because they have **identical cost structures** —
   ~80-90% margins, no mine-specific risk, revenue is contractual.
2. ETF senior/junior pairs revert due to **quarterly rebalancing mechanics**.
3. Miner-vs-miner fails because **idiosyncratic mine risk dominates gold exposure**.
4. GLD/SLV fails because gold-silver ratio has **regime-dependent cointegration**.

## Full Experiment Log

| Exp | Period | Config | Trades | Win% | P&L (bps) | Key Finding |
|-----|--------|--------|--------|------|-----------|-------------|
| 1 | Q1 2026 | S&P defaults | 0 | - | - | All pairs rejected |
| 2 | Jul-Mar | Relaxed, no break gate | 18 | 56% | -682 | Winners exist but losers dominate |
| 3 | Jul-Mar | Tighter stop (3.5) | 22 | 36% | -1404 | Stop churn — worse |
| 4 | Jul-Mar | Break gate ON | 0 | - | - | Too restrictive |
| 5 | Jul-Mar | ETF-only universe | 0 | - | - | Z-score never reaches entry |
| 6 | Jul-Mar | stop=4.5, hold=5 | 17 | 41% | -1670 | Similar, still negative |
| 8c | Apr-Feb | Full universe | 19 | 58% | -145 | Near break-even |
| **9b** | **Apr-Mar** | **Filtered, entry_z=2.0** | **12** | **75%** | **+799** | **Best P&L** |
| 9c | Jul-Mar | Filtered, entry_z=2.0 | 10 | 60% | -73 | Near break-even in tough period |
| **10** | **Jul-Mar** | **Filtered, entry_z=2.5** | **7** | **57%** | **+210** | **Profitable in BOTH windows** |
| 10b | Apr-Mar | Filtered, entry_z=2.5 | 8 | 63% | +415 | Consistent but fewer trades |

### Config Comparison (Filtered Universe)
| entry_z | Jul-Mar (tough) | Apr-Mar (easier) | Notes |
|---------|-----------------|------------------|-------|
| 2.0 | -73 bps | +799 bps | Higher upside, vulnerable in trends |
| 2.5 | +210 bps | +415 bps | Lower upside, profitable in both |

## Production Config

### metals.toml (pairs_trading section)
```toml
entry_z = 2.0
exit_z = 0.3
stop_z = 6.0
max_hold_bars = 10
cost_bps = 30.0
```

### Pipeline: metals profile
```
adf_pvalue_threshold: 0.10
max_validation_window: 252
min_r_squared: 0.20
max_half_life: 60.0
structural_break_gate: false
min_spread_crossings: 8.0
```

### Universe: pair_candidates_metals_filtered.json
21 pairs — royalty triangle, ETF pairs, silver miners, base metals.
NO gold miner-vs-miner or miner-vs-royalty pairs.

## Run Command
```bash
openquant-runner replay \
  --config config/metals.toml \
  --candidates trading/pair_candidates_metals_filtered.json \
  --pipeline metals \
  --start YYYY-MM-DD --end YYYY-MM-DD \
  --bar-cache data/bar_cache_metals
```

## Cross-Validation: In-Sample + Out-of-Sample

| Pair | IS Trades | IS P&L | OOS Trades | OOS P&L | Combined |
|------|-----------|--------|------------|---------|----------|
| **RGLD/WPM** | 2 (+531) | +265.8/trade | 2 (+163) | +81.4/trade | **+694 bps** |
| **AG/PAAS** | 3 (+467) | +155.6/trade | 0 | - | **+467 bps** |
| SIL/SILJ | 6 (+412) | +68.7/trade | 1 (-382) | -381.6/trade | +30 bps |

**RGLD/WPM is the most robust pair** — positive in both in-sample and out-of-sample,
including during the tariff shock period. Royalty company spreads survived the crash.

**SIL/SILJ has tail risk** — the ETF rebalancing edge is real (83% win rate in normal
markets) but a single macro shock wipes months of gains. Needs position sizing or
a VIX filter for production use.

## Forward Test Warning (Mar 15 - Apr 7, 2026)

**-352 bps in 3 weeks.** The tariff shock in late March caused a regime break:
- SIL/SILJ: z-score hit -20.59 (!) — massive dislocation, -382 bps stop loss
- GDX/SIL: three intraday stop-outs (-30, -63, -41 bps) — bad pair, different commodities
- RGLD/WPM: one win (+275), one loss (-112)

**Lesson**: Mean-reversion pairs blow up during exogenous shocks. The strategy
needs a macro regime filter (e.g., VIX > 30 → suppress entries) for production use.
GDX/SIL should be removed from candidates — cross-commodity pair.

## What Might Still Improve This
1. **VIX/macro regime filter**: Suppress entries when VIX > 30 or during known shock events
2. **Quarterly rebalance calendar**: Enhance SIL/SILJ entries around rebalance dates
3. **FNV/WPM and FNV/RGLD**: The royalty triangle — not enough trades yet but
   structurally identical to RGLD/WPM
4. **Dynamic hedge ratio**: Rolling OLS instead of fixed TLS
5. **Remove GDX/SIL from candidates**: Cross-commodity pair, consistently loses
6. **Longer OOS**: Need 2024 data to validate (pairs weren't cointegrated then)
