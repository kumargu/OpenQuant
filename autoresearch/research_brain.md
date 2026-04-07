# OpenQuant Research Brain

Distilled knowledge from V1 experiments + research. Read this before every experiment.

## V1 Best Config (avg_bps=+13.4, 186 trades, 54.3% WR)
- entry_z=2.0, exit_z=0.5, stop_z=6.0, min_hold=1
- lookback floor=20 bars, ADF: calm=0.10, volatile=0.05

## What We Proved Works (V1, 12 experiments)
- **Higher entry threshold**: 2.0σ, not 1.0. Below 2.0, extra trades are losers.
- **Wider stops**: 6.0σ gives spreads room to overshoot before reverting.
- **Earlier exits**: 0.5σ captures bulk of move without risking reversal.
- **Stable z-scores**: Lookback floor=20 bars. Below 20, fast-pair z-scores are noise.
- **Looser ADF screening**: p<0.10 calm, p<0.05 volatile. Old thresholds over-filtered.
- **Structural break gate**: Keep it. Broken pairs ARE genuinely broken.

## What We Proved Doesn't Work
- entry_z=1.5: dead zone, trades at 1.5-2.0σ are losers.
- entry_z=2.5: too few trades (100 vs 186).
- max_hold_bars config: dead knob, per-pair override dominates.
- lookback floor=15: too low, noise returns.
- Removing structural break gate: admits broken pairs.
- HL-heavy scoring (40%): fast HL ≠ profitable.
- min_hold=3: forces day-1 winners into losers.

## Regime Pattern (Critical — #1 unsolved problem)
Strategy alternates good/bad months based on intra-sector dispersion:
- **GOOD** (high sector correlation): May, Jul, Aug, Oct, Jan-Mar
- **BAD** (high sector dispersion): Jun, Sep, Nov
- Bad months have MORE trades (over-trading into bad regime)
- No dispersion gate exists

## Pair Utilization Problem
- 13,573 candidates → 645 loaded → 78 traded (12% utilization)
- 567 pairs selected but NEVER trade. 88% waste.
- GOOG/GOOGL (same company) traded 21 times — exclude
- Top winners: financials (AXP/BAC, COF/JPM, GS/JPM)
- MCO/NTRS lost -733 bps over 4 trades — per-pair learning needed
- Root cause (Zhu 2024, Yale): problem is low convergence probability, not lack of cointegrated pairs. We optimize for stationarity when we should optimize for tradability.

---

## V2 Research Findings (3 parallel research agents, Apr 2026)

### Gap 1: Sector Dispersion Gate (HIGHEST PRIORITY)
No awareness of cross-sectional sector dispersion. Simple implementation:
```
dispersion = std(sector_returns) / mean_abs(sector_returns)
if dispersion > 90th percentile:
    reduce sizing 50%, tighten entry_z to 2.5, tighten stop_z to 4.0
```
~50 lines of Rust, computed daily from universe of traded symbols.

### Gap 2: Spread CUSUM (Catch decoupling before stop loss)
We have CUSUM on returns but NOT on spread residuals. MQL5 research shows CUSUM detected NVDA/INTC break one day after announcement.
```
limit = 0.948 × (sqrt(n) + 2n/sqrt(n))  // 95% CI
trigger when |cusum_value| > limit → close position, pause pair
```
Estimated: cuts 30-50% of stop losses. Adapt existing cusum.rs code.

### Gap 3: Kalman Filter Hedge Ratio
Static OLS beta drifts between weekly recomputations. Kalman filter updates beta every bar:
```
State: [mu, gamma] (intercept, hedge ratio)
Observation: y1_t = [1, y2_t] × [mu, gamma]' + noise
```
~100-150 lines of Rust. Crates: `kalman_filters` v1.0.1, `adskalman-rs`. Or hand-roll for 2-state system.

### Gap 4: Spread Crossing Frequency Filter (Fixes 88% non-trade problem)
Count zero-crossings of demeaned spread. Reject if annualized crossings < 12. If the spread doesn't oscillate, you cannot trade it. ~10 lines of Rust in pipeline.rs.

### Gap 5: Per-Pair Optimal Entry via Minimum Profit Condition
Lin, McCrae & Gulati (2006): compute mean first-passage time from entry threshold U to zero. Optimize U per pair to maximize expected total profit MTP(U). Eliminates one-size-fits-all entry_z. Pairs with MTP ≈ 0 are exactly those that never trade. ~200 lines in new stats/min_profit.rs.

### Gap 6: Hurst Exponent
ADF alone has low power (Firoozye 2025). Spreads with H < 0.5 (anti-persistent) revert faster. Ramos-Requena (2024): Hurst + cointegration profitable 2019-2024, cointegration-only was not. Rolling R/S on 50-bar window. ~100 lines.

