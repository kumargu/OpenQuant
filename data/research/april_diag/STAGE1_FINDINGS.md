# Stage 1 — April-2026 vs OOS diagnostics on the dom060 baseline

**Setup:** 5 walk-forward replays through the live engine path (`openquant-runner replay --engine basket`), per-trade diagnostic TSV emitted via the new `--trade-tsv` flag. Capital 10k, leverage 4×, n_active_baskets=8. Walk-forward fit built strictly OOS from each `--start`.

All 5 windows reproduce `walkforward_v1.json` exactly (Sharpe matches to 3 decimals).

## Headline table — real trades only (filter: `bars_held >= 1`)

The dom060 + cap=8 design produces a huge number of "0-bar artifact" trades: a basket's state machine enters on bar N, the portfolio cap excludes it that same session, the engine flattens. These show up as ClosedTrade records with `bars_held=0`, `spread_move=0`. They are bookkeeping, not held positions, and I filter them out below.

| window | trades_all | 0-bar artifact | real | W | L | win% | Lmed_held | Lmed_adv | Lp90_adv | L_we% | L_gt2HL% |
|---|---|---|---|---|---|---|---|---|---|---|---|
| test0 (2024-07..2024-12) | 740 | 647 | 93 | 69 | 24 | 74% | 7.0 | 0.587 | 3.489 | 29% | 29% |
| test1 (2025-01..2025-06) | 374 | 276 | 98 | 77 | 21 | 78% | 15.0 | 0.754 | 4.402 | 33% | 52% |
| test2 (2025-07..2025-12) | 161 |  95 | 66 | 53 | 13 | 80% | 14.0 | 1.005 | 6.209 | 53% | 38% |
| test3 (2026-01..2026-03) | 141 |  82 | 59 | 45 | 14 | 76% |  9.0 | 0.807 | 3.515 | 42% | 50% |
| **april (2026-04)**      |  69 |  45 | 24 | 13 | 11 | **54%** | 9.0 | **1.006** | 3.323 | **63%** | 36% |

`L_we%` = fraction of losers exiting via `window_end` (still open at replay's last day).
`L_gt2HL%` = fraction of losers with calendar `days_held > 2 × half_life_days` (per-basket OU fit).
`Lp90_adv` is the 90th-percentile of `max_adverse_z` over losers.

## What the comparison says

- **April's distinctive feature is NOT extreme adverse-z tail.** April `Lp90_adv = 3.32` is *lower* than test1 (4.40) and test2 (6.21), both of which were profitable. April's median adverse-z (1.006) matches test2 (1.005), which earned +15.9%.
- **April's distinctive feature IS the win-rate collapse (54% vs 74–80%) and stuck-at-window-end fraction (63% vs 29–53%).** The strategy's small-magnitude wins didn't fire; losers that did open never reverted.
- **The strategy universally holds losers ~2–3× longer than winners.** Across all 5 windows: winners median 2–3 trading days, losers median 7–15. This is a property of always-in-position basket flips + cap-cycling, not an April pathology.
- **`L_gt2HL%` does not separate April from OOS** (April 36%, OOS range 29–52%). HL-cap-violation is universal, not April-specific.

## April losers — close-up

```
basket                                p   held  d_to_max  maxAdv   HL    held/HL  spread_move
energy:COP                            -1   28      22      3.32    1.3   21.9    -0.0161
chips:NVDA                            +1   28      28      7.76    2.1   13.1    -0.2148   ← single worst trade
hc_providers:UNH                      -1   28      21      2.86    5.7    4.9    -0.0072
utilities:SO                          +1    9       4      0.99    3.3    2.7    -0.0078
entsw:ADBE                            +1   10       7      1.03    5.6    1.8    -0.0031
hc_providers:CI                       +1   28      28      2.36   21.8    1.3    -0.2484   ← 2nd worst trade, slow HL
faang:AAPL                            +1    9       3      0.69   15.9    0.6    -0.0202
energy:XOM                            +1    8       8      0.70   16.1    0.5    -0.0285
insurance:PGR                         +1    1       1      0.11    2.4    0.4    -0.0023
faang:AAPL                            +1    6       4      1.01   15.9    0.4    -0.0421
utilities:D                           -1    6       6      0.50   28.4    0.2    -0.0197
```

Four trades held to window end (`held=28` calendar days = 19 trading days). For three of them (NVDA, COP, UNH) the HL is 1.3 – 5.7 days — they were stale by a factor of 5–22×. CI is a slow-HL basket where 28 days is only 1.3× HL but `max_adv` still exceeded 2.0 and spread moved 24%.

`days_to_max_adverse` for the stuck losers is at or near `held` — the adverse excursion was still *growing* when the window ended. These were not "hit a peak and recovered" trades; they were continuing-drift trades.

## Path A vs Path B decision

The reviewer's branching rule:
- **Path A (z-stop):** "Choose if April losers show large `max_adverse_z` quickly."
- **Path B (HL-adaptive max-hold):** "Choose if April losers are mostly long-stale positions with modest adverse z drift."

Evidence:

1. **`days_to_max_adverse / held` ≈ 1 for the stuck losers.** Adverse z was not "hit early and stay there" — it was "still climbing at exit." That weakens the case for a hard z-stop that fires on adverse magnitude.
2. **3 of 4 worst losers had `held/HL >> 2`.** That's a direct Path B signal.
3. **OOS windows have similar / worse adverse-z tails than April** (`Lp90_adv` test1=4.40, test2=6.21 vs april=3.32). A flat z-stop at 2.0 would clip aggressively in profitable windows where adverse excursions are part of normal reversion trades.
4. **HL is self-adjusting per basket.** CI has HL=22, NVDA has HL=2.1; a 2× HL cap holds CI long enough to revert (correct) but cuts NVDA on day 4 (also correct).

**Recommendation: Path B.** Test `max_hold_days = ceil(multiplier × half_life_days)` with `multiplier ∈ {1.5, 2.0, 2.5}` plus a no-cap control. Acceptance bar from the reviewer: April improves ≥ 3 pp cum or clear max-DD reduction; mean OOS Sharpe within 0.2 of the +2.31 baseline.

## Caveats for the buddy reviewer

- Sample size: 11 April losers, 13–24 OOS losers per window. Inferences are anecdotal-strength, not statistical.
- The `held` column in the TSV is calendar days; `bars_held` is trading days. The reviewer's `2 × half_life` rule is more naturally expressed in trading days but `half_life_days` from the OU fit is calibrated in calendar days (the fit operates on daily-close residuals). I used calendar days for consistency with the OU fit; cross-check if you disagree.
- CI's failure mode (slow HL, persistent drift) is not cleanly addressed by Path B alone — Stage 2 may want a secondary floor like `max_hold = min(k × HL, MAX_DAYS_HARD_CAP)`.
- The 0-bar-artifact filter is justified by zero economic impact (no order is placed; the basket is excluded by the portfolio cap that same session) but the buddy reviewer should validate that interpretation against `process_session_close` in `basket_live.rs`.
