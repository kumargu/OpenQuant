# Autoresearch Plan — Road to $1000/2 weeks

## Goal
$1000-1400 per 2 weeks with $10K capital. Consistent, not lucky.

---

## Autoresearch Mode
- Run autonomously, don't stop
- Commit after every major finding
- Pause at milestones to check plan progress
- No code changes in Phase 1 — only analysis scripts and findings
- Log everything in pattern_research.md

## Phase 1: CHEAT (read-only, learn from bar cache)

NO code changes. Watch bars, patterns, what the engine does vs should do.

### 1A. Validate patterns on LOSING months (Nov, Sep 2025)
- Run the same 13-avenue analysis on Nov 18-28 and Sep data
- Key question: do intraday z>2.0 reversions still work in volatile months?
- If yes: the strategy is robust, just need intraday execution
- If no: need a regime filter to sit out bad months
- Compare: which pairs survive volatile months vs calm months?

### 1B. Find the RIGHT set of 8-12 pairs
- Not 33 (too few) and not 155 (too many losers)
- Look at bar cache for ALL months: which pairs consistently revert?
- A pair must work in BOTH good months (Jan, Mar) AND bad months (Nov, Sep)
- Cross-validate: pick pairs from Jan data, test on Nov data
- Target: 8-12 pairs that produce 5-10 trades/day intraday

### 1C. Watch Kalman filter behavior
- Add debug logs showing Kalman beta vs OLS beta for key pairs
- Does Kalman drift cause bad entries? Or does it help?
- Compare z-scores with and without Kalman on same bar data
- NO business logic change — just observe and log

### 1D. Study slippage from bar data
- Use open/close/high/low within bars to estimate real execution cost
- How much does a market order slip in the first minute?
- What's the cost difference between entry at bar open vs bar close?
- This determines: is 10 bps realistic or should we assume 15-20?
- Also: how many trades per day before we start moving prices?

### 1E. Find the balance: trades vs quality
- Too few trades = sitting idle, not making money
- Too many trades = slippage eats the edge
- Sweep entry_z from 1.5 to 3.5 on bar cache data
- For each: count trades AND net P&L after costs
- Find the sweet spot: maximum dollar P&L, not maximum trades or bps

---

## Phase 2: CONFIGURE (config changes only, no engine code)

Based on Phase 1 findings, change configs and run replays.

### 2A. Set optimal pairs list
- From 1B: create the curated pair candidates file
- These pairs must be justified by Phase 1 cross-validation

### 2B. Tune entry/exit parameters
- From 1E: set entry_z, exit_z, intraday_confirm_bars
- From 1D: set cost_bps to realistic value
- Set max_concurrent, max_daily_entries based on 1E sweet spot

### 2C. Run replay on UNSEEN period
- Use config from 2A+2B on a period we did NOT look at in Phase 1
- This is the honest test — no cheating here
- Target: $1000-1400 per 2 weeks on the unseen data
- If it fails: go back to Phase 1 and understand why

---

## Phase 3: CODE (engine changes, then validate on fresh data)

Only after Phase 1 and 2 confirm the approach works.

### 3A. Intraday rolling z-score
- The #1 engine gap from research
- Compute z from minute-bar window instead of daily closes
- This unlocks the intraday reversions we found in Phase 1

### 3B. Duplicate symbol exposure block
- Block entry if either leg already in an open position
- Simple check in PairsEngine::on_bar()

### 3C. Remove max_hold, keep min_hold
- Let z decide exits, not a timer
- min_hold prevents panic churn

### 3D. Validate on COMPLETELY FRESH data
- Do NOT reference any pairs or patterns from Phase 1 cheating
- Run live paper trading for 2 weeks
- Measure: did we hit $1000?
- If yes: ship it
- If no: what's the gap? Back to Phase 1 on the new data

---

## Other TODOs (parked, not forgotten)

- WebSocket reconnect logic in stream.rs
- GICS data completion for full S&P 500
- Metals intraday optimization
- Limit order execution (reduce slippage)
- Opening gap strategy (58% revert)
- Per-pair notional sizing (Phase D)
