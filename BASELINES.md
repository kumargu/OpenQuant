# Baselines

Commit notes:
- metals / pairs references below were captured from earlier validation branches
- basket overlay reference below is from merged `main` commit `facfa00b`

## Metals (--engine metals)

Replay: `2025-07-01` to `2026-03-28`

| Trades | Wins | Win Rate | Total bps | Avg bps |
|--------|------|----------|-----------|---------|
| 66 | 46 | 70% | +4904.2 | +74.3 |

## S&P 500 (--engine snp500)

### Q1 2026

Replay: `2026-01-02` to `2026-03-28`

| Trades | Wins | Win Rate | Total bps | Avg bps |
|--------|------|----------|-----------|---------|
| 22 | 17 | 77% | +353.5 | +16.1 |

### Main branch reference (Q1 2026)

Commit: `e2ac6a3` (main)

| Trades | Wins | Win Rate | Total bps | Avg bps |
|--------|------|----------|-----------|---------|
| 25 | 16 | 64% | +383.8 | +15.4 |

## Basket (--engine basket)

### Main branch reference (2026 YTD)

Commit: `facfa00b` (main)

Replay window: `2026-01-02` to `2026-04-30`

The basket runner now supports an opt-in leadership overlay in paper/noop mode.
Baseline behavior is unchanged unless overlay flags are passed.

| Mode | Cum Return | Sharpe | Max DD | Trading Days |
|------|------------|--------|--------|--------------|
| Basket baseline | +3.36% | 0.47 | 21.5% | 82 |
| Basket + leadership overlay (`faang,chips`) | +76.53% | 2.54 | 30.6% | 82 |
