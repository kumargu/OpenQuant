# OpenQuant Autoresearch — Program Brief

**Human-edited.** The agent reads this to understand the goal. Do not
edit this file programmatically.

## What this system does

Quant-lab (separate repo at `/Users/gulshan/quant-lab/`) discovers S&P 500
pair trading candidates by scanning 124,251 pairs with fixed-formation
z-score backtests. It exports the top-100 by realized total_bps as a
candidates JSON file.

OpenQuant's runner replays those candidates through the real Rust trading
engine on 1-min IEX bars. The pair-picker validates structural quality
(ADF, R², half-life, β stability) and selects top-40 by structural score.
The engine trades with rolling z-score, stop-loss, and max-hold.

## Current baseline (validated across 3 windows)

| window | trades | win% | P&L |
|---|---|---|---|
| May-Jul 2025 | 56 | 76.8% | +$4,364 |
| Sep-Nov 2025 | 30 | 66.7% | +$2,486 |
| Jan-Apr 2026 | 38 | 73.7% | +$3,399 |
| **Aggregate** | **124** | **73.4%** | **+$10,249** |

## What the agent should experiment with

The agent edits EDITABLE CONSTANTS in `train.py`:
- `CANDIDATES` — which lab-generated candidates file to use
- `REPLAY_START` / `REPLAY_END` — which window to replay
- `NAME` — descriptive slug for this experiment
- `HYPOTHESIS` — one-sentence prediction BEFORE running

The agent can also modify:
- `config/pairs.toml` — engine parameters (entry_z, exit_z, stop_loss,
  max_hold, lookback, cost_bps, top_k)
- `engine/crates/core/src/pairs/` — trading logic (entry/exit rules,
  z-score computation, position sizing)
- `engine/crates/pair-picker/src/` — validation pipeline, scoring formula

## Rules

1. **Write the hypothesis BEFORE running.** No hypothesis = no experiment.
2. **One change per experiment.** Don't change entry_z AND stop_loss in the
   same run — you won't know which moved the needle.
3. **Buddy reviewer MANDATORY** before claiming a signal or changing
   direction. Without it, the agent drifts into grid search.
4. **Mock server for replay.** Start `python scripts/mock_alpaca.py` before
   running. Replay always uses parquet data (same as lab). Never use
   Alpaca's timeframe=1Day endpoint.
5. **Don't modify prepare.py or program.md.**
6. **Append only** to results.tsv and NOTEBOOK.md.

## Current target

$1,000/2 weeks on $10K capital. The baseline is ~$1,100/month gross.
Improvements should come from better entry/exit timing, tighter stop-loss,
or better candidate selection — not from adding complexity.
