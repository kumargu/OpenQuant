# Issue #321 — session summary (2026-05-02 → 2026-05-03)

Checkpoint of findings from a full-day investigation that started at issue #321
(Phase 1 timing-snapshot experiment) and walked through to a much bigger
question: **why is the strategy's Sharpe so low, and how do we get it to
production-quant levels?**

This document is the "where we landed" memory, separate from the per-experiment
notebook entries in `NOTEBOOK_basket.md`.

---

## What we shipped (real, durable)

1. **PR #322 merged** — replay session-close race fix. `BarDrivenSessionTrigger`
   gets a back-channel ack so the bar emitter can't overwrite `SharedCloses`
   while `process_session_close` is still running. Eliminates ~$15 cash drift
   between identical replay runs that used to look like real strategy
   variation. Non-controversial determinism fix.

2. **`[runner].decision_offset_minutes_before_close` TOML knob** — added to
   `basket_picker::Universe` schema. Defaults to 0 = byte-identical to
   production behavior. Plumbed through `parquet_bar_source` (drops bars past
   cutoff at emission time) AND `basket_fits` (warmup loader uses same offset
   so fit/runtime are consistent). Live path is unchanged.

3. **Cherry-picked determinism fix `9395e22d`** onto our branch — `HashMap →
   BTreeMap` in float-sum hot paths. Replay was non-deterministic by ~1%
   Sharpe between re-runs of identical configs. Now bit-exact.

---

## What we learned

### Issue #321 itself: timing knob is not a real signal under proper stats

- 4-variant sweep on no-mining v1 / cap=8 / 9 months: baseline 7.12% / Sharpe
  0.472, t-5m 6.69% / 0.457, t-15m 8.27% / 0.533 (point-best), t-30m 4.44% /
  0.349.
- Buddy reviewer ran block-bootstrap on ΔSharpe between every pair: **all 95%
  CIs span zero with width ~1.0**. t-test |t| ≈ 0.79.
- Same 4 variants on FAANG-only universe ranked t-30m as the *best* (+1.0
  Sharpe over baseline) — universe-flip is consistent with the timing knob
  being noise, not a real intraday signal.
- **Recommendation: ship baseline (offset=0). Don't deploy any non-baseline
  variant.** Phase 1 question remains unanswered without walk-forward
  validation on multiple windows.

### The bigger question we accidentally answered

**The strategy works — on its intended deployment shape, not on FAANG-only.**

| config | cum return | Sharpe |
|---|---|---|
| FAANG-only / cap=4 / 4x lev | -36.8% | -2.40 (lost money) |
| no-mining v1 / cap=8 / 4x lev | +7.1% | 0.472 (works) |

FAANG-only at cap=4 was a degenerate stress case — 4 always-on baskets in a
single trending sector with no cross-sectional rotation. The strategy needs
the cap-gated rotation across many sectors to extract its alpha.

### Drawdown forensics on the working baseline

- 96% of days underwater (181 of 189). Only 5 distinct drawdown episodes but
  the first two (Jul-Oct, Oct-Jan) account for most pain.
- 51% positive days / 49% negative days. Win days total +$11,260; loss days
  total -$10,548; net +$712. **Coin flip with a tiny edge.**
- **9 of 10 worst days are dominated by 2 sectors: hc_providers (5 days) and
  chips (4 days).** Six specific basket-targets (MOH, HUM, AMD, NVDA, MU,
  ADI) keep showing up in worst-3 contributors.
- Pattern: **targets that trended hardest against their peer mean are the
  strategy's worst performers.** AMD behaves "normally" → +0.87 P&L. NVDA
  underperformed peers by 60% → -0.54 P&L. MU rocketed +321% → strategy
  was short → -0.37 P&L.

### Per-basket Sharpe ranking (active days, 9 months)

Top 5: TFC +3.50, AMD +2.03, COP +1.93, PNC +1.82, CI +1.62.
Bottom 5: HBAN -2.07, NVDA -2.05, GOOGL -1.93, SO -1.33, PGR -1.25.

**Chips alpha exists, it's just specific.** AMD wins big, NVDA loses
big — same sector, opposite outcomes. So "drop chips" is wrong; "drop
NVDA/MU/ADI from chips" is right.

### Trimmed universe (in-sample) — proves alpha headroom exists

Drop the 10 worst-Sharpe baskets (Sharpe < -1.0) → re-run same 9 months:

| metric | baseline (43) | trimmed (33) | Δ |
|---|---|---|---|
| cum return | +7.12% | **+36.68%** | +29.6 pp |
| Sharpe | 0.472 | **1.417** | +0.95 |
| max drawdown | -24.5% | **-27.9%** | -3.4 pp (worse) |
| daily σ | $158 | $194 | +23% |

**+0.95 Sharpe lift in-sample.** This is the upper bound of what
universe-pruning alone can deliver. It tells us **alpha headroom is real**
— the strategy isn't fundamentally limited at Sharpe 0.5.

But: 7 of 10 worst days are SHARED between baseline and trimmed. Trimming
didn't fix the structural Jul-Oct 2025 drawdown — same regime events still
hurt. And concentration created NEW bad days (Feb 6 -$772 was the
trimmed-only worst day).

---

## What we did NOT prove

- **OOS performance of the trimmed universe.** This was in-sample selection.
  User explicitly chose to skip 2024 walk-forward (correct call given how
  hard 2025 already was).
- **That the timing knob has any signal.** Buddy review showed CIs span
  zero.
- **Data quality.** META has 60% of summer 2025 days with <380 RTH bars;
  MSFT 42%. We never ran the audit our own architecture rule says to do
  every loop.
- **Whether Sharpe 1.42 generalizes.** The proof would be: a theory-driven
  rule that produces the same drop list without using any out-of-sample
  P&L information.

---

## What's queued (in priority order)

1. **Data validation audit.** 100 random (symbol, day) pairs, parquet vs
   Alpaca, flag price divergences > $0.01 + bar-completeness anomalies. The
   META/MSFT gaps could be silently distorting the per-basket Sharpe ranking
   we used to build the drop list.

2. **Stationarity gate** at basket-picker admission time. ADF (or KPSS)
   test on the spread residual over the residual_window_days. Reject
   baskets where the spread is integrated/trending. **Hypothesis:** this
   should reject roughly the same baskets we hand-picked in-sample
   (NVDA, GOOGL, META, MU, etc all visibly non-stationary on the chart).
   If the gate's reject list overlaps significantly with our drop list,
   that's evidence the rule generalizes — much stronger than in-sample
   trimming.

3. **Volatility-targeted per-basket sizing.** Trimming alone increased the
   MDD because cap=8 with fewer baskets concentrates risk. Standard quant
   fix is to scale per-basket allocation inversely with realized
   basket-spread σ. High-σ baskets (chips, hc_providers) get less capital;
   low-σ baskets (banks_regional) get more. Same gross, dramatically less
   variance.

4. **Investigate the order_fail asymmetry.** Baseline 7 BP rejections,
   trimmed 70. Cap=8 may be too tight at capital=$10k × leverage=4x.

---

## Files / artifacts

- `autoresearch/NOTEBOOK_basket.md` — chronological experiment log (most
  recent: full-universe sweep entry, FAANG sweep entry).
- `autoresearch/issue_321/WINNING_CONFIG.md` — the recommended ship config
  + comprehensive caveats.
- `autoresearch/issue_321/dashboard/aapl_basket_human_walk.md` — both my
  chart-walk and the buddy reviewer's independent walk on the AAPL basket.
- `autoresearch/issue_321/dashboard/*.png` — 9 basket Layer-1 dashboards
  (price + log-spread + thresholds for each sector representative).
- `config/basket_universe_v1_no_mining_*.toml` — 4 timing variants of
  the working universe (baseline, t5m, t15m, t30m).
- `config/basket_universe_v1_trimmed.toml` — in-sample-trimmed variant
  with the 10 worst-Sharpe targets dropped (for reproducibility of the
  +1.42 Sharpe number).
- `config/basket_universe_v1_faang_*.toml` — FAANG-only variants
  (research-only, NOT a deployable config).

---

## How to reproduce the headline number

```bash
cd /Users/gulshan/OpenQuant
./engine/target/release/openquant-runner replay \
  --engine basket \
  --universe config/basket_universe_v1_no_mining_baseline.toml \
  --start 2025-07-01 --end 2026-03-31 \
  --capital 10000 \
  --n-active-baskets 8 \
  --data-dir /tmp/oq-baseline \
  --report-tsv /tmp/oq-baseline/report.tsv
# Expect: cum_return=+0.0712 sharpe=0.472 max_dd=+0.2450 n_days=189
```

Trimmed run is the same with `--universe config/basket_universe_v1_trimmed.toml`.
