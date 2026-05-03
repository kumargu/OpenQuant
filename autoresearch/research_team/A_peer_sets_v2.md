# Memo A v2 — Peer-set construction for basket stat-arb

**Status:** v2. ~4hr deep research pass over v1. The core question: what do production stat-arb desks actually use to construct peer sets, why is GICS-only insufficient, and which alternatives are likely to have changed our 0/4 FAANG outcome vs. which are academic curiosities? This memo verifies v1's claims against primary sources and corrects three of them where v1 over-stated the evidence.

---

## Executive summary

1. **Avellaneda–Lee 2010 PCA-residual stat-arb is the most thoroughly documented method, with explicit math.** 60-day window, top-15 (or variable, by 55% explained-variance threshold) PCA factors, OU residual, s-score normalization `s_i = (X_i(t) − m_i)/σ_eq,i`, entry/exit thresholds `s_bo = s_so = 1.25`, `s_bc = 0.75`, `s_sc = 0.50`. Stocks rejected if mean-reversion speed `κ < 252/30 = 8.4` (i.e. half-life > ~22 trading days). Sharpe 1.44 (1997-2007), but **0.9** in 2003-2007. **Authors document degradation post-2003.** This is not a magic bullet — it has aged.

2. **v1's claim that Sarmento-Horta's Sharpe 3.79 result demonstrates clustering "moves the needle" on US equities is misleading.** Verified primary source: Sarmento-Horta tested on **208 commodity-linked ETFs at 5-min frequency, Jan 2009 – Dec 2018**. NOT US equities, NOT FAANG-style mega-caps. The 3.79 Sharpe applies to a universe with no FAANG problem and no GICS-vs-residual debate. Their ARODs (Hurst < 0.5, half-life ∈ [78 min, 20000 min], cointegration p-value, mean-crossings ≥ 12/year) are reusable as filters but their reported Sharpe is not directly transferable.

3. **Cartea-Cucuringu-Jin 2023 SPONGE on US equities is fragile.** Reported Sharpe 1.1 collapses to **0.28 with 0.05% transaction costs** — TC eat 4× the gross profit. Replication paper found returns dropped from 49.3% to 10.7% when two outlier stocks (EP, CPWR) are excluded. **This is the closest published direct test of clustering-based stat-arb on US equities, and it does not survive realistic costs or robustness checks.**

4. **Practitioner replication of clustering-stat-arb consistently underperforms the academic claims.** Han-He-Jun-Toh (2021) reports Sharpe 2.69 on US equities → independent practitioner replication achieves 0.4. DBSCAN on US equity returns reliably produces "one giant cluster + outliers." OPTICS is parameterless in name only — `xi`, `min_samples`, and the upstream PCA component count all matter.

5. **Three methods v1 missed deserve mention.** (a) Box-Tiao canonical decomposition (1977) and d'Aspremont's sparse extension — generalized eigenvalue problem on predictability matrix, gives ranked mean-reverting baskets directly. Mostly used on rates/swaps, not equities. (b) Partial cointegration (Clegg-Krauss 2018) — state-space split of spread into MR and random-walk components; `R²[MR]` > threshold is the screening statistic; reports 12% net annual return on S&P 500 1990-2015. (c) Hoberg-Phillips TNIC text-based industry classification — annually-rebuilt cosine similarity on 10-K product descriptions; addresses the GICS-staleness problem directly (V/MA reclassifications, AMZN-as-cloud, etc.) but is not widely used in published stat-arb.

6. **For the FAANG/entsw 0/4 outcome, the most defensible recommendation is:** combine Avellaneda-Lee-style residual construction (strip top-k PCs from the universe) with a per-basket admission test that includes a *minimum-explained-variance check on the residual* and a partial-cointegration `R²[MR]` filter. **DO NOT rely solely on cluster-based pair selection (DBSCAN/OPTICS/SPONGE) — the published evidence on US equities is fragile and replication is a known issue.** TNIC similarity is plausibly useful as a *complement* to GICS for the FAANG-style mega-cap issue but no published stat-arb replication exists.

---

## Methods (10)

### Method 1 — Avellaneda-Lee PCA residuals (canonical academic)

**Citation.** Avellaneda, M. & Lee, J.-H. (2010), "Statistical arbitrage in the U.S. equities market," *Quantitative Finance* 10(7), 761–782. SSRN id 1153505. Read directly from PDF (NYU mirror).

**Math (verified from paper, equations cited inline).**

Universe: stocks with market cap > $1B at trade date (avoid survivorship bias). Estimation window M = 252 trading days for the *correlation matrix*; secondary 60-day window for residual OU estimation.

PCA setup. Compute standardized returns `Y_ik = (R_ik − R̄_i)/σ_i` over M = 252 days (Eq. 8 of paper). Eigendecompose correlation matrix. Eigenportfolios:

```
F_jk = Σ_i (v_i^(j) / σ_i) R_ik     (paper Eq. 9)
```

Number of factors. Two options stated in paper: (a) fixed number ≈ 15 (matching number of industry sectors), or (b) variable number such that retained eigenvalues exceed a fraction of trace (i.e., explained-variance threshold). Paper notes "in G8 economies, stock returns are explained by approximately m = 15 factors (or between 10 and 20)... approximately 50% of the variance" (page 2 of PDF, line 330).

Residual model. After projecting return onto top-m factors, the residual `X_i(t)` is modeled as Ornstein-Uhlenbeck:

```
dX_i(t) = κ_i (m_i − X_i(t)) dt + σ_i dW_i(t),  κ_i > 0    (paper Eq. 12)
```

Equilibrium variance `σ_eq,i = σ_i/√(2κ_i)`. The s-score:

```
s_i = (X_i(t) − m_i) / σ_eq,i    (paper Eq. 15)
```

Trading thresholds (verified on page 11 of PDF):
- buy-to-open: `s_i < −1.25`
- sell-to-open: `s_i > +1.25`
- close-short: `s_i < +0.75`
- close-long: `s_i > −0.50`

**Mean-reversion speed cutoff (admission criterion).** Paper at page 9-10 explicitly states: "We selected stocks with mean-reversion times less than 1/2 period (κ > 252/30 = 8.4)." Equivalently, half-life < ~22 trading days. **This is exactly the kind of admission gate OpenQuant should consider for basket spreads.**

