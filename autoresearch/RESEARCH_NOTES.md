# Pair-Trading & Statistical Arbitrage: Research Notes

**Purpose:** Foundation for the OpenQuant pair-picker rebuild. Six independent scorers will be implemented; these notes give each one its theoretical ground, exact formulas, reference code, and known failure modes.

**Context:**
- Strategy: 30-bar rolling z-score on log-spread, minute bars.
- Target: ~$1k / 2wk on $10k equity, S&P 500 pairs.
- Costs: 5 bps / side, limit orders.
- Oracle: brute-force evaluation on historical minute bars = ground truth "winner" label.
- Pair-picker job: given pre-eval features, predict `P(oracle_label = winner)`.

Reference taxonomy throughout follows Krauss (2017) — five approaches: distance, cointegration, time-series (OU / Kalman), stochastic control, ML, plus "other" (copulas, PCA, etc.).

---

## 1. Distance Method — Gatev, Goetzmann & Rouwenhorst (2006)

**Paper.** Gatev, E., Goetzmann, W. N., & Rouwenhorst, K. G. (2006). *Pairs Trading: Performance of a Relative-Value Arbitrage Rule.* Review of Financial Studies, 19(3), 797–827. https://doi.org/10.1093/rfs/hhj020. Pre-print (NBER w7032): https://www.nber.org/papers/w7032. Wharton PDF: http://stat.wharton.upenn.edu/~steele/Courses/434/434Context/PairsTrading/PairsTradingGGR.pdf

**Formation period.**
- Period length in original paper: **12 months of daily data** on CRSP US stocks, 1962–2002.
- Normalize each stock's price series to start at 1 via cumulative total return: `P_i_t_tilde = P_i_t / P_i_t0`, with dividends reinvested.
- For every pair (i, j), compute the **sum of squared deviations (SSD)** between the two normalized price paths:

  ```
  SSD(i,j) = Σ_{t=1..T_formation} ( P_i_t_tilde − P_j_t_tilde )^2
  ```

- Select the **top 20 pairs with the smallest SSD** as the traded portfolio.

**Trading rule.**
- Trading period = next 6 months.
- Let `σ_ij` be the **standard deviation of the normalized-price spread** measured over the formation period.
- Enter when `|P_i_t_tilde − P_j_t_tilde| > 2·σ_ij` (long the low, short the high).
- Exit when the spread crosses zero (prices re-converge).
- Force close at the end of the 6-month trading period.
- Dollar-neutral: equal dollar long and short at entry; positions do not rebalance intraperiod.

**Reported performance (original paper).** Average **annualized excess return of ~11%** on fully-invested top-20 portfolio (before costs). Decays roughly linearly from 1962 to 2002 — edge was already eroding.

**Known weaknesses (from Do & Faff 2010, Krauss 2017).**
1. **Arbitrary two-sigma threshold** — not derived from any optimization, just a convention. Nothing guarantees it maximizes profit or Sharpe.
2. **No hedge-ratio optimization** — implicitly assumes 1:1 (normalized). Works by construction on price paths but is not a cointegrating vector; the "spread" may not be stationary.
3. **Converges to correlation-like pair selection** — selects co-moving stocks, not mean-reverting ones. Many selected pairs are same-sector duplicates.
4. **Declining profitability** — Do & Faff (2010, "Does Simple Pairs Trading Still Work?") documented near-zero net returns on CRSP post-2002 after costs.
5. **Opens trades when divergence is just noise** — not regime-aware.
6. **SSD picks low-volatility pairs** by construction, which have small absolute spread moves and thus small dollar P&L.

**Implementation reference.**
- arbitragelab: `arbitragelab/distance_approach/basic_distance_approach.py` — https://github.com/hudson-and-thames/arbitragelab/blob/master/arbitragelab/distance_approach/basic_distance_approach.py
- Alternative formulation (Chen et al. 2019): `pearson_distance_approach.py` — uses Pearson correlation on returns instead of SSD on normalized prices.
- Docs: https://hudson-and-thames-arbitragelab.readthedocs-hosted.com/en/latest/distance_approach/distance_approach.html
- Hudson & Thames replication blog: https://hudsonthames.org/distance-approach-in-pairs-trading-part-i/

**For the oracle predictor.** Distance SSD is cheap to compute — O(T) per pair — and gives a single scalar. Easy scorer; weak signal.

---

## 2. Cointegration Method — Engle-Granger, Johansen, Vidyamurthy

**Primary references.**
- Engle, R. F., & Granger, C. W. J. (1987). *Co-integration and Error Correction: Representation, Estimation, and Testing.* Econometrica, 55(2), 251–276.
- Johansen, S. (1988). *Statistical analysis of cointegration vectors.* Journal of Economic Dynamics and Control, 12(2-3), 231–254.
- Vidyamurthy, G. (2004). *Pairs Trading: Quantitative Methods and Analysis.* Wiley. https://www.wiley.com/en-us/Pairs+Trading%3A+Quantitative+Methods+and+Analysis-p-9780471460671

**Engle-Granger two-step (the canonical pair-trading recipe).**
1. OLS regression of log prices: `log(P_y_t) = α + β·log(P_x_t) + ε_t`. Call `β` the hedge ratio.
2. ADF test on residuals `ε_t`. Null: unit root ⇒ no cointegration. Reject at e.g. 5% ⇒ cointegrated pair.
3. If cointegrated, the residual `ε_t` is a stationary mean-reverting "spread" by construction.

**Johansen (multivariate).** Tests for `r` cointegrating vectors in a VAR(p). For a pair, only `r = 1` is meaningful. Use the trace statistic or maximum eigenvalue statistic. Johansen handles both variables symmetrically (no "dependent" choice bias). Standard lag selection via AIC/BIC on the VAR.

