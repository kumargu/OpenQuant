# AAPL basket — human chart-walk (no math, no fit, just eyes)

Two independent reads of `aapl_basket_layer1.png` (target = AAPL, peers =
{AMZN, MSFT, GOOGL, META, NVDA}, window 2025-07-01 → 2026-03-31).

The point of this exercise: stop hiding behind metrics. If a human can't see
the pattern, no algo will find it.

---

## Read 1 — primary author

Walking the middle panel chronologically:

- **Jul 1 — Aug 4 (4 weeks):** Spread oscillates -0.20 to -0.36. Low-grade chop. No signal.
- **Aug 4 — Aug 7 (3 days):** Sharp plunge from -0.18 → -0.42. Looks like exhaustion.
- **Aug 8 — Aug 14 (1 week):** Rebound to -0.25. **Visible "fade-extreme" trade.**
- **Aug 15 — Sep 30:** Tight band -0.30 to -0.25. Skip.
- **Oct 1 — Oct 15:** Drift up -0.30 → -0.18.
- **Oct 15 — Nov 15:** Reverses cleanly -0.18 → -0.30. **Visible range-trade.**
- **Nov 15 — Dec 22:** Reverses again -0.30 → -0.20. **Visible range-trade.**
- **Dec 22 — Jan 31:** Sideways -0.20 to -0.25. Skip.
- **Feb 1 — Feb 28:** Climbs to new high near -0.12. **Visible fade-extreme.**
- **Mar:** Pulls back to -0.18.

**Count: ~4 tradable moves.** All around a "new center" near -0.22, not
the fit μ of -0.43.

**Verdict:** Patterns exist but the fit anchor is wrong. Strategy needs a
μ that updates as the spread settles into a new equilibrium.

---

## Read 2 — buddy reviewer (independent, no shared notes)

Walking the same panel:

- **Jul:** Slow drift -0.30 → -0.22. No reversion. Nothing to fade.
- **Late Jul → mid-Aug:** Sharp drop -0.22 → -0.38. ~16 log-units in 2 weeks.
- **Mid-Aug → mid-Sep:** Snap back -0.38 → -0.20. **Clean V — round-trip ~15 units.**
- **Sep:** Choppy -0.25 to -0.20. Tight to size.
- **Oct:** Steady climb -0.22 → -0.13. **No pullback worth fading. Trend.**
- **Nov:** More climb plus small pullback to -0.18, back to -0.12. Marginal.
- **Dec:** Sideways-up -0.15 to -0.10. Tight chop.
- **Jan:** Drifts up to -0.10 to -0.08.
- **Feb:** Pullback -0.10 → -0.18. **Modest fade for a bull.**
- **Mar:** Rips to new highs near -0.05. Pure trend.

**Count: ~2 clean discretionary trades.** Aug V (~15 log-units) and Feb
pullback (~8 log-units).

**Verdict:**
- Series is **persistently drifting upward** from -0.30 to -0.05 over 9
  months — a one-way ~25 log-unit move with one V interruption.
- **No visible long-run mean exists within the window.** The spread acts
  integrated/trending, not stationary.
- AAPL is structurally outperforming the equal-weighted mega-cap basket;
  NVDA's AI beta and AMZN/META's idiosyncratic moves swamp any
  AAPL-specific pricing error.
- Refitting μ is "lipstick on a pig" — the underlying assumption
  (mean-reversion exists) is violated by the data.
- **The basket pick is the bug.** Better fix is dropping this basket or
  adding a stationarity gate that would have rejected it before any
  trade.
- The killer observation: **μ = -0.43 is OUTSIDE the entire visible range
  of the spread.** Lowest point was -0.38 in Aug. The strategy was being
  told "fade above -0.43 toward -0.43" while the series never touched
  -0.43 once.

---

## Reconciling

| my read | reviewer read | reconciled |
|---|---|---|
| 4 tradable cycles | 2 clean cycles | Reviewer is closer. The Oct→Nov "reversal" I called is more honestly read as a small pullback inside a multi-month uptrend. |
| Patterns around new center -0.22 | Series has no center, drifts | Reviewer's framing is more rigorous; "new center" only works if the spread settles, which it doesn't — it keeps drifting. |
| Need updating μ | Need to drop the basket | Reviewer is right that refit alone won't fix non-stationarity. |
| Basket pick "probably fine" | Basket pick is the bug | Reviewer is right. AAPL vs equal-weight mega-caps mixes too many factors. |

## Implications

1. The "stale fit" framing was incomplete. **The series isn't stationary, so no fit captures the truth.** A rolling refit chases a moving target; a regime detector pauses but doesn't generate alpha; a stationarity gate would have prevented the trade entirely.

2. **The basket-picker needs a stationarity test** at admission time, not just OU fit / Bertram threshold computation. ADF or KPSS on the residual is the standard tool. Whatever the v1 baseline used to pick this basket, it didn't reject AAPL/{AMZN,MSFT,GOOGL,META,NVDA} despite the chart screaming "trending."

3. **The next experiment is NOT a timing sweep** — it's running this same chart-walk on a different basket where the strategy has a chance to work. Insurance basket (BH return -2%, narrow dispersion) is the cleanest candidate from our BH ranking.

4. If insurance ALSO looks non-stationary by eye, the entire v1 universe needs a stationarity audit before any further timing/parameter experiments.

---

## Note for future sessions

When picking a next basket to chart-walk, prefer the BH-flat / low-dispersion sectors:
- insurance (BH -2.0%) — best candidate
- hc_providers (BH -6.3%)
- utilities (BH +18.6%, narrow dispersion: NEE +41% / SO +5%)

Avoid the trending/dispersing sectors:
- chips (BH +149%, MU +321%)
- mining (BH +50%)
- energy (BH +44%)
- faang (already walked, broken)
