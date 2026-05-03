# Research Memo: Production Peer-Set Construction for Basket Stat-Arb (v1)

**Status:** v1 — preliminary. Will be deepened by the v2 research pass.

## Question
What do production stat-arb desks actually use to construct peer sets? Why is GICS-only insufficient, especially for FAANG-style mega-caps? Which alternatives are likely to have changed our 0/4 FAANG outcome vs. which are academic curiosities?

## Findings

### Method 1 — Risk-model residual peers (BARRA / Axioma / Quantopian-style)
The practitioner standard. Form `r_it = Σ B_ij f_jt + ε_it` against ~50–70 style + industry factors (size, value, momentum, residual vol, beta, growth, leverage, plus GICS dummies), then cluster or pair on residuals `ε`. Two stocks are "peers" only if their residual returns are correlated *after factor exposures are stripped*.
Refs:
- [Barra USE4 Methodology](https://www.top1000funds.com/wp-content/uploads/2011/09/USE4_Methodology_Notes_August_2011.pdf)
- [Quantopian Risk Model whitepaper](https://www.quantopian.com/papers/risk)
- [Quantopian factor-based risk notebook](https://github.com/quantopian/research_public/blob/master/notebooks/lectures/Factor_Based_Risk_Management/notebook.ipynb)

Why it's the desk standard: GICS Communications Services lumps GOOGL (search/ads), META (social/ads), NFLX (subscription), VZ (telecom). Their *raw* returns share a sector dummy; their *residuals* don't. Trading raw means trading the missing factors, not the spread.

### Method 2 — PCA / eigenportfolio residuals (Avellaneda–Lee 2010)
The canonical academic statarb residual model — but unlike Barra it's used live by several quant desks because it's data-only. Decompose the universe with rolling PCA on returns, keep top-k eigenportfolios as factors, OU-fit the residual `ε_it`. Trade residual mean-reversion. Sharpe ~1.4 net 1997–2007 in their backtest; weaker post-2003.
Refs:
- [Avellaneda & Lee, Quantitative Finance 10(7), 2010](https://papers.ssrn.com/sol3/papers.cfm?abstract_id=1153505)
- [arbitragelab PCA approach docs](https://hudson-and-thames-arbitragelab.readthedocs-hosted.com/en/latest/other_approaches/pca_approach.html)

For our problem: a 5-PC residual model would *de-FAANG* AAPL — its returns project heavily onto the first PC (market) and a tech-style PC, leaving a small residual whose peers are determined by *what's left over*, not by sector membership.

### Method 3 — Clustering on correlation matrices (DBSCAN / OPTICS / hierarchical)
The most-cited modern academic approach. Sarmento & Horta (2020) is the reference: PCA → OPTICS clusters → cointegration filter → Hurst<0.5. They report Sharpe 3.79 vs 2.59 for GICS-grouped baselines on the same universe — direct evidence that *replacing GICS with data-driven clustering* moves the needle.
Refs:
- [Sarmento & Horta, Expert Systems w/ Apps 2020](https://www.sciencedirect.com/science/article/abs/pii/S0957417420303146)
- [Hudson & Thames "ML for Pairs Selection"](https://hudsonthames.org/employing-machine-learning-for-trading-pairs-selection/)
- [arbitragelab OPTICS/DBSCAN module](https://hudson-and-thames-arbitragelab.readthedocs-hosted.com/en/latest/technical/api/arbitragelab/ml_approach/optics_dbscan_pairs_clustering/index.html)

DBSCAN's *practitioner* virtue: it leaves outliers unclustered. AAPL/AMZN/GOOGL/META with their idiosyncratic factor loadings would likely be flagged as noise points and *correctly excluded* from any cluster — i.e., the algorithm itself would refuse to admit our 0/4 FAANG baskets.

### Method 4 — Signed-graph spectral clustering (SPONGE)
The newest method actually run on US equities by an academic-practitioner team (Cartea is a working systematic PM). Cluster *signed* correlation graphs so positively-correlated stocks share clusters and negatively-correlated stocks repel. Sharpe ~1+, Sortino dominates Laplacian/k-means/SPONGEsym alternatives on the same universe.
Refs:
- [Cartea, Cucuringu & Jin, ICAIF 2023](https://papers.ssrn.com/sol3/papers.cfm?abstract_id=4560455)
- [Cucuringu et al. SPONGE, AISTATS 2019](https://arxiv.org/abs/1904.08575)
- [multi-pair graph clustering, arXiv 2406.10695](https://arxiv.org/abs/2406.10695)

### Method 5 — Hierarchical clustering on residual correlations (HRP-style trees)
de Prado's HRP itself is a sizing method, but the underlying machinery (single-linkage tree on residual correlation distance `d_ij = √(2(1-ρ_ij))`) is the standard *practitioner* clustering for stat-arb peer groups. Produces dendrograms you cut at a user-chosen depth — not at the arbitrary boundary GICS draws.
Refs:
- [de Prado, "Building Diversified Portfolios that Outperform OOS," J. Portfolio Mgmt 2016](https://en.wikipedia.org/wiki/Hierarchical_Risk_Parity)
- [skfolio HRP](https://github.com/skfolio/skfolio)

## What goes wrong with GICS — documented failure modes
- **Sub-industry reclassifications mid-history:** V/MA moved from Tech → Financials in March 2023.
- **Conglomerate problem:** AMZN is "Consumer Discretionary" but ~17% of revenue is AWS (cloud). META and GOOGL share an ad-stock factor invisible in GICS Communications Services.
- **Single-name dominance within sector:** A GICS-equal-weight basket containing one mega-cap dominates the spread. AAPL ≈ 30% of equal-weighted FAANG basket's volatility.

## FAANG-specific best practice
There is no documented "FAANG basket" desk practice in published literature, and that's itself the answer: practitioners do *not* trade FAANG-as-a-basket because the names don't share factor exposures cleanly. Instead:
- (a) hedge each mega-cap individually against a *custom factor portfolio* derived from BARRA residuals
- (b) trade *within-industry* sub-cuts (AAPL vs MSFT as platform-rent peers; META vs GOOGL as ad-stock peers; AMZN vs WMT/COST as e-comm peers)
- (c) avoid them entirely as targets in residual-PCA strategies

## Honest assessment — what would have moved our outcome

| Method | Likely impact on FAANG/entsw 0/4 | Confidence |
|---|---|---|
| Avellaneda–Lee PCA residuals | High — strips factors that dominate FAANG; admission test would have rejected current baskets | High |
| BARRA-style residual peers | High — same reason; this is what desks actually do | High but requires a factor model we don't have |
| DBSCAN/OPTICS on residual correlations | Medium-high — would likely flag AAPL/AMZN/GOOGL/META as noise points and refuse admission | High |
| HRP/hierarchical tree cut | Medium — better than GICS, but cut depth is a hyperparameter | Medium |
| SPONGE / signed-graph | Low for our specific problem — addresses universe partitioning, not factor-mismatch within mega-caps | Medium |
| Cointegration-only ranking (Engle-Granger sweep) | Low — same trap we're already in: passes a stationarity test on a never-valid relationship | High |

**Bottom line**: the FAANG/entsw failure is exactly the failure mode that *risk-model residuals* (Methods 1 and 2) were designed to solve, and that DBSCAN-style clustering (Method 3) was designed to detect at admission time.

## What this v1 memo is missing (deferred to v2)
- Actual code excerpts from arbitragelab / quantopian-risk / skfolio showing the API, not just docs links
- Math from Sarmento & Horta — exact thresholds, exact features, statistical-tests sequence
- Practitioner discussion threads (Quant Stack Exchange, Wilmott, Reddit r/algotrading, Twitter) — where do desks REALLY argue about peer-set construction
- Failure-mode counter-examples — when DBSCAN/PCA-residual approaches FAIL in practice
- Specific code snippet that would replace our `validate()` function with a residual-clustering admission test