### Gap 7: RVOL Entry Gate
System has ZERO volume awareness. RVOL for pairs should be INVERTED from directional: want NORMAL volume (0.3-3.0x). Very low = stale prices. Very high = event-driven, may not revert. Already computed in features but not used by pairs engine.

### Gap 8: Dynamic Cost Estimation (Corwin-Schultz)
Replace flat cost_bps=10.0 with per-pair estimate from High/Low prices:
```
beta = ln(H/L)² for two consecutive periods
S = 2*(exp(alpha) - 1) / (1 + exp(alpha))
```
~50 lines. Aggregate minute bars to 15-min first. No Rust crate exists.

### Gap 9: BOCPD on Spread (Early warning)
Current regime gate counts consecutive losses — LAGGING indicator. BOCPD detects distributional shifts BEFORE they become losses. Rust crates: `changepoint` v0.15.0, `augurs-changepoint` v0.10.2. When changepoint probability > 0.8: close position, pause entries.

### Gap 10: Cut Portfolio from 40 to 15-20 Pairs
Research consensus (Gatev 2006): above 30 pairs, no diversification benefit. Our 40 pairs means many low-quality filler pairs. Cut to 15-20 high-tradability pairs.

### Gap 11: Same-Company Exclusion
GOOG/GOOGL, BRK.A/BRK.B are not real pairs trades. Hard-code exclusion list or detect via CUSIP/CIK matching.

---

## CRITICAL: bps ≠ dollars (discovered in live paper trading Apr 6)
- Engine reports +1,450 bps across 19 trades but account is DOWN $1,062
- cost_bps=10.0 is theoretical — real Alpaca execution costs are much higher
- At $1K/leg notional, a 10 bps edge = $1.00 profit but bid-ask alone costs $2-5
- $1K/leg is below minimum viable scale
- Issue #229 warned: real round-trip costs are 50-100+ bps, not 10
- **V2 must validate real execution costs before any other optimization**

## V2 Constraints
- **Capital: $10K daily** — size positions accordingly
- **Goal: net profit in any rolling 2-week window** — not annual Sharpe, not avg_bps. Actual dollars, actual 2-week P&L.
- **Rust changes allowed** — V2 is structural, not config knobs
- **Metric: 2-week rolling P&L in dollars** (not bps)

## V2 Experiment Priority (by expected impact × implementation ease)

| # | Experiment | Type | Effort | Expected Impact |
|---|-----------|------|--------|-----------------|
| 0 | **Cost validation + notional sizing** | Analysis + Config | Low | CRITICAL — if real costs > edge, nothing else matters |
| 1 | Sector dispersion gate | Rust | Low (50 LOC) | HIGH — directly fixes seasonal losses |
| 2 | Spread crossing frequency filter | Rust | Low (10 LOC) | HIGH — fixes 88% non-trade problem |
| 3 | Same-company pair exclusion | Rust | Low (10 LOC) | LOW — removes 21 noise trades |
| 4 | Spread CUSUM for live decoupling | Rust | Med (adapt existing) | HIGH — cuts 30-50% stop losses |
| 5 | RVOL entry gate (0.3-3.0x) | Rust | Low (20 LOC) | MED — blocks stale/event entries |
| 6 | Cut portfolio to 20 pairs | Config | Low | MED — fewer but better pairs |
| 7 | Kalman filter hedge ratio | Rust | Med (150 LOC) | HIGH — eliminates stale beta |
| 8 | Per-pair optimal entry_z | Rust | Med (200 LOC) | HIGH — replaces one-size-fits-all |
| 9 | Hurst exponent in scoring | Rust | Med (100 LOC) | MED — better pair quality |
| 10 | Dynamic cost (Corwin-Schultz) | Rust | Med (50 LOC) | LOW — better P&L accuracy |
| 11 | BOCPD on spread | Rust | Med (crate) | MED — early regime warning |

## Key Literature
- Gatev (2006): 2.0σ entry, top 20 pairs, no volume filter
- Avellaneda & Lee (2010): 60-day lookback, OU process
- Chan (2013): lookback ≈ half-life, Kelly sizing
- de Prado (2018): meta-labeling, deflated Sharpe, purged CV
- Clegg (2014): 4.9% pairs stay cointegrated year-over-year
- Krauss (2017): regime detection critical for pairs trading
- Zhu (2024, Yale): profitability decline from lower convergence probability
- Rad, Low & Faff (2016): cointegration best in turbulent markets
- Lin, McCrae & Gulati (2006): minimum profit bounds for pair selection
- Ramos-Requena (2024): Hurst H<0.5 predicts faster mean-reversion
- Firoozye (2025): ADF has low power, use ADF+KPSS+VR+Hurst battery
- Chen et al. (2021): SAPT "hold" action, Sharpe +326% vs standard pairs
- Corwin & Schultz (2012): spread estimation from H/L prices
- Sarmento & Horta (2020): PCA+DBSCAN clustering, Sharpe 3.79 vs 3.58 sector-based
