---
name: market-data
description: Use when working on data ingestion, Alpaca API integration, bar fetching, data quality checks, staleness detection, or the Python orchestration layer (paper_trading/ module).
---

# Market Data & Python Orchestration

## Trigger

Activate when working on `paper_trading/`, Alpaca API calls, bar data fetching, data quality, or the Python-Rust boundary (PyO3/maturin).

## Architecture

```
Alpaca API → Python adapters → validation → canonical bars → Rust engine (via PyO3)
```

- **Python owns**: data collection, API integration, scheduling, monitoring, alerting
- **Rust owns**: feature computation, signal evaluation, risk gating, backtesting
- Trading logic in Python = bug. Move it to Rust.

## Alpaca Integration

The `paper_trading/alpaca_client.py` module handles:
- Bar fetching (historical and live)
- Account/position queries
- Order submission (paper trading)

### Staleness rule
**Never act on delayed/stale market data.** Better to do nothing than trade on old info. Check timestamps before processing any bar.

## Python-Rust Boundary

```python
# Python calls Rust via PyO3 (maturin-built)
import openquant_core
engine = openquant_core.Engine(config)
result = engine.on_bar(bar_data)
```

- Data format between Python and Rust must be explicit and validated
- Rust rejects malformed input clearly
- JSON for config/metadata, structured dicts for bar data

## Data Quality Checks

Every ingestion pipeline must validate:
- Timestamp monotonicity
- No duplicate bars
- No missing intervals (detect gaps)
- OHLC consistency: `low <= open,close <= high`
- Volume > 0 for trading hours
- Price > 0
- Session boundary correctness

Bad data must be rejected or flagged — never silently pass into the feature engine.

## Key Files

| File | Purpose |
|---|---|
| `paper_trading/runner.py` | Main trading loop |
| `paper_trading/alpaca_client.py` | Alpaca API wrapper |
| `paper_trading/config.py` | Configuration loading |
| `paper_trading/benchmark.py` | Backtest comparison runner |
| `paper_trading/scanner.py` | Universe scanning |
| `paper_trading/data_quality.py` | Validation checks |
| `openquant.toml` | All tunable parameters |

## Commands

```bash
# Build Rust engine for Python
cd engine && maturin develop --release

# Run benchmark comparison (required for signal/risk PRs)
python -m paper_trading.benchmark --compare

# Save baseline
python -m paper_trading.benchmark --save-baseline

# Run paper trading
python -m paper_trading
```

## Rules

1. **Config validated at startup** — fail fast on invalid config
2. **Secrets in env vars only** — never in source code or committed files
3. **Structured logging** — timestamps, module name, context fields
4. **Source adapters are thin** — parse, validate, transform; no business logic
5. **Monitor the full pipeline** — data freshness, feed health, engine process health
