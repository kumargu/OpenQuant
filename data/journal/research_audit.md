# Research Audit: OpenQuant Implementation vs. Cited Literature

Date: 2026-03-26
Auditor: Claude (Opus 4.6)
Files reviewed: `engine/crates/pair-picker/src/scorer.rs`, `scripts/capital_sim.py`,
`scripts/daily_walkforward_dashboard.py`, `docs/priority_queue_research.md`,
`docs/pair_selection_learnings.md`, `docs/debugging_learnings.md`,
GitHub issues #192-#196.

---

## A. Priority Scoring: Our Formula vs. Avellaneda-Lee S-Score

### What we claim
`docs/priority_queue_research.md` line 66-68 and `scorer.rs` line 258-274 claim:
```
priority = |z| x sqrt(kappa) / sigma_spread
```
and cite "Avellaneda-Lee (2010) S3".

### What Avellaneda-Lee (2010) actually says
The s-score in Avellaneda & Lee (2010), "Statistical Arbitrage in the US Equities
Market", Quantitative Finance 10(7): 761-782, is defined as:

```
s_i = -(X_i(t) - m_i) / sigma_eq,i
```

where:
- X_i(t) is the current value of the idiosyncratic residual (from PCA or ETF regression)
- m_i is the estimated equilibrium mean of the OU process
- sigma_eq,i = sigma_i / sqrt(2 * kappa_i) is the equilibrium standard deviation
- kappa_i is the mean-reversion speed
- sigma_i is the diffusion coefficient of the OU process

Substituting sigma_eq:
```
s_i = -(X_i - m_i) * sqrt(2 * kappa_i) / sigma_i
```

The paper also requires kappa > 252/30 = 8.4 (annualized) for a stock to be
tradeable, and uses s-score thresholds of |s| > 1.25 to enter, |s| < 0.5 to exit.

### VERDICT: DIFFERENT FORMULA

Our formula `|z| x sqrt(kappa) / sigma_spread` is NOT the Avellaneda-Lee s-score.

The actual s-score IS effectively a z-score -- it measures deviation from equilibrium
in units of the equilibrium standard deviation. If we define:
```
z = (spread - mean) / sigma_spread
```
then since sigma_eq = sigma / sqrt(2*kappa), the s-score is:
```
s = z * sigma_spread * sqrt(2*kappa) / sigma = z * sqrt(2*kappa) / sigma * sigma_spread
```

But this only equals our formula if sigma_spread = sigma (the OU diffusion coefficient).
In practice sigma_spread is the rolling standard deviation of the spread, while sigma is
the OU diffusion parameter -- these are NOT identical. The relationship is
sigma_eq = sigma / sqrt(2*kappa), so sigma = sigma_eq * sqrt(2*kappa). For a stationary
OU process, the unconditional variance of X is sigma^2 / (2*kappa) = sigma_eq^2,
meaning sigma_spread (the sample std of the spread) approximates sigma_eq, NOT sigma.

Therefore:
- Avellaneda-Lee s-score = (spread - m) / sigma_eq = z (our z-score IS already close to
  the s-score, since sigma_spread approximates sigma_eq for a stationary OU process)
- Our priority formula = |z| * sqrt(kappa) / sigma_spread adds a kappa speed factor and
  a risk normalization that are NOT part of the original s-score

