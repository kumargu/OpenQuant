# Basket-engine Autoresearch NOTEBOOK

Append-only experiment log for monetization-matrix experiments and live/paper run reviews.

Review structure for all future runs:
- Use [RUN_REVIEW_TEMPLATE.md](/Users/gulshan/OpenQuant/autoresearch/RUN_REVIEW_TEMPLATE.md:1)
- Always record `Good`, `Bad`, `Signal Quality`, `Learnings`, and `Next Action`

---

## issue_321_decision_timing_full_universe_q3-2025_q1-2026 (2026-05-02T17:00:00)

- **issue**: [#321 — Phase 1 timing offset sweep](https://github.com/kumargu/OpenQuant/issues/321)
- **scope**: same 4-variant timing sweep, but on the **deployment config that actually works** (no-mining v1, 43 baskets, 8 sectors, cap=8) instead of the FAANG-only stress case
- **window**: 2025-07-01 → 2026-03-31 (189 trading days)
- **why this redo**: per-basket P&L analysis on existing monetization-matrix runs showed FAANG-only at cap=4 was a degenerate stress case (always-on, single-sector, trending regime); the strategy works on the full universe with cap-gated cross-sectional rotation. Re-running #321 on that.

### Engine summary

| variant | offset | cum return | Sharpe | MDD | order_fails |
|---|---|---|---|---|---|
| baseline | 0 | +7.12% | 0.472 | -24.5% | 7 |
| t-5m | 5 | +6.69% | 0.457 | -24.4% | 36 |
| **t-15m** | **15** | **+8.27%** | **0.533** | **-23.4%** | 73 |
| t-30m | 30 | +4.44% | 0.349 | -24.1% | 40 |

### Verdict (revised after buddy review)

- **All 4 variants positive.** Strategy works on this config — `baseline` (offset=0) is the recommended ship config.
- **No statistically distinguishable timing edge.** Buddy reviewer ran block-bootstrap (B=2000, block=10) on ΔSharpe between every pair; **all 95% CIs span zero with width ~1.0** (point estimates 0.06 to 0.12). Lo-2002 standard-error on the Sharpe difference gives t ≈ 0.8 — not significant.
- **t-15m's apparent +0.061 Sharpe edge is sampling noise.** Re-draws of the same 189-day window could have ranked any variant first.
- **Universe-flip is the killer.** Same 4 variants, same dates: FAANG-only at cap=4 ranked t-30m best (+1.0 Sharpe over baseline). Full universe at cap=8 ranks t-30m worst (-0.12 vs baseline). A real intraday signal would be additive across names, not sign-flipping.
- **Per-basket attribution inverts vs equity ranking.** t-30m has highest raw per-basket P&L sum but lowest equity Sharpe — confirms cap-gated cross-sectional rotation is doing the work, timing knob is second-order at best.

### Edge attribution

t-15m wins big on: insurance:MET (+0.10), utilities:EXC (+0.07), hc_providers:CI (+0.06), chips:MU (+0.04), energy:CVX (+0.03).
t-15m loses big on: energy:XOM (-0.11), energy:MPC (-0.08), insurance:PRU (-0.05).
Net per-basket: +0.07 log-units → roughly the observed +0.06 Sharpe edge.

20 of 40 baskets show IDENTICAL P&L across all 4 variants — timing is a second-order knob; most trades fire regardless of offset.

### Order rejection asymmetry

t-15m had **73 BP-exceeded rejections** vs baseline's 7 (10×). The winning variant tried more trades; rejected more; the ones that fired captured more value. Worth investigating whether BP gate is too tight at capital=10k × leverage=4x ÷ cap=8.

### Caveats / open questions
- The +0.06 Sharpe edge is small; no confidence interval on this single window. Buddy reviewer asked to challenge whether it's distinguishable from noise.
- Per-basket P&L attribution suggested t-30m had highest raw signal (+0.70) but t-30m has the LOWEST equity (+4.44%). Attribution doesn't model the cap=8 admission filter which rotates baskets in/out. The cap-gated equity is the truth.
- META has 60% of intraday days with missing bars in summer 2025; MSFT 42%. Could be silently distorting t-30m's result on those names.

### Next steps
- DONE: buddy review challenged the t-15m claim — see CI bands above. Hedged.
- **Audit META/MSFT bar completeness before any further timing sweep.** META has 60% of summer-2025 days with <380 RTH bars; MSFT 42%. The intraday-timing experiment depends on bars at specific minutes — if META's 15:30 bar is missing on 60% of days, t-30m's snapshot for META falls back silently to whatever earlier bar exists. Could be the source of the universe-flip.
- **Re-run with BP=∞** to remove the cap-filter cherry-pick confound from the timing comparison.
- **Do not advance to Phase 2** (VWAP / median snapshot robustness) until Phase 1 has a reproducible answer across at least 2 walk-forward folds.
- Investigate the BP-rejection asymmetry separately — t-15m had 73 rejections vs baseline's 7 (10×). Suggests BP gate may be too tight at capital=10k × leverage=4x ÷ cap=8.
- Working config saved to `autoresearch/issue_321/WINNING_CONFIG.md` — recommends **baseline (offset=0)** as the ship config, not t-15m.

---

## issue_321_decision_timing_faang_q3-2025_q1-2026 (2026-05-02T16:22:00, revised after buddy review)

- **issue**: [#321 — Autoresearch: test one-shot basket entry timing and close-snapshot robustness](https://github.com/kumargu/OpenQuant/issues/321) (Phase 1 only — timing offset)
- **scope**: One-shot daily decision-time sweep on a single basket (FAANG) over 9 months
- **window**: 2025-07-01 → 2026-03-31 (189 trading days, 4 baskets — AAPL, GOOGL, META, AMZN)
- **universe**: `config/basket_universe_v1_faang_{baseline,t5m,t15m,t30m}.toml`
- **knob**: `[runner].decision_offset_minutes_before_close ∈ {0, 5, 15, 30}` (added on `exp/issue-321-decision-timing`; defaults to 0 = byte-identical to production)
- **infrastructure changes shipped before this experiment**:
  - PR #322 — replay session-close race fix (consumer-ack back-channel; replay was non-deterministic by ~1% Sharpe pre-fix)
  - branch `exp/issue-321-decision-timing` — TOML knob + cherry-picked determinism fix `9395e22d`
- **status**: complete; **conclusion revised after buddy reviewer caught false claims in initial writeup** (see "REVIEW NOTE" below)
- **hypothesis (pre-registered)**: A pre-close snapshot (t-5m / t-15m / t-30m) may produce more robust daily decisions than the last 1-min bar, because closing-print noise / late prints / microstructure should bias the close more than a stable mid-session price. Prior: monotone improvement with longer offset OR no signal.

### Engine summary

| variant | offset (min) | cum return | Sharpe | max DD | n_days | final P&L |
|---|---|---|---|---|---|---|
| baseline | 0 | -36.82% | -1.677 | -44.4% | 189 | -$3682 |
| t-5m | 5 | -36.49% | -1.671 | -44.2% | 189 | -$3649 |
| t-15m | 15 | -36.90% | -1.649 | -44.5% | 189 | -$3690 |
| **t-30m** | **30** | **-30.08%** | **-1.395** | **-40.2%** | **189** | **-$3008** |

### REVIEW NOTE — initial decomposition was wrong

A first pass of this writeup framed the t-30m edge as "4 timing-sensitive flips concentrated in Aug 4-7, 2025; trade sets identical after Aug 7; the {0, 5, 15} cluster is a same-decision-set noise floor." A buddy reviewer challenged each of those claims by reading the report TSVs directly. Verified against the data:

- **All four variants disagree with baseline on 188/189 days** (every day except 2025-07-01, before any positions exist). Days where the daily-PnL delta exceeds $10:
  - t-5m vs baseline: 72 / 189 days
  - t-15m vs baseline: 125 / 189 days
  - t-30m vs baseline: 132 / 189 days
- The "trade sets identical after Aug 7" claim was false. The original log-extraction pulled only `BASKET_INTENT` lines (position transitions). It missed daily share-rebalancing orders, which differ per variant because target-share rounding depends on the close price the share-computation reads, and that close price IS the per-variant snapshot.
- The "4 trades drove the +$675 edge" claim was also false. Top-3 days (by |Δ daily PnL|) account for the entire edge:

  | date | Δ daily PnL (t-30m − baseline) |
  |---|---|
  | 2025-08-06 | +$365 |
  | 2025-08-07 | +$175 |
  | 2025-12-19 | +$171 |
  | top-3 sum | **+$711** |
  | total over 189 days | **+$675** |
  | rest (186 days) | **−$37** |

  The edge is THREE outlier days, not four trades, and not Aug-confined — Dec 19 contributes as much as Aug 7.
- The "{0, 5, 15} cluster is a noise floor" framing was wrong. Those three variants don't share a trade set; their 0.028 Sharpe spread is the integral of 188 daily disagreements with smaller per-day magnitude, partially cancelling. **The experiment as designed has no measured noise floor.**

### Bigger methodological problem (also caught by reviewer)

`basket_fits.rs` and `parquet_bar_source.rs` both consume `decision_offset_minutes_before_close`. So each variant has both:
- a **different walk-forward fit** (the OU mean / std / Bertram threshold are computed from a daily-close history sampled at the variant's offset), AND
- a **different runtime z** (numerator = today's snapshot at the variant's offset).

Phase 1 as currently structured measures a confounded "different fit + different snapshot" effect. To isolate "decision-time noise" the experiment needs a 2×2:

| | runtime on close | runtime on t-30m |
|---|---|---|
| **fit on close** | baseline | **pure decision-time noise** |
| **fit on t-30m** | (rare in practice) | t-30m as currently implemented |

The (fit on close, runtime on t-30m) cell is the only one that isolates timing-snapshot noise. We don't have it. Without it, the +0.28 Sharpe gap can't be attributed to timing — it's plausibly fit-history drift dominating.

### Good
- **Replay determinism is now bit-exact** post-PR-#322. Same configs re-run produce identical reports across sequential and parallel — enables credible cross-run comparison.
- The TOML-only knob worked end-to-end across the full sweep — fit warmup loader, parquet bar source, and basket_live consumer all respect the per-variant cutoff. Codex P1 review caught a hang on resume-with-already-processed-day path before merge; fixed before shipping.
- 4 parallel runs of 189 sessions completed in ~95 min wall-clock — research velocity is healthy.
- **Buddy reviewer caught three demonstrably false claims** before they were shipped — exactly the workflow the `feedback_buddy_reviewer.md` rule exists for.

### Bad
- **FAANG-only universe has negative expected return on this window.** Baseline -36.8% / Sharpe -1.68 over 9 months. With 4 highly correlated targets and one peer set, no diversification across baskets, the strategy is structurally a bet on FAANG cross-sectional reversion during a period of earnings-driven trending — the wrong regime. This bounds the experiment's power to detect *any* timing effect.
- Phase 1 was structured as a confounded experiment (fit and runtime move together) — it cannot answer "does decision-time noise matter" without the 2×2 split.
- The headline outperformance for t-30m is 3 outlier days plus noise. Either way (4 trades or 3 days) the n is too small for a confidence interval that excludes zero.

### Signal Quality
- **No measured noise floor in this experiment** (initial cluster-spread framing was wrong). Cannot calibrate t-30m's +0.28 Sharpe gap against anything credible.
- **t-30m's +$675 over 9 months = 3 outlier days + −$37 over 186 days.** This is fragile-tail signal, not a structural property of late-day trading.
- **Cannot distinguish snapshot effect from fit effect.** The 2×2 decomposition is required before any ship-vs-don't-ship call.

### Learnings
- The Phase-1 hypothesis is **not testable** as currently implemented — the experiment confounds fit-history with runtime snapshot. Need to add a way to source fit and runtime offsets independently before the question can be answered.
- The reviewer's protocol (read the TSVs directly, count days, find top-N divergent dates) would have caught the false intent-extraction in seconds. Adding a per-experiment "decomposition sanity table" in the notebook (n_days_diff, top-3 contribution share) before drawing conclusions is cheap insurance.
- A 9-month FAANG-only window with 4 traded targets is genuinely too small to detect a low-frequency timing edge against a losing baseline. Either the universe must broaden (full v1) OR the strategy P&L must be roughly flat in the test window OR we need many windows.

### Next Action
1. **Add a second TOML knob `[runner].fit_offset_minutes_before_close`** that defaults to `decision_offset_minutes_before_close` (so existing TOMLs and the experiment so far are byte-identical) but can be set independently. This unlocks the 2×2 fit/runtime split.
2. **Re-run Phase 1 as a 2×2** on the same FAANG / 9-month window: {fit_offset ∈ {0, 30}} × {decision_offset ∈ {0, 30}}. The (0, 30) cell is the pure timing-noise estimate. Compare against the (0, 0) baseline to measure timing alone.
3. **Defer the broader-universe re-run** until Phase 1 itself measures what it claims. A different universe with the same fit/runtime confound is a different broken experiment.
4. **Defer Phase 2 (VWAP / median snapshots)** until Phase 1 has a real isolation of timing noise.

---

## review_signal_quality_2026_04_30 (2026-04-30T00:00:00)

- **run_type**: `paper`
- **scope**: evaluate whether the April 29, 2026 book opened from prior-day signals still has valid mean-reversion conviction
- **window**: April 29, 2026 open book reviewed on April 30, 2026
- **config**: `basket_universe_v1.toml`, recovered local state, paper execution
- **status**: partial

### Good
- Most active baskets still look like live mean-reversion bets rather than exhausted noise.
- `12` of `14` reviewed active baskets remained outside threshold, so the thesis was still statistically alive.
- `8` of `14` had already moved toward zero from entry, which is the behavior we want.
- Stronger examples were `AMD`, `MU`, `ADBE`, `PSX`, `COP`, and `RF`.

### Bad
- Local recovered state did not exactly match the broker book, so basket-level interpretation is useful but not exact.
- `6` of `14` baskets widened further after entry instead of compressing.
- The clearest wrong-way trade was `MPC`, which moved from entry `z=0.57` to current `z=2.06`.
- A few entries were only modestly beyond threshold at entry, which makes them tradable but not high-conviction.

### Signal Quality
- High-conviction entries: `AMD` short-spread, `MU` long-spread, `ADBE` long-spread, `PSX` long-spread, `COP` short-spread
- Low-conviction entries: `OXY` short-spread, `RF` long-spread once they moved close to flat / inside threshold
- Mean reversion already happening: `AMD`, `NVDA`, `MU`, `ADBE`, `PSX`, `COP`, `RF`, `OXY`
- Wrong-way / widening trades: `MPC`, `ADI`, `CVX`, `HBAN`, `PNC`, `XOM`

### Metrics
- Filled orders on April 28, 2026 closeout day: `24 / 24`
- Open positions in reviewed April 29, 2026 book: `42`
- Gross market value of reviewed live book: `$31,264.92`
- Net market value of reviewed live book: `$395.01`
- Realized P/L: not computed in this review
- Unrealized P/L at review time: `-$90.20`

### Learnings
- Keep: baskets that enter with clear stretch and remain outside threshold while starting to compress
- Change: log entry conviction explicitly so we can separate strong stretch from barely-over-threshold trades
- Stop doing: treating all threshold breaches as equally good signals

### Next Action
- Add a per-run conviction summary that ranks entries by entry `|z|`, threshold distance, and whether they are compressing after day 1

---

## monet_rank_by_abs_z_cap8_q3_2025 (2026-04-27T00:50:54)

- **policy**: `rank-by-abs-z` (cap 8)
- **window**: 2025-07-01 → 2025-09-30
- **universe**: basket_universe_v1_no_mining.toml
- **hypothesis**: Baseline (rank-by-abs-z, cap 15, no-mining v1) reproduces the Q3 2025 cell from PR #319: cum +7.94%, Sharpe +1.97.

### Engine summary
- cum_return: 0.131291
- sharpe: 2.015582
- max_drawdown: ?
- final_equity: 11312.92
- n_orders: ?

### Monetization diagnostics
- avg pre-net gross: $36,680
- avg post-net gross: $22,008
- avg post-round gross: $22,011
- post-net / pre-net: 0.600
- post-round / post-net: 1.000
- avg active baskets: 8.0
- avg skipped at cap: 0.0
- avg overlap per selected: 4.50
- avg zero-share legs: 0.0
- avg zero-share baskets: 0.0
- total turnover: $0
- replay time: 1898.3s

---

## ops_recovery_paper_2026_04_29 (2026-04-29T13:47:00)

- **scope**: recover local paper runner continuity on Wednesday, April 29, 2026
- **yesterday review**: Tuesday, April 28, 2026 paper account orders were a pure closeout set
- **yesterday execution summary**: 24 / 24 orders filled; 8 sells-to-close and 16 buys-to-close; gross filled notional about $13.75k sell and $13.97k buy
- **today pre-existing activity**: Wednesday, April 29, 2026 already had 42 filled opening orders in the paper account before local restart, resulting in 42 open symbols on the broker
- **state issue**: no local `config/basket_universe_v1.fits.state.json` existed, so the live runner would fail closed when it saw broker positions without a basket snapshot
- **recovery action**: rebuilt `config/basket_universe_v1.fits.state.json` via replay through April 28, 2026 using `basket_universe_v1.toml`
- **recovery result**: recovered snapshot is internally valid (`last_processed_trading_day = 2026-04-28`, 15 active baskets) but does not match the live broker book exactly; replay-recovered book implied 24 symbols while broker held 42
- **operational learning**: the live paper workflow depends on preserving the basket state snapshot alongside the broker book; without it, restart safety is degraded and reconciliation becomes approximate rather than exact
- **current status**: restarted the paper runner from repo root in tmux session `paper-run-20260429`; it is running on the recovered snapshot and refreshing April 29 bars

---

## monet_equal_weight_cap8_q3_2025 (2026-04-27T01:23:01)

- **policy**: `equal-weight` (cap 8)
- **window**: 2025-07-01 → 2025-09-30
- **universe**: basket_universe_v1_no_mining.toml
- **hypothesis**: Baseline (rank-by-abs-z, cap 15, no-mining v1) reproduces the Q3 2025 cell from PR #319: cum +7.94%, Sharpe +1.97.

### Engine summary
- cum_return: 0.131227
- sharpe: 2.015335
- max_drawdown: ?
- final_equity: 11312.27
- n_orders: ?

### Monetization diagnostics
- avg pre-net gross: $36,679
- avg post-net gross: $22,007
- avg post-round gross: $22,011
- post-net / pre-net: 0.600
- post-round / post-net: 1.000
- avg active baskets: 8.0
- avg skipped at cap: 27.4
- avg overlap per selected: 4.50
- avg zero-share legs: 0.0
- avg zero-share baskets: 0.0
- total turnover: $0
- replay time: 1927.2s

---

## monet_overlap_penalized_z_cap8_q3_2025 (2026-04-27T01:54:38)

- **policy**: `overlap-penalized-z` (cap 8)
- **window**: 2025-07-01 → 2025-09-30
- **universe**: basket_universe_v1_no_mining.toml
- **hypothesis**: Baseline (rank-by-abs-z, cap 15, no-mining v1) reproduces the Q3 2025 cell from PR #319: cum +7.94%, Sharpe +1.97.

### Engine summary
- cum_return: -0.022689
- sharpe: -0.254267
- max_drawdown: ?
- final_equity: 9773.11
- n_orders: ?

### Monetization diagnostics
- avg pre-net gross: $32,474
- avg post-net gross: $27,641
- avg post-round gross: $27,728
- post-net / pre-net: 0.851
- post-round / post-net: 1.003
- avg active baskets: 8.0
- avg skipped at cap: 26.9
- avg overlap per selected: 1.61
- avg zero-share legs: 0.2
- avg zero-share baskets: 0.0
- total turnover: $0
- replay time: 1897.5s

---

## monet_rank_by_abs_z_cap8_q4_2025 (2026-04-27T14:57:45)

- **policy**: `rank-by-abs-z` (cap 8)
- **window**: 2025-10-01 → 2025-12-31
- **universe**: basket_universe_v1_no_mining.toml
- **hypothesis**: Baseline (rank-by-abs-z, cap 15, no-mining v1) reproduces the Q3 2025 cell from PR #319: cum +7.94%, Sharpe +1.97.

### Engine summary
- cum_return: 0.029705
- sharpe: 0.641648
- max_drawdown: ?
- final_equity: 10297.06
- n_orders: ?

### Monetization diagnostics
- avg pre-net gross: $35,726
- avg post-net gross: $21,436
- avg post-round gross: $21,445
- post-net / pre-net: 0.600
- post-round / post-net: 1.000
- avg active baskets: 8.0
- avg skipped at cap: 0.0
- avg overlap per selected: 4.50
- avg zero-share legs: 0.0
- avg zero-share baskets: 0.0
- total turnover: $0
- replay time: 1927.8s

---

## monet_rank_by_abs_z_cap8_q1_2026 (2026-04-27T15:28:23)

- **policy**: `rank-by-abs-z` (cap 8)
- **window**: 2026-01-01 → 2026-03-31
- **universe**: basket_universe_v1_no_mining.toml
- **hypothesis**: Baseline (rank-by-abs-z, cap 15, no-mining v1) reproduces the Q3 2025 cell from PR #319: cum +7.94%, Sharpe +1.97.

### Engine summary
- cum_return: 0.138254
- sharpe: 2.525080
- max_drawdown: ?
- final_equity: 11382.54
- n_orders: ?

### Monetization diagnostics
- avg pre-net gross: $38,906
- avg post-net gross: $23,802
- avg post-round gross: $23,833
- post-net / pre-net: 0.612
- post-round / post-net: 1.001
- avg active baskets: 8.0
- avg skipped at cap: 0.0
- avg overlap per selected: 4.50
- avg zero-share legs: 0.0
- avg zero-share baskets: 0.0
- total turnover: $0
- replay time: 1838.1s

---

## monet_equal_weight_cap8_q4_2025 (2026-04-27T15:59:31)

- **policy**: `equal-weight` (cap 8)
- **window**: 2025-10-01 → 2025-12-31
- **universe**: basket_universe_v1_no_mining.toml
- **hypothesis**: Baseline (rank-by-abs-z, cap 15, no-mining v1) reproduces the Q3 2025 cell from PR #319: cum +7.94%, Sharpe +1.97.

### Engine summary
- cum_return: 0.027223
- sharpe: 0.605466
- max_drawdown: ?
- final_equity: 10272.23
- n_orders: ?

### Monetization diagnostics
- avg pre-net gross: $35,652
- avg post-net gross: $21,391
- avg post-round gross: $21,414
- post-net / pre-net: 0.600
- post-round / post-net: 1.001
- avg active baskets: 8.0
- avg skipped at cap: 31.2
- avg overlap per selected: 4.50
- avg zero-share legs: 0.0
- avg zero-share baskets: 0.0
- total turnover: $0
- replay time: 1867.6s

---

## monet_equal_weight_cap8_q1_2026 (2026-04-27T16:30:09)

- **policy**: `equal-weight` (cap 8)
- **window**: 2026-01-01 → 2026-03-31
- **universe**: basket_universe_v1_no_mining.toml
- **hypothesis**: Baseline (rank-by-abs-z, cap 15, no-mining v1) reproduces the Q3 2025 cell from PR #319: cum +7.94%, Sharpe +1.97.

### Engine summary
- cum_return: 0.136137
- sharpe: 2.484855
- max_drawdown: ?
- final_equity: 11361.37
- n_orders: ?

### Monetization diagnostics
- avg pre-net gross: $38,869
- avg post-net gross: $23,780
- avg post-round gross: $23,806
- post-net / pre-net: 0.612
- post-round / post-net: 1.001
- avg active baskets: 8.0
- avg skipped at cap: 28.5
- avg overlap per selected: 4.50
- avg zero-share legs: 0.0
- avg zero-share baskets: 0.0
- total turnover: $0
- replay time: 1838.1s

---

## monet_overlap_penalized_z_cap8_q4_2025 (2026-04-27T17:02:17)

- **policy**: `overlap-penalized-z` (cap 8)
- **window**: 2025-10-01 → 2025-12-31
- **universe**: basket_universe_v1_no_mining.toml
- **hypothesis**: Baseline (rank-by-abs-z, cap 15, no-mining v1) reproduces the Q3 2025 cell from PR #319: cum +7.94%, Sharpe +1.97.

### Engine summary
- cum_return: 0.103441
- sharpe: 2.381870
- max_drawdown: ?
- final_equity: 11034.41
- n_orders: ?

### Monetization diagnostics
- avg pre-net gross: $37,059
- avg post-net gross: $34,531
- avg post-round gross: $34,778
- post-net / pre-net: 0.932
- post-round / post-net: 1.007
- avg active baskets: 8.0
- avg skipped at cap: 30.6
- avg overlap per selected: 0.68
- avg zero-share legs: 0.0
- avg zero-share baskets: 0.0
- total turnover: $0
- replay time: 1927.8s

---

## monet_overlap_penalized_z_cap8_q1_2026 (2026-04-27T17:32:54)

- **policy**: `overlap-penalized-z` (cap 8)
- **window**: 2026-01-01 → 2026-03-31
- **universe**: basket_universe_v1_no_mining.toml
- **hypothesis**: Baseline (rank-by-abs-z, cap 15, no-mining v1) reproduces the Q3 2025 cell from PR #319: cum +7.94%, Sharpe +1.97.

### Engine summary
- cum_return: 0.207947
- sharpe: 3.613242
- max_drawdown: ?
- final_equity: 12079.47
- n_orders: ?

### Monetization diagnostics
- avg pre-net gross: $39,442
- avg post-net gross: $36,347
- avg post-round gross: $36,382
- post-net / pre-net: 0.922
- post-round / post-net: 1.001
- avg active baskets: 8.0
- avg skipped at cap: 27.4
- avg overlap per selected: 0.63
- avg zero-share legs: 0.0
- avg zero-share baskets: 0.0
- total turnover: $0
- replay time: 1837.8s

---

## monet_lambdasweep_q3_op_z_lam0.00_cap8 (2026-04-27T18:08:38)

- **policy**: `overlap-penalized-z` (cap 8)
- **window**: 2025-07-01 → 2025-09-30
- **universe**: basket_universe_v1_no_mining.toml
- **hypothesis**: Baseline (rank-by-abs-z, cap 15, no-mining v1) reproduces the Q3 2025 cell from PR #319: cum +7.94%, Sharpe +1.97.

### Engine summary
- cum_return: -0.002408
- sharpe: 0.091129
- max_drawdown: ?
- final_equity: 9975.92
- n_orders: ?

### Monetization diagnostics
- avg pre-net gross: $32,560
- avg post-net gross: $23,392
- avg post-round gross: $23,222
- post-net / pre-net: 0.718
- post-round / post-net: 0.993
- avg active baskets: 8.0
- avg skipped at cap: 26.9
- avg overlap per selected: 3.07
- avg zero-share legs: 0.1
- avg zero-share baskets: 0.0
- total turnover: $0
- replay time: 1927.4s

---

## monet_lambdasweep_q3_op_z_lam0.10_cap8 (2026-04-27T18:40:45)

- **policy**: `overlap-penalized-z` (cap 8)
- **window**: 2025-07-01 → 2025-09-30
- **universe**: basket_universe_v1_no_mining.toml
- **hypothesis**: Baseline (rank-by-abs-z, cap 15, no-mining v1) reproduces the Q3 2025 cell from PR #319: cum +7.94%, Sharpe +1.97.

### Engine summary
- cum_return: -0.032155
- sharpe: -0.385684
- max_drawdown: ?
- final_equity: 9678.45
- n_orders: ?

### Monetization diagnostics
- avg pre-net gross: $31,857
- avg post-net gross: $23,978
- avg post-round gross: $23,933
- post-net / pre-net: 0.753
- post-round / post-net: 0.998
- avg active baskets: 8.0
- avg skipped at cap: 26.9
- avg overlap per selected: 2.67
- avg zero-share legs: 0.2
- avg zero-share baskets: 0.0
- total turnover: $0
- replay time: 1927.3s

---

## monet_lambdasweep_q3_op_z_lam0.25_cap8 (2026-04-27T19:12:52)

- **policy**: `overlap-penalized-z` (cap 8)
- **window**: 2025-07-01 → 2025-09-30
- **universe**: basket_universe_v1_no_mining.toml
- **hypothesis**: Baseline (rank-by-abs-z, cap 15, no-mining v1) reproduces the Q3 2025 cell from PR #319: cum +7.94%, Sharpe +1.97.

### Engine summary
- cum_return: -0.029370
- sharpe: -0.344119
- max_drawdown: ?
- final_equity: 9706.30
- n_orders: ?

### Monetization diagnostics
- avg pre-net gross: $32,497
- avg post-net gross: $25,648
- avg post-round gross: $25,624
- post-net / pre-net: 0.789
- post-round / post-net: 0.999
- avg active baskets: 8.0
- avg skipped at cap: 26.9
- avg overlap per selected: 2.31
- avg zero-share legs: 0.2
- avg zero-share baskets: 0.0
- total turnover: $0
- replay time: 1927.3s

---

## monet_lambdasweep_q3_op_z_lam1.00_cap8 (2026-04-27T19:45:00)

- **policy**: `overlap-penalized-z` (cap 8)
- **window**: 2025-07-01 → 2025-09-30
- **universe**: basket_universe_v1_no_mining.toml
- **hypothesis**: Baseline (rank-by-abs-z, cap 15, no-mining v1) reproduces the Q3 2025 cell from PR #319: cum +7.94%, Sharpe +1.97.

### Engine summary
- cum_return: 0.027457
- sharpe: 0.597371
- max_drawdown: ?
- final_equity: 10274.58
- n_orders: ?

### Monetization diagnostics
- avg pre-net gross: $33,267
- avg post-net gross: $29,124
- avg post-round gross: $29,201
- post-net / pre-net: 0.875
- post-round / post-net: 1.003
- avg active baskets: 8.0
- avg skipped at cap: 26.9
- avg overlap per selected: 1.30
- avg zero-share legs: 0.1
- avg zero-share baskets: 0.0
- total turnover: $0
- replay time: 1927.4s
