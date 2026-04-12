# Autoresearch — the whole system

There are **two programs**: quant-lab and the runner in replay mode.

Quant-lab discovers candidate pairs. The runner replays them through the
real Rust engine on historical bars. The experiment result is actual P&L
from the engine — not a proxy metric, not an oracle score, not a Python
backtest. The engine doesn't know it's replaying.

---

## What autoresearch is and isn't

**IS:**
- Quant-lab (`/Users/gulshan/quant-lab/`) scans the S&P 500 universe, ranks by realized total_bps, exports `monthly_pairs_YYYYMM.json`
- The runner (`openquant-runner replay --pipeline lab --candidates <file>`) validates candidates through the Rust pair-picker's structural scoring, then trades them on 1-min bars
- Results are measured in actual trades, win rate, and P&L from the engine log
- The loop: lab generates candidates → runner replays → measure P&L → adjust lab parameters → repeat

**IS NOT:**
- A standalone picker binary (deleted — pair-picker is a validation library now)
- An oracle-vs-picker scoring system (the oracle crate exists but the primary metric is now replay P&L)
- A Python backtest (lab does backtesting for candidate discovery, but validation is always through the Rust engine)

---

## Data rule: 1-min IEX bars everywhere

All daily close prices — in quant-lab, in the Rust pair-picker, in engine
warmup, in autoresearch — are derived by aggregating 1-min Alpaca IEX bars
to RTH session close (13:30–20:00 UTC). NEVER use Alpaca's `timeframe=1Day`
endpoint. It produces different close prices (up to $2/bar divergence vs
1-min aggregation) which causes β/ADF/R² disagreements between systems.

**Data validation** — runs FIRST at the start of every autoresearch loop:
1. Pull any missing recent trading days from Alpaca 1-min API → append to parquets.
2. Pick 100 random stocks × random days across weeks, fetch from Alpaca, compare against parquets. Flag if any close price diverges by >$0.01.
3. Fail the loop if validation fails — don't run experiments on bad data.

## Three data modes

| mode | data source | when |
|---|---|---|
| **Replay/backtest** | persisted parquets via `scripts/mock_alpaca.py` | experiments, autoresearch |
| **Live trading** | real Alpaca 1-min websocket | production |
| **Paper trading** | real Alpaca 1-min websocket | pre-production validation |

Replay ALWAYS uses the mock server backed by parquets. This guarantees
lab and engine see identical prices. The mock server is not a hack — it's
the canonical offline data path.

## The experiment loop

```
┌────────────────────────────────────────────────────────────────────┐
│                                                                    │
│  ① QUANT-LAB (Python, /Users/gulshan/quant-lab/)                  │
│     - Scan 124k S&P 500 pairs with fixed-formation z-score        │
│     - Rank by realized total_bps in trading window                │
│     - Export top-100 as monthly_pairs_YYYYMM.json                 │
│                                                                    │
│  ② MOCK SERVER                                                     │
│     python3 scripts/mock_alpaca.py --port 8787                    │
│     Serves ~/quant-data parquets as Alpaca-format HTTP            │
│                                                                    │
│  ③ REPLAY                                                          │
│     ALPACA_DATA_URL=http://127.0.0.1:8787/v2/stocks/bars \        │
│     openquant-runner replay \                                      │
│       --engine snp500 --pipeline lab \                             │
│       --candidates pairs/monthly_pairs_YYYYMM.json \            │
│       --start YYYY-MM-DD --end YYYY-MM-DD                         │
│                                                                    │
│     Rust pair-picker validates + scores (structural quality)      │
│     Engine trades on 1-min bars (rolling z-score, stop-loss,      │
│     max-hold, weekly pair regeneration)                            │
│                                                                    │
│  ④ MEASURE                                                         │
│     Parse engine.log for ENTRY/EXIT events                        │
│     Compute: trades, win rate, total P&L, per-pair breakdown      │
│                                                                    │
│  ⑤ ITERATE                                                         │
│     Adjust lab parameters (formation window, filter thresholds,   │
│     candidate pool size) and repeat from ①                        │
│                                                                    │
└────────────────────────────────────────────────────────────────────┘
```

## Pipeline profiles

| profile | flag | use case |
|---|---|---|
| `lab` | `--pipeline lab` | **Production for lab-sourced candidates.** Structural hard gates relaxed (lab already filtered), scoring + ranking active. The picker computes α/β on current data and selects top-40 by structural quality score. |
| `default` | (no flag) | For candidates that haven't been pre-filtered by lab. Full structural gates: ADF p<0.05, R²≥0.30, structural break <45%, β CV<0.20. Too strict for lab candidates — rejects 100%. |
| `metals` | `--pipeline metals` | Metals/commodities with relaxed thresholds. |

**Always use `--pipeline lab` with lab-sourced candidates.** The default
pipeline's hard gates reject lab candidates because lab selects by realized
P&L (empirical edge) while the picker validates structural quality
(different objective). Both are needed — lab discovers, picker ranks —
but the hard gates must be off.

## Division of labor (validated)

> Lab does a good job finding raw eligible pairs. OpenQuant does a good
> job filtering and trading it neatly.

Evidence (3-window OOS, Jan-Apr 2026 + Sep-Nov 2025 + May-Jul 2025):

| window | trades | win% | P&L |
|---|---|---|---|
| May-Jul 2025 | 56 | 76.8% | +$4,364 |
| Sep-Nov 2025 | 30 | 66.7% | +$2,486 |
| Jan-Apr 2026 | 38 | 73.7% | +$3,399 |
| **Aggregate** | **124** | **73.4%** | **+$10,249** |

The picker's structural scoring is essential — taking lab's top-40 by
realized bps directly (preserve_input_order=true) swung P&L from
+$3,399 → −$2,809 on the same candidates. The picker filters out
lucky-trade-but-structurally-weak pairs.

## Config

`config/pairs.toml` section `[pair_picker]`:

```toml
[pair_picker]
top_k = 40                    # max pairs per picker run
preserve_input_order = false  # true = skip score-sort (experimental only)
```

## Hypothesis discipline

Before running a replay, write the hypothesis. Three fields:

- **prediction** — what I expect the P&L to do and why
- **result** — what actually happened (trades, win%, P&L)
- **update** — what I now believe differently

If you can't write the prediction before running, the experiment is not
ready. This is the only thing that makes the loop research instead of
grid search.

## Buddy reviewer rule (MANDATORY)

Before claiming a signal, changing direction, or shipping picker/engine
changes: spawn a buddy reviewer agent. Without it, Claude drifts into
grid search and misses bugs. This was validated multiple times — the
capital-inflation bug, the rolling-mean drift-laundering bug, and the
"30 pairs is the right number" misframing were all caught by buddy
review or could have been caught earlier with one.

## What's deliberately not here

**No standalone pair-picker binary.** Deleted. The pair-picker is a
validation library called by the runner.

**No candidate discovery in Rust.** Deleted (graph.rs, thompson.rs,
lockfile.rs). Lab owns discovery. Rust owns validation + trading.

**No `timeframe=1Day` calls.** All daily bars are derived from 1-min
IEX bars. The `alpaca.rs` functions `fetch_daily_bars_range` and
`fetch_daily_bars` aggregate from 1-min internally.

**No regime.rs gate.** Deferred to lab where it has full formation
window and Python flexibility. The field exists in ActivePair for
schema compatibility but always returns -1.0.
