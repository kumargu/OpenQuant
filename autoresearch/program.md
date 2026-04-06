# autoresearch

This is an experiment to have the LLM do its own quant research.

## Setup

To set up a new experiment, work with the user to:

1. **Agree on a run tag**: propose a tag based on today's date (e.g. `apr6`). The branch `autoresearch/<tag>` must not already exist — this is a fresh run.
2. **Create the branch**: `git checkout -b autoresearch/<tag>` from current main.
3. **Read the in-scope files**: Read these files for full context:
   - `autoresearch/SPEC.md` — design decisions and rules.
   - `config/pairs.toml` `[pairs_trading]` section — current strategy parameters.
   - `engine/crates/core/src/pairs/mod.rs` — PairState: spread computation, z-score, entry/exit logic, frozen exit context, regime gate, P&L tracking.
   - `engine/crates/core/src/pairs/engine.rs` — PairsEngine: manages multiple pairs, position cap, trade recording.
   - `engine/crates/pair-picker/src/pipeline.rs` — pair validation gates (ADF, R², half-life, structural break, regime robustness).
   - `engine/crates/pair-picker/src/scorer.rs` — pair scoring formula and component weights.
4. **Verify setup**: Run `python autoresearch/prepare.py` to confirm the engine builds and data exists.
5. **Initialize results.tsv**: Create `autoresearch/results.tsv` with just the header row. The baseline will be recorded after the first run.
6. **Confirm and go**: Confirm setup looks good.

Once you get confirmation, kick off the experimentation.

## Experimentation

Each experiment runs a historical replay over ~1 year (Apr 2025 – Mar 2026, ~252 trading days). Bars are cached locally after first fetch so subsequent experiments are fast. The replay uses the exact same `PairsEngine::on_bar()` code path as live trading — the engine doesn't know it's replaying. You launch it simply as: `python autoresearch/train.py > run.log 2>&1`.

**What you CAN do:**
- Modify `config/pairs.toml` `[pairs_trading]` section — entry_z, exit_z, stop_z, lookback, max_hold_bars, min_hold_bars. No build needed.
- Modify Rust source in `engine/crates/core/src/pairs/` — entry/exit logic, regime gate, position management. Requires build (handled by train.py).
- Modify Rust source in `engine/crates/pair-picker/src/` — validation gates, scoring formula, half-life estimation, ADF thresholds. Requires build.
- Modify `autoresearch/train.py` if you want to change how metrics are computed or add new measurements.

**What you CANNOT do:**
- Modify `autoresearch/prepare.py`. It is read-only.
- Modify `engine/crates/runner/src/` — the replay runner, data pipeline, and bar feeding are fixed.
- Modify `trading/active_pairs.json` directly — pair-picker regenerates it during replay.
- Change the replay date range (fixed at Q1 2026 in train.py).
- Add new Rust crate dependencies.

**The goal is simple: make money in any rolling 2-week window with $10K capital.** Not theoretical bps — actual dollar P&L. V1 showed +13.4 avg_bps but the live account LOST money because real costs > theoretical costs and $1K/leg was too small. V2 must close this gap.

