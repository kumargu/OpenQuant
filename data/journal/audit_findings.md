# Simulation Audit Findings — 2026-03-26

Files audited:
- `scripts/capital_sim.py`
- `scripts/daily_walkforward_dashboard.py`

---

## BUG-1 (CRITICAL — P&L error): `compute_trade_pnl` in `daily_walkforward_dashboard.py` still has beta in the P&L formula

**File:** `scripts/daily_walkforward_dashboard.py`, lines 344–347

**What the bug is:**
`compute_trade_pnl` (used by `daily_walkforward_dashboard.py`'s `run_simulation`) scales the B-leg return by `trade.pair.beta`:

```python
# direction == 1: long spread
pnl = capital * ret_a - capital * trade.pair.beta * ret_b
# direction == -1: short spread
pnl = -capital * ret_a + capital * trade.pair.beta * ret_b
```

`capital_sim.py`'s `compute_pnl` was fixed to remove beta from P&L (both legs get equal dollar exposure). But `daily_walkforward_dashboard.py`'s function was NOT updated — it still multiplies the B-leg return by beta.

**What the correct behavior should be:**
For a dollar-neutral trade, both legs get exactly `capital` dollars of exposure. The P&L is:
```python
pnl = capital * ret_a - capital * ret_b   # direction == 1
pnl = -capital * ret_a + capital * ret_b  # direction == -1
```
Beta drives the entry signal (z-score) but not the P&L calculation.

**P&L impact:**
For a typical beta of ~0.6 (low-beta pair), the B-leg P&L contribution is compressed by 40%. For a winning trade where B gives +1% and capital=$500: correct B contribution = $5.00; buggy B contribution = $3.00. This is a ~$2/trade systematic understatement of P&L on the B leg for direction=1 winners, and over-statement for direction=-1. The net direction of bias depends on the beta distribution, but for beta < 1 pairs it systematically understates total P&L when A and B move in the expected direction. **This is a separate simulation from `capital_sim.py` so it may explain divergent results between the two sims.**

---

## BUG-2 (MODERATE — incorrect exit condition): Reversion exit condition has wrong sign for direction=1

**File:** `scripts/capital_sim.py`, line 216; `scripts/daily_walkforward_dashboard.py`, line 425

**What the bug is:**
The exit condition for `direction == 1` (long A, short B — entered because z was very negative, i.e., A is cheap relative to B) is:

```python
elif trade.direction == 1 and z > -eff_exit:
```

When we enter `direction=1`, z was below `-entry_z` (e.g., z < -1.0). Reversion means z moves back toward 0. We want to exit when `z >= -eff_exit` (z has reverted enough, e.g., z >= -0.3, meaning |z| < 0.3). The condition `z > -eff_exit` fires when z is greater than negative 0.3, i.e., z > -0.3. This includes z = 0 and z = +1.0, which is correct for the reversion case.

However: when `eff_exit = 0.3` (floor), the condition becomes `z > -0.3`. But the spread can overshoot to the positive side (z = +0.5) without triggering exit, because we check `z > -eff_exit` not `|z| < eff_exit`.

**Actually,** this is only half the exit logic — we should also exit when z overshoots on the other side (z becomes very positive after entering long when z was very negative). A trade entered at z = -2.0 that overshoots to z = +2.5 is never exited by the reversion condition (since z > -0.3 fires correctly) — wait, it IS exited because z > -0.3 is TRUE at z = +2.5. So the condition is actually correct for catching both reversion AND overshoot.

**Revised assessment:** The condition is correct — it exits on either reversion OR overshoot for direction=1. No change needed. Strike this item.

---

## BUG-3 (MODERATE — capital leak on rotation): Rotation exits do not remove trades before re-computing available capital for signal re-capping

**File:** `scripts/capital_sim.py`, lines 411–428

**What the bug is:**
When rotation occurs, rotated trades are removed from `open_trades` (line 413). Then `signals` is re-filtered and re-capped (lines 419–429). The re-cap formula re-reads `available_capital / 2` which was already incremented by freed capital. This part is correct.

However, the `min_capital_leg` in the re-cap uses raw Python `max(pa, pb * abs(params.beta), MIN_TRADE_CAPITAL)` (line 423). For high-priced stocks (e.g., NVDA at ~$900), `min_capital_leg` could exceed `available_capital / 2`, silently dropping signals. This is actually correct defensive behavior, but the issue is that `pa` and `pb` used in the re-cap lambda (line 423) were captured from the pre-rotation scan when the signal was first computed. These are from earlier in Step 2, not re-fetched at rotation time. If prices moved significantly since then (intraday sim doesn't apply, but the prices array is by day), they are the same day's prices, so this is a non-issue in daily sim. Mark as low severity.

**P&L impact:** Negligible in daily sim.

---

## BUG-4 (MODERATE — misleading metric): `ret_per_trade` in `summarize()` uses an arbitrary divisor

**File:** `scripts/capital_sim.py`, line 735

**What the bug is:**
```python
"ret_per_trade": all_pnl / (n_trades * total_capital / 10) if n_trades else 0,
```
`total_capital / 10` = $1,000 is used as the implied capital per trade. But the actual `capital_per_leg` from pair_portfolio.json defaults to $500/leg = $1,000 total. At first glance this looks right, but if any pair has `capital_per_leg=1000` (NVDA/AMD does), the divisor is wrong for those trades. The metric is displayed in logs/dashboard but not the headline number — it's just confusing.

**What the correct behavior should be:**
Use the actual `capital_used` per closed trade: `all_pnl / sum(t.capital_used * 2 for t in closed)` as a fraction, or use `cm_all['avg_return_per_trade']` from Rust which is already computed correctly.

**P&L impact:** Does not affect actual P&L, only a derived display metric.

---

## BUG-5 (MODERATE — wrong formula direction): `compute_z` uses formation-window `spread_mean` for standardization — but `spread_mean` is computed over the last 30 days of the formation window, not the full 90-day window

**File:** `scripts/daily_walkforward_dashboard.py`, lines 307–310

**What the bug is:**
In `scan_pair`, the spread mean and std used for z-scoring are computed over only the last 30 days of the 90-day formation window:
```python
window = spread[-30:]
mean = sum(window) / len(window)
std = math.sqrt(sum((s - mean) ** 2 for s in window) / (len(window) - 1))
```

The `PairParams` stores these 30-day stats. Then `compute_z` uses them to z-score current prices:
```python
spread = math.log(price_a) - params.alpha - params.beta * math.log(price_b)
return (spread - params.spread_mean) / params.spread_std
```

But `alpha` and `beta` are fitted on the full 90-day OLS window, while `spread_mean` is from only the last 30 days. If there is a trend in the spread during the formation period, the last-30-day mean will be biased away from the full-window mean. The z-score will not center around zero correctly.

**What the correct behavior should be:**
Either:
- Compute `spread_mean` from the full 90-day spread (consistent with the OLS fit), OR
- Accept the 30-day window as an intentional recency bias (it makes the signal more reactive)

The current choice is neither documented nor named — it's a silent mismatch between the OLS window and the z-score normalization window. This should be explicitly documented or unified.

**P&L impact:** Biases z-scores for pairs with trending spreads. Could cause false signals (z appears large when it's actually just the trend in the last 30 days). Difficult to quantify without data, but likely contributes to `max_hold` exits on pairs that never revert because the entry signal was biased.

---

## BUG-6 (LOW — dead code): `ENTRY_Z_CAP = 3.0` is defined but never used

**File:** `scripts/daily_walkforward_dashboard.py`, line 55

**What the bug is:**
```python
ENTRY_Z_CAP = 3.0         # |z| > this is structural break, NOT reversion (research #192)
```
This constant is defined and has a comment referencing research, but it is never referenced anywhere in the file. The actual cap logic uses `p_entry_z + 1.5` (line 513), which equals 2.5 when `entry_z=1.0`. So the effective cap is 2.5, not 3.0.

**What the correct behavior should be:**
Either replace the hardcoded `+ 1.5` with `ENTRY_Z_CAP - p_entry_z` (using the named constant), or delete the constant. The current state means the constant is misleading — it says 3.0 but the code enforces 2.5.

**P&L impact:** None on P&L. Confusing for readers.

---

## BUG-7 (LOW — dead code): `rolling_z` computation in `run_simulation` is O(n) per trade per day but only used in a debug log line

**File:** `scripts/daily_walkforward_dashboard.py`, lines 394–406

**What the bug is:**
For each open trade on each day, the code loops over 30 prior days to compute a rolling z-score:
```python
start = max(0, day - 30)
recent = []
for ii in range(start, day + 1):
    ...
    recent.append(s)
```
Then `rolling_z` is only used in:
```python
logger.debug(f"... rolling_z={rolling_z:.4f} | drift={abs(z - rolling_z):.4f} ...")
```
It is never used in any exit decision. This is O(30 × n_open_trades) per day of pure wasted computation that was added experimentally and never wired into logic.

**What the correct behavior should be:**
Remove the `rolling_z` block entirely (lines 394–406), or wrap it in `if logger.isEnabledFor(logging.DEBUG):` so it only runs when debug logging is actually active.

**P&L impact:** None on P&L. Performance regression: ~30 price lookups + sqrt per open trade per day.

---

## BUG-8 (LOW — dead code / hardcoded magic number): `pairs_scanned=29` is hardcoded

**File:** `scripts/daily_walkforward_dashboard.py`, line 557

**What the bug is:**
```python
pairs_scanned=29, pairs_selected=n_selected,
```
The `pairs_scanned` field in `DayRecord` is hardcoded to 29. This was presumably the number of candidates when the code was written, but the universe may change. The actual number of candidates iterated is `len(candidates)` which is passed in.

**What the correct behavior should be:**
Replace `29` with `len(candidates)` or count actual scan iterations.

**P&L impact:** None on P&L. Incorrect diagnostic data in `DayRecord`.

---

## BUG-9 (LOW — divergent configs): `capital_sim.py` and `daily_walkforward_dashboard.py` use different quality gate thresholds for entry

**File:** `scripts/capital_sim.py`, lines 80–82 vs `scripts/daily_walkforward_dashboard.py`, lines 62–64

**What the bug is:**

| Threshold | `capital_sim.py` | `daily_walkforward_dashboard.py` |
|---|---|---|
| `MIN_R2_ENTRY` | 0.70 | 0.85 |
| `MAX_HL_ENTRY` | 5.0 | 4.0 |
| `MIN_ADF_ENTRY` | -2.5 | -2.5 |

The two simulations use different quality gates. `capital_sim.py` is more permissive (R²>0.70, HL<5.0) while `daily_walkforward_dashboard.py` is stricter (R²>0.85, HL<4.0). This means the two sims see different universes of valid signals and cannot be directly compared.

Also: `daily_walkforward_dashboard.py`'s `scan_pair` hard-filters `hl > 5.0` at scan time (line 299), but `capital_sim.py` imports `scan_pair` from it and then applies its own `MAX_HL_ENTRY = 5.0` filter. So the effective filter for `capital_sim.py` is `hl > 5.0` (from scan_pair's hard filter), meaning `capital_sim.py`'s own `MAX_HL_ENTRY = 5.0` is redundant but aligned.

However, the R² gate differs: `scan_pair` uses `MIN_R2 = 0.30` at scan time. Then `capital_sim.py` applies `MIN_R2_ENTRY = 0.70` and `daily_walkforward_dashboard.py` applies `MIN_R2_ENTRY = 0.85` at entry time. These two filters produce genuinely different results.

**What the correct behavior should be:**
The entry-time quality gates should either be unified or there should be a single source of truth (e.g., in `pair_portfolio.json` defaults). The divergence makes A/B comparisons across scripts meaningless.

**P&L impact:** `capital_sim.py` allows weaker-R² pairs in, which likely increases trade count but reduces win rate. The $26/day result from `capital_sim.py` vs whatever `daily_walkforward_dashboard.py` shows cannot be compared directly.

---

## MISSING LOGGING — Items to add (no P&L impact)

### LOG-1: No trace ID on trades
**File:** `scripts/capital_sim.py`, lines 437–455

Each `Trade` has no unique ID. The `HOLD`, `EXIT`, `ENTER`, and `ROT_CHECK` log lines all use `{trade.pair_id()}` which is just `NVDA/AMD`. If the same pair opens and closes multiple times, log lines are ambiguous — you cannot tell which "NVDA/AMD trade" a HOLD line belongs to.

**Fix:** Add `trade_id: str` field to `Trade` dataclass, set at entry time as `f"{leg_a}/{leg_b}:{day}"`, and include it in all log lines.

### LOG-2: No log when a signal is skipped because `actual_capital * 2 > available_capital`
**File:** `scripts/capital_sim.py`, line 309

The check `if actual_capital * 2 <= available_capital:` silently drops a signal if capital is unavailable. There is no log line for this rejection. This is important diagnostic information — it tells us how often we have a valid signal but can't act on it.

**Fix:** Add a `logger.debug` line before the `if` block showing the signal was rejected due to capital constraints.

### LOG-3: No log when entry is skipped because a different signal already consumed available capital (Step 3 loop break)
**File:** `scripts/capital_sim.py`, line 434

```python
if available_capital < capital * 2:
    break
```
This silently stops entering new trades. A log line here would show how often good signals are left on the table due to capital exhaustion.

### LOG-4: No log when z-cap rejects a signal (z too extreme)
**File:** `scripts/capital_sim.py`, line 298

```python
if abs(z) > entry_z and abs(z) < entry_z + 1.5:
```
Signals with `|z| >= entry_z + 1.5` are silently dropped. These are potentially the most interesting events (possible structural break or data error). They should be logged at DEBUG level.

### LOG-5: No daily log line showing how many pairs passed quality gate vs were rejected
**File:** `scripts/capital_sim.py`, Step 2 scan loop (lines 252–324)

The scan loop rejects pairs for: missing prices, earnings blackout, `params is None`, beta < 0.1, beta instability, quality gate (r2/hl/adf), capital insufficient, z outside band. Each rejection type has a debug log, but there is no end-of-scan summary (e.g., "Day 150: scanned 41 pairs, 12 passed quality gate, 3 had valid z, 2 had sufficient capital, 1 entered"). This daily funnel would pinpoint whether the $26/day is from too few signals (funnel too narrow) or poor signal quality (signals exist but revert slowly).

---

## ROOT CAUSE HYPOTHESIS for $26/day on $10K

Based on the audit, the most likely causes of low P&L:

1. **BUG-1 (highest impact):** `daily_walkforward_dashboard.py` still has beta in P&L. If you are quoting the $26/day from that script, the number is understated by `(1 - beta)` on the B-leg for winning direction=1 trades. With typical beta=0.6, B-leg P&L is 40% of what it should be.

2. **Utilization:** With 41 pairs but $500/leg default and $10K pool, you can run up to 10 simultaneous pairs (`10K / ($500*2) = 10`). But quality filters (R²>0.70, HL<5.0) and z-band (1.0–2.5) likely only fire 1–3 signals/day. Low utilization directly explains low absolute P&L. The opportunity cost metric from Rust (`cm['opportunity_cost']`) quantifies this.

3. **BUG-5 (spread_mean bias):** If the 30-day trailing mean is biased, z-scores are miscalibrated, leading to entries that never revert within max_hold — max_hold exits drag down the average.

4. **Entry z = 1.0:** With entry_z=1.0 (very low threshold), the system enters at weak deviations. At $500/leg and z=1.0 (one standard deviation), the expected reversion P&L is roughly `sigma_spread * capital * 1 sd reversion`. For a typical spread_std of 0.03 log-points and $500/leg, that's $15 gross per trade, minus $12 cost (12bps round-trip) = ~$3 net. This is extremely thin.

---

## Summary Table

| ID | Severity | File | Lines | Affects P&L | Fix Priority |
|---|---|---|---|---|---|
| BUG-1 | CRITICAL | dashboard.py | 344–347 | YES — beta in P&L formula | Fix immediately |
| BUG-2 | N/A | both | exit logic | No (logic is correct) | No action |
| BUG-3 | LOW | capital_sim.py | 411–428 | Negligible | Low |
| BUG-4 | LOW | capital_sim.py | 735 | Display only | Low |
| BUG-5 | MODERATE | dashboard.py | 307–310 | YES — biased z-scores | Investigate |
| BUG-6 | LOW | dashboard.py | 55 | No | Remove dead code |
| BUG-7 | LOW | dashboard.py | 394–406 | No (perf waste) | Remove dead code |
| BUG-8 | LOW | dashboard.py | 557 | No | Trivial fix |
| BUG-9 | MODERATE | both | config | YES — different universes | Unify configs |
| LOG-1 | — | capital_sim.py | Trade dataclass | No | Add trade_id |
| LOG-2 | — | capital_sim.py | 309 | No | Add debug log |
| LOG-3 | — | capital_sim.py | 434 | No | Add debug log |
| LOG-4 | — | capital_sim.py | 298 | No | Add debug log |
| LOG-5 | — | capital_sim.py | scan loop | No | Add daily funnel log |