**Our priority formula is a COMPOSITE RANKING metric that combines elements from multiple
sources (Avellaneda-Lee for z, Lee-Leung-Ning 2023 for kappa, risk parity for 1/sigma).
The docs/priority_queue_research.md line 64-68 acknowledges this ("synthesized from
literature"), but scorer.rs line 258 says "Avellaneda-Lee signal strength x OU speed x
risk normalisation" which implies it IS the Avellaneda-Lee formula. It is not.**

### Recommendation
1. Rename the function comment from "Avellaneda-Lee signal strength" to "Composite
   priority score inspired by Avellaneda-Lee (z), OU theory (kappa), and risk parity
   (1/sigma)". Be explicit this is a custom metric, not a published formula.
2. Note that our z-score already approximates the Avellaneda-Lee s-score for stationary
   processes. The priority score adds speed and risk adjustments on top.
3. Consider whether the sqrt(kappa) term is justified. The s-score uses sqrt(2*kappa)
   inside sigma_eq, but that's in the denominator to normalize, not as a multiplicative
   factor. Our usage as a multiplicative weight is a design choice, not a derivation.
4. The missing sqrt(2) factor (we use sqrt(kappa), not sqrt(2*kappa)) is minor but
   should be noted.

---

## B. Opportunity Cost Rotation: Our Logic vs. Leung & Li (2015)

### What we claim
`scorer.rs` line 362-372 and `capital_sim.py` line 13-17 cite Leung & Li (2015) for:
```
REPLACE if: unrealized_return > 0
        AND remaining_per_day < best_queued_per_day - 2 * cost_per_day
```

### What Leung & Li (2015) actually says
Leung & Li (2015), "Optimal Mean Reversion Trading with Transaction Costs and
Stop-Loss Exit", IJTAF 18(03), formulate an optimal DOUBLE STOPPING problem:
- First stopping time: when to ENTER (buy the spread)
- Second stopping time: when to EXIT (sell the spread)

The discount rate r in their framework represents the time value of money / risk-free
rate, NOT explicitly the "expected return of the best alternative trade". Their model
solves for ONE pair at a time. The paper does not address:
- Portfolio-level capital allocation across multiple pairs
- Rotating capital from one trade to another
- Comparing the value of an open position against a queued signal
- Any "remaining edge vs. queued signal" comparison

The connection to opportunity cost is an INTERPRETATION: if you set the discount rate
r equal to the expected return of your best alternative, then the optimal thresholds
become pickier (enter only when the signal is strong enough to beat the alternative).
This is economically sound reasoning, but it is our own adaptation, not a formula
from the paper.

### VERDICT: CREATIVE ADAPTATION, NOT DIRECT IMPLEMENTATION

Our rotation logic is a reasonable heuristic inspired by the economic intuition behind
Leung & Li, but the specific formula:
```
remaining_per_day < best_queued_per_day - 2 * cost_per_day
```
does NOT appear in their paper. The paper solves for fixed optimal thresholds via
variational inequalities, not for dynamic per-bar rotation decisions.

The `docs/priority_queue_research.md` line 70-72 states: "The discount rate r in OU
optimal stopping = expected return of best alternative. Higher r -> pickier entry,
faster exit." This is a valid economic interpretation but should be clearly marked as
our interpretation, not a direct citation.

### Recommendation
1. Change citations from "Leung & Li (2015)" to "Inspired by Leung & Li (2015)
   opportunity-cost interpretation" or "Adapted from the economic intuition in
   Leung & Li (2015)".
2. The rotation formula itself is a standard opportunity-cost comparison found in
   portfolio management textbooks. It doesn't need a single-paper citation -- it's
   a straightforward "is the marginal return of continuing this trade less than the
   marginal return of starting a new one, net of switching costs?" comparison.
3. The 2x cost buffer is a good engineering choice (prevents churn) but is also
   not from the paper. Label it as a practical guard.

---

## C. Dollar-Neutral P&L vs. Beta-Weighted Spread: THE MISMATCH

### The contradiction

**Entry signal (z-score)** is computed from a beta-weighted log spread:
```python
# daily_walkforward_dashboard.py line 296, 335
spread = log(A) - alpha - beta * log(B)
z = (spread - spread_mean) / spread_std
```

**P&L computation** is dollar-neutral (equal $ per leg, NO beta):
```python
# capital_sim.py line 127-143
ret_a = (exit_pa - entry_pa) / entry_pa
ret_b = (exit_pb - entry_pb) / entry_pb
pnl = c * ret_a - c * ret_b  # equal $ per leg
```

### Why this is a problem

The z-score says: "the beta-weighted spread ln(A) - beta*ln(B) is 2.5 sigma away from
its mean." If beta = 1.3, this means A has diverged from 1.3 x B in log terms. The
z-score is calibrated to this specific relationship.

But the P&L is computed as if we put equal dollars in each leg. If beta = 1.3, the
hedged portfolio should have $1.30 in B for every $1.00 in A (in log-return terms,
which approximates to dollar terms for small moves). By putting equal dollars in each
leg, we are UNDERHEDGED on B.

**Concrete example**: Say beta = 1.3, z = -2.5 (spread is below mean, so we go long A
short B expecting reversion). The spread reverts, meaning A outperforms 1.3*B. But our
dollar-neutral position only captures A outperforming 1.0*B. The excess 0.3*B exposure
is unhedged market risk, not spread reversion alpha.

The comment in `capital_sim.py` line 128-133 says:
> "Previous bug: beta-scaling the B leg meant low-beta pairs had 40-50% of B-leg
> capital sitting idle in P&L terms."

This was a real problem, but the fix created a different problem. The correct approach
for a beta-weighted spread is to use beta-weighted position sizes:
```
shares_A = capital / price_A
shares_B = beta * capital / price_B
total_capital = capital * (1 + beta)
```

### VERDICT: GENUINE MISMATCH

The signal (z-score) and the P&L (dollar-neutral) are measuring different things.
The z-score detects divergence in a beta-weighted relationship, but the P&L captures
returns from an equal-dollar relationship. For beta near 1.0, the error is small.
For beta = 0.5 or beta = 1.5, the error is significant.

Our quality gate requires beta > 0.1, but there's no upper bound check. A pair with
beta = 1.5 would have substantial mismatch.

### Recommendation
1. **Option A (preferred)**: Use beta-weighted position sizes. Allocate capital such
   that the dollar exposure ratio matches beta. Total capital = c_a + c_b where
   c_b = beta * c_a. This aligns P&L with the signal.
2. **Option B**: Use a dollar-neutral spread for BOTH signal and P&L. Redefine the
   spread as ln(A) - ln(B) (no beta). This loses the cointegration framework but
   makes the signal and P&L consistent.
3. **At minimum**: Log the beta of each trade and measure whether high-beta pairs
   have systematically different win rates than low-beta pairs. If beta is always
   near 1.0 in practice (which same-sector equity pairs often are), the mismatch
   may be tolerable.
4. Add a quality gate: reject pairs with beta > 1.5 or beta < 0.7 (keeping pairs
   where dollar-neutral is a reasonable approximation of beta-neutral).

---

## D. The 12 bps Round-Trip Cost: Is It Realistic?

### What we use
`daily_walkforward_dashboard.py` line 61: `COST_BPS = 12`
Applied as: `cost = 2 * capital * 12 / 10_000` = 0.12% of total position.

At $5K per leg ($10K total position): cost = $1.20 per round-trip trade.

### Reality check

**Alpaca commissions**: Zero. Alpaca does not charge commissions for self-directed
individual accounts trading US-listed securities. Paper trading is also free.

**Real costs on Alpaca live trading**:
- Commission: $0
- SEC fee: ~$5.10 per $1M notional on sells = ~$0.05 on a $10K sell
- FINRA TAF: $0.000119/share, max $5.95/order = ~$0.01 on small orders
- Total regulatory: ~$0.06 per round-trip on a $10K position = 0.6 bps

**Slippage (the real cost)**:
- At $5K/leg in S&P 500 stocks, market impact is negligible (< 1 bps)
- Bid-ask spread for liquid stocks: 1-3 bps per leg = 2-6 bps round-trip
- Daily bars mean we use market-on-close or next-open, so slippage is real but small

**Realistic total**: 3-8 bps round-trip for liquid S&P 500 names at $5K/leg.

### VERDICT: COST IS CONSERVATIVE (OVER-ESTIMATED BY ~50-100%)

12 bps is roughly 1.5-4x the actual cost for liquid S&P 500 pairs on Alpaca. This is
GOOD for robustness -- if the strategy works at 12 bps, it will work better in reality.
However, it means our backtest understates real performance.

At 33 round-trip trades, the over-count is approximately:
- Excess cost per trade: ~6 bps * $10K = $0.60
- Total over-count: 33 * $0.60 = ~$20

This is small in absolute terms. The conservatism is fine.

### Recommendation
1. Keep 12 bps as the default -- conservative cost assumptions are good practice.
2. Add a sensitivity analysis: run the backtest at 5 bps, 12 bps, and 20 bps to show
   the strategy isn't cost-sensitive. If it only works at 5 bps, that's a red flag.
3. Document that 12 bps includes a ~6-8 bps buffer for slippage/execution uncertainty.
4. For paper trading specifically, note that paper fills are at mid-price with zero
   slippage, so the 12 bps is entirely a conservatism buffer in that context.

---

## E. Quality Gate Thresholds: Data Provenance

### What we use
`capital_sim.py` line 81-83:
```python
MIN_R2_ENTRY = 0.70
MAX_HL_ENTRY = 5.0
MIN_ADF_ENTRY = -2.5
```

`daily_walkforward_dashboard.py` line 60-64:
```python
MIN_R2 = 0.30        # scan threshold (looser)
MIN_R2_ENTRY = 0.85  # entry threshold (tighter)
MAX_HL_ENTRY = 4.0   # tighter HL for entry
MIN_ADF_ENTRY = -2.5
```

### Contradiction #1: Two different R2 thresholds
- `capital_sim.py` uses R2 >= 0.70 for entry
- `daily_walkforward_dashboard.py` uses R2 >= 0.85 for entry
- `docs/pair_selection_learnings.md` line 19 says "R2 >= 0.85 is the quality floor"
  and "R2 < 0.70 pairs are essentially noise"

These files use DIFFERENT thresholds for the same quality gate.

### Contradiction #2: Two different HL thresholds
- `capital_sim.py` uses HL <= 5.0
- `daily_walkforward_dashboard.py` uses HL <= 4.0
- `docs/pair_selection_learnings.md` line 32 says "HL > 5d: can still work (loosened
  from 4d to 5d added 4 winning trades)"

