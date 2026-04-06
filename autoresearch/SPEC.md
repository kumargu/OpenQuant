# OpenQuant Autoresearch — Integration Spec

## Rules

1. **Commit locally only** — never push unless Gulshan explicitly asks
2. **Never merge** — let Gulshan merge
3. **Use autoresearch's actual code** — adapt train.py/prepare.py/program.md, don't build custom frameworks
4. **Replay is the test** — same PairsEngine::on_bar() code path as live trading. No separate backtest
5. **All math in Rust** — Python is plumbing only
6. **No live trading** — replay mode only, no Alpaca orders
7. **Don't reinvent** — if autoresearch already handles it (git loop, TSV logging, branching), use it as-is
8. **Both pair-picker AND engine are research targets** — the LLM modifies both
9. **Bar cache correctness > speed** — stale/corrupt data is worse than slow experiments

## How autoresearch works (unchanged)

```
autoresearch/
  program.md    — LLM reads this, follows the loop
  prepare.py    — one-time setup (READ-ONLY)
  train.py      — LLM modifies this, runs experiments
  results.tsv   — experiment log (untracked)
```

The loop (from program.md — we keep this EXACTLY):
1. LLM reads program.md + train.py
2. LLM edits train.py
3. git commit
4. Run: `uv run train.py > run.log 2>&1`
5. Extract: `grep "^val_bpb:" run.log`
6. If improved → keep. If worse → git reset
7. Log to results.tsv
8. NEVER STOP

## Our adaptation

### What changes

| autoresearch file | What we change | Why |
|---|---|---|
| `train.py` | Replace ML training with: build runner + run replay + print metrics | Our "training" is a replay experiment |
| `prepare.py` | Replace data download with: verify Rust builds, verify Alpaca keys, verify data exists | Our "data prep" is ensuring the engine compiles |
| `program.md` | Replace ML context with quant context | Domain-specific research brief |
| `pyproject.toml` | Minimal — we don't need torch for replay | Only need deps for metric extraction |
| `results.tsv` | Same format, different columns | `avg_bps` instead of `val_bpb` |

### What stays exactly the same

- The experiment loop structure in program.md
- Git branching (`autoresearch/<tag>`)
- TSV logging format (5 columns, tab-separated)
- Keep/discard/crash logic
- `NEVER STOP` autonomous execution
- `results.tsv` untracked by git
- Redirect output: `> run.log 2>&1`
- Extract metric via grep

## File mapping

### `train.py` → runs one experiment

In autoresearch: trains a GPT for 5 min, prints val_bpb.
For us: builds runner (if Rust changed), runs replay, parses EXIT logs, prints avg_bps.

The LLM modifies the **strategy code** (config + Rust), NOT train.py itself.
train.py is the harness that runs the experiment and reports the metric.

BUT — following autoresearch pattern — if the LLM wants to change what
the experiment measures or how it runs, it CAN modify train.py. The key
constraint is that `prepare.py` is read-only.

### `prepare.py` → one-time environment setup

In autoresearch: downloads data shards, trains BPE tokenizer.
For us: verifies Rust toolchain, builds the runner, verifies Alpaca API keys exist.

READ-ONLY. The LLM never modifies this.

### `program.md` → research brief

