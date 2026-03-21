---
name: backtesting
description: Use when running backtests, generating comparison tables for PRs, performing walk-forward validation, purged cross-validation, or evaluating strategy performance. Covers the benchmark runner, metrics, and the data-driven PR workflow.
---

# Backtesting & Evaluation

## Trigger

Activate when running benchmarks, creating backtest comparison tables, evaluating strategy metrics, or working on walk-forward/purged CV code.

## Commands

```bash
# Run benchmark comparison (REQUIRED for any signal/risk/strategy PR)
python -m paper_trading.benchmark --compare

# Run benchmark for specific category
python -m paper_trading.benchmark --category crypto --days 7

# Save baseline on main branch
python -m paper_trading.benchmark --save-baseline

# Criterion benchmarks (Rust performance)
cd engine && cargo bench -p pair-picker
cd engine && cargo bench -p openquant-core
```

## PR Backtest Table (mandatory)

Every PR touching signals, risk, or strategy MUST include:

```markdown
| Metric | Before | After | Delta |
|---|---|---|---|
| Total return | X% | Y% | +/-Z% |
| Sharpe | ... | ... | ... |
| Max drawdown | ... | ... | ... |
| Win rate | ... | ... | ... |
| Profit factor | ... | ... | ... |
| Trade count | ... | ... | ... |
```

Include per-category AND aggregated metrics.

## Metrics Hierarchy

| Priority | Metrics | Rule |
|---|---|---|
| Primary | Expectancy, profit factor, Sharpe | Must improve or not regress |
| Secondary | Win rate, max drawdown | Should not regress significantly |
| Neutral | Trade count | More trades ≠ better |

A PR that improves win rate but tanks profit factor is a regression.

## Evaluation Principles

1. **Net of costs always** — commissions, spread, slippage. Gross profits are meaningless
2. **Compare against null** — is this result distinguishable from random?
3. **Check for fragility** — profit concentrated in few trades? Parameter-sensitive? Regime-dependent?
4. **No future leakage** — features computed only from past/current data
5. **Deterministic replay** — same inputs → same outputs every time
6. **Honest fill model** — next-bar-open or explicit rule; document assumptions

## Walk-Forward Validation

```
[--- train ---|--- test ---]
      [--- train ---|--- test ---]
            [--- train ---|--- test ---]
```

- Sequential windows, no overlap between train and test
- Check for degradation over time
- If walk-forward shows decay, strategy is fragile

## Purged Cross-Validation

For overlapping observations (e.g., label horizons that span multiple bars):
- Purge test observations that overlap with training labels
- Embargo period between train/test to prevent leakage
- See `paper_trading/purged_cv.py`

## Performance Gates (CI)

CI runs bench_gate tests to catch catastrophic regressions:
- Gate = baseline × 30-70x (CI runners are ~25x slower than local M4)
- Gates catch "100x slower" regressions, not "2% regression"
- Use local `cargo bench` for precision measurements

When a gate fails: investigate first, don't widen the threshold.

## Key Backtest Assumptions to Document

- Fill rule (next-bar-open, midpoint, etc.)
- Slippage model (fixed bps, volatility-adjusted)
- Fee model (per-trade, percentage)
- Warm-up period (excluded from metrics)
- Session boundaries
- Data source and range
