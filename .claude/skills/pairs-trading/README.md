# Pairs Trading — Reference

## Mathematical Foundations

### Augmented Dickey-Fuller (ADF) Test
Tests null hypothesis that a time series has a unit root (non-stationary).
- Test statistic: t-ratio of `φ` in `Δy_t = φ * y_{t-1} + Σ(γ_i * Δy_{t-i}) + ε_t`
- Critical values depend on sample size and number of lags
- For cointegration: apply ADF to OLS residuals (Engle-Granger two-step)

**Implementation gotcha**: The lag selection matters. Too few lags → size distortion. Too many → power loss. Use information criteria (AIC/BIC) or fixed rule like `floor(12 * (T/100)^0.25)`.

### Ornstein-Uhlenbeck Process
Model for mean-reverting spread: `dS = θ(μ - S)dt + σdW`
- θ = speed of mean reversion
- μ = long-run mean
- σ = volatility of spread
- Half-life = `ln(2) / θ`

### Kalman Filter for Hedge Ratio
State-space model where the hedge ratio is the hidden state:
- Observation: `Y_t = β_t * X_t + ε_t`
- State: `β_t = β_{t-1} + η_t`
- Kalman gain adapts β over time, tracking structural changes

### Normal Inverse Gaussian (NIG) Distribution
Heavy-tailed distribution for modeling spread returns:
- Parameters: α (tail heaviness), β (asymmetry), δ (scale), μ (location)
- Conjugate update: Murphy 2007 — verify formula for conjugate posterior
- Better fit than Gaussian for financial return distributions

## Pair Lifecycle

```
Candidate → Screened → Cointegrated → Scored → Active → Trading → Monitor → (Demote if broken)
```

### Promotion criteria
- ADF p-value < configured threshold (e.g., 0.05)
- Half-life within tradeable range (e.g., 5-60 bars)
- Both legs liquid (min volume threshold)
- Spread z-score historically mean-reverting

### Demotion criteria
- Cointegration breaks (rolling ADF p-value exceeds threshold)
- Half-life extends beyond tradeable range
- One leg becomes illiquid
- Sustained spread divergence (structural break)

## File Contracts

### `active_pairs.json`
- Produced by: pair-picker binary
- Consumed by: trading engine (PairsEngine)
- Format: array of pair objects with symbols, hedge ratio, scoring metadata
- Canonical pair ID: alphabetically ordered `"AAPL_MSFT"` format

### `data/experiment_bars_*.json`
- Historical bar data for backtesting pair candidates
- Keyed by date and timeframe (1min, 5min, 15min)

## Key Parameters (in `openquant.toml`)

| Parameter | Purpose | Typical Range |
|---|---|---|
| `adf_pvalue_threshold` | Cointegration gate | 0.01 - 0.10 |
| `min_half_life` | Fastest acceptable mean reversion | 3 - 10 bars |
| `max_half_life` | Slowest acceptable mean reversion | 30 - 100 bars |
| `zscore_entry` | Spread z-score to enter | 1.5 - 2.5 |
| `zscore_exit` | Spread z-score to exit | 0.0 - 0.5 |
| `lookback_window` | Rolling stats window | 60 - 252 bars |
