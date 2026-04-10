# Oracle Spec (Mode A) — locked v1

The oracle is the "cheater" — a deterministic Rust binary that runs the
simplest possible pair-trading strategy on full historical minute bars
and produces the **ground truth** classification for every ordered pair
in the universe.

Everything else (the pickers, autoresearch itself) is measured against
the oracle. If the oracle is wrong, every measurement is wrong. So this
spec is the single most important document in the rebuild.

**The oracle does not implement the strategy.** The strategy code already
lives in `engine/crates/core/src/pairs/` (see `PairState::on_price`). The
oracle binary imports that function directly and calls it for every pair,
one pair at a time, with the portfolio-level gates of `PairsEngine::on_bar`
bypassed. This guarantees zero drift between what the oracle measures and
what the real engine trades — they are literally the same Rust code. The
"Strategy" section below is therefore a *specification of what that code
already does*, not a second implementation to write.

---

## Principle

> The oracle has no intelligence. It has no statistics. It has no model.
> It runs a fixed strategy over historical data and reports what made money.

The oracle must be:

1. **Deterministic** — same input produces same output, forever
2. **Simple** — no parameters to tune beyond what's explicitly declared here
3. **Versioned** — if the strategy definition changes, it's a new oracle version
4. **Cheap** — ~minutes to run on the full S&P 500 universe
5. **Complete** — every ordered pair gets a verdict, no bias, no sector filter

---

## Inputs

- **Dataset**: `~/quant-data/bars/v1_sp500_2025-2026_1min/*.parquet`
  (regular session filter applied at read time: 13:30-20:00 UTC)
- **Universe**: all symbols with a parquet file in the dataset
  (no sector prefilter, no survivorship filter beyond the dataset itself)
- **Eval period**: passed as argument, e.g. `2025-09-01` → `2025-11-30`
  (inclusive, UTC)

For every ordered pair `(A, B)` where `A < B` lexicographically:
- Build the joined minute-bar stream on the intersection of timestamps
- Run the strategy (below)
- Record the verdict

---

## Strategy (v1 — fixed, never tuned)

```
LOG_SPREAD(t)        = ln(close_A(t)) − ln(close_B(t))
ROLLING_MEAN(t)      = mean of LOG_SPREAD over the last 30 bars
ROLLING_STD(t)       = std of LOG_SPREAD over the last 30 bars
Z(t)                 = (LOG_SPREAD(t) − ROLLING_MEAN(t)) / ROLLING_STD(t)
```

**Warm-up**: first 30 bars of each trading day have no Z (insufficient
rolling window). No entries during warm-up.

**Entry**:
- Enter SHORT (short A, long B) when `Z(t) >= 2.0` and no position open
  and not in warm-up
- Enter LONG (long A, short B) when `Z(t) <= -2.0` and no position open
  and not in warm-up
- Notional per leg: **$5000** per leg ($10k total per trade)

**Exit**:
- For a SHORT position: exit when `Z(t) <= 0.0`
- For a LONG position:  exit when `Z(t) >= 0.0`
- Also exit at the last bar of the trading day (no overnight positions)

**No stop-loss**. No max-hold timer. No intraday re-entry after exit. One
trade per pair per day maximum. Position holds until z crosses zero or
the day ends.

**Minimum hold**: 1 bar (i.e. you cannot enter and exit on the same bar).

**Costs**: **5 bps per side**, i.e. 10 bps round-trip. Applied as fixed
P&L deduction on exit. This matches limit-order execution (Phase 1D
finding) and the engine's current cost assumption.

**P&L calculation (per trade)**:
```
entry_spread_bps = LOG_SPREAD(entry) * 10000
exit_spread_bps  = LOG_SPREAD(exit)  * 10000
raw_bps = signed_by_direction(exit_spread_bps - entry_spread_bps)
  where signed_by_direction(x) = -x for SHORT, +x for LONG
net_bps = raw_bps - 10   # 10 bps round-trip cost
```

---

## Output schema (one row per pair)

For each ordered pair `(leg_a, leg_b)` in the universe, compute over the
eval period:

