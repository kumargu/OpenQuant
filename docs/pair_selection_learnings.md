# Pair Selection Learnings

Hard-won insights from log analysis, pattern mining, and backtesting.
Use these when selecting pairs for the trading universe.

## Finding pairs that actually revert

### Autocorrelation is the best pre-filter
- **AC(1) < -0.10** is the threshold for "mean-reverting" spreads
- Our pattern analysis found 38/265 same-sector pairs pass this
- AC(1) < -0.40 pairs (e.g., NOW pairs, DIS/NFLX) have 72-89% win rates
- Positive AC(1) means trending — avoid these pairs entirely

### NOW (ServiceNow) is a reversion magnet
- META/NOW, GOOGL/NOW, ADBE/NOW, MSFT/NOW, CRM/NOW all have AC1 < -0.42
- None were in our original 29-pair candidate list — a huge blind spot
- Likely mechanism: NOW is a high-beta tech name that overshoots on sector moves then reverts

### R² ≥ 0.85 is the quality floor
- Loosening R² from 0.85 to 0.75 adds trades but win rate drops from 67% to 50%
- R² < 0.70 pairs are essentially noise — OLS relationship is meaningless
- The R² gate is doing more work than any other filter

### Beta must be positive and stable
- Negative beta (e.g., COST/WMT at -0.51) means the OLS relationship is nonsensical
- Beta changing > 30% between scans means the pair's structure is breaking
- Beta CV (coefficient of variation) > 0.5 is a red flag

### Half-life sweet spot: 2-5 days
- HL < 2d: reverts too fast, can't capture it on daily bars
- HL 2-5d: matches our 4-7 day average hold period
- HL > 5d: can still work (loosened from 4d to 5d added 4 winning trades)
- HL > 10d: too slow, max_hold timeout will always fire

## When NOT to trade

### Earnings blackout (±5 trading days)
- Several of our biggest losers had 5-10% single-leg moves = earnings
- Static calendar (data/earnings_calendar.json) filters these out
- Blocking earnings entries improved P&L by +$564 on 29 pairs

### |z| > 3.0 is a structural break, not a reversion opportunity
- 0/3 trades with |z| > 3.0 reverted in our data
- OU theory: P(|z| > 3) = 0.27% under stationarity — if you see it, the pair is probably broken
- Z-cap at 3.0 is a safety guard

### Both legs moving same direction (80% of trades)
- This is normal for correlated pairs — P&L comes from the differential
- The risk: during market crashes, both legs drop together but differentials widen unpredictably
- Consider reducing exposure during high-VIX periods

## Holding period patterns

### Winners revert in 1-6 days; day 10 exits are losers
- Hold 1-3d: 100% win rate (small sample)
- Hold 4-6d: 100% win rate (larger sample)
- Hold 10d (max_hold): 43% win rate
- If it hasn't reverted by day 6, the probability of reversion drops sharply

### SHORT spread trades outperform LONG
- SHORT: 88% win rate vs LONG: 59% (on our data)
- Literature confirms this (Stambaugh et al. 2012): overpricing corrects more reliably
- Don't disable long, but consider asymmetric sizing

## Day-of-week effects (from pattern analysis)
- Varies by pair — no universal "Monday effect"
- Check the patterns dashboard (dashboards/patterns_dashboard.html) per pair
- Some pairs show higher absolute moves on Mondays (weekend news absorption)

## What we learned about debugging pairs strategies
- See docs/debugging_learnings.md for the log-driven debugging approach
