# Market Data & Python Orchestration — Reference

## Canonical Data Model

The system uses internal canonical types. External sources are translated through adapters.

### Bar Event (canonical)
```
timestamp, open, high, low, close, volume, vwap (optional)
```
- All timestamps in UTC
- Session boundaries must be explicit
- Warm-up bars excluded from trading but used for indicator initialization

### Session Awareness
- Regular hours, pre/post-market distinction
- Half days, holidays
- Overnight gap detection
- Time-of-day features (minutes since open, session bucket)

## Source Adapter Pattern

Every data source enters through a thin adapter:
1. Handle authentication
2. Make API calls with rate limiting
3. Parse response into canonical form
4. Emit validation flags
5. Independently testable

Adapters contain NO business logic, NO feature computation, NO trading decisions.

## Python Project Structure

```
paper_trading/
  alpaca_client.py    # Alpaca API adapter
  runner.py           # Main trading loop
  runner_multi.py     # Multi-pair runner
  config.py           # Config loading/validation
  benchmark.py        # Backtest comparison
  scanner.py          # Universe scanning
  data_quality.py     # Validation checks
  trade_monitor.py    # Position/trade monitoring
  walkforward.py      # Walk-forward validation
  tearsheet.py        # Performance reports
  cli.py              # CLI interface
```

## Timing Principles

- Event timestamp vs source timestamp vs ingestion timestamp — keep distinct
- Bar close ≠ bar availability (allow for processing delay)
- Decision timestamp must be logged for every signal
- No future information leakage — features computed only from past/current bars

## Strategy Lifecycle (from research to live)

1. **Observation** → collect data, notice patterns
2. **Hypothesis** → narrow, testable idea with exact rules
3. **Backtest** → honest simulation with realistic costs
4. **Out-of-sample** → walk-forward, parameter sensitivity
5. **Paper trading** → real-time, simulated execution
6. **Tiny live** → smallest practical size, strict limits
7. **Scaling** → gradual increase if behavior is stable

No stage can be skipped. Promotion requires evidence, not hope.

## Monitoring Checklist

- [ ] Data feed flowing? (freshness check)
- [ ] Data valid? (quality checks passing)
- [ ] Engine process running?
- [ ] Positions reconciled with broker?
- [ ] Error rates normal?
- [ ] Alerts delivered when something breaks?