**Performance (paper Table 4).** PCA-based portfolio Sharpe:
- 1997-2007 average: 1.44
- 1997-2002: 2.0+ in many years
- 2003-2007: 0.9 average
- 2007 (liquidity crisis): −0.5

Authors explicit: "PCA-based strategies have an average annual Sharpe ratio of 1.44 over the period 1997 to 2007, with stronger performances prior to 2003. During 2003–2007, the average Sharpe ratio of PCA-based strategies was only 0.9" (abstract).

**Limitations the authors flag (page 8 onwards).**
1. Number of factors is unstable: "the variance explained by a fixed number of PCA eigenvectors varies significantly across time, which leads us to conjecture that the number of explanatory factors needed to describe stock returns... is variable."
2. Mean-reversion speed `κ < 8.4` rejection: "Estimation of parameters sometimes leads to values of κ_i < 8.4. When κ_i crosses this threshold, we reject the model and (i) do not open trades or (ii) close open trades." This is regime-detection at the per-name level.
3. Drift model (modified s-score, Eq. 17): authors tried adding a drift term; "the effect of incorporating a drift in these time-scales of a few days is minor." Pure mean-reversion is the dominant signal.

**Would this admit/reject FAANG?** AAPL/META/GOOGL/AMZN have huge first-PC (market) loadings and significant tech-style loadings; after stripping, residuals are small-magnitude. The κ admission test on those small residuals is the gate. *In our universe* with the specific question of admitting AAPL-AMZN-GOOGL-META as a basket: each individual stock's residual would be tested for κ > 8.4 against ITS OWN historical residual. The basket-level question (do their residuals co-move?) is downstream — and Avellaneda-Lee don't trade baskets, they trade name-vs-factor pairs. **A faithful Avellaneda-Lee deployment would not have *built* a FAANG basket at all** because their methodology trades each name as a residual against the factor portfolio, not in basket form. This is a structural rejection.

### Method 2 — BARRA-style risk-model residuals (practitioner standard)

**Citation.** Menchero, J., Orr, D.J., Wang, J. (2011), "The Barra US Equity Model (USE4) Methodology Notes," MSCI. PDF read directly.

**Math (verified Eq. 3.1 of doc).**

```
r_n = f_c + Σ_i X_ni · f_i + Σ_s X_ns · f_s + u_n
```

where `f_c` = country factor return, `f_i` = industry factor return (i = 1...60 GICS-derived industries in USE4), `f_s` = style factor return, `X_ns` = stock n's exposure to style s, `u_n` = specific (residual) return. Estimation: weighted least squares cross-sectional regression each period.

**Style factors (USE4).** Beta, Momentum, Size, Earnings Yield, Residual Volatility, Growth, Book-to-Price, Leverage, Liquidity, Non-linear Size — these are the canonical names from the public USE4 documentation; the explicit list with weights is in tables that don't extract cleanly from the PDF. Modern Barra GEM3/USE5 add 1-2 more (Dividend Yield, Long-term reversal). v1's "50–70 factors" claim is misleading — the *industry+country* count is ~60, but **only ~10 style factors**. The total parameter space is country + 60 industries + 10 styles ≈ 71, but most of those are industry dummies, not orthogonal alpha factors.

**Multiple-industry exposures (relevant to AMZN).** Section 2.5 of the BARRA doc explicitly addresses conglomerates: "Multiple-industry membership is modeled in USE4 by examining the impact of two key explanatory variables — Assets and Sales — on the market value of a given stock." Industry exposure weight is `X_nk = 0.75 · X_nk^A + 0.25 · X_nk^S`. Maximum 5 industries per firm. **This explicitly handles "AMZN is 17% AWS" — AMZN gets fractional retail + cloud + internet exposures, not single-bucket Consumer Discretionary.** This directly addresses one of v1's stated GICS failure modes.

**For pair/basket trading.** Practitioners take the residual `u_n` series across N stocks and either (a) cluster the residual correlation matrix or (b) form long-short baskets at the residual level. The factor model "absorbs" the systematic risks Avellaneda-Lee would absorb via PCA, but with named, interpretable factors (you can tell whether a basket is short-biased on Momentum or long-biased on Size).

**Limitations.**
1. Requires a factor-model build (descriptors, regression, factor returns history). Not a pure data-only method.
2. Factors are stock-style, not high-frequency. Weekly/monthly granularity. For intraday stat-arb the residuals are stale.
3. Not in the public domain at the data level — the USE4 *methodology* is public but the descriptor weights/factor returns are not. Vendors charge $$$.
4. Open-source approximations exist (Quantopian Risk Model, Fama-French style multi-factor) but lose the multiple-industry-exposure feature.

**Would this admit/reject FAANG?** A BARRA residual on AAPL strips out the country, the relevant industries (Technology Hardware, partial Consumer Electronics), AND the style factors (high Momentum, large Size, low Book-to-Price). The residual is what's *left over after every named factor is accounted for*. A FAANG basket constructed at the residual level would test whether the residuals of 4 mega-caps mean-revert against each other after all named factors stripped — which is structurally the trade you actually want, not the spurious sector-dummy trade. **Of all methods surveyed, BARRA-residual peers most directly address the FAANG mismatch problem.** But this is also the least replicable for retail without licensing.

### Method 3 — Sarmento-Horta OPTICS clustering on PCA loadings (most-cited academic ML method)

**Citation.** Sarmento, S.M. & Horta, N. (2020), "Enhancing a Pairs Trading strategy with the application of Machine Learning," *Expert Systems with Applications* 158, 113490. Github code at simaomsarmento/PairsTrading. Source code read directly (`class_SeriesAnalyser.py`).

**CORRECTION TO V1.** v1 strongly implied this was on US equities. **It is not.** Verified from multiple secondary sources and confirmed by an independent thesis replication (Meloncelli 2023, Luiss University): the universe is **208 commodity-linked ETFs at 5-min frequency, Jan 2009 – Dec 2018.** This is a different beast from US equities — there's no FAANG, no GICS, no mega-cap concentration problem. The 3.79 Sharpe vs. 2.59 GICS-grouped baseline applies to commodity ETFs.