**Vidyamurthy's framing.** Vidyamurthy (2004) presents cointegration as a VECM (vector error correction model) and argues for pair selection by economic peer groups first, then testing for cointegration second. He emphasizes that perfect cointegration requires perfect factor alignment — unrealistic — and that most "cointegrated" pairs are actually weakly cointegrated with slowly non-stationary common factors. He introduces the idea of picking a **tolerance band** around the equilibrium residual rather than a fixed sigma rule.

**Threshold choice — traditional.** The cointegration literature is surprisingly vague on thresholds. Most practitioners use the **same 2σ rule as Gatev** on the residual series — which defeats the point of having a proper statistical spread. Vidyamurthy suggests a profit-optimization over an assumed stationary distribution but does not give a closed-form rule. This gap is what Lin et al. (2006) and Puspaningrum et al. (2010) fill with the "minimum profit" approach (see Section 2b), and what Bertram (2010) fills rigorously under the OU assumption (see Section 4).

**Lin, McCrae, Gulati (2006) — Minimum Profit Optimization.**
- Paper: "Loss protection in pairs trading through minimum profit bounds." Advances in Decision Sciences, 2006, 73803.
- Extension: Puspaningrum, Lin & Gulati (2010), "Finding the Optimal Pre-set Boundaries for Pairs Trading Strategy Based on Cointegration Technique." Journal of Statistical Theory and Practice, 4(3), 391–419. https://doi.org/10.1080/15598608.2010.10411994
- Idea: After Engle-Granger, fit residuals to AR(1), then numerically search over threshold values to maximize **Minimum Total Profit (MTP)** over a trading horizon using mean first-passage time theory for stationary AR(1). For a U-trade: `P ≥ N·U` where N is number of unit pairs. Trade-off: higher U ⇒ more profit per trade but fewer trades; lower U ⇒ more trades but less per-trade.

**Known weaknesses.**
1. **ADF power collapses on short samples.** For N < ~500, ADF has low power against persistent near-unit-root spreads ⇒ many true pairs rejected.
2. **Asymmetric Engle-Granger.** Swapping X and Y gives different β and a different test statistic. Johansen fixes this at 2x compute cost.
3. **Contiguity required.** ADF assumes serially observed data. Filtering out gaps (e.g., nights, weekends, halted days) violates the test's autocorrelation structure. At minute bars, crossing session boundaries silently biases the test toward false "cointegrated" verdicts.
4. **Regime-switching cointegration.** Pairs that cointegrate over a 60-day formation window often break in the out-of-sample window. Formal tests (Gregory-Hansen 1996, Hatemi-J 2008) exist for structural breaks but are rarely used in production.
5. **No economic sense built in** — pure statistical fit can find spurious pairs.

**Implementation references.**
- arbitragelab cointegration: https://github.com/hudson-and-thames/arbitragelab/tree/master/arbitragelab/cointegration_approach
  - `engle_granger.py`
  - `johansen.py`
  - `minimum_profit.py` — the Puspaningrum/Lin optimization
  - `coint_sim.py` — synthetic generator for tests
- statsmodels: `statsmodels.tsa.stattools.adfuller`, `statsmodels.tsa.vector_ar.vecm.coint_johansen`
- Minimum profit docs: https://hudson-and-thames-arbitragelab.readthedocs-hosted.com/en/latest/cointegration_approach/minimum_profit.html

---

## 3. Time-Series / State-Space — Kalman Filter for Dynamic Hedge Ratio

**Primary reference.** Elliott, R. J., Van Der Hoek, J., & Malcolm, W. P. (2005). *Pairs Trading.* Quantitative Finance, 5(3), 271–276. https://doi.org/10.1080/14697680500149370. Open PDF: http://stat.wharton.upenn.edu/~steele/Courses/434/434Context/PairsTrading/PairsTradingQFin05.pdf

**Model.** Elliott et al. propose a **discrete-time mean-reverting Gauss-Markov model** for the spread, observed in Gaussian noise:

```
State eq:       x_{k+1} = A·x_k + B + C·ε_{k+1}
Observation eq: y_k     = x_k + D·ω_k
```

where `x_k` is the unobserved true spread, `y_k` is the observed spread, and `ε, ω` are standard normal. This is the discrete analogue of the continuous OU process `dX = ρ(μ−X)dt + σ dW`. Kalman filter gives the optimal linear estimator of the latent `x_k`; the filter's innovation sequence is the basis for the trading signal (magnitude vs. predicted equilibrium drift).

**Dynamic hedge ratio (the more common modern application).**
The state is the hedge ratio `β_t`, the observation is `P_y_t`, and the observation matrix is `P_x_t`:

```
State eq:  β_{t+1}  = β_t + w_t,         w_t ~ N(0, Q)   [random walk β]
Obs eq:    P_y_t    = α_t + β_t·P_x_t + v_t, v_t ~ N(0, R)
```

The Kalman filter updates `β_t` every bar. The residual `P_y_t − α_t − β_t·P_x_t` is the spread; cointegration is recovered dynamically because β adapts. Q governs how fast β drifts, R is spread noise. Both are tuned by (a) maximum-likelihood on historical data or (b) adaptive filters (innovation-based Q estimation).

**Classic arbitragelab / Chan reference implementation.** Chan's *Algorithmic Trading* (2013), Chapter 3, walks through exactly this on EWA/EWC (Australia/Canada ETFs). He initializes β₀ by OLS and tunes Q, R by eyeballing the smoothness of the resulting spread. arbitragelab has `hedge_ratios/kalman_hedge_ratio.py` — https://github.com/hudson-and-thames/arbitragelab/tree/master/arbitragelab/hedge_ratios. pykalman (Python) is the standard library reference.

