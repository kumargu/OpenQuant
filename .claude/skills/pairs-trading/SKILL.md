---
name: pairs-trading
description: Use when working on pair discovery, cointegration testing, spread modeling, hedge ratio estimation, pair scoring, or the pair-picker crate. Covers statistical methods, mathematical foundations, and the pairs trading pipeline.
---

# Pairs Trading

## Trigger

Activate when working on `engine/crates/pair-picker/`, `active_pairs.json`, spread/cointegration logic in core, or Claude API candidate generation.

## Pipeline

```
Universe screening → Cointegration testing → Hedge ratio estimation → Spread construction → Scoring → Ranking → active_pairs.json → Trading engine
```

## Key Statistical Methods

### Cointegration (Engle-Granger)
1. Run OLS regression: `Y = α + βX + ε`
2. Extract residuals `ε`
3. Run ADF test on residuals
4. Reject null of no cointegration if ADF p-value < threshold

**Critical**: ADF requires contiguous observations. Never filter scattered indices — multi-day gaps invalidate serial correlation assumptions and bias toward false acceptance.

### Hedge Ratio
- OLS slope `β` from the cointegration regression
- Kalman filter for time-varying hedge ratio (preferred for live trading)
- Must be recomputed on config reload but preserve warm-up state

### Spread Construction
- `spread = Y - β * X` (or log prices for log-spread)
- Z-score of spread for entry/exit signals: `z = (spread - μ) / σ`
- Rolling window for μ and σ — window length is a key parameter

### Half-Life of Mean Reversion
- From Ornstein-Uhlenbeck: regress `Δspread` on `spread_{t-1}`
- `half_life = -ln(2) / θ` where θ is the regression coefficient
- Shorter half-life → faster mean reversion → more tradeable

## Scoring Components

When scoring pair candidates, consider:
- **Cointegration strength** — ADF test statistic (more negative = stronger)
- **Half-life** — speed of mean reversion
- **Spread stability** — rolling cointegration consistency
- **Correlation** — price correlation (high correlation + cointegration = good)
- **Liquidity** — both legs must be liquid enough to trade
- **Sector alignment** — same-sector pairs often have stronger economic rationale

## Pair Selection Learnings (from backtesting — see `docs/pair_selection_learnings.md`)

**Always consult `docs/pair_selection_learnings.md` when selecting or evaluating pairs.** Key points:

- **Autocorrelation AC(1) < -0.10** is the best pre-filter for mean-reverting pairs. Run `python3 scripts/pattern_analysis.py` to compute for all candidates
- **R² ≥ 0.85** is the quality floor — loosening to 0.75 halves the win rate
- **Beta must be positive (> 0.1) and stable (CV < 0.30)** — negative or unstable beta means the pair relationship is broken
- **Half-life 2-5 days** matches the observed 4-7 day hold period for winners
- **|z| > 3.0 at entry is a structural break**, not a reversion signal (0/3 reverted)
- **Earnings blackout ±5 days** — check `data/earnings_calendar.json`
- **NOW pairs are the strongest mean-reverters** in our universe (AC1 < -0.42)
- **SHORT spread trades outperform LONG** (88% vs 59% win rate, confirmed by Stambaugh et al. 2012)

## Pattern Analysis Tools

- `scripts/pattern_analysis.py` — autocorrelation, day-of-week effects, holding curves for all 265 pairs → `dashboards/patterns_dashboard.html`
- `scripts/daily_walkforward_dashboard.py` — full walk-forward simulation with logging → `dashboards/walkforward_dashboard.html`
- `scripts/pair_deep_dive.py` — single-pair deep dive with per-bar logging → `data/journal/walkforward.log`

## Guard Rails

- Exclude ETFs from pair universe (they're baskets, not individual names)
- Validate universe against known instrument lists
- Deduplicate pairs — canonical ordering (alphabetical) for pair IDs
- Maximum candidates per API call — respect `max_tokens` limits
- Log every pair promotion/demotion with structured fields and scores
