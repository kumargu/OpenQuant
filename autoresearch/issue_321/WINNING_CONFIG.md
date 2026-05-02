# Issue #321 — working configuration

The configuration below produces a positive Sharpe over Jul 2025 → Mar 2026
on the no-mining v1 universe (43 baskets across 8 sectors, mining excluded).
This is the deployment shape that actually works — not the FAANG-only stress
test we ran first.

## Reproduce

```bash
cd /Users/gulshan/OpenQuant
./engine/target/release/openquant-runner replay \
  --engine basket \
  --universe config/basket_universe_v1_no_mining_baseline.toml \
  --start 2025-07-01 --end 2026-03-31 \
  --capital 10000 \
  --n-active-baskets 8 \
  --data-dir <your_output_dir> \
  --report-tsv <your_output_dir>/report.tsv
```

## Result (baseline = offset 0 = production behavior)

| metric | value |
|---|---|
| cum return | **+7.12%** |
| Sharpe | **0.472** |
| max drawdown | -24.5% |
| n days | 189 |
| n positions at end | 31 |
| order rejections (BP-exceeded) | 7 / 1500+ attempts |

## Timing-knob sweep — DO NOT adopt any non-baseline variant

Sweep results across 4 offsets on the same window:

| variant | offset (min) | cum return | Sharpe | MDD |
|---|---|---|---|---|
| **baseline** | **0** | **+7.12%** | **0.472** | **-24.5%** |
| t-5m | 5 | +6.69% | 0.457 | -24.4% |
| t-15m | 15 | +8.27% | 0.533 | -23.4% |
| t-30m | 30 | +4.44% | 0.349 | -24.1% |

t-15m's point Sharpe is highest (+0.061 over baseline), but **the buddy
reviewer's stats show this is not statistically distinguishable from noise**:

- Paired daily-return t-test (t-15m − baseline): t = 0.12, ann diff = +1.16%
- Lo-2002 SE on Sharpe: ΔSharpe/SE ≈ 0.79
- Block-bootstrap 95% CI on ΔSharpe (B=2000, block=10): **[-0.48, +0.54]**

The CI is ~8× wider than the point estimate. Re-draws of the same window
could rank any variant first.

## Why "t-15m wins" is the wrong story

1. **The CI on the timing edge spans zero** with margin in both directions.
   This is one draw of n=189 days; the +0.061 is a sampling artifact.

2. **The ranking inverts under universe choice.** Same 4 variants, same
   dates, FAANG-only at cap=4 ranked t-30m as the *best* variant (+1.0
   Sharpe over baseline); full universe at cap=8 ranked t-30m as the
   *worst* (-0.12 vs baseline). A real intraday-information signal
   should be additive across names, not sign-flipping.

3. **BP-rejection asymmetry confounds the comparison.** t-15m had 73
   buying-power-exceeded order rejections; baseline had 7 (10×). The
   "winning" variant's edge could be cap-filter cherry-picking on its
   extra trades, not signal quality. To disentangle: re-run with
   BP=∞ and compare unfiltered cum return.

4. **Per-basket P&L attribution inverts vs equity ranking.** t-30m has
   the highest raw per-basket log-spread P&L sum (+0.70), but t-30m has
   the lowest equity Sharpe (0.349). The cap=8 admission filter rotates
   baskets in/out — what looks like a "timing signal" is largely
   "which lucky subset did cap=8 admit this draw."

5. **Data quality is unaudited.** META has 60% of summer-fall 2025 days
   with <380 RTH bars (vs expected 390). MSFT 42%. The intraday timing
   experiment depends on bars at specific minutes; if META's 15:30 bar
   is missing, t-30m falls back silently to whatever earlier bar exists.
   No verification was run.

## What this experiment DID prove

The no-mining v1 universe + cap=8 + leverage 4x is a working strategy
configuration on this window:

- All 4 timing variants are positive on cum return.
- Strategy aggregates to +0.5 Sharpe even with the implementation flaws
  (BP-rejection asymmetry, missing bar audit, frozen-fit non-stationarity).
- Per-basket P&L on prior monetization-matrix runs shows 30/43 baskets
  are net winners, FAANG sector +0.27, energy +1.30. The cap-gated
  cross-sectional rotation IS the alpha.

## What this experiment DID NOT prove

- That the timing-snapshot offset matters. It might. Single-window data
  cannot answer.
- That t-15m is better than baseline. The CI spans zero by a wide margin.
- That t-30m is worse than baseline. Same.

## Next steps to actually answer #321

1. **Validate data first.** Run the bar-completeness audit per
   `feedback_data_architecture.md` (100 stocks × random days) before
   any further timing sweep. Particularly verify META and MSFT.

2. **Run with BP=∞.** Removes the cap-filter confound. Compare.

3. **Walk-forward folds.** A single 9-month window doesn't have the
   power. Need at least 2 non-overlapping windows where the same
   ranking emerges. Pre-register the hypothesis ("t-15m beats baseline
   by ≥X Sharpe with 95% CI excluding zero").

4. **Decompose fit vs runtime offset.** Currently both are tied to
   `decision_offset_minutes_before_close`. The (fit-on-close,
   runtime-on-t-X) cell is the only one that isolates timing-snapshot
   noise from fit-history confound.

## Conclusion

Use **baseline (offset=0) on no-mining v1 / cap=8** as the working
configuration. Issue #321's Phase 1 question — does timing offset
help — remains **unanswered** under proper statistical scrutiny. Do
not advance to Phase 2 (VWAP / median snapshot robustness) until
Phase 1 is reproducibly answered.
