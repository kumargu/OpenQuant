# Trading Baselines

## Current: Walk-forward pair-picker driven (2026-03-22)

**Approach**: Pair-picker selects pairs daily using 150-day rolling window.
No hardcoded pairs. Trade only what passes validation.

**Key finding**: The original $446/day on hardcoded GLD/SLV was fake alpha —
directional SLV exposure during silver's historic rally. Pair-picker correctly
rejects GLD/SLV on every date (not cointegrated, beta unstable).

**Status**: Building walk-forward simulation (Feb-Mar 2026).
BAC/WFC is first pair-picker approved pair but loses money due to
insufficient intraday volatility. Investigating vol-gating.

See data/backtest_history.json for all runs indexed by commit.