Same structure as Karpathy's. Sections:
1. Setup (branch, read files, verify, init TSV, go)
2. Experimentation (what you CAN/CANNOT do, the goal, simplicity criterion)
3. Output format (the `---` block with metrics)
4. Logging results (TSV format)
5. The experiment loop (LOOP FOREVER)
6. Domain context (how pairs trading works, what's losing money)

## Metric

**Primary: `avg_bps`** — average net P&L per trade in basis points. Higher is better.
(Opposite direction from val_bpb which is lower-is-better.)

Extracted from train.py output:
```
grep "^avg_bps:" run.log
```

Printed in the same `key: value` format as autoresearch:
```
---
avg_bps:      7.3
trades:       31
win_rate:     61.3
net_pnl_bps:  226.3
sharpe:       1.23
max_dd_bps:   -89.2
replay_secs:  185
```

## What the LLM modifies (scope)

**Config (no build needed):**
- `config/pairs.toml` [pairs_trading] — entry_z, exit_z, stop_z, lookback, max_hold_bars

**Rust source (requires cargo build):**
- `engine/crates/core/src/pairs/mod.rs` — PairState, entry/exit logic
- `engine/crates/core/src/pairs/engine.rs` — PairsEngine, position management
- `engine/crates/pair-picker/src/pipeline.rs` — validation gates
- `engine/crates/pair-picker/src/scorer.rs` — scoring formula
- `engine/crates/pair-picker/src/regime.rs` — regime robustness
- `engine/crates/pair-picker/src/stats/` — ADF, half-life, OLS

## Implementation status

All done:
1. `prepare.py` — verifies env, builds runner, checks Alpaca keys
2. `train.py` — builds, runs replay with --bar-cache, parses EXIT logs, prints avg_bps
3. `program.md` — quant domain context with research phase in the loop
4. `net_bps` added to EXIT log line in pairs/mod.rs
5. `bar_cache.rs` — file-based cache with integrity headers, write-once, atomic writes
6. `--bar-cache` flag wired into replay runner
7. End-to-end tested: `python3 autoresearch/prepare.py` then `python3 autoresearch/train.py`
8. Baseline: avg_bps=-5.7 (Q1 2026), avg_bps=8.0 (March only)

## Cache integrity

Bar cache protects against corruption:
- **Write-once**: existing files never overwritten
- **Atomic writes**: .tmp file → rename (crash leaves .tmp, not corrupt cache)
- **Header check**: first line is `#bars:N` — if bar count doesn't match, file deleted
- **Parse check**: any unparseable JSON line → file deleted as corrupt
- **Layout**: `data/bar_cache/minute/{SYMBOL}/{YYYY-MM-DD}.jsonl`

## V2 Run Plan

### Pre-flight (after market close)
```bash
# 1. Stop paper trading (kill the running process)
# 2. Check today's paper P&L
# 3. Cut V2 branch
git checkout -b autoresearch/apr6V2
# 4. Init fresh results.tsv with V1 best as baseline
# 5. Start the loop
python3 autoresearch/train.py > run.log 2>&1
```

### V2 experiment order (from research_brain.md)
0. Cost validation + notional sizing — CRITICAL: bps > 0 but $ < 0. Real costs >> 10 bps. Find minimum viable notional.
1. Sector dispersion gate — directly fixes seasonal losses
2. Spread crossing frequency filter — fixes 88% non-trade problem
3. Same-company exclusion — removes GOOG/GOOGL noise
4. Spread CUSUM — catches decoupling before stop loss
5. RVOL entry gate — blocks stale/event entries
6. Cut portfolio to 20 — fewer better pairs
7. Kalman filter — dynamic hedge ratio
8. Per-pair optimal entry_z — minimum profit condition
9. Hurst exponent — augment ADF
10. BOCPD on spread — early regime warning

### Key differences from V1

**Scope:** V1 was config knob turning. V2 is structural Rust changes — new gates, filters, algorithms.

**Metric:** V1 optimized avg_bps (theoretical). V2 optimizes for dollar profit in a rolling 2-week window with $10K capital and realistic costs. avg_bps is still measured but the real test is: does this make money in dollars?

**Capital:** $10K daily. Size positions accordingly (fewer pairs, larger per-leg notional).

**Goal:** Net positive P&L in any rolling 2-week window. Not every day — some days will lose. But the 2-week trend must be up.

**Costs:** cost_bps must be validated against real Alpaca fills (likely 30-50 bps, not 10).

Each experiment requires `cargo build --release` (30-90s).
The LLM reads research_brain.md before EVERY experiment to stay focused.

### Quant model (parked for V3)
Trained 11M param GPT on 104MB quant corpus (val_bpb=1.589).
Model learns quant text patterns but can't reason at this size.
Checkpoint at: ~/.cache/autoresearch-quant/model.pt
Corpus at: autoresearch/corpus/ (arXiv q-fin + our research)
V3: fine-tune 7B model on growing corpus for actual Q&A.