The learning doc describes a specific loosening from 4d to 5d. capital_sim.py adopted
the loosened value; daily_walkforward_dashboard.py still has the old tight value.

### Data provenance
The thresholds were derived empirically from the 265-pair S&P 500 scan (per
`docs/pair_selection_learnings.md` line 10: "38/265 same-sector pairs pass"). The
29-pair candidate list was the result of these filters, not the input.

However, these are FITTED to one specific period (the formation window used during
development). There's no out-of-sample validation documented.

### VERDICT: INCONSISTENT THRESHOLDS + NO OOS VALIDATION

The two main simulation files disagree on R2 (0.70 vs 0.85) and HL (5.0 vs 4.0)
thresholds. This means different parts of the system apply different quality gates,
and results from one cannot be compared with results from the other.

### Recommendation
1. **Unify thresholds**: Pick one set and put it in a shared config (TOML or a shared
   Python constants module). Both `capital_sim.py` and `daily_walkforward_dashboard.py`
   must read from the same source.
2. **Document the data provenance**: "These thresholds were fitted to S&P 500 sector
   pairs over [date range] and have not been validated out-of-sample."
3. **For S&P 500 pairs**: R2 >= 0.85 is likely too tight (will reject many tradeable
   pairs). R2 >= 0.70 with the knowledge that 0.70-0.85 pairs underperform is
   reasonable if you size them smaller.