**Pipeline (verified from `class_SeriesAnalyser.py:apply_OPTICS`).**

1. Compute returns. Standardize. PCA dimensionality reduction (n_components is a tuning hyperparameter, sweeped via silhouette score; arbitragelab's reproduction defaults to 10).
2. Feature vector for clustering = PCA loadings transposed: each ticker becomes a point in n-component space (`pca.components_.T`). Then `StandardScaler.fit_transform` is applied.
3. OPTICS with defaults: `min_samples` (passed in), `max_eps=2`, `xi=0.05`, `metric='euclidean'`, `cluster_method='xi'`. Code line:
   ```python
   clf = OPTICS(min_samples=min_samples, max_eps=max_eps, xi=xi,
                metric='euclidean', cluster_method=cluster_method)
   ```
4. Within each cluster, generate all C(n,2) pairs.
5. Apply ARODs filter on each pair (verified `class_SeriesAnalyser.py:check_properties`):
   - `min_half_life=78` (units = minutes for 1-min data; or 78 trading periods)
   - `max_half_life=20000`
   - `hurst_threshold=0.5` (must be < 0.5)
   - `min_zero_crossings` (configurable; secondary source says ≥ 12 per year)
   - cointegration p-value threshold (configurable)

**Performance (as published).** Sharpe 3.79 (proposed PCA→OPTICS→ARODs), 3.58 (alternative baseline), 2.59 (GICS-grouped baseline). Net of 0.05% transaction costs. **Universe is commodity ETFs, not equities.**

**Limitations / failure modes (from practitioner replications).**
- **Imbalanced clusters on equities.** Independent replication daehkim (2019) reports DBSCAN/OPTICS on US equities yields "a huge proportion of the stocks are bunched into a single cluster" with eps=1.8, minPoints=3.
- **Hyperparameter sensitivity.** The Hudson&Thames documentation explicitly notes DBSCAN is "for when the user needs a more hands-on approach to doing the clustering step, given the parameter sensitivity of this method." OPTICS is "basically parameterless" but in fact `xi`, `min_samples`, and the upstream PCA dim are all tunable.
- **Replication gap.** Independent replication of Han-He-Jun-Toh (2021) on US equities (a different ML pairs paper but methodologically similar): the academic claim is Sharpe 2.69; the replicator achieves Sharpe 0.4. Quoted from the practitioner: "Unfortunately we didn't replicate the results of the paper. Though we didn't utilize their data, nor had the full hyper-params they would have used."

**Would this admit/reject FAANG?** Open question on US equities specifically. On commodity ETFs (the original domain), there's no FAANG analogue. If applied to US equities directly: AAPL/AMZN/GOOGL/META have very different first-PC loadings (each is a heavyweight in different aspects of the market portfolio). They would likely NOT cluster together on PCA loadings. They might individually be tagged as noise (-1) by DBSCAN/OPTICS, OR might land in separate clusters with non-FAANG peers. v1's claim that they would be "flagged as noise points" is plausible for DBSCAN but unverified.

### Method 4 — SPONGE signed-graph clustering (newest academic method on US equities)

**Citation.** Cucuringu, M., Davies, P., Glielmo, A., Tyagi, H. (2019), "SPONGE: A generalized eigenproblem for clustering signed networks," AISTATS. arXiv:1904.08575. PDF read directly. Application: Cartea, A., Cucuringu, M., Jin, Q. (2023), "Correlation Matrix Clustering for Statistical Arbitrage Portfolios," ICAIF '23, ACM. SSRN id 4560455.

**Math (verified from page 3-4 of arXiv PDF).** Given signed correlation graph G, decompose into G+ (positive edges) and G− (negative edges). Define Laplacians `L+ = D+ − A+`, `L− = D− − A−`. SPONGE solves the generalized eigenvalue problem (paper Eq. 3.6 / 3.7):

```
min_{Y^T Y = I}  Tr( Y^T (L− + τ+ D+)^{−1/2} (L+ + τ− D−) (L− + τ+ D+)^{−1/2} Y )
```

equivalent to finding the smallest k generalized eigenvectors of `(L+ + τ− D−, L− + τ+ D+)`. SPONGEsym variant uses symmetric Laplacians: `L+_sym = (D+)^{−1/2} L+ (D+)^{−1/2}`. Then k-means++ on the resulting embedding. Hyperparameters τ+, τ− > 0 are regularization. Computation: LOBPCG eigensolver (preconditioned).

**Application to US equities (Cartea et al. 2023 / Jin et al. 2024 follow-up arXiv:2406.10695).**

Universe: S&P 500 historical constituents, Jan 2000 – Dec 2022, daily. Adjacency matrix = correlation of *residual returns* over last 60 days (residuals from a factor model). Clusters formed via SPONGEsym. Within each cluster, mean-reversion logic: 5-day cluster mean of returns, long stocks below / short stocks above, hold 3-10 days.

**Performance (verified from arXiv:2406.10695 secondary).**
- SPONGEsym original (Cartea 2023): Sharpe 1.1
- SPONGEsym replication, no TC: Sharpe 1.17
- **SPONGEsym replication with 0.05% TC: Sharpe 0.28**

**This is the critical finding.** With realistic round-trip costs (which are below typical retail costs), the strategy collapses. Quote from replication paper: *"transaction costs incurred is four times larger than the net profit from the strategy"*.

**Outlier-driven returns.** When two stocks (EP = Emporium Petroleum, CPWR) are excluded, annualized returns drop from 49.33% to 10.73%. This is a hallmark of a fragile strategy — the outliers should not dominate. Quote: "high sensitivity... to exclusion of highly volatile, profit-driving stocks."

**Post-2016 performance decline** noted in the same paper across all variants.

**Would this admit/reject FAANG?** The signed-graph approach uses correlation magnitude AND sign as features. AAPL/AMZN/GOOGL/META have positive pairwise correlations of moderate magnitude post-residual. They might or might not cluster together, depending on τ+/τ−. The paper does not directly test FAANG admission, and the failure modes (TC sensitivity, outlier dependence) suggest cluster-internal trading is the brittle part, not cluster construction.

### Method 5 — Hierarchical Risk Parity (HRP) tree clustering (de Prado, sizing-not-selection)

**Citation.** López de Prado, M. (2016), "Building Diversified Portfolios that Outperform Out of Sample," *Journal of Portfolio Management* 42(4), 59–69.

**Math (verified from secondary, Wikipedia HRP article).** Three steps:

1. **Tree clustering.**
   - Distance: `d_ij = sqrt( 0.5 · (1 − ρ_ij) )` where ρ_ij is Pearson correlation.
   - Mantegna distance (secondary): `d̃_ij = sqrt( Σ_n (d_ni − d_nj)^2 )`.
   - Single-linkage agglomerative clustering on the distance matrix.

2. **Quasi-diagonalization.** Reorder rows/cols of covariance matrix to put correlated assets adjacent. No basis change (unlike PCA).

3. **Recursive bisection.** Allocate inverse-variance weights within each binary split:
   ```
   α = 1 − V1 / (V1 + V2)
   w[L1] *= α; w[L2] *= (1 − α)
   ```

**Performance.** Monte Carlo only in original paper: HRP variance 0.0671 vs CLA 0.1157 vs IVP 0.0928 (all out-of-sample). Sharpe improvement over CLA ~31%.

**Counter-evidence (recent).** *"One recent study comparing out-of-sample risk-adjusted returns of the HRP algorithm to those of a simple 1/N allocation method found that the 1/N method outperforms HRP across all experimental setups"* (Springer 2025 Computational Economics). HRP is sizing, not pair selection — but the *clustering* component is sometimes used standalone for peer-set construction by cutting the dendrogram at user-chosen depth. This use is poorly studied.

**Would this admit/reject FAANG?** HRP's clustering step would put AAPL/AMZN/GOOGL/META somewhere in the dendrogram based on their pairwise correlations. With high correlation magnitude, they cluster together. With low/medium correlation (more typical of mega-cap residuals), they end up far apart. Cut depth is a hyperparameter. Net: not a clean rejector.

### Method 6 — Box-Tiao canonical decomposition (1977, the "forgotten method")

**Citation.** Box, G.E.P. & Tiao, G.C. (1977), "A canonical analysis of multiple time series," *Biometrika* 64(2), 355–365. Modern application: d'Aspremont, A. (2008), "Identifying small mean reverting portfolios," arXiv:0708.3048.

**Math (verified from secondary + d'Aspremont PDF directly).** Given multivariate VAR(1):

```
p_t = q(t-1, t-2, ..., t-k) + ε_t
```

Box-Tiao define predictability of a basket with weights w as:

```
λ(w) = (w^T A w) / (w^T B w)
```

where B = E[p p^T] (variance), A = E[(q − p)(q − p)^T] (residual variance). Solving the generalized eigenvalue problem `Aw = λBw` gives a decomposition into baskets ranked by predictability. **The eigenvector with smallest λ is the most-mean-reverting linear combination** — this is the basket you want for stat-arb.

d'Aspremont's sparse extension imposes ‖w‖_0 ≤ k (cardinality constraint), making the basket implementable. Solved via ℓ1 penalization. Applied to US swap rates (1Y-30Y) and FX, not equities.

**Why this matters.** Box-Tiao gives a *direct* basket-construction objective: maximize stationarity of the linear combination. Unlike PCA (which maximizes variance) or correlation clustering (which proxies similarity), this is the right loss function for stat-arb. d'Aspremont notes (page 1 of PDF): "We seek to adapt these results to the problem of estimating sparse (i.e. small) [mean-reverting] portfolios."

**Comparison with Johansen.** Bewley & Orden (1994, *Journal of Econometrics*) compared Box-Tiao and Johansen estimators in finite samples: "The distributions of the Box-Tiao estimator are found to be less dispersed and leptokurtic in a variety of interesting cases." Box-Tiao has BETTER finite-sample properties than Johansen — which is the state-of-the-art for multivariate cointegration in modern stat-arb literature. v1 missed this.

**Why nobody uses it.** Empirical equities applications are sparse — d'Aspremont tests on swaps. For 500-stock universes the predictability matrix is N×N and the eigendecomposition is dense. Not because it's wrong, but because nobody wrote a clean library and the method got eclipsed by Johansen.

**Would this admit/reject FAANG?** Sparse Box-Tiao on FAANG would directly optimize: of all 4-stock weight vectors with FAANG members, which one is most mean-reverting? If no such combination exists with high λ_min, the method *quantifies* the rejection rather than asserting it. This is the cleanest way to test "should this basket exist as a stat-arb basket."

### Method 7 — Partial cointegration (Clegg-Krauss 2018, R `partialCI` package)

**Citation.** Clegg, M. & Krauss, C. (2018), "Pairs trading with partial cointegration," *Quantitative Finance* 18(1), 121–138. R package `partialCI` (CRAN).

**Math (verified from secondary).** Standard cointegration assumes the spread is fully stationary (AR(1) with |φ| < 1). Partial cointegration relaxes this: spread = MR component + random walk component:

```
Spread_t = M_t + R_t
M_t = ρ M_{t-1} + ε_M,t       ε_M,t ~ N(0, σ²_M)        (mean-reverting)
R_t = R_{t-1} + ε_R,t          ε_R,t ~ N(0, σ²_R)        (random walk)
```

**Selection statistic: `R²[MR]`** = fraction of total variance attributable to the mean-reverting component. The R `partialCI` package outputs this directly. A pair "partially cointegrated" with R²[MR] = 0.86 means 86% of daily spread variance is mean-reverting; the rest is permanent drift.

Estimation: maximum likelihood via Kalman filter on the state-space model. Likelihood ratio test against pure-random-walk and pure-cointegrated nulls.

**Performance.** Backtested on S&P 500 1990-2015 (survivor-bias-free), reports >12% annualized return after transaction costs. v1 missed this.

**Why this matters for OpenQuant.** The standard cointegration test (Engle-Granger ADF on residuals) is binary — either passes or fails. But many "almost-cointegrated" pairs have stationary MR components masked by a random-walk drift. Partial cointegration's R²[MR] is *the right ranking statistic* for pair selection. It distinguishes:
- "Truly random walk" (R²[MR] = 0) — reject
- "Mostly random walk with a tradeable MR layer" (R²[MR] = 0.3) — borderline; could be tradeable on a long horizon
- "Mostly mean-reverting" (R²[MR] > 0.7) — strong candidate
- "Pure cointegration" (R²[MR] = 1) — best, but rare in noisy data

**Would this admit/reject FAANG?** Direct test: compute spread of any two FAANG names with hedge ratio, fit partial cointegration model, look at R²[MR]. If < 0.3, reject. If > 0.5, accept. This is *exactly* the per-basket admission test OpenQuant should add. The R package is mature, the statistic is well-defined, the literature backs it up.

### Method 8 — Hoberg-Phillips TNIC text-based industry classification

**Citation.** Hoberg, G. & Phillips, G. (2016), "Text-Based Network Industries and Endogenous Product Differentiation," *Journal of Political Economy* 124(5), 1423–1465. Data library at hobergphillips.tuck.dartmouth.edu.

**Method.** For each pair of public US firms in any year:
1. Parse Item 1/1A (business description) of 10-K filing.
2. Compute word-vector cosine similarity.
3. Higher similarity = more closely related products.
4. Industry classification: each firm's "industry" = all firms with similarity above a threshold (TNIC-3 ≈ 3-digit SIC granularity).

**Updated annually.** Unlike GICS (which V/MA reclassification took ~10 years to recognize), TNIC tracks evolving business models year-by-year. AMZN's TNIC industry includes more cloud peers (MSFT, IBM) over time as AWS grows.

**Why v1 missed this.** No published stat-arb paper has used TNIC for pair selection in a formal sense. It's an academic-finance tool, not a quant-trading tool. But it solves exactly the problems v1 flagged: V/MA reclassification, AMZN-as-cloud, conglomerate problem.

**Practical limitations.**
- Annual update only. For a high-frequency strategy, this is too slow.
- Requires the TNIC dataset (free for academics, otherwise need to build it from raw 10-Ks).
- No empirical evidence yet that it produces better stat-arb pairs than GICS or BARRA — it's plausible but untested in production.

**Would this admit/reject FAANG?** TNIC would likely separate META/GOOGL (ad-tech) from AAPL/AMZN (hardware/retail). FAANG-as-basket would not pass TNIC's similarity threshold. **Plausibly the cleanest *industry-classification-based* rejector for FAANG**, but the method has not been validated for stat-arb.

### Method 9 — Autoencoder embedding for stat-arb

**Citation.** "End-to-End Policy Learning of a Statistical Arbitrage Autoencoder Architecture," arXiv:2402.08233 (2024).

**Method.** Encoder → latent dim k (tested k ∈ {3, 5, 6, 8, 10, 15, 20, 30, 50}; optimal 10-15) → Decoder reconstructs returns. Loss: `λ · MSE(Z_t, Ẑ_t) + (1 − λ) · Sharpe(w_t, R_{t+1})`, λ = 0.5. End-to-end backprop through trading policy.

**OU + s-score on residuals**: same as Avellaneda-Lee. Entry/exit thresholds ±1.25 / ±0.5 / ±0.75. R² > 0.25 confidence filter.

**Universe.** 5188 US stocks Jan 2000 – Dec 2022. Filtered for $5+ price, $1bn+ cap, $1m+ vol.

**Performance.** Pre-cost gross Sharpe 1.51-1.81. **Post-cost performance "uncompetitive due to high turnover"** — direct quote from authors. PCA-OU baseline achieves 0.87-0.96 pre-cost.

**Authors' acknowledged limitations.**
- After-cost Sharpe drops below threshold for production use.
- Intraday focus missing: "successful StatArb strategies often operate on intraday data."
- Limited baseline comparisons (no Gu et al. IPCA).

**Would this admit/reject FAANG?** The autoencoder embedding is a non-linear generalization of PCA. It would likely strip the same factor exposures. The OU s-score thresholds with R² > 0.25 filter would reject pairs whose residuals don't fit the OU model — which would tend to reject FAANG residuals if they're noise-dominated. Same logic as Avellaneda-Lee, with marginal improvements pre-cost.

### Method 10 — Distance method (Gatev et al. 2006, the OG benchmark)

**Citation.** Gatev, E., Goetzmann, W.N., Rouwenhorst, K.G. (2006), "Pairs trading: Performance of a relative-value arbitrage rule," *Review of Financial Studies* 19(3), 797–827.

**Method.** Normalize each stock to $1 at start of 12-month formation period. Compute Euclidean distance between normalized price series for all C(N,2) pairs. Top M pairs by smallest distance enter trading period. Open trade when spread > 2σ; close when spread crosses zero. (verified from Zhu 2024 replication)

**Performance evolution (this is critical).**
- Gatev et al. original 1962-2002: 11% annualized excess return
- Zhu (2024) replication 2003-2023 on CRSP universe (13386 securities): top-strategy 6.2% annual excess return, **Sharpe 1.35** — *with* 1-day delay rule and conservative cost assumptions.
- Rubesam (2021): monthly returns dropped from 0.22-0.43% (pre-GFC) to 0.04-0.07% (post-GFC) — Zhu disputes due to calculation errors but confirms the *direction* of decay.

**Why this matters.** Distance method is the brain-dead simplest peer selection. It still works (Sharpe 1.35 on the largest universe). It's the right benchmark to beat. **OpenQuant's basket method should be benchmarked against the Gatev distance method as a sanity check** — if a clever clustering method can't beat 6.2% annual excess return / 1.35 Sharpe, it's adding complexity without alpha.

**Would this admit/reject FAANG?** Distance method is GICS-agnostic. AAPL-AMZN normalized prices over 12 months might be far apart (different drift); they would not be admitted as a top-M closest pair. Distance method's bias is toward stocks with similar PRICE PATHS — not similar fundamentals. So FAANG-as-basket via distance method depends entirely on whether the four price series happen to track each other in the formation window. In high-dispersion years, they would NOT track. Distance method auto-rejects them then.

---

## What practitioners actually say (5 quoted passages)

**1. Ernie Chan on cointegration breakdown (epchan blogspot, June 2011).**

> "Often, cointegration for a pair breaks down for an extended period, maybe as long as a half a year or more."

Chan's recommended response: investigate WHY (e.g., GLD-GDX broke when oil rose AND miner hedging clipped upside in 2008). His fix: add a third variable — for GLD-GDX, USO (oil futures). Adding USO restored profitability. *Implication for OpenQuant: persistent FAANG/entsw 0/4 might not be a "wrong basket" diagnosis but an "incomplete basket" diagnosis. Maybe the missing variable is a specific style/dispersion factor.*

**2. Brian Stanley (QuantRocket, 2024) on pairs trading viability.**

> "Two stocks may cointegrate in-sample, but they often wander apart out-of-sample as the fortunes of the respective companies diverge."
> "The strategy was profitable for the first two years following publication, but was unprofitable thereafter [GLD/GDX case]."
> "Successful pairs trading requires a robust research pipeline for continually identifying and selecting new pairs to replace old pairs that stop working."

*Implication for OpenQuant: the lab-discovers / Rust-validates monthly cycle is consistent with this advice. The frequency of pair-set refresh is a first-order parameter, not a constant.*

**3. Cliff Asness (2016, "Resisting the Siren Song of Factor Timing").**

> "Factor timing strategies are quite weak historically."

(quoted from C v2 cross-reference) Asness argues against timing factors based on dispersion or similar regime signals. His recommendation: diversify across factors, harvest unconditionally. For OpenQuant: this is the *opposite* of what C v2 recommends (regime gate). Worth noting tension.

**4. Hudson & Thames documentation on copula approaches.**

> "The fitting algorithms, sampling and exact trading strategies are separated for their own dedicated articles to keep the length manageable... The common trading logics used for copulas are still relatively primitive."

*Implication: copula approach is over-hyped in academic literature; the practitioner-facing docs admit the trading logic isn't well-developed. Skip copulas as a peer-set tool — they're tail-dependence models, not selection methods.*

**5. Practitioner replication (Adam, Hecatus Research, Medium 2024).**

> "Unfortunately we didn't replicate the results of the paper. Though we didn't utilize their data, nor had the full hyper-params they would have used."

(re: Han-He-Jun-Toh 2021 unsupervised pairs trading on US equities) Academic Sharpe 2.69 → practitioner Sharpe 0.4. *Implication: published clustering-stat-arb numbers should be discounted aggressively.*

---

## Counter-examples and failure modes

**Where DBSCAN fails on US equities.** DBSCAN on standardized PCA loadings tends to produce one giant cluster and many noise points. Specifically, daehkim's 2019 replication on US equities with `eps=1.8, minPoints=3` gives 11 clusters, but with "a huge proportion of the stocks bunched into a single cluster." The OPTICS variant is supposed to handle varying-density clusters but is in practice equally hyperparameter-sensitive (`xi`, `min_samples`).

**Where SPONGE on US equities fails.** Cartea-Cucuringu-Jin 2023 results: Sharpe 1.1 → 0.28 with realistic costs. Two outlier stocks drive the entire return. Strategy degrades post-2016. This is the closest direct test of clustering-based stat-arb on US equities, and it does not survive realistic costs.

**Where Avellaneda-Lee fails.** Authors themselves report Sharpe degradation 1997-2002 → 2003-2007 (1.44 → 0.9). Volume-conditioning helps recover Sharpe to 1.51 in 2003-2007 but introduces a separate signal. Post-publication (2010), no major academic update has shown PCA-residual stat-arb still works at scale on US equities.

**Where copula methods fail.** Hudson & Thames docs concede "common trading logics... are still relatively primitive." No published evidence copula-based pair selection outperforms simpler methods.

**Where HRP fails.** Recent Springer 2025 paper (Computational Economics) reports 1/N outperforms HRP "across all experimental setups" on real OOS data. HRP's clustering layer is unstable (single-linkage suffers from chaining); Ward's-linkage variants help.

**Where the autoencoder approach fails.** Pre-cost Sharpe 1.51-1.81 → uncompetitive after costs (authors' own admission). High turnover.

**The general failure mode.** Every published clustering-stat-arb paper has at least one of: (a) backtest period that doesn't extend to recent markets, (b) Sharpe collapse with realistic TC, (c) outlier-driven returns, (d) replication gap (academic > practitioner Sharpe by 5-10×). v1's section on "what the literature shows works" overstated the case. The honest read: **published clustering-stat-arb has a small alpha that is real-but-fragile, and the FAANG-mismatch problem is not what these methods are designed to solve.**

---

## Cross-references with parallel scopes (B, C, D)

C v2 (regime switching) is the only parallel draft that exists at write-time. B and D have not yet been drafted; flags below for what to check when they appear.

### Cross-reference with C v2 (regime switching, Memo C)

C argues the FAANG/entsw failure is a *regime-conditional* problem (model-class layer), not a peer-set construction problem (basket-construction layer). C explicitly flags the tension at line 326-334:

> "A's framing implies the problem is structural at the basket-construction layer. H3 [C's hypothesis] argues the problem is regime-conditional at the model-class layer. **These are not mutually exclusive.** Both could be true: FAANG baskets might be poor regardless (A) AND the broader 6-out-of-8 sector weakness in this period might be regime (C)."

**A's view:** C is right that they're not mutually exclusive. **My read: A is necessary for FAANG specifically, C is necessary for cross-sector.** Specifically:

- The FAANG/entsw 0/4 outcome was *always* predictable from first principles (mega-cap factor mismatch → spread doesn't mean-revert). A residual-construction fix (BARRA, PCA-residuals, partial cointegration on residuals) would have rejected those baskets at admission. This is structural, not regime.
- The broader 22/35 vs 0/8 split across 6 normal sectors vs 2 mega-cap sectors is consistent with: (i) fixed FAANG/entsw rejection is sufficient to explain the 0/8, AND (ii) the 22/35 in normal sectors might still be regime-influenced — but the 22 wins suggest the regime is not catastrophic for *good* baskets.

**Concrete test that distinguishes A from C** (suggested by C v2 line 332):

> "If we had run the same MR-on-baskets in 2018-2019 (mega-cap concentration era #1), would FAANG/entsw have failed similarly? If yes, A's hypothesis is sufficient. If no, C is necessary."

This is exactly the right walk-forward to run.

**Where A and C agree:** C's suggested signals (cross-sectional dispersion, factor-leadership) would *also* function as proxies for "FAANG-style basket is unsafe to trade right now." So a regime gate operating at the *basket level* (not the strategy level) is consistent with A's thesis: bad baskets get rejected always; good baskets get gated in adverse regimes.

**Conflict:** C cites Asness against factor-timing. If Asness is right, regime gates are net-negative. A is silent on this; the peer-set fix doesn't depend on regime detection.

### Anticipated cross-reference with B (regime detection methodology) — file not present at write-time

If B confirms that regime-detection literature converges on HMM/BOCPD/dispersion as dominant techniques (as C v2 anticipates), then there's no direct contradiction with A. Peer-set fixes (A) and regime gates (B/C) are orthogonal interventions.

If B's literature includes specific papers on *peer-set as upstream signal for regime* — e.g., "when peer-correlation structure shifts, regime is changing" — then A and B would be tightly coupled. Worth checking.

### Anticipated cross-reference with D (failure-mode catalog) — file not present at write-time

If D's catalog includes "GICS peer sets are wrong" as a specific failure mode, that supports A. If D's catalog is dominated by single-name structural breaks (M&A, earnings, fraud), that's a different concern (basket-internal stop-out logic) but doesn't directly contradict A.

If D ranks "wrong peer set" as a frequent / high-impact failure relative to "wrong regime," that's evidence A is the higher-priority fix. If D ranks "regime mis-timing" higher, C should be prioritized.

---

## Recommendation for OpenQuant

### Highest-confidence, lowest-cost fix

**Add per-basket admission tests at the residual level, before any cluster/correlation reasoning.** Specifically:

1. **Strip top-k PCs from the universe** (k = 5-15, or variable by 50% explained-variance threshold per Avellaneda-Lee). Use `pair-picker` crate's existing PCA infrastructure if any, or add a small one. This is cheap to compute on close-prices we already have.
2. **Compute basket spread from residuals**, not raw returns. The spread that matters is `Σ_i w_i · ε_i` where ε_i is the i-th name's PCA-residual return.
3. **Apply κ admission test** (Avellaneda-Lee Eq. 12): fit OU on the spread, require κ > 252/30 ≈ 8.4 (half-life < 22 days). Reject baskets whose spread doesn't mean-revert fast enough.
4. **Apply partial-cointegration `R²[MR]` filter** as a secondary check. Require R²[MR] > 0.5 for admission, > 0.7 for high-priority.

This combines the most defensible parts of three methods (Avellaneda-Lee + Clegg-Krauss) without requiring a vendor factor model or a brittle clustering pass. **Confidence: high.** Speculative magnitude: this would have rejected FAANG/entsw at admission with high probability, eliminating most of the 0/8 outcomes.

### Medium-confidence enhancements

5. **Industry classification: GICS + TNIC override.** When TNIC similarity disagrees with GICS by > 1 GICS-level (sector vs. industry), use TNIC. Specifically: AMZN's TNIC neighbors include MSFT and IBM (cloud); GICS classifies AMZN as Consumer Discretionary. Use TNIC for AMZN's peer set. This is a one-time annual update.
6. **Distance method as a sanity-check baseline.** Run the Gatev distance method on the same universe and same windows as our basket method. If Gatev outperforms, our method has no edge. If Gatev underperforms, the alpha is real. **Cheap to add, high diagnostic value.**

### What NOT to ship

7. **Do NOT use DBSCAN/OPTICS on PCA loadings as a primary peer-set construction method on US equities.** Published evidence is fragile (Cartea 2023, Sarmento-Horta 2020 on different domain), practitioner replication consistently underperforms (5-10× Sharpe gap), and DBSCAN's failure mode on US equities (one giant cluster) is well-documented.
8. **Do NOT use SPONGE / signed-graph clustering as a primary method.** Cartea 2023's Sharpe 0.28 with realistic costs settles this.
9. **Do NOT rely on copula-based peer selection.** Hudson & Thames docs themselves admit the trading logic isn't well-developed. It's a tail-dependence model, not a selection method.

### Stance on "regime gate vs. peer-set fix" debate (re: C v2)

Sequence the work: **fix peer set first, then add regime gate.** A's fixes are higher-confidence on the FAANG/entsw subset (where the failure is structural). C's fixes are higher-confidence on the broader timing question. **Do NOT add a regime gate to a system with bad baskets — the gate will be calibrated on poisoned data.** This is the same point C makes at line 393.

---

## What's still unknown (open questions)

1. **Is `R²[MR]` > 0.5 the right threshold for partial cointegration admission?** Clegg-Krauss don't specify a calibration. Should be backtested on historical data.

2. **Does TNIC-based peer override actually improve stat-arb hit rates?** No published evidence. Need a small experiment: pull TNIC data for our universe, compute peer set, compare to GICS-based peer set on a held-out window.

3. **What's the right number of PCs to strip in residual construction?** Avellaneda-Lee say 15 (fixed) or variable-by-explained-variance. Fixed-k is easier to implement; variable-k is more theoretically defensible but has refit-window dependence. Worth a small sweep.

4. **Box-Tiao on basket construction directly — is anyone using it?** It's the cleanest theoretical fit (most-mean-reverting basket weights), but empirical equities applications are sparse. d'Aspremont's sparse extension is implementable; nobody has tested it on a 500-stock universe at scale.

5. **Walk-forward 2018-2019 for FAANG specifically.** Did our methodology fail in the prior mega-cap concentration era? If yes (and it would have), A's recommended fix is structural. If no, C's regime fix is needed.

6. **Can A's residual-construction fix be implemented in Rust without rewriting the universe?** Specifically: do we have access to the cross-sectional return matrix at sufficient cadence to run rolling PCA inside `pair-picker`? If not, this becomes a quant-lab Python pre-processing job that exports PC residuals to Rust.

7. **Cost of the residual-construction layer.** Rolling PCA every D days × N stocks × M lookback. For S&P 500 with D=21, N=500, M=252: ~$O(N^2 M)$ = ~63M ops per refresh. Manageable but not trivial. Worth profiling.

8. **Does the Avellaneda-Lee 2010 regime-degradation issue (Sharpe 1.44 → 0.9) repeat for our method?** They observed gradual erosion of the residual-MR premium 2003-2007. We are 18 years later. The premium may have eroded further. Don't assume any of these methods produce 1990s-style Sharpe today.

---

## Sources (all primary or carefully-cited secondary)

**Foundational papers (read directly).**
- Avellaneda, M. & Lee, J.-H. (2010). "Statistical arbitrage in the U.S. equities market." *Quantitative Finance* 10(7), 761-782. PDF read directly via Berkeley Traders mirror, equations and thresholds verified. https://traders.studentorg.berkeley.edu/papers/Statistical%20arbitrage%20in%20the%20US%20equities%20market.pdf
- Menchero, J., Orr, D.J., Wang, J. (2011). "The Barra US Equity Model (USE4) Methodology Notes." MSCI. PDF read directly. https://www.top1000funds.com/wp-content/uploads/2011/09/USE4_Methodology_Notes_August_2011.pdf
- Cucuringu, M., Davies, P., Glielmo, A., Tyagi, H. (2019). "SPONGE: A generalized eigenproblem for clustering signed networks." AISTATS. arXiv:1904.08575. PDF read directly. https://arxiv.org/abs/1904.08575
- d'Aspremont, A. (2008). "Identifying small mean reverting portfolios." arXiv:0708.3048. PDF read directly. https://arxiv.org/abs/0708.3048
- Zhu, X. (2024). "Examining Pairs Trading Profitability." Yale senior essay. PDF read directly. https://economics.yale.edu/sites/default/files/2024-05/Zhu_Pairs_Trading.pdf

**Application papers (verified via abstract / secondary).**
- Sarmento, S.M. & Horta, N. (2020). "Enhancing a Pairs Trading strategy with the application of Machine Learning." *Expert Systems with Applications* 158, 113490. Universe (208 commodity-linked ETFs, 5-min, Jan 2009-Dec 2018) verified via multiple secondary sources and Meloncelli 2023 thesis replication. https://www.sciencedirect.com/science/article/abs/pii/S0957417420303146
- Cartea, A., Cucuringu, M., Jin, Q. (2023). "Correlation Matrix Clustering for Statistical Arbitrage Portfolios." ICAIF '23. SSRN 4560455. https://ssrn.com/abstract=4560455
- Jin, Q. et al. (2024). "Statistical arbitrage in multi-pair trading strategy based on graph clustering algorithms in US equities market." arXiv:2406.10695. SPONGEsym replication with 0.28 Sharpe @ 5bps cost. https://arxiv.org/abs/2406.10695
- Clegg, M. & Krauss, C. (2018). "Pairs trading with partial cointegration." *Quantitative Finance* 18(1), 121-138. R²[MR] statistic. https://www.tandfonline.com/doi/abs/10.1080/14697688.2017.1370122
- López de Prado, M. (2016). "Building Diversified Portfolios that Outperform Out of Sample." *J. Portfolio Management* 42(4), 59-69. SSRN 2708678. https://papers.ssrn.com/sol3/papers.cfm?abstract_id=2708678
- Hoberg, G. & Phillips, G. (2016). "Text-Based Network Industries and Endogenous Product Differentiation." *J. Political Economy* 124(5), 1423-1465. https://hobergphillips.tuck.dartmouth.edu/
- Krauss et al. (2024). "End-to-End Policy Learning of a Statistical Arbitrage Autoencoder Architecture." arXiv:2402.08233. https://arxiv.org/abs/2402.08233
- Gatev, E., Goetzmann, W.N., Rouwenhorst, K.G. (2006). "Pairs trading: Performance of a relative-value arbitrage rule." *Review of Financial Studies* 19(3), 797-827.

**Code references.**
- Sarmento's `class_SeriesAnalyser.py` (read directly): https://github.com/simaomsarmento/PairsTrading/blob/master/classes/class_SeriesAnalyser.py — OPTICS defaults `max_eps=2, xi=0.05`, ARODs `min_half_life=78, max_half_life=20000, hurst_threshold=0.5`.
- Hudson & Thames `arbitragelab` OPTICS-DBSCAN module (read directly): https://github.com/hudson-and-thames/arbitragelab/blob/master/arbitragelab/ml_approach/optics_dbscan_pairs_clustering.py — feature vector is `pca.components_.T` standardized; default `num_features=10`.
- R `partialCI` package (CRAN): https://github.com/matthewclegg/partialCI — Clegg's reference implementation, R²[MR] statistic.

**Practitioner sources.**
- Ernie Chan, "When cointegration of a pair breaks down" (2011). http://epchan.blogspot.com/2011/06/when-cointegration-of-pair-breaks-down.html
- Ernie Chan, "Selecting tradeable pairs: which measure to use?" (2009). http://epchan.blogspot.com/2009/12/selecting-tradeable-pairs-which-measure.html
- Brian Stanley, "Is Pairs Trading Still Viable?" QuantRocket. https://www.quantrocket.com/blog/pairs-trading-still-viable/
- daehkim, "Pair Trading: A market-neutral trading strategy with integrated Machine Learning" (2019). https://daehkim.github.io/pair-trading/ — DBSCAN replication gap documentation.
- Adam (Hecatus Research), "Unsupervised Learning as Signals for Pairs Trading and StatArb" (Medium 2024). https://medium.com/call-for-atlas/unsupervised-learning-as-signals-for-pairs-trading-and-statarb-c5d6bf3db7cb — academic Sharpe 2.69 → practitioner Sharpe 0.4.
- Hudson & Thames, copula trading docs: https://hudsonthames.org/copula-for-pairs-trading-introduction/ — admits "common trading logics... are still relatively primitive."

**Replication / counter-evidence.**
- Meloncelli, L. (2023). "Developing a Pairs Trading Investment Strategy with Python." Luiss thesis. https://tesi.luiss.it/38606/1/756931_MELONCELLI_LORENZO.pdf — confirms Sarmento-Horta universe and details replication challenges.
- Springer (2025). "An Empirical Evaluation of Distance Metrics in Hierarchical Risk Parity Methods for Asset Allocation," *Computational Economics*. https://link.springer.com/article/10.1007/s10614-025-10848-w — recent finding that 1/N outperforms HRP across all experimental setups.

**Cross-references.**
- C v2 memo (read in full): /Users/gulshan/OpenQuant/autoresearch/research_team/C_regime_switching_v2.md
- A v1 memo: /Users/gulshan/OpenQuant/autoresearch/research_team/A_peer_sets_v1.md (revised by this v2)
