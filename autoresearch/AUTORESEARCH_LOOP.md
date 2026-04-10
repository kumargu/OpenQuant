# Autoresearch — the whole system

There is **one program**: autoresearch.

It runs the oracle when needed, invokes picker binaries, scores them
against the oracle verdicts, writes a row to the leaderboard, appends
to the notebook, commits to git, and loops. That's it. No separate
"harness." No "experiment runner service." No layered Python modules
calling each other. One process, one responsibility: get better
pickers by running experiments.

---

## What autoresearch is and isn't

**IS:**
- A single Python program at `autoresearch/autoresearch.py`
- The only code that reads the leaderboard, writes the leaderboard, updates the notebook, or commits to git
- The thing the human interacts with ("run autoresearch", "stop autoresearch")
- The thing an agent (Claude) drives when autonomous mode is turned on

**IS NOT:**
- A library of reusable "experiment" utilities
- A harness with a `run(spec) → row` API
- A plugin system
- Anything that another Python file imports from

If you find yourself wanting to import something from autoresearch into
another Python file, you're recreating the harness abstraction. Don't.

---

## The only things autoresearch talks to

1. **Dataset** (read-only)
   `~/quant-data/bars/v1_sp500_2025-2026_1min/*.parquet`
   Accessed via the small `autoresearch/dataset.py` reader. Never written.

2. **Oracle binary** (Rust)
   `cargo run -p core --bin oracle -- ...`
   Reads dataset, uses real `PairState::on_price`, writes
   `~/quant-data/oracle/<version>/verdicts.parquet`.
   Autoresearch runs it once per oracle version, then caches.

3. **Picker binaries** (Rust)
   `cargo run -p pair-picker --bin picker-score -- --picker X --params Y ...`
   Reads dataset, runs scoring, writes a ranking parquet to a temp file.
   Autoresearch spawns this per experiment, reads the output, discards
   the temp file.

4. **Leaderboard** (parquet, append-only)
   `~/quant-data/experiments/v1.parquet`
   Autoresearch reads it at the start of every iteration (to know what's
   been tried) and appends one row at the end of every iteration.

5. **Notebook** (markdown, append-only)
   `autoresearch/NOTEBOOK.md`
   Human-readable log. Autoresearch writes a block per iteration.

6. **Directive** (one-line text file, human-owned)
   `autoresearch/HUMAN_DIRECTIVE.md`
   Where the human sets the current goal or says "STOP". Autoresearch
   reads it at the start of every iteration.

7. **Git** (workspace)
   Autoresearch commits any new picker Rust files it wrote during an
   iteration, using the experiment_id in the commit message. Never
   pushes.

That's the entire interface surface. Seven things. All of them are
read or invoked from one file: `autoresearch.py`.

---

## What autoresearch does, every iteration

```
┌────────────────────────────────────────────────────────────┐
│  autoresearch.py  (one program, one loop)                  │
│                                                            │
│  while True:                                               │
│                                                            │
│    ① read leaderboard (parquet)                            │
│    ② read notebook (last N entries)                        │
│    ③ read directive (human goal or STOP)                   │
│    ④ IF STOP in directive: write summary, exit             │
│    ⑤ form hypothesis (agent-decided OR sweep)              │
│    ⑥ IF new picker code needed:                            │
│         write Rust file in pair-picker/src/pickers/        │
│         cargo check → cargo build --release                │
│         on build error: git checkout, retry different idea │
│    ⑦ spawn picker binary                                   │
│    ⑧ read picker ranking parquet                           │
│    ⑨ read oracle verdicts parquet                          │
│    ⑩ compute metrics:                                      │
│         coverage, precision, loser_rate,                   │
│         dollar_recovery, rank_correlation                  │
│    ⑪ append row to leaderboard                             │
│    ⑫ append block to notebook                              │
│    ⑬ git commit (if new Rust code) with experiment_id      │
│    ⑭ check stop conditions: goal met? plateau? budget?     │
│    ⑮ IF stop: write summary, exit                          │
│                                                            │
│  (no external caller, no imports from this file,           │
│   no harness layer, no split responsibilities)             │
│                                                            │
└────────────────────────────────────────────────────────────┘
```

Each numbered step is ~5-30 lines of Python inside `autoresearch.py`.
Total file size target: under 500 lines. If it's longer than that,
something's wrong.

---

## The experiment row (what step ⑩ writes)

One append per iteration. Parquet schema:

```
experiment_id            string   (hash of params + time)
iteration                int64    (monotonic counter)
timestamp                timestamp
picker_name              string
picker_git_sha           string
picker_params_json       string
bars_version             string
oracle_version           string
train_start              string
train_end                string
eval_start               string
eval_end                 string
top_k                    int32

# metrics
coverage                 float64
precision                float64
loser_rate               float64
dollar_recovery          float64
rank_correlation         float64
n_winners_total          int32
n_winners_in_topk        int32
n_losers_in_topk         int32
wall_clock_seconds       float64

# narrative
hypothesis               string   (what was predicted before running)
outcome                  string   (one line: confirmed/refuted/surprising)
```

No schema split across files. This is the whole schema. Autoresearch
defines it; autoresearch is the only writer.

---

## Hypothesis discipline (step ⑤)

Before running anything, autoresearch writes the hypothesis in the
notebook. Three fields, enforced as a rule:

- **prediction** — what I expect the metric to do and why
- **result** — (filled in step ⑫) what actually happened
- **update** — (filled in step ⑫) what I now believe

If the agent driving the loop can't write the prediction before running,
the experiment is not ready. This is the only thing that makes the loop
research instead of grid search.

---

## Stop conditions (step ⑭)

The loop exits on any of these, checked every iteration:

1. **Goal reached** — metric hits target for N consecutive iterations
2. **Plateau** — last 20 experiments show no improvement
3. **Budget exhausted** — ran N experiments total
4. **Human STOP** — directive file contains "STOP"
5. **Catastrophic build failure** — 3 consecutive cargo builds fail

On any exit: write summary to notebook, print the top 5 leaderboard
rows, print surprising findings, tell the human what to look at.

---

## Who drives it

Three modes, from least to most autonomous. All three run the same
`autoresearch.py`:

**Mode 1 — One-shot (dev / debugging)**
Human runs `python autoresearch.py --once --picker X --params Y`.
Runs exactly one iteration with a human-supplied hypothesis. Writes
the row. Exits. Used for ad-hoc runs and for smoke-testing the machinery.

**Mode 2 — Human-in-the-loop**
Human runs `python autoresearch.py` in an interactive session with
claude-code. The agent (Claude) forms the hypothesis for each iteration,
runs it via the same `--once` path or as a continuous loop with human
approval between iterations. Every 1-3 iterations the human reads the
notebook and says continue/redirect.

**Mode 3 — Autonomous**
Human runs `python autoresearch.py --auto --budget 50` with a directive
set. The program forms its own hypotheses (either by delegating to an
LLM call or by running a simple rule-based proposer), runs them, stops
when a condition is met. Used once Mode 2 has proven the machinery
reliable.

**Same program in all three modes.** The `--auto` flag turns on the
hypothesis generator; everything else is identical. No separate
"autonomous runner." No separate "interactive harness." One program.

---

## Safety rails (enforced inline in the program)

These are checks inside `autoresearch.py`, not a separate validation
layer:

- Before writing any Rust file: verify it's under `pair-picker/src/pickers/`
- Before committing: verify the file is in the allowed path
- Before running cargo build: save current HEAD in case of rollback
- Before running any experiment: verify oracle verdicts exist for the eval period
- Never writes to: `engine/crates/core/*`, `~/quant-data/bars/*`, `~/quant-data/oracle/*`, `ORACLE_SPEC.md`, `AUTORESEARCH_LOOP.md` (this file)
- Never runs TEST-set experiments unless `--test-run` flag is explicitly passed (and the human sees it in their shell)

---

## What's deliberately not here

**No `harness.py`.** Deleted. The word "harness" does not appear in this
architecture anymore.

**No `run_experiment(spec) → row` function** exposed to other Python
code. That logic lives as inline code inside `autoresearch.py`'s loop
body.

**No "experimentation framework."** The thing that runs experiments is
the same thing that decides what to run, writes the results, and loops.
There is no framework, only a program.

**No abstraction layer between autoresearch and the Rust binaries.**
Autoresearch shells out directly via `subprocess.run(...)`. If we ever
need to call the same picker from a Rust context, that's a separate
binary invocation, not a shared library.

---

## The meta-point (again, because it keeps getting lost)

The reason we've rebuilt the pair-picker five times is we had no loop.
Each rebuild was a one-shot guess followed by manual validation weeks
later. **Autoresearch inverts that: measure first, guess second, build
third, measure again.** The loop is the thing. Everything else —
dataset, oracle, picker binaries, leaderboard, notebook — exists to
serve the loop.

If you can't point at one program that does the whole cycle in a single
file, you don't have autoresearch; you have a distributed responsibility
system that will rot the same way the old pair-picker did.

**One program. `autoresearch.py`. That's the whole system.**