4. **ADF <= -2.5**: This corresponds roughly to the 10% critical value for a sample
   of ~90 observations. It's a lenient threshold. The 5% critical value is about -2.88
   and 1% is about -3.51. Using -2.5 means we accept pairs where we'd fail to reject
   the unit root null at the 10% level. This is intentionally lenient but should be
   documented as such.

---

## F. Max Hold = 2.5 x Half-Life vs. Research Saying 2 x Half-Life

### What we implement
`scorer.rs` line 16-17 and `capital_sim.py` line 87:
```
max_hold = ceil(2.5 * half_life), capped at 10
```

### What the research docs say
`docs/priority_queue_research.md` line 55: "Dynamic max_hold = 2 x half_life cuts
losers faster"

`scorer.rs` line 14: "Default: 2.5x half-life, capped at 10 days -> ~82% expected
reversion."

Issue #195 body: "Per-pair max_hold = 2 x half_life (not fixed 7-10d for all)"

### The math
For an OU process, after k half-lives the expected fraction of reversion is:
```
1 - 2^{-k}
```
- k = 2.0: 1 - 2^{-2.0} = 1 - 0.25 = 75% expected reversion
- k = 2.5: 1 - 2^{-2.5} = 1 - 0.177 = 82.3% expected reversion
- k = 3.0: 1 - 2^{-3.0} = 1 - 0.125 = 87.5% expected reversion