**Known weaknesses.**
1. **Q, R tuning is underdetermined.** Wrong Q makes β too sticky or too twitchy. MLE on Q, R often gives corner solutions.
2. **Assumes Gaussian innovations.** Jump events (earnings, index rebalances) break the Gaussian assumption.
3. **No cointegration test.** Dynamic β can always find a fit — stationarity of residuals is not guaranteed and must be checked separately.
4. **Initialization matters.** Early bars have large smoothing uncertainty.

**Why this is interesting for us.** Minute-bar β estimates are unstable with rolling OLS because of minute-level noise. A Kalman filter with small Q acts as a natural low-pass on β and may give stabler residuals than rolling-OLS β.

**Reference implementations.**
- pykalman: https://pykalman.github.io/
- arbitragelab hedge_ratios module: https://github.com/hudson-and-thames/arbitragelab/tree/master/arbitragelab/hedge_ratios
- QuantStart tutorial (Chan's EWA/EWC example in Python): https://www.quantstart.com/articles/Kalman-Filter-Based-Pairs-Trading-Strategy-In-QSTrader/

---

## 4. Stochastic Control — Bertram (2010) OU Optimal Thresholds **[PRIMARY FOR US]**

**Paper.** Bertram, W. K. (2010). *Analytic solutions for optimal statistical arbitrage trading.* Physica A: Statistical Mechanics and its Applications, 389(11), 2234–2243. https://doi.org/10.1016/j.physa.2010.01.045. SSRN: https://papers.ssrn.com/sol3/papers.cfm?abstract_id=1505073

Bertram solves the problem: **given an OU spread with known parameters and a transaction cost, pick the entry / exit levels that maximize expected return per unit time** (or Sharpe per unit time). Unlike Gatev's "2σ" heuristic, this is a rigorous first-principles optimum.

### 4.1 Model

Log-price (or spread) `X_t` follows an OU process with long-term mean `θ`, mean-reversion speed `μ` (confusingly called `μ` in Bertram's notation — NOT the drift), and diffusion `σ`:

```
dX_t = μ·(θ − X_t)·dt + σ·dW_t
```

Traded quantity: `p_t = exp(X_t)` (exponential OU). Trader enters at level `a`, exits at level `m`, with symmetric round-trip cost `c` applied once per round-trip. By symmetry of the stationary OU distribution, the optimal pair satisfies `m = 2θ − a` (so entry and exit are equidistant from the mean).

### 4.2 OU Parameter Fit (discrete data)

Bertram assumes parameters are supplied. In practice, fit from discretely sampled observations `X_0, X_Δ, X_{2Δ}, …` one of two ways:

**AR(1) regression (fast).** The exact discrete transition of continuous OU over step Δ is

```
X_{t+Δ} = X_t·e^(−μΔ) + θ·(1 − e^(−μΔ)) + η_t
η_t ~ N( 0, σ²·(1 − e^(−2μΔ))/(2μ) )
```

Fit `X_{t+Δ} = a + b·X_t + ε_t` by OLS, then

```
μ      = −ln(b) / Δ
θ      = a / (1 − b)
σ_OU²  = Var(ε) · 2μ / (1 − b²)
```

(Sigma formula uses `b² = e^(−2μΔ)`.) This is the standard "Smith" recipe used by Chan, arbitragelab, and essentially every OU pair-trading reference.

**Exact MLE (slightly better).** Log-likelihood of the above conditional Gaussian transitions. arbitragelab's `optimal_mean_reversion/ou_model.py` uses MLE via scipy: the docstring is "Finds the optimal Ornstein–Uhlenbeck model coefficients depending on the portfolio prices time series given (p. 13)" with the identity `sigma_tilde_squared = sigma_squared * (1 − exp(−2·μ·dt)) / (2·μ)`.

### 4.3 Expected Trade Length and Variance (Bertram Eqs. 9, 10)

Trade length `T` = time from entry at `a` back to exit at `m`, i.e. a first-passage time of OU. Define the scaled deviations:

```
z_a = (a − θ)·√(2μ) / σ
z_m = (m − θ)·√(2μ) / σ
```

Bertram derives, using the moment-generating function of the OU first-passage time:

**Expected trade length (Eq. 9):**

```
E[T] = (π / μ) · ( Erfi((m − θ)·√μ/σ) − Erfi((a − θ)·√μ/σ) )
```

where `Erfi(x) = −i·Erf(i·x)` is the imaginary error function.

**Variance of trade length (Eq. 10):**

```
V[T] = ( w1(z_m) − w1(z_a) − w2(z_m) + w2(z_a) ) / μ²
```

where `w1` and `w2` are series involving gamma, digamma, and Erfi functions. They are not elementary but are implemented in arbitragelab as private helpers `_w1`, `_w2` in `ou_optimal_threshold_bertram.py`.

### 4.4 Expected Return and Variance per Unit Time (Bertram Eqs. 5, 6)

Per round-trip profit: `r(a, m, c) = m − a − c`. Per unit time:

```
μ_s(a, m, c) = (m − a − c) / E[T]                                (Eq. 5)
σ_s²(a, m, c) = (m − a − c)² · V[T] / E[T]³                      (Eq. 6)
```

**Sharpe ratio per unit time (Eq. 15):**

```
S(a, m, c, r_f) = ( μ_s(a, m, c) − r_f/E[T] ) / sqrt( σ_s²(a, m, c) )
```

### 4.5 Optimal Thresholds (Eq. 13)

By symmetry, the optimum has `m = 2θ − a`, so the problem collapses to one variable `a`. Setting the derivative of expected return per unit time to zero gives the transcendental equation:

```
exp( μ·(a − θ)² / σ² ) · ( 2·(a − θ) + c )  =  σ·sqrt(π/μ) · Erfi( (a − θ)·√μ/σ )    (Eq. 13)
```

Solve numerically for `a` (arbitragelab uses `scipy.optimize.fsolve`). Then `m = 2θ − a`.

For Sharpe ratio maximization, no closed form — use Nelder-Mead on the negative Sharpe ratio (arbitragelab: `scipy.optimize.minimize`, `method='Nelder-Mead'`).

### 4.6 Reference implementation

**arbitragelab — primary reference for this project:**
- `arbitragelab/time_series_approach/ou_optimal_threshold_bertram.py` — https://github.com/hudson-and-thames/arbitragelab/blob/master/arbitragelab/time_series_approach/ou_optimal_threshold_bertram.py
- Parent class: `ou_optimal_threshold.py`
- Also see `optimal_mean_reversion/ou_model.py` for the MLE fit.
- Docs: https://hudson-and-thames-arbitragelab.readthedocs-hosted.com/en/latest/time_series_approach/ou_optimal_threshold_bertram.html
- Blog: https://hudsonthames.org/optimal-trading-thresholds-for-the-o-u-process/

Key function signatures (from the actual code):
```python
expected_trade_length(a, m)     # (π/μ) * (erfi_scaler(m) − erfi_scaler(a))
trade_length_variance(a, m)     # (w1 − w1 − w2 + w2) / μ²
expected_return(a, m, c)        # (m − a − c) / E[T]
return_variance(a, m, c)        # (m − a − c)² * V[T] / E[T]³
get_threshold_by_maximize_expected_return(c)    # fsolve on Eq. 13
get_threshold_by_maximize_sharpe_ratio(c, rf)   # Nelder-Mead
```

### 4.7 Jurek & Yang (2007) — dynamic allocation comparison

**Paper.** Jurek, J. W., & Yang, H. (2007). *Dynamic Portfolio Selection in Arbitrage.* SSRN 882536. https://papers.ssrn.com/sol3/papers.cfm?abstract_id=882536

Different problem framing: rather than a static entry/exit level, Jurek & Yang solve for the **continuous optimal position size** as a function of the current spread and time horizon, under CRRA or Epstein-Zin utility. They use HJB / dynamic programming on the OU spread. Key findings:

- **Intertemporal hedging demand** is significant — a non-myopic arbitrageur scales differently from a myopic one.
- There is a **"stabilization region"** bounded above and below: within it, larger divergence ⇒ larger position. Outside it, further divergence ⇒ smaller position (capitulation arm, because mean-reversion speed seems too slow relative to horizon).
- No analytic entry/exit threshold — positions are continuous.
- Applied to Siamese twin shares (Royal Dutch / Shell type): delivers higher Sharpe than a 2σ threshold rule.

arbitragelab implements Jurek & Yang as `stochastic_control_approach/ou_model_jurek.py` — https://github.com/hudson-and-thames/arbitragelab/blob/master/arbitragelab/stochastic_control_approach/ou_model_jurek.py. For a discrete-order, limit-order-driven system like ours, **Bertram is the better fit** — it outputs the entry/exit levels we actually trade on. Jurek is more useful as a position-sizing overlay on top of Bertram's thresholds.

### 4.8 Known caveats with Bertram

1. **Assumes OU perfectly.** Real spreads have jumps, fat tails, regime shifts.
2. **Constant parameters.** No adaptation to changing θ, μ, σ intraday. Practitioners re-fit daily.
3. **Transaction cost model is a round-trip constant.** Doesn't model slippage as a function of spread size or order book depth.
4. **First-passage time `E[T]` diverges as `a → θ`.** The math is well-behaved for `|a − θ| > 0`, but numerical solvers can struggle near the mean.
5. **Wide thresholds ⇒ high expected profit per trade but few trades.** At minute bars with short windows, we may fit `μ` such that E[T] is larger than the available trading window, producing a "theoretically optimal" threshold that never triggers.

---

## 5. Machine Learning — Krauss et al. (2017), Sarmento & Horta (2020)

**Survey.** Krauss, C. (2017). *Statistical Arbitrage Pairs Trading Strategies: Review and Outlook.* Journal of Economic Surveys, 31(2), 513–545. https://doi.org/10.1111/joes.12153

Krauss reviews >90 papers and groups them into five categories (the taxonomy we're using). Key ML-flavored findings:
- ML approaches are still early and **mostly used for pair selection or signal filtering**, not for replacing the mean-reversion premise.
- Deep learning approaches (Krauss, Do & Huck 2017) showed some alpha in pair selection on S&P 500 during 2010–2015 but with decaying edge.

**Sarmento & Horta (2020).** Sarmento, S. M., & Horta, N. (2020). *Enhancing a Pairs Trading strategy with the application of Machine Learning.* Expert Systems with Applications, 158, 113490. https://doi.org/10.1016/j.eswa.2020.113490. Book: *A Machine Learning Based Pairs Trading Investment Strategy*, SpringerBriefs (2020). ISBN 978-3-030-47250-4.

**Three-stage pair selection pipeline** (the pattern worth copying for universe reduction):

1. **Dimensionality reduction via PCA** on return series. Each stock becomes a vector in principal-component space.
2. **Unsupervised clustering** in PC space with **DBSCAN or OPTICS** — density-based clustering that handles arbitrary cluster shapes and doesn't require specifying k. OPTICS is preferred over DBSCAN for data of varying density.
3. **Within-cluster filtering**: take all intra-cluster candidate pairs and filter with:
   - Cointegration test (Engle-Granger, 5% p-value),
   - Hurst exponent `H < 0.5` (enforces mean-reversion),
   - Half-life of mean reversion inside a sensible range (e.g., 1 day to 1 month).

**Results.** Sarmento & Horta report Sharpe ~3.79 using ML-filtered selection vs. ~3.58 distance / ~2.59 naive on the same universe.

**Why this matters for us.** **Use ML only as a universe reducer**, not as a signal. For S&P 500 there are 500·499/2 ≈ 125k pairs. Clustering + Hurst filter reduces this to a tractable few hundred candidates before any expensive statistical test runs.

**HDBSCAN** is a modern improvement over DBSCAN/OPTICS that automatically handles varying density and returns a hierarchical cluster tree — worth considering for S&P 500 sector clusters.

**Reference implementations.**
- arbitragelab: `arbitragelab/ml_approach/optics_dbscan_pairs_clustering.py` — https://github.com/hudson-and-thames/arbitragelab/blob/master/arbitragelab/ml_approach/optics_dbscan_pairs_clustering.py
- Docs: https://hudson-and-thames-arbitragelab.readthedocs-hosted.com/en/latest/ml_approach/ml_based_pairs_selection.html
- Hudson & Thames blog: https://hudsonthames.org/employing-machine-learning-for-trading-pairs-selection/
- scikit-learn: `sklearn.cluster.DBSCAN`, `sklearn.cluster.OPTICS`
- hdbscan library: https://github.com/scikit-learn-contrib/hdbscan

**Known weaknesses.** Clustering is unstable across window sizes. Results depend strongly on the distance metric chosen (correlation, euclidean, dynamic-time-warping).

---

## 6. Copulas — Liew & Wu (2013), Stander et al. (2013)

**Primary papers.**
- Liew, R. Q., & Wu, Y. (2013). *Pairs trading: A copula approach.* Journal of Derivatives & Hedge Funds, 19, 12–30. https://doi.org/10.1057/jdhf.2013.1
- Stander, Y., Marais, D., & Botha, I. (2013). *Trading strategies with copulas.* Journal of Economic and Financial Sciences, 6(1), 83–108.
- Xie, Liew, Wu, Zou (2016). *Pairs Trading with Copulas.* Journal of Trading, 11(3), 41–52.

**Core idea.** Sklar's theorem: for any joint distribution `F(x,y)` with marginals `F_X`, `F_Y`, there exists a copula `C` such that `F(x,y) = C(F_X(x), F_Y(y))`. Instead of assuming the spread is linear (correlation / OLS-residual), model the joint distribution of the two assets' returns with a copula — which captures tail dependence and nonlinear co-movement.

**Mispricing Index (MPI).** Define, at each time `t`, the conditional probability of one asset's return given the other (using the copula):

```
MI_t^(X|Y) = P( R_t^X ≤ r_t^X  |  R_t^Y = r_t^Y )
MI_t^(Y|X) = P( R_t^Y ≤ r_t^Y  |  R_t^X = r_t^X )
```

Interpretation: `MI^(X|Y)` close to 0 ⇒ X is "unusually low" given Y ⇒ X is underpriced relative to Y. Close to 1 ⇒ X is overpriced.

Because the unconditional marginal is Uniform(0,1), a "fair" MPI would average 0.5. Persistent deviations indicate mispricing.

**Entry signal.** Accumulate the cumulative mispricing index (flag-index) `M_t = Σ (MPI_s − 0.5)`. Enter when `|M_t|` crosses a threshold (typical: ±0.6). Exit when `M_t` crosses 0 (mispricing has unwound). The flag-index is the copula analogue of the z-score.

**Known families.** Gaussian, Student-t (tail dependence), Clayton (lower-tail dependence), Gumbel (upper-tail dependence), Frank, mixed copulas. Fit by MLE after marginal fits (ECDF or KDE for nonparametric marginals).

**Strengths.** Captures tail co-movement that correlation misses; works for non-Gaussian return distributions; no stationarity assumption on the spread.

**Weaknesses.**
1. **Copula selection is tricky** — wrong family ⇒ biased conditional probabilities. Goodness-of-fit tests (Cramer-von Mises) are weak for small samples.
2. **Marginal fit also matters** — bad marginals feed bad inputs to the copula.
3. **No explicit spread or hedge ratio** — position sizing is ambiguous (Liew & Wu use equal dollar legs).
4. **Minute-bar returns have very different copula structure than daily** due to microstructure; most published copula pair-trading work is on daily.

**Reference implementations.**
- arbitragelab: `arbitragelab/copula_approach/` — https://github.com/hudson-and-thames/arbitragelab/tree/master/arbitragelab/copula_approach
  - Archimedean (Clayton, Gumbel, Frank): `copula_approach/archimedean/`
  - Elliptical (Gaussian, Student-t): `copula_approach/elliptical/`
  - Mixed copulas: `copula_approach/mixed_copulas/`
  - `pairs_selection.py`, `vine_copula_partner_selection.py`
- Docs: https://hudson-and-thames-arbitragelab.readthedocs-hosted.com/en/latest/trading/mispricing_index_strategy.html
- Hudson & Thames blog: https://hudsonthames.org/copula-for-pairs-trading-overview-of-common-strategies/
- Python: `copulas` library (https://github.com/sdv-dev/Copulas), `statsmodels.distributions.copula`

---

## arbitragelab — full module map

Repo: https://github.com/hudson-and-thames/arbitragelab

| Approach | arbitragelab module | Key file |
|---|---|---|
| Distance (Gatev) | `distance_approach/` | `basic_distance_approach.py`, `pearson_distance_approach.py` |
| Cointegration (Engle-Granger, Johansen) | `cointegration_approach/` | `engle_granger.py`, `johansen.py` |
| Minimum Profit (Lin/Puspaningrum) | `cointegration_approach/` | `minimum_profit.py` |
| Cointegration simulation | `cointegration_approach/` | `coint_sim.py` |
| Multi-asset cointegration | `cointegration_approach/` | `multi_coint.py`, `sparse_mr_portfolio.py` |
| Hedge ratios (OLS, TLS, Kalman, min-half-life) | `hedge_ratios/` | `kalman_hedge_ratio.py` and siblings |
| Kalman / state-space signals | `time_series_approach/` | `h_strategy.py`, `quantile_time_series.py` |
| **Bertram OU threshold** | `time_series_approach/` | **`ou_optimal_threshold_bertram.py`** |
| Zeng OU threshold | `time_series_approach/` | `ou_optimal_threshold_zeng.py` |
| Regime-switching | `time_series_approach/` | `regime_switching_arbitrage_rule.py` |
| Optimal mean reversion (OU fit, CIR, XOU) | `optimal_mean_reversion/` | `ou_model.py`, `cir_model.py`, `xou_model.py`, `heat_potentials.py` |
| Stochastic control (Jurek, Mudchanatongsuk, convergence) | `stochastic_control_approach/` | `ou_model_jurek.py`, `ou_model_mudchanatongsuk.py`, `optimal_convergence.py` |
| ML-based selection (PCA + DBSCAN/OPTICS) | `ml_approach/` | `optics_dbscan_pairs_clustering.py` |
| ML filters / TAR / NN regressors | `ml_approach/` | `filters.py`, `tar.py`, `neural_networks.py`, `regressor_committee.py` |
| Copulas (Archimedean, elliptical, mixed, vine) | `copula_approach/` | `archimedean/`, `elliptical/`, `mixed_copulas/`, `vinecop_strategy.py`, `pairs_selection.py` |
| Spread selection / filtering | `spread_selection/` | various |
| Trading / execution wrappers | `trading/` | various |

**Other libraries worth knowing.**
- `statsmodels` — `tsa.stattools.adfuller`, `tsa.vector_ar.vecm.coint_johansen`, `tsa.stattools.coint` (Engle-Granger wrapper).
- `pykalman` — state-space models and Kalman filter — https://pykalman.github.io/
- `copulas` by SDV — https://github.com/sdv-dev/Copulas
- `hurst` — Hurst exponent — https://pypi.org/project/hurst/
- `PyPortfolioOpt` — for sizing after pair selection — https://github.com/robertmartin8/PyPortfolioOpt
- `mlfinlab` — de Prado techniques, purged CV, meta-labeling.

---

## Comparison Matrix

| Approach | Measures | Timescale assumed | Compute / pair | Failure modes | Outputs $/day? |
|---|---|---|---|---|---|
| **Distance (Gatev)** | L2 distance between normalized price paths, 2σ threshold | Daily in original; scale-free in principle | O(T) | Ad-hoc threshold; no hedge ratio; selects low-vol pairs; edge decayed post-2002 | No — binary accept only, no profit model |
| **Cointegration (Engle-Granger / Johansen)** | Test whether a linear combo of log-prices is stationary | Any; but ADF needs ~100–500 contiguous obs for power | O(T) OLS + O(T) ADF + optional Johansen eigendecomp | Low power at short samples; asymmetry in EG; regime breaks; contiguity sensitive | No — p-value, not $ |
| **Cointegration + Minimum Profit (Lin/Puspaningrum)** | Cointegrated spread; optimize threshold for Minimum Total Profit via AR(1) first-passage | Daily in paper; assumes AR(1) spread | O(T) + numerical threshold search | Inherits coint weaknesses; AR(1) is coarser than OU; MTP is conservative | **Yes** — MTP in dollars per window |
| **Kalman filter (Elliott et al. / Chan)** | Latent true spread or time-varying β; residual is signal | Any | O(T) filter forward pass | Q, R tuning; Gaussian assumption; no built-in cointegration test | Indirectly — z-score of innovations, not $ |
| **Bertram OU optimal thresholds** | Expected $/time and Sharpe/time of entry/exit on OU spread with cost | Continuous OU; discrete sampling via AR(1) or MLE | O(T) fit + O(1) threshold root-find | Requires valid OU fit; sensitive to μ̂; first-passage diverges near mean; microstructure noise biases μ̂ upward at minute bars | **Yes** — μ_s(a,m,c) in $/sec or $/day directly |
| **Jurek & Yang stochastic control** | Optimal continuous position as f(spread, horizon) under CRRA/EZ utility | Continuous OU | O(T) fit + grid evaluation of HJB | Complex; utility-dependent; more position-sizing than selector | Yes — expected utility-adjusted return |
| **ML clustering (Sarmento & Horta)** | Pair universe reduction (PCA ⇒ DBSCAN/OPTICS clusters ⇒ Hurst < 0.5) | Any | O(N²) for distances + O(N log N) clustering | Cluster instability; hyperparameter sensitivity | No — returns candidate list, not $ |
| **Copula + MPI (Liew & Wu)** | Conditional quantile mispricing from joint copula fit | Daily in literature; minute less tested | O(T) marginal fit + copula MLE; O(T) CDF evaluation | Wrong copula family; marginal fit errors; no explicit hedge ratio | No — flag index, not $ |

**"Expected $/day" winner.** Only **Bertram** and **Minimum Profit** produce a defensible expected-dollar-per-day number from the model itself. Everything else can be ranked by auxiliary metrics (p-value, SSD, Hurst, cluster density, MPI threshold hit frequency) but requires an empirical simulation layer to convert to $.

---

## Key question: Does Bertram work at minute-bar timescale?

**Short answer.** Theoretically yes; practically, it is biased, and the most important literature result is that **minute-bar OU estimates of the mean-reversion speed μ are upward-biased by microstructure noise, by a factor that can exceed 6x.** Naive minute-bar Bertram will overstate edge and pick thresholds that are too tight.

### The dt sensitivity problem

The OU discrete transition is
```
X_{t+Δ} = X_t·e^(−μΔ) + θ·(1 − e^(−μΔ)) + η_t
```
As Δ → 0, `e^(−μΔ) → 1 − μΔ`, so the AR(1) coefficient `b → 1 − μΔ`. The information content of a single observation about μ scales with Δ:

- At daily bars (Δ = 1 day), a true half-life of 5 days gives `b ≈ 0.87` — easily distinguishable from 1.
- At minute bars (Δ = 1/390 day), the same true half-life gives `b ≈ 0.99964` — separated from 1 by only ~4e-4. Estimating this from noisy data is hard.

**Sample size still helps.** 12k observations over a month (390 × 30) is many more observations than daily's 252/year — so the raw sample size is fine. The issue is **bias**, not variance.

### Microstructure noise — the real problem

Holý & Tomanová (2025, formerly 2018). *Estimation of Ornstein-Uhlenbeck Process Using Ultra-High-Frequency Data with Application to Intraday Pairs Trading Strategy.* Annals of Operations Research, published 2025. arXiv: https://arxiv.org/abs/1811.09312. https://doi.org/10.1007/s10479-025-06855-7

Key results from the paper (quoted / paraphrased from the abstract and intro):

> "Ornstein-Uhlenbeck parameters estimated by methods ignoring the noise are biased and inconsistent because the Ornstein-Uhlenbeck process contaminated by independent Gaussian white noise and observed at discrete equidistant times follows an **ARMA(1,1) process instead of an AR(1) process**."

> "On average, the speed of reversion τ is **6.36 times higher** when estimated via noise-sensitive methods versus noise-robust alternatives."

> "Even when the variance of the noise is relatively small, it has a great impact on the estimated parameters, and reliance on biased estimates can lead to wrong decisions and have harmful consequences."

**Mechanism.** Observation noise `ω` adds to each discrete observation: `Y_t = X_t + ω_t`. The autocorrelation of `Y` at lag 1 is lower than of `X`, making the AR(1) coefficient `b̂` appear closer to zero than the true value — i.e. faster decay — i.e. **overestimated μ**. Bertram will then think the spread reverts faster than it does, E[T] will be too small, and optimal thresholds will be too tight, entries will be too frequent, and realized Sharpe will collapse because the spread does not revert as predicted.

The paper's remedy:
- **TICK-MLE-NR** (tick-level maximum likelihood with noise): estimates `(θ, μ, σ, ω²)` jointly, treating the observed process as an ARMA(1,1) or irregular-sampling likelihood with Gaussian noise.
- Compared to minute-aggregated data: "tick data with noise-robust methods outperform the noise-robust estimators based on 1-minute data."
- The paper explicitly tests minute-bar aggregation (labeled "1MIN-MLE-NR") and finds it a reasonable fallback when tick data are unavailable — **as long as the noise-robust correction is applied**.

Hudson & Thames wrote a companion piece: *Caveats in Calibrating the OU Process* (Aug 2021, ~9k words). https://hudsonthames.org/caveats-in-calibrating-the-ou-process/ — the landing page lists keywords Euler scheme, mean reversion, Monte-Carlo, parameter estimation, and discusses exactly this bias.

### Sample size heuristics for OU

From Tang & Chen (2009) and Phillips & Yu (2005) (discrete-sampled continuous-time model estimation):

- For a half-life `τ_1/2 = ln(2)/μ`, you want **at least ~10 half-lives of observations** for a usable `μ̂`. E.g., half-life 5 days ⇒ at least 50 days of contiguous data. At minute bars, half-life 30 minutes ⇒ ~300 minutes (~5 hours) minimum — easily met in a single session.
- **Bias in `μ̂` is O(1/N)** in the AR(1) plug-in, but noise bias is O(1) (doesn't vanish with N) — sample size alone does not fix it.

### Verdict for OpenQuant

1. **Bertram's framework is mathematically fine at minute bars** — nothing in the continuous-time math requires daily sampling. 12k observations is plenty for sampling-variance purposes.
2. **But the naive OLS/MLE fit of (θ, μ, σ) from minute bars is biased.** Without a noise correction, you will **overestimate μ** by ~2–6x, **underestimate E[T]** by the same factor, and **choose entry/exit levels that are far too tight**. In backtest this looks like "the model says we should be making 5 trades per day at 30 bps each" — reality says "those trades lose money because the spread did not revert as quickly as modeled."
3. **Two mitigations to consider** (not to implement now):
   - Fit OU on **lower-frequency subsamples** (e.g., every 10 minutes or every 30 minutes) where noise ratio is smaller. Loses sample but keeps bias down.
   - Fit the **ARMA(1,1) noise-robust model** (Holý & Tomanová) that estimates observation-noise variance jointly.
4. **For the pair-picker scorer**, Bertram at minute bars is useful as a **rank-ordering signal** even if absolute μ is biased — as long as the bias is roughly uniform across pairs, the relative ordering (which pair has the fastest, cleanest OU) is preserved. Do **not** use the absolute `μ_s(a,m,c)` dollar number as a position-sizing input without validation against the oracle.
5. **Cross-check against the oracle.** Exactly this kind of bias is what the brute-force minute-bar oracle is designed to expose. If Bertram says pair X has expected 8 bps/trade and the oracle says 2 bps/trade, the calibration is the problem, not the framework.

### Minute-bar-specific additional caveats beyond OU bias

- **Session boundaries.** Overnight gaps are not OU innovations. Either exclude them from the fit and the trading, or treat them as observation gaps.
- **Close-to-open autocorrelation.** First/last ~15 minutes of the session have different dynamics (opening auction, closing auction effects).
- **Bid-ask bounce.** Trade-to-trade prices bounce between bid and ask — this is exactly the independent Gaussian noise Holý & Tomanová model. Use midpoint prices, not last-trade prices, to partially mitigate.
- **Half-life interpretation.** A half-life of "15 minutes" means 15 minutes of *trading time*, not wall clock. Be careful crossing sessions.

---

## Summary of scorers for the OpenQuant rebuild

Rank each pair by a single scalar, use scalars as features in a supervised model (labels from the oracle), or gate pairs by thresholds. Each scorer here is a **standalone, parallel** computation — no pipeline coupling.

1. **Distance SSD scorer** — cheap baseline; O(T) per pair.
2. **Engle-Granger ADF t-stat scorer** — standard cointegration; most-negative t-stats first.
3. **Half-life scorer** — from AR(1) OU fit: `half_life = −ln(2)/ln(b)`. Rank by half-life in a target band.
4. **Hurst exponent scorer** — `H < 0.5` ⇒ mean-reverting (Sarmento-Horta filter).
5. **Bertram optimal-threshold scorer** — expected return per unit time `μ_s(a*, m*, c)` given fit OU, with noise-robust caveat noted above.
6. **Copula MPI frequency scorer** — how often does |flag-index| cross its threshold in the formation window?
7. **Kalman-residual stationarity scorer** — fit Kalman β on formation, test stationarity of innovations.
8. **(Optional) ML cluster density scorer** — binary: is the pair in a dense PCA-DBSCAN cluster?

Each scorer is one function, one formula, one reference. The six sections above provide the math, the papers, and the arbitragelab files to crib from.

---

## References (bundled)

**Primary papers.**
- Gatev, Goetzmann & Rouwenhorst (2006). *Pairs Trading: Performance of a Relative-Value Arbitrage Rule.* RFS 19(3), 797–827. https://doi.org/10.1093/rfs/hhj020
- Engle & Granger (1987). *Co-integration and Error Correction.* Econometrica 55(2), 251–276.
- Johansen (1988). *Statistical analysis of cointegration vectors.* JEDC 12(2-3), 231–254.
- Vidyamurthy (2004). *Pairs Trading: Quantitative Methods and Analysis.* Wiley.
- Elliott, Van Der Hoek & Malcolm (2005). *Pairs Trading.* Quantitative Finance 5(3), 271–276. https://doi.org/10.1080/14697680500149370
- Bertram (2010). *Analytic solutions for optimal statistical arbitrage trading.* Physica A 389(11), 2234–2243. https://doi.org/10.1016/j.physa.2010.01.045. SSRN: https://papers.ssrn.com/sol3/papers.cfm?abstract_id=1505073
- Jurek & Yang (2007). *Dynamic Portfolio Selection in Arbitrage.* SSRN 882536. https://papers.ssrn.com/sol3/papers.cfm?abstract_id=882536
- Lin, McCrae & Gulati (2006). *Loss protection in pairs trading through minimum profit bounds.* Advances in Decision Sciences 2006, 73803.
- Puspaningrum, Lin & Gulati (2010). *Finding the Optimal Pre-set Boundaries for Pairs Trading Strategy Based on Cointegration Technique.* J. Stat. Theory & Practice 4(3). https://doi.org/10.1080/15598608.2010.10411994
- Liew & Wu (2013). *Pairs trading: A copula approach.* J. Derivatives & Hedge Funds 19, 12–30. https://doi.org/10.1057/jdhf.2013.1
- Stander, Marais & Botha (2013). *Trading strategies with copulas.* J. Econ. & Fin. Sciences 6(1), 83–108.
- Krauss (2017). *Statistical Arbitrage Pairs Trading Strategies: Review and Outlook.* J. Econ. Surveys 31(2), 513–545. https://doi.org/10.1111/joes.12153
- Sarmento & Horta (2020). *Enhancing a Pairs Trading strategy with the application of Machine Learning.* Expert Systems with Applications 158, 113490.
- Holý & Tomanová (2018/2025). *Estimation of Ornstein-Uhlenbeck Process Using Ultra-High-Frequency Data with Application to Intraday Pairs Trading Strategy.* arXiv:1811.09312 / Annals of Operations Research. https://arxiv.org/abs/1811.09312

**Books.**
- Chan, E. (2013). *Algorithmic Trading: Winning Strategies and Their Rationale.* Wiley. (Ch. 3 Kalman filter, Ch. 5 mean reversion.)
- Chan, E. (2021). *Quantitative Trading, 2nd Ed.* Wiley.

**Code.**
- arbitragelab (Hudson & Thames): https://github.com/hudson-and-thames/arbitragelab
- statsmodels: https://www.statsmodels.org/stable/tsa.html
- pykalman: https://pykalman.github.io/
- scikit-learn OPTICS/DBSCAN: https://scikit-learn.org/stable/modules/clustering.html
- hdbscan: https://github.com/scikit-learn-contrib/hdbscan

**Hudson & Thames blog posts with pedagogical derivations.**
- *Optimal Trading Thresholds for the O-U Process* — https://hudsonthames.org/optimal-trading-thresholds-for-the-o-u-process/
- *Pairs Trading with Stochastic Control and OU process* — https://hudsonthames.org/pairs-trading-with-stochastic-control-and-ou-process/
- *Distance Approach in Pairs Trading: Part I* — https://hudsonthames.org/distance-approach-in-pairs-trading-part-i/
- *Machine Learning for Trading Pairs Selection* — https://hudsonthames.org/employing-machine-learning-for-trading-pairs-selection/
- *Copula for Pairs Trading: Strategies Overview* — https://hudsonthames.org/copula-for-pairs-trading-overview-of-common-strategies/
- *Caveats in Calibrating the OU Process* — https://hudsonthames.org/caveats-in-calibrating-the-ou-process/
