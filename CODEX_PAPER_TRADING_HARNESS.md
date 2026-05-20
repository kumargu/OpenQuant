# OpenQuant Paper Trading Harness

This harness governs daily paper-trading operation. The goal is hands-free execution through the runner, with enough logging and review to understand every dollar made or lost.

## Operating Principle

Paper trading must behave like future live trading, except with paper broker credentials. Do not place manual orders. Do not call Alpaca directly to patch positions unless the user explicitly requests emergency intervention.

The runner owns:

- data refresh
- fit loading or rebuild
- broker reconciliation
- strategy decision
- order generation
- order submission
- post-submit reconciliation
- state persistence
- logs and journal records

## Daily Start

Start from the `main` worktree unless the user says otherwise.

Before starting:

1. Confirm branch and commit.
2. Confirm working tree is clean or identify unrelated local output.
3. Confirm no stale runner is active.
4. Confirm the intended mode:
   - basket_only
   - basket with leadership overlay
5. Start the runner through `openquant-runner`, not ad hoc scripts.

Example `basket_only` paper run:

```bash
RUST_LOG=info openquant-runner paper --engine basket --execution paper
```

Example leadership-overlay paper run:

```bash
RUST_LOG=info openquant-runner paper --engine basket --execution paper \
  --leadership-overlay-sectors faang,chips \
  --leadership-mode replace-with-long-only \
  --leadership-ret5d-threshold 0.02 \
  --leadership-breadth5d-threshold 0.56 \
  --leadership-long-only-leverage 4.0
```

Use a persistent process manager or terminal session. The run should not depend on an interactive shell staying open accidentally.

## Required Logging

Paper runs must log enough to explain the day.

Minimum expectation:

- startup phase
- fit artifact source or rebuild
- state snapshot source
- broker positions at startup
- overlay config, if enabled
- overlay active/inactive decision
- target notional summary
- generated orders
- accepted/failed orders
- post-submit reconciliation
- persisted state path

Preferred command shape when file logging is needed:

```bash
RUST_LOG=info openquant-runner paper --engine basket --execution paper \
  2>&1 | tee -a data/journal/engine.log
```

For debugging, use:

```bash
RUST_LOG=debug openquant-runner paper --engine basket --execution paper \
  2>&1 | tee -a data/journal/engine.log
```

High log volume is acceptable for now. Missing evidence is not.

## During The Day

Do not interfere with open paper positions unless there is a clear operational failure.

Acceptable checks:

- runner process is alive
- bars are arriving
- no repeated reconciliation failures
- broker positions are not wildly divergent from target
- logs show expected session state

Do not change strategy mode mid-run without recording why.

## Session Close

The basket runner decides at session close plus grace.

Expected close flow:

1. Build daily close snapshot.
2. Run basket engine.
3. Apply overlay policy if enabled and active.
4. Convert target notionals to target shares.
5. Diff against broker shares.
6. Submit aggregate paper orders.
7. Reconcile broker positions.
8. Persist state.
9. Write logs and journal.

If this sequence does not happen, treat it as an operational bug.

## End-of-Day Review

After the session close cycle completes, write a short review.

Record:

- date
- strategy mode
- overlay active/inactive
- starting equity
- ending equity
- daily P/L
- largest winners
- largest losers
- order count
- reconciliation status
- what worked
- what went wrong
- lesson for future strategy/research
- next action

Keep the review factual. Losses should produce either a clear explanation or a research follow-up.

## Failure Handling

If startup fails:

- read the first error
- inspect state path and fit artifact path
- check broker reconciliation logs
- do not delete state blindly

If reconciliation fails:

- stop and inspect broker positions
- compare target gross vs actual gross
- do not submit manual offsetting orders unless explicitly instructed

If the strategy loses money:

- do not hand-wave it as noise
- compare against `basket_only` and overlay expectations
- inspect whether the day was a mean-reversion regime or leadership/trend regime
- open or update a research issue when the loss exposes a new failure mode

## Promotion Discipline

Paper mode is where strategy changes earn trust.

A mode is paper-worthy only if:

- replay supports it
- startup is deterministic
- restart behavior is tested
- logs explain decisions
- operator command is simple
- `basket_only` behavior remains available

Live-money promotion requires a separate decision. Do not infer live approval from paper success.