### VERDICT: INCONSISTENCY IN DOCUMENTATION, CODE IS CLEAR

The code consistently uses 2.5x (scorer.rs default, capital_sim.py HOLD_MULTIPLIER).
The research docs and issue #195 say 2x. This is a documentation inconsistency, not
a code bug. The code was likely updated from 2.0 to 2.5 after the research was written,
and the docs/issues were not updated.

### Does it matter?
For a pair with HL = 3 days:
- 2.0x: max_hold = 6 days (75% expected reversion)
- 2.5x: max_hold = 8 days (82% expected reversion)

The difference is 2 extra days of hold. From `docs/pair_selection_learnings.md` line
54-58:
- Hold 4-6d: 100% win rate
- Hold 10d (max_hold): 43% win rate

So the extra days from 2.5x push some trades into the 7-10 day zone where win rate
drops. The 2.0x multiplier would exit those trades earlier with a smaller loss.

However, `capital_sim.py` line 88 notes: "sweep shows 7-10d tie at $13/d; longer = more
reversions". So the parameter sweep found that 2.5x and longer holds were empirically
equivalent or better in $/day terms.

### Recommendation
1. Update issue #195 and priority_queue_research.md to say 2.5x (matching the code).
2. The choice between 2.0x and 2.5x is a trade-off between:
   - 2.0x: Fewer max_hold exits, tighter stop, more capital rotation
   - 2.5x: More reversions captured, but longer capital lock-up
3. Consider making this configurable per pair or per regime. Fast-reverting pairs
   (HL=2d) should use 2.0x (max_hold=4d). Slow pairs (HL=5d) are already capped at
   10d regardless.

---

## Summary of Findings

| Item | Severity | Status |
|------|----------|--------|
| A. Priority score != Avellaneda-Lee s-score | MEDIUM | Misleading citation. Formula is custom composite, not from the paper. |
| B. Rotation != Leung & Li (2015) | LOW | Creative adaptation of the economic intuition. Citation should say "inspired by". |
| C. Dollar-neutral P&L vs beta-weighted spread | HIGH | Signal and P&L are measuring different things. Mismatch grows with beta distance from 1.0. |
| D. 12 bps cost | LOW | Conservative by ~50-100%. Good for robustness. |
| E. Quality gate inconsistency | HIGH | Two main files use different R2 (0.70 vs 0.85) and HL (5.0 vs 4.0) thresholds. |
| F. 2.5x vs 2.0x half-life | LOW | Code is consistent at 2.5x. Docs/issues say 2.0x. Documentation drift only. |

### Priority actions (ordered by impact)
1. **Fix C**: Resolve the spread/P&L mismatch. Either beta-weight the position sizes
   or use a dollar-neutral spread for both signal and P&L. This is a correctness issue.
2. **Fix E**: Unify quality gate thresholds into a single config source. Divergent
   thresholds mean results from different scripts are not comparable.
3. **Fix A**: Correct the attribution in scorer.rs. The formula is fine as a ranking
   metric; it just shouldn't be called the "Avellaneda-Lee s-score".
4. **Fix B, F**: Update documentation to match code. Low priority but prevents future
   confusion.

---

## Sources
- Avellaneda, M. & Lee, J.-H. (2010). "Statistical Arbitrage in the US Equities
  Market." Quantitative Finance, 10(7): 761-782.
  https://math.nyu.edu/~avellane/AvellanedaLeeStatArb071108.pdf
- arbitragelab documentation on PCA/s-score:
  https://hudson-and-thames-arbitragelab.readthedocs-hosted.com/en/latest/other_approaches/pca_approach.html
- Leung, T. & Li, X. (2015). "Optimal Mean Reversion Trading with Transaction Costs
  and Stop-Loss Exit." IJTAF 18(03): 1-31. https://arxiv.org/abs/1411.5062
- Lee, Leung & Ning (2023). "Optimal Mean Reversion Trading with Transaction Costs."
- Alpaca fee schedule: https://alpaca.markets/support/commission-clearing-fees
- Krauss, C. (2017). "Statistical Arbitrage Pairs Trading Strategies: Review and
  Outlook." Journal of Economic Surveys, 31(2): 513-545.
