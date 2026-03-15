# Optimization Log

Track the impact of each engine improvement. Each entry is one commit.

## Baseline (d0b4ed6)
**Strategy:** Mean-reversion only, z-score buy/sell, no stops, no trend filter
**Data:** BTC/USD, 30 days, 5-min bars (8616 bars)

| Metric | Value |
|---|---|
| Trades | 44 |
| Win rate | 63.6% |
| Total P&L | $340.79 |
| Expectancy/trade | $7.75 |
| Avg win | $73.61 |
| Avg loss | $107.52 |
| Profit factor | 1.20 |
| Max drawdown | $386.96 |
| Sharpe | 0.07 |

**Problems identified:**
1. No stop loss → worst trade -$315 (-3.15%), held 193 bars
2. No trend filter → buys dips in downtrends
3. Avg loss > avg win ($107 vs $73)
4. Holds too long (100-250 bars = 8-20 hours)

---

## Optimization 1: Exit Rules (stop loss + max hold)
**Commit:** (pending)
**Change:** Added exit.rs — stop loss (2%), max hold (100 bars), take profit (disabled)

| Metric | Baseline | With Exits | Change |
|---|---|---|---|
| Trades | 44 | 48 | +4 (more exits → more re-entries) |
| Win rate | 63.6% | 62.5% | -1.1% (stops trigger as losses) |
| Total P&L | $340.79 | $107.26 | -$233 |
| Expectancy/trade | $7.75 | $2.23 | -$5.52 |
| Avg win | $73.61 | $75.57 | +$1.96 |
| Avg loss | $107.52 | $119.99 | +$12.47 worse |
| Profit factor | 1.20 | 1.05 | -0.15 |
| Max drawdown | $386.96 | $531.33 | +$144 worse |
| Sharpe | 0.07 | 0.02 | -0.05 |

**Analysis:** Stop loss and max hold made things WORSE on this data.
- Stops are cutting losses at -2% but the mean-reversion often recovers past -2%
- Max hold forces exits at flat prices that would have eventually profited
- The strategy's edge IS holding through volatility — stops fight that

**Conclusion:** A 2% stop is too tight for BTC's 5-min volatility. Options:
1. Widen stop to 3-4% (let mean-reversion work)
2. Use ATR-based stops (adapt to volatility)
3. Keep exits but pair with a trend filter (avoid entering against trend)

---

## Optimization 2: Tighten buy z-score threshold (-2.0 → -2.2)
**Commit:** df6ae2c
**Change:** Raised entry bar from z < -2.0 to z < -2.2 across engine, pybridge, and CLI defaults

**Data:** BTC/USD + ETH/USD, 30 days, 1-min bars (77k bars total)

| Metric | Old Baseline (7d, z=-2.0) | New Baseline (30d, z=-2.2) | Notes |
|---|---|---|---|
| Trades | 59 | 120 | More data → more trades |
| Win rate | 61.0% | 62.5% | +1.5% (fewer false entries) |
| Total P&L | $645.03 | $1,176.17 | +$531 (longer window) |
| Expectancy/trade | $10.93 | $9.80 | Slightly lower per-trade |
| Profit factor | 1.61 | 1.99 | Significantly improved |
| Sharpe | 0.17 | 0.18 | Marginal improvement |
| Max drawdown | $465.35 | $656.11 | Worse — driven entirely by ETH |

**Per-asset breakdown:**
| Asset | Trades | Win Rate | P&L | Verdict |
|---|---|---|---|---|
| BTC/USD | 114 | 65.8% | +$1,832.28 | Strong fit |
| ETH/USD | 6 | 0.0% | -$656.11 | Strategy doesn't work here |

**Analysis:**
- Tighter z-score threshold improved selectivity (win rate up, PF up)
- BTC is the primary profit driver — strategy fits well
- ETH is a consistent loser with 0% win rate — mean-reversion fails on ETH in this period
- Max drawdown increase is entirely from ETH losses

**Conclusion:** The -2.2 threshold is an improvement. Next priorities:
1. Add trend filter to avoid ETH-style downtrend entries (#9)
2. ATR-based dynamic stops instead of fixed 2% (#10)
3. Per-asset parameter tuning — BTC and ETH need different configs (#11)