```
leg_a:                string
leg_b:                string
total_trades:         int
total_winning_trades: int
win_pct:              float (0.0 - 1.0)
total_net_bps:        int   (sum of net_bps across all trades)
avg_net_bps:          float (total_net_bps / total_trades)
median_hold_minutes:  float
max_hold_minutes:     int
max_drawdown_bps:     int
monthly_bps:          dict { "YYYY-MM": int }
classification:       "WINNER" | "AVERAGE" | "LOSER" | "INACTIVE"
```

Plus diagnostic metrics for downstream debugging — these are what the
pickers' scoring functions will be scored against:

```
observed:
  spread_std_bps:         float  (std of LOG_SPREAD * 10000 over eval period)
  crossings_per_day:      float  (count of zero-crossings / trading days)
  mean_reversion_minutes: float  (median minutes from ±2σ entry to 0 exit)
  adf_pvalue:             float  (ADF on full-period LOG_SPREAD)
  ou_theta:               float  (OU mean-reversion speed)
  ou_sigma:               float  (OU diffusion)
  median_volume_a_usd:    float  (median dollar volume per minute, leg A)
  median_volume_b_usd:    float  (median dollar volume per minute, leg B)
```

These are computed *from the data*, not from the strategy — they're there
so autoresearch can produce diagnostic reports like:
> "Bertram missed AAL/DAL — spread_std=12.3, crossings=18/day,
>  mean_reversion_minutes=14, OU theta=0.045. Bertram's score was
>  0.12 because it used 60-day OU fit on daily closes instead of
>  minute bars."

---

## Classification rules

Given a pair's monthly P&L over the eval period:

```
total_months = number of months with at least one trade
profit_months = number of months where monthly_bps > 0

WINNER:   total_net_bps > 300 AND profit_months / total_months >= 0.60
          AND total_trades >= 20
AVERAGE:  total_net_bps > 0 AND not WINNER
          AND total_trades >= 20
LOSER:    total_net_bps <= 0 AND total_trades >= 20
INACTIVE: total_trades < 20
```

The thresholds (300 bps total, 60% profit months, 20 trades) are arbitrary
but explicit. They live in this spec file and nowhere else. Changing them
= new oracle version.

---

## What the oracle does NOT do

- **Does not rank pairs**. Ranking is downstream eval work.
- **Does not filter by liquidity**. Every pair gets a verdict, even illiquid ones — the INACTIVE class handles low-activity pairs.
- **Does not apply regime detection**. The strategy is regime-naive.
- **Does not do multi-day position sizing**. Flat size, one trade per day.
- **Does not care about market-neutrality** beyond the flat dollar-matched legs.
- **Does not simulate Alpaca order rejections, partial fills, or slippage beyond the 5 bps assumption**. Pure limit-order model.

These are all deliberate simplifications. The oracle is the *floor* of
what any real strategy should achieve — if a picker finds pairs that the
oracle classifies as WINNER, those pairs make money under the simplest
possible rules. If the real engine has fancier logic that finds *more*
alpha, great. But the oracle is the baseline, not the ceiling.

---

## Versioning

This spec defines **oracle v1**. Any change to:
- The strategy definition (entry, exit, costs, z-score parameters)
- The classification thresholds
- The observed metrics schema
- The dataset version it reads

...is a new oracle version. Output goes to:

```
~/quant-data/oracle/v1_bars/v1_strategy_z30_entry2_exit0_cost5bps/verdicts.parquet
~/quant-data/oracle/v1_bars/v1_strategy_z30_entry2_exit0_cost5bps/MANIFEST.json
```

The MANIFEST pins the exact bars version + eval period + this spec file's
git hash. Reproducibility is non-negotiable.

---

## Locked decisions (v1)

1. **Entry threshold: 2.0** — textbook default, conservative baseline
2. **Notional per leg: $5,000** — matches Phase 1 math for the $1000/2wk goal
3. **Session filter: regular session only** (13:30-20:00 UTC)
4. **Classification thresholds: 300 bps / 60% profit-months / 20 trades** —
   ship v1 with these, calibrate after observing the distribution across
   the ~125k pair verdicts. Recalibration = oracle v2.
5. **One row per unordered pair `(A, B)` with A < B lexicographically** —
   strategy is symmetric, no information lost by skipping `(B, A)`

Any change to the above = new oracle version.
