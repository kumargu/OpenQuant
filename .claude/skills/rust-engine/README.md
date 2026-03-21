# Rust Engine — Reference

## Architecture Principles

The Rust engine is the quantitative core. It owns deterministic market-data processing, feature extraction, signal evaluation, risk checks, backtesting, and portfolio accounting.

### Separation of concerns

```
market events → feature computation → signal scoring → risk gating → order intent → fill simulation → portfolio accounting → metrics
```

Keep noncritical work (JSON parsing, filesystem writes, network) outside the core loop.

### State machines over condition trees

Use explicit state machines for: order lifecycle, position lifecycle, risk trip states, engine startup/shutdown. A clear state machine beats scattered conditionals.

### Typed domain models

Prefer domain types (`Price`, `SpreadBps`, `SignalScore`, `PositionSide`) over raw `f64`/`String`. Makes invalid states hard to represent.

## Numerical Principles

- Be deliberate about numeric types — `f64` for throughput, consider integer-scaled for exact accounting
- Don't mix raw prices, percentages, basis points, ratios without explicit conversion
- Incremental math (rolling mean, EWMA, running drawdown) preferred where mathematically sound
- Every formula should document: inputs, units, assumptions, warm-up behavior, edge cases

## Module Guidelines

### Features (`features/`)
- Rolling stats, indicators, derived measures, regime inputs
- Each feature answers: what does it measure, why does it matter, what are its units, valid range, warm-up requirement

### Signals (`signals/`)
- Scoring models, threshold logic, strategy rules
- Keep strategies in separate modules (`signals/strategy_name.rs`)
- Decision logic must be rule-based and explainable

### Risk (`risk/`)
- Hard gates: max notional, max loss, cooldown, spread filter, stale data rejection
- Risk checks are not suggestions — they are barriers

## Data-Driven Development

Every signal/risk/strategy change requires a PR with backtest comparison:
1. Run backtest on `main` → baseline
2. Make change on feature branch
3. Run backtest → candidate
4. PR includes before/after table (per-category + aggregated)
5. One hypothesis per PR — no bundling

### Metrics hierarchy
- **Primary**: expectancy, profit factor, Sharpe
- **Secondary**: win rate, max drawdown
- **Neutral**: trade count

## Concurrency

Single-threaded fast code is often better than poorly designed concurrent code. Use parallelism only where justified: partitioned backtests, parameter sweeps, independent symbol processing. Never introduce shared mutable state without measurement.
