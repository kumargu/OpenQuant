# Breakout-count analysis (2026-05-03)

Re-doing the "how many tradable opportunities exist" question, this time
across the full no-mining v1 universe with rolling-window stats — not the
stale fit μ that produced the misleading "4 in 9 months" earlier.

## Method

For each of the 43 baskets over Jul 2025 → Mar 2026 (189 trading days):

1. Compute log-spread = log(target) − mean(log(peers))
2. Rolling 30-day mean (μ_t) and std (σ_t) — tracks the basket's CURRENT
   center, not a fixed historical fit. This is what a discretionary
   chartist would use.
3. z_t = (spread_t − μ_t) / σ_t
4. Count "breakouts": z crosses ±1.5σ
5. Count "tradable round-trips": breakouts that mean-revert to within
   ±0.5σ within 20 trading days

## Universe-level numbers

| metric | count | per-basket avg |
|---|---|---|
| visible breakouts (\|z\|>1.5σ) | **321** | 7.5 |
| tradable round-trips | **238** | 5.5 |
| strategy net log-P&L | +0.483 | — |
| strategy hit rate | 22 / 43 | 51% |

The earlier "4 in 9 months" framing was wrong. It came from AAPL alone
anchored on a fit μ of -0.43 that the spread never visited (its visible
range was -0.42 to -0.12 on this window). Using a rolling μ that tracks
where the spread actually sits, AAPL has 6 tradable round-trips. The
universe averages 5-7 per basket.

**The opportunity universe is plenty wide** — 238 round-trips in 9 months
is an order of magnitude larger than what the strategy claims to be
trading on. The bottleneck isn't pattern frequency; it's signal direction.

## Sector aggregate (sorted by strategy P&L)

| sector | baskets | tradable round-trips | strategy P&L | win rate |
|---|---|---|---|---|
| **energy** | 6 | 39 | **+1.00** | **6/6** |
| hc_providers | 6 | 25 | +0.45 | 4/6 |
| insurance | 6 | 38 | +0.21 | 3/6 |
| banks_regional | 6 | 34 | +0.13 | 3/6 |
| utilities | 6 | 40 | -0.09 | 4/6 |
| chips | 5 | 21 | -0.17 | 2/5 |
| entsw | 4 | 21 | -0.25 | **0/4** |
| **faang** | 4 | 20 | **-0.80** | **0/4** |

**FAANG and entsw have 0/N hit rate.** 8 baskets, 0 winners despite 41
visible tradable opportunities. This is not a "missing opportunities"
problem; the strategy is systematically taking the wrong side.

## Per-basket efficiency (log-P&L per tradable breakout)

**Top 10 — strategy correctly catches mean-reversion when it appears:**

| basket | n_tradable | P&L per breakout |
|---|---|---|
| chips:AMD | 3 | +0.29 |
| hc_providers:CI | 3 | +0.17 |
| hc_providers:CNC | 2 | +0.15 |
| insurance:AIG | 3 | +0.08 |
| energy:MPC | 4 | +0.07 |
| banks_regional:TFC | 4 | +0.07 |
| hc_providers:HUM | 3 | +0.05 |
| energy:OXY | 6 | +0.04 |
| energy:COP | 8 | +0.03 |
| insurance:MET | 8 | +0.03 |

**Bottom 10 — strategy wrong-side every time:**

| basket | n_tradable | P&L per breakout |
|---|---|---|
| hc_providers:MOH | 2 | -0.16 |
| chips:NVDA | 4 | -0.13 |
| chips:MU | 3 | -0.12 |
| faang:GOOGL | 4 | -0.10 |
| chips:ADI | 4 | -0.07 |
| entsw:INTU | 4 | -0.06 |
| faang:META | 5 | -0.06 |
| hc_providers:UNH | 7 | -0.04 |
| banks_regional:HBAN | 6 | -0.04 |
| insurance:TRV | 5 | -0.03 |

The pattern is consistent with our earlier chart-walks: targets that
trended hardest against their peer mean (NVDA underperformed peers,
GOOGL outperformed, MU rocketed) are exactly the baskets where the
strategy bled — it was structurally short the rippers and long the
laggards on a window where dispersion didn't mean-revert.

## What this changes about our framing

Earlier (wrong): *the strategy doesn't have enough tradable patterns*

Now (right): *the strategy has plenty of patterns; it just takes the
wrong side on 2 sectors. FAANG/entsw are ~50% of the negative
contribution despite being only 8/43 = 19% of baskets.*

If we can stop those 8 baskets from contributing -1.05 in log-P&L
(net −0.80 + −0.25), the remaining 35 baskets contribute +1.93 — a
2.3× return. **That's where Sharpe lift would actually come from.**

But — we cannot identify "the 8 to drop" from in-sample P&L without
overfitting. The structural feature ("0/N hit rate sector with
dominant single-name effects") is observable but not yet a
ex-ante-testable rule.

## Open questions

1. Is "0/N hit rate sector" a stable property across windows (regime-
   independent), or is FAANG/entsw losing here because of THIS regime
   only?
2. What's the structural difference between energy (6/6 winners) and
   FAANG (0/4)? Energy has more uniform dispersion across the basket
   members; FAANG/entsw have one dominant name (GOOGL, ORCL post-spike).
   Could a "max-member-share-of-spread-variance" gate distinguish them?
3. The strategy's per-trade edge is small (+0.002 log-units / breakout
   universe-wide). Is the inefficiency in entry/exit timing or in
   holding too long / wrong direction?

These are diagnostic questions for next session. No new code to ship
based on this analysis alone.