The metric is still `avg_bps` from replay (it's what we can measure), but the TRUE test is: would this make dollars at $10K capital with realistic costs? The replay period is fixed, so you don't need to worry about date selection. Everything in the strategy and pair selection logic is fair game: change thresholds, add new gates, modify scoring, restructure entry/exit logic. The only constraint is that the code compiles, the replay runs without crashing, and there are enough trades (>=10) for the metric to be meaningful.

**Build time** is a soft constraint. Config-only changes need no build (instant). Rust changes take ~30-90 seconds incremental. Prefer config experiments when exploring a parameter space, save Rust changes for structural improvements.

**Simplicity criterion**: All else being equal, simpler is better. A small improvement that adds ugly complexity is not worth it. Conversely, removing a gate and getting equal or better results is a great outcome — that's a simplification win. When evaluating whether to keep a change, weigh the complexity cost against the improvement magnitude. A 0.5 bps improvement that adds 50 lines of hacky code? Probably not worth it. A 0.5 bps improvement from deleting dead code? Definitely keep. An improvement of ~0 but much simpler code? Keep.

**The first run**: Your very first run should always be to establish the baseline, so you will run the experiment as is.

## Output format

Once the script finishes it prints a summary like this:

```
---
avg_bps:      -2.1
trades:       47
sharpe:       -0.15
win_rate:     42.6
net_pnl_bps:  -98.7
max_dd_bps:   -234.5
build_secs:   4.2
replay_secs:  185.3
```

You can extract the key metric from the log file:

```
grep "^avg_bps:" run.log
```

## Logging results

When an experiment is done, log it to `autoresearch/results.tsv` (tab-separated, NOT comma-separated — commas break in descriptions).

The TSV has a header row and 5 columns:

```
commit	avg_bps	trades	status	description
```

1. git commit hash (short, 7 chars)
2. avg_bps achieved (e.g. 7.3) — use 0.0 for crashes
3. number of completed trades — use 0 for crashes
4. status: `keep`, `discard`, or `crash`
5. short text description of what this experiment tried

Example:

```
commit	avg_bps	trades	status	description
a1b2c3d	-2.1	47	keep	baseline (entry_z=1.0)
b2c3d4e	7.3	31	keep	raise entry_z to 2.0
c3d4e5f	4.1	28	discard	entry_z=2.5 (worse than 2.0)
d4e5f6g	0.0	0	crash	added dispersion gate (compile error)
```

## The experiment loop

The experiment runs on a dedicated branch (e.g. `autoresearch/apr6`).

LOOP FOREVER:

1. **RESEARCH** — Before touching code, think like a quant researcher:
   - Read results.tsv: what experiments worked? What didn't? Why?
   - Analyze the replay.log: which pairs are losing money? Which exit reasons dominate? Are stop losses clustered on certain dates (regime events)?
   - Form a hypothesis grounded in quant finance literature (Gatev et al. 2006, Vidyamurthy 2004, Avellaneda & Lee 2010, de Prado 2018). Don't just try random numbers — reason about WHY a change should improve avg_bps.
   - Read the relevant source code to understand the current behavior before changing it.
   - Write your hypothesis in the experiment description (results.tsv column 5).
2. **IMPLEMENT** — Edit config and/or Rust source based on your hypothesis.
3. git commit
4. Run the experiment: `python autoresearch/train.py > run.log 2>&1` (redirect everything — do NOT use tee or let output flood your context)
5. Read out the results: `grep "^avg_bps:\|^trades:\|^sharpe:" run.log`
6. If the grep output is empty or shows `BUILD FAILED`, the run crashed. Run `tail -n 50 run.log` to read the error and attempt a fix. If you can't get things to work after more than a few attempts, give up on this idea.
7. Record the results in the tsv (NOTE: do not commit the results.tsv file, leave it untracked by git)
8. If avg_bps improved (higher) AND trades >= 10, you "advance" the branch, keeping the git commit
9. If avg_bps is equal or worse, or trades < 10, you git reset back to where you started
10. **IMPORTANT**: When reverting, KEEP any logging/observability improvements you added. Cherry-pick or re-apply log additions separately. Don't throw away visibility just because the experiment's hypothesis failed.

The idea is that you are a completely autonomous quant researcher trying things out. If they work, keep. If they don't, discard. And you're advancing the branch so that you can iterate. If you feel like you're getting stuck in some way, you can rewind but you should probably do this very very sparingly (if ever).

**Timeout**: Each experiment should take ~3-5 minutes (replay time depends on Alpaca API speed). If a run exceeds 15 minutes, kill it and treat it as a failure (discard and revert).

**Build failures**: If `cargo build` fails, read the error. If it's a typo or missing import, fix and retry. If the approach is fundamentally wrong, revert and try something else.

**NEVER STOP**: Once the experiment loop has begun (after the initial setup), do NOT pause to ask the human if you should continue. Do NOT ask "should I keep going?" or "is this a good stopping point?". The human might be asleep, or gone from a computer and expects you to continue working *indefinitely* until you are manually stopped. You are autonomous. If you run out of ideas, think harder — use your knowledge of quant finance papers (Gatev, Vidyamurthy, Avellaneda & Lee, de Prado), study the code for inefficiencies, try combining previous near-misses, try more radical structural changes. The loop runs until the human interrupts you, period.

**LOGS ARE TRUTH**: This is not a prototype. This engine manages real money. Hiding problems behind clean metrics while the account bleeds is how quant funds blow up.

Rules for observability:
- If you can't tell WHY the engine is behaving a certain way, add `tracing::info!` with structured fields (key=value).
- When you see a `warn!` in the log, investigate it DEEPLY before moving on. Warnings are symptoms of real problems.
- When you see an `error!`, DEBUG it. Don't accept "it happens sometimes." Find the root cause.
- Add `tracing::error!` with a `bug=true` field for states that should NEVER happen — impossible z-scores, negative prices, NaN spreads, positions that appear from nowhere. These are `bug!` moments that demand immediate investigation.
- Every experiment: grep the replay log for WARN and ERROR lines. If they're new, understand them before evaluating avg_bps.
- avg_bps means NOTHING if the account is losing money. The dollar P&L is the only truth. If bps and dollars disagree, the dollars are right and the bps calculation is lying.

**EXPAND THE PLAN**: The experiment list in research_brain.md is a starting point, not a ceiling. If during your experiments you discover a new gap, a new technique, or a better approach — add it. Update research_brain.md with new findings. Reorder priorities based on what you learn. The V2 plan is a living document that evolves as you experiment. You are the researcher — if the data tells you something unexpected, follow it.

As an example use case, the user might leave you running while they sleep. Each experiment takes ~3-5 minutes, so you can run approx 12-20 per hour, for a total of 100-160 over 8 hours. The user wakes up to a results.tsv with dozens of experiments, sorted by what worked and what didn't.

## Domain context

### How pairs trading works

The strategy trades the spread between two correlated stocks:

```
spread = ln(price_A) - alpha - beta * ln(price_B)
z = (spread - rolling_mean) / rolling_std

z < -entry_z  →  LONG spread:  BUY A, SELL B
z > +entry_z  →  SHORT spread: SELL A, BUY B
|z| < exit_z  →  CLOSE (spread reverted to mean)
|z| > stop_z  →  STOP LOSS (spread diverged further)
days_held >= max_hold_bars → FORCE EXIT
```

Entry uses rolling z-score (adapts to market). Exit uses frozen entry-time mean/std (prevents drift-induced exits).

### How pairs are selected (pair-picker)

13,573 same-sector S&P 500 pairs are validated through a pipeline:
1. OLS regression → R² ≥ 0.30 (MIN_R_SQUARED in pipeline.rs)
2. Engle-Granger ADF test → p-value < 0.05 (hardcoded)
3. OU half-life → must be 1-40 days (MIN/MAX_HALF_LIFE)
4. Beta stability → rolling CV ≤ 0.35, structural break detection
5. Regime robustness → cointegrated in both calm and volatile markets
6. Composite scoring → weighted: cointegration 35%, half-life 25%, beta stability 25%, R² 15%
7. Top 40 pairs selected for trading

### What's losing money (Q1 2026)

- **Jan-Feb**: -$5,159. Low intra-sector correlation. Spreads trended instead of reverting. 29% win rate.
- **March**: +$2,375. High intra-sector correlation. Spreads reverted. 63% win rate.
- Same pairs, same logic — the market environment changed.

### V1 results (SOLVED — do not re-test these)

V1 ran 12 experiments and improved avg_bps from -4.8 to +13.4. These are SETTLED:
- entry_z=2.0 is optimal (1.5 is dead zone, 2.5 too few trades)
- exit_z=0.5, stop_z=6.0 is optimal
- lookback floor=20 is optimal (15 is too noisy)
- ADF p<0.10 calm, p<0.05 volatile is optimal
- Structural break gate stays (removing it admitted broken pairs)
- Scoring weights stay at 35/25/25/15 (HL-heavy was worse)
- min_hold=1 is optimal (3 forces winners into losers)

### V2 research targets (from research_brain.md — ranked by impact)

Read `autoresearch/research_brain.md` for full details on each gap. Summary:

0. **Cost validation + notional sizing** (CRITICAL, DO THIS FIRST) — engine reports positive bps but account loses money. Real Alpaca costs >> 10 bps. At $1K/leg, bid-ask alone eats the edge. Steps:
   - Increase cost_bps from 10 to 30-50 (realistic for Alpaca) and re-run replay
   - With $10K capital: either 5 pairs × $1K/leg or 10 pairs × $500/leg
   - Find the notional + cost_bps combo where replay P&L is still positive in DOLLARS
   - Update config accordingly before any structural experiments
1. **Sector dispersion gate** (~50 LOC) — suppress entries when sector correlation is low. Directly fixes Jun/Sep/Nov losses.
2. **Spread crossing frequency filter** (~10 LOC) — reject pairs with <12 annual zero-crossings. Fixes 88% non-trade problem.
3. **Same-company exclusion** (~10 LOC) — GOOG/GOOGL, BRK.A/BRK.B are not real pairs trades.
4. **Spread CUSUM** (adapt existing cusum.rs) — detect cointegration breakdown during live trading. Cuts 30-50% stop losses.
5. **RVOL entry gate** (~20 LOC) — block entries on stale (RVOL<0.3) or event-driven (RVOL>3.0) volume.
6. **Cut portfolio to 20 pairs** (config change) — fewer, higher-quality pairs.
7. **Kalman filter hedge ratio** (~150 LOC) — dynamic beta updated every bar.
8. **Per-pair optimal entry_z** (~200 LOC) — minimum profit condition, replaces one-size-fits-all threshold.
9. **Hurst exponent** (~100 LOC) — H<0.5 predicts faster reversion, augments ADF.
10. **BOCPD on spread** (crate integration) — early warning of regime shifts.
