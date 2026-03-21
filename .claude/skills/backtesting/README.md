# Backtesting & Evaluation — Reference

## Backtest Engine Architecture

The backtester shares core logic with the live/paper trading engine:
- Same feature code
- Same signal code
- Same risk gates
- Same portfolio accounting
- Different: data source (historical replay vs live) and fill source (simulated vs broker)

This alignment ensures paper-trading results are meaningful.

## Fill Simulation

### Supported models
- **Next-bar-open**: simplest, most conservative
- **Midpoint + slippage**: `mid ± slippage_bps * mid`
- **Bar-range constrained**: fills bounded by bar high/low

### Edge cases to handle
- Gaps through stops (fill at gap price, not stop price)
- Stop and target both touched in same bar (assume worst case)
- Zero-volume bars (no fill possible)
- Missing bars (skip, don't interpolate)

## Portfolio Accounting

- Realized PnL: closed positions, fee-adjusted
- Unrealized PnL: mark-to-market open positions
- Capital curve: initial capital + cumulative realized PnL
- Exposure: notional value of open positions / capital
- Turnover: total traded notional / average capital

## Evaluation Metrics

### Per-trade
- Average win / average loss
- Median win / median loss
- Max favorable excursion (MFE)
- Max adverse excursion (MAE)
- Holding time distribution

### Per-strategy
- Expectancy: `(win_rate * avg_win) - (loss_rate * avg_loss)`
- Profit factor: `gross_profit / gross_loss`
- Sharpe: `mean(returns) / std(returns) * sqrt(252)`
- Max drawdown: peak-to-trough decline in equity
- Calmar: `annualized_return / max_drawdown`

### Fragility indicators
- Profit concentration: what % of total profit comes from top 5 trades?
- Regime dependence: performance by market regime
- Parameter sensitivity: ±10% on key params, does it survive?
- Cost sensitivity: double the slippage — still profitable?

## Scenario Testing

Stress strategies under difficult conditions:
- Flash crash days
- High-volatility periods
- Thin liquidity windows
- Strong trending regimes (bad for mean reversion)
- Gap-heavy sessions
- Worse-than-expected slippage

## SQLite Journal

Every bar processed gets logged:
- Feature snapshot (z-score, volatility, volume, etc.)
- Signal output (fired/not, score, reason)
- Risk gate result (passed/blocked, rejection reason)
- Fill result (price, slippage)
- Engine version (git SHA)

Enables post-hoc analysis: "why did we lose on this trade?"

## Benchmark Infrastructure

### Criterion (Rust performance)
```rust
fn bench_backtest(c: &mut Criterion) {
    c.bench_function("backtest_1k_bars", |b| {
        b.iter(|| engine.run_backtest(&bars_1k))
    });
}
```

### CI Performance Gate
```rust
#[test]
#[ignore] // only runs with --release -- --ignored
fn bench_gate_backtest_1k() {
    let elapsed = measure(|| engine.run_backtest(&bars));
    assert!(elapsed < Duration::from_millis(5), "gate: {elapsed:?}");
}
```

### Python Benchmark Runner
```python
# paper_trading/benchmark.py
# Runs strategy on historical data, produces metrics table
# --compare: shows before/after
# --save-baseline: saves current as baseline
```
