# OpenQuant — Paper Trading Vision

**Status:** Active development
**Started:** 2026-03-14
**Goal:** Build a paper trading system that receives signals from TradingView and tracks simulated trades with full accounting.

## Why TradingView?

- Best charting platform, already used for analysis
- Pine Script for strategy prototyping
- Built-in webhook alerts (Pro plan) can push signals to our system
- We chart and analyze on TV, we execute and account on our side

## Architecture

```
TradingView (Pine Script Strategy)
        │
        │  Webhook (HTTP POST)
        │  { symbol, action, price, qty }
        ▼
┌─────────────────────┐
│  OpenQuant Webhook   │
│  Receiver (Python)   │
│  - Validates signal  │
│  - Applies risk gate │
│  - Records trade     │
└─────────┬───────────┘
          │
          ▼
┌─────────────────────┐
│  Paper Portfolio     │
│  (JSON files + index)│
│  - Positions         │
│  - P&L tracking      │
│  - Trade journal     │
│  - Daily snapshots   │
└─────────────────────┘
```

## Phases

### Phase 1 — Webhook receiver + trade logging (current)
- Python FastAPI server receives TradingView webhooks
- Validates and logs paper trades to SQLite
- Simple position tracking (qty, avg price, unrealized P&L)
- CLI to view open positions and trade history

### Phase 2 — Risk gates + portfolio rules
- Position size limits
- Max drawdown checks
- Sector/symbol concentration limits
- Reject trades that violate rules (log rejection reason)

### Phase 3 — P&L tracking + daily snapshots
- Fetch live prices to mark positions to market
- Daily equity curve snapshots
- Trade-level P&L attribution
- Win rate, Sharpe, max drawdown metrics

### Phase 4 — Dashboard
- Web UI showing positions, P&L, equity curve
- Trade journal with notes
- Strategy performance comparison

## TradingView Setup

1. Write Pine Script strategy on TradingView
2. Add alert with webhook URL pointing to our server
3. Alert message format (JSON):
```json
{
  "secret": "{{your_secret}}",
  "symbol": "{{ticker}}",
  "action": "{{strategy.order.action}}",
  "price": {{close}},
  "qty": {{strategy.order.contracts}},
  "strategy": "{{strategy.order.id}}"
}
```

## Non-Goals (for now)
- No live trading — paper only
- No direct TradingView API integration (doesn't exist)
- No broker connectivity yet
- No sub-second latency requirements
