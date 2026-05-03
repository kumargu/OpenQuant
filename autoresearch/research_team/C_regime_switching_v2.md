# Memo C v2 — Regime-conditional MR/momentum switching

**Status:** v2. ~4hr research pass. The hypothesis under test (H3): mean-reversion-on-spread is structurally a short-vol-of-dispersion bet; OpenQuant's 2025-H2 → 2026-Q1 underperformance is a regime-conditional failure, not a peer-set or stationarity-test failure. The fix is a regime gate that switches model class (MR off / momentum on / stand aside).

---

## Executive summary

The literature on regime-dependent stat-arb is rich but mixed. Five things hold up cleanly:

1. **The premium is regime-dependent.** Khandani & Lo (2007), Avellaneda & Lee (2010), Daniel & Moskowitz (2016), MSCI (2025) all document that quant equity / stat-arb returns are regime-conditional. The 2025 quant drawdown is a live, documented example — Goldman Sachs prime services estimated quant equity managers lost ~4.2% Jun-Jul 2025; "quality factor" suffered a 17% drawdown since July; the most-shorted basket more than doubled from April-October 2025. ([Bloomberg garbage rally](https://www.bloomberg.com/opinion/articles/2025-08-04/hedge-funds-are-hurting-from-a-garbage-rally-who-s-to-blame))

2. **There are TWO different "bad regimes" for stat-arb, with different mechanisms and different signals.** (a) Forced-unwind / liquidity / crowding episodes (2007-08, 2025 Q3) — caught by funding-stress signals (TED, FRA-OIS) and crowding indices. (b) Trending-leadership / mega-cap concentration regimes (2018-2020 "value drawdown") — caught by cross-sectional dispersion and factor-leadership signals. The 2025 episode is closer to (a) than (b). Our Aug 2025–Mar 2026 episode probably mixes both.

3. **OpenQuant's basket-spread MR is exposed to BOTH regimes.** When mega-caps absorb capital and equal-weight peers fade, basket spreads diverge persistently (regime b). When forced unwinds cascade through crowded short legs ("junk rally"), basket spreads also diverge — but the mechanism is positions, not fundamentals (regime a). FAANG/entsw 0/8 across both windows is consistent with both regimes hitting consecutively.

4. **OOS replication of regime gates is mixed.** Bulla et al. (2011) is the cleanest long-record paper showing Markov-switching asset allocation beats unconditional OOS net of costs (vol -41%, excess return +18-200 bps). The QSTrader HMM example shows Sharpe 0.37 → 0.48, modest. Many in-sample claims of "Sharpe doubled with regime gate" do not replicate. Asness/AQR explicitly push back: factor-timing has "weak historical track record."

5. **Stand-aside is more defensible than model-class flip.** No evidence in the published literature that the same residual-stat-arb baskets give useful momentum signal in adverse regimes. Wood-Roberts-Zohren's success is on TS-momentum on futures, not residual-stat-arb on equities.

**Recommendation:** A regime gate is justified, but as a *risk-off filter* (stand aside in adverse regime) rather than a model-class flip (MR ↔ momentum). Implement with **two** signals from different families: (i) cross-sectional dispersion *trend* (CSV momentum), captures regime b; (ii) factor-leadership *trend* of crowded longs (mega-cap basket vs. rest), captures regimes a and b. Require both to flag adverse before disabling. Data-source cost is low: both signals are computable from constituent close-prices we already have. Do NOT use single-state HMM-on-basket-spread-P&L as primary gate — known to fail on novel regimes, and the 2025-2026 regime is novel relative to anything in our basket-spread training set.

---

## Documented evidence stat-arb is regime-dependent

### 1. August 2007 "Quant Quake" (Khandani & Lo, NBER w14465 / J. Financial Markets 2011)

**The strategy they back-tested.** Lehmann (1990) / Lo-MacKinlay (1990) contrarian:
```
w_{i,t} = -(1/N) * (r_{i,t-1} - r_{M,t-1})
```
where r_M is equal-weighted market return; w sums to zero. Long past losers, short past winners. Holding period: typically one period (daily or weekly).

**Pre-Aug-2007 vs. Aug 6-9, 2007.** The contrarian strategy had been profitable for years. Then in the week of Aug 6, 2007 it suffered "unprecedented losses" — described as a 4-sigma+ event for several large quant funds. By Aug 10 the strategy snapped back to profitability.

**Cause attribution.** Khandani-Lo concluded this was *not* a regime change in the underlying mean-reversion premium. It was forced unwind: "the rapid 'unwind' of one or more sizable quantitative equity market-neutral portfolios... possibly due to a margin call or risk reduction." A multi-strategy fund (probably hit by subprime losses in another book) liquidated stat-arb positions, cascading through other funds running similar trades.

**Regime feature retrospectively visible:** crowding. Pre-event, leverage in long-short stat-arb books had been climbing. The strategy was not in a regime where MR didn't work; it was in a regime where the *capacity* of the trade had been overwhelmed. This is a different failure mode than what we're hypothesizing for OpenQuant 2025-Q3.

**For us:** doesn't directly support H3, but establishes the principle that stat-arb returns are regime-conditional (here, on funding-liquidity / crowding). [Khandani-Lo 2007/2011](https://www.nber.org/system/files/working_papers/w14465/w14465.pdf)

### 2. Avellaneda & Lee 2010 — post-2003 Sharpe decline

PCA-residual stat-arb on US equities had average Sharpe 1.44 1997-2007, but:
- Pre-2003: stronger
- 2003-2007: Sharpe **0.9** (down ~37%)
- ETF-based version: Sharpe 1.1 1997-2007, "similar degradation since 2002"

Avellaneda-Lee attributed the decline to "increased competition" and "decline in mean-reversion premium." When they added trading-volume timing (only trade when volume signals confirm), Sharpe recovered to 1.51 in 2003-2007.

**For us:** structurally similar to H3 — they observed regime decay and used a *signal-conditional gate* (volume) to recover Sharpe. They did NOT switch to momentum; they refined the entry condition. [Avellaneda-Lee 2010](https://math.nyu.edu/~avellane/AvellanedaLeeStatArb071108.pdf)

### 3. COVID-2020 dispersion spike

The COVID episode is more about correlation explosion than dispersion divergence. Realized correlations spiked to ~0.85+ (vs. ~0.25-0.40 normal), meaning *all* stocks moved together. This kills both stat-arb (no spread to trade) and pairs trading (cointegration breaks because residuals are dominated by the single market factor).

The Slow Momentum/Fast Reversion paper (Wood, Roberts, Zohren, 2021) reports their DMN+CPD model improved Sharpe by ~33% over 1995-2020, and **~67% over 2015-2020 alone** — most of the gain came from changepoint-aware behavior in COVID and the run-up to it. Their thesis: time-series momentum "makes bad bets at turning points where trends reverse." They use online changepoint detection (BOCPD, Adams-MacKay 2007) to switch between slow trend and fast reversion modes.

**For us:** strongest published evidence that regime/changepoint-aware switching beats unconditional. Sample size is limited (~5 years where the regime gate is the dominant alpha). [Wood-Roberts-Zohren 2021](https://arxiv.org/abs/2105.13727)

### 4. 2023-2026 mega-cap concentration era (the regime under test)

Multiple practitioner sources document this:

- Top 10 S&P 500 = 40% of index (2025 year-end), up from 14% in 1995
- Magnificent-7 hedge fund crowding peaked Jun 2024 at ~21% net exposure
- Hedge fund VIP basket returned +31% YTD vs. S&P +19% in 2023 — the *dominant* names traded with momentum, not mean reversion
- In May 2025, the S&P 500 increased 6.29% while ~30% of constituents declined — substantial single-name dispersion masked by index level

**Why this kills basket-spread MR specifically:** equal-weight peer baskets aggregate over single names whose alpha is moving in *opposite* directions (mega-cap absorbing capital, smaller peers losing it). The basket spread is essentially short the dispersion premium. When dispersion is high *and* persistent in one direction, MR loses on every side: buying the "cheap" basket loses because cheap stays cheap; shorting the "rich" basket loses because rich gets richer.

**The 2025 quant drawdown is the live evidence for this regime mechanism.** Concrete numbers from MSCI and Goldman Sachs Prime Services:
- **GS Prime estimate: quant equity managers lost ~4.2% Jun-Jul 2025** ([NY Ledger / DNYUZ](https://thenyledger.com/markets/why-a-garbage-rally-powered-by-junk-stocks-could-explain-quant-hedge-funds-no-good-very-bad-summer/))
- **Quality factor: -17% drawdown since July 2025** (largest since late-2020/early-2021)
- **Most-shorted basket: more than doubled Apr-Oct 2025**, +30% just in Sept
- **Jan 2026: -2.8% over two weeks for US-focused quant funds (UBS estimate)**
- **MSCI on summer 2025:** *"momentum stocks did not do well... profitable stocks also underperformed... heavily shorted stocks also outperformed, a clear headwind for the short leg"*. MSCI also note *"performance drag was greatly magnified by large interaction effects among these factors... among the most heavily shorted stocks, nearly half of them had high exposure to residual volatility"*. ([MSCI summer 2025 wobble](https://www.msci.com/research-and-insights/blog-post/unraveling-summer-2025s-quant-fund-wobble))

**Resonanz Capital (2025) precondition list:** "long stretch of factor outperformance (quality, low vol, AI complex) with **low** dispersion [pair-spread dispersion = high crowding]; triggers are macro or earnings surprises that flip leadership; flow paths show slow net-downs, widening pair spreads, and rising intra-factor correlations." Note: their "low dispersion" is *inter-pair* — meaning many pairs were correlated (crowded), not inter-stock. Different from CSV. ([Resonanz on quant unwinds](https://resonanzcapital.com/insights/crowding-deleveraging-a-manual-for-the-next-quant-unwind))

**For us:** strongest direct evidence yet for H3, with documented mechanisms and live 2025 examples. Two distinct mechanisms, both apply to OpenQuant's basket-spread MR:
1. *Trending-mega-cap regime:* equal-weight basket fades because the mega-cap is the trade, not the average. Fits FAANG 0/8.
2. *"Junk rally" / short-squeeze regime:* short legs of crowded baskets get squeezed; the same names that "should" mean-revert keep going up. Fits any sector where shorts were crowded with other quants.

---

## Named regime detectors (~10)

For each, we extract math, code (where applicable), and practitioner usage. Data requirements are flagged.

### 1. Two-state Gaussian HMM on returns

`s_t ∈ {0,1}`, transition matrix P, emission `r_t | s_t ~ N(μ_{s_t}, σ²_{s_t})`. Baum-Welch (EM) for fitting; forward-backward for online inference.

```python
from hmmlearn.hmm import GaussianHMM
hmm = GaussianHMM(n_components=2, covariance_type="full", n_iter=1000).fit(rets)
state = hmm.predict(rets)[-1]
```

**Best OOS evidence:** Bulla et al. 2011 — US/JP/DE equity indices 1969-2009, vol -41%, +18.5-201.6 bps net of costs. QSTrader OOS 2005-2014: Sharpe 0.37→0.48, max DD 56%→24%.

**Critique.** Two-state HMMs detect a *vol* regime, not a stat-arb regime. Asness 2016: factor-timing has "weak historical track record." For our problem, dispersion + concentration is closer.

### 2. Three-state HMM (bull / sideways / bear)

`n_components=3`. Sideways = enable MR; bull/bear = disable. Reus et al. 2020 (MDPI) empirical claim: factor portfolio rotating on HMM regime outperforms any single factor.

**Critique.** Three-state harder to estimate; "sideways" often absorbs into extremes. Strong lookback dependence.

### 3. Markov-switching dynamic regression (Hamilton 1989, statsmodels)

`y_t = μ_{s_t} + φ_{s_t} y_{t-1} + ε_t`, ε_t ~ N(0, σ²_{s_t}). Hamilton filter for inference.

```python
mod = sm.tsa.MarkovRegression(rets, k_regimes=2, switching_variance=True)
res = mod.fit()
prob_state1 = res.smoothed_marginal_probabilities[1]
```

**For us.** Direct fit for H3's spirit: regress basket-spread P&L on own lags; switch off when AR(1) coef ≥ 1.0 (trending, not mean-reverting). Best framing of "model-class switching." Issue: needs sufficient spread P&L history.

### 4. Bayesian online changepoint detection (BOCPD; Adams-MacKay 2007)

Track run-length distribution P(r_t | x_{1:t}):
```
P(r_t = 0     | x_{1:t}) ∝ Σ_{r_{t-1}} P(r_{t-1} | x_{1:t-1}) · π(x_t | r_{t-1}) · H(r_{t-1})
P(r_t = r_{t-1}+1 | x_{1:t}) ∝ P(r_{t-1} | x_{1:t-1}) · π(x_t | r_{t-1}) · (1 - H(r_{t-1}))
```
H = hazard (often constant 1/λ for geometric prior); π = predictive density. Exact O(T²); approximations O(T).

**Practitioner usage.** Wood-Roberts-Zohren 2021 use BOCPD as input to DMN; +33% Sharpe overall, +67% in 2015-2020. Best fit for "the regime just changed" framing. Implementation non-trivial; reference impl exists. ([Adams-MacKay](https://arxiv.org/abs/0710.3742))

### 5. VIX-conditional gate

`trade_on_t = 1{VIX_t ≤ θ}`. Canonical retail rule. Pairs naturally with momentum (Barroso-Santa-Clara). For us: insufficient — VIX was elevated but not crisis-level in 2025-2026, missing the dispersion mechanism.

### 6. Cross-sectional dispersion (CSV / Cboe DSPX)

**Math.** Realized CSV: `CSV_t = sqrt( (1/N) Σ_i (r_{i,t} - r̄_t)² )`. DSPX (implied): forward-looking 30-day expected dispersion from option prices, modified VIX methodology applied to S&P 500 components. Launched 2023-09-27.

**Practitioner usage.** S&P Dow Jones frames CSV as "the opportunity in stock selection." MSCI decompose CSV into systematic vs. idiosyncratic shares. High *systematic* share = factor-driven regime; high *idiosyncratic* share = dispersed regime.

**Why this fits H3.** When CSV is high and persistent, MR-on-spreads loses (dispersion isn't reverting). When CSV is low, baskets hug each other (no opportunity). Sweet spot: moderate CSV with mean-reverting trajectory.

**Data needed.** DSPX direct, only since 2023-09. Realized CSV computable daily from constituent returns we already have. Cheap.

[Cboe DSPX](https://www.cboe.com/us/indices/dispersion/), [S&P Dispersion paper](https://www.spglobal.com/spdji/en/documents/research/research-dispersion-measuring-market-opportunity.pdf)

### 7. Implied correlation index (Cboe ICJ / KCJ / COR3M)

**Math.** `ρ_implied = (σ_index² - Σ_i w_i² σ_i²) / (2 Σ_{i<j} w_i w_j σ_i σ_j)`. Forward-looking; complementary to CSV.

**Practitioner usage.** "Calm index / busy constituents" = low implied correlation = idiosyncratic regime. Dispersion traders use it directly.

**Data needed.** Cboe EOD free; granular data costs. Computable from option chains we don't ingest. **Skip for now.** [Cboe Implied Correlation](https://www.cboe.com/us/indices/implied/)

### 8. Factor-leadership-trend filter (the regime indicator H3 actually wants)

**Math.** Construct a long-mega-cap-short-rest equal-dollar portfolio. Compute its 60-day cumulative return r_lead. Trade-off indicator: 1{r_lead > θ}, where θ is calibrated.

**Intuition.** When mega-caps trend and the rest fade, our baskets — which equal-weight constituents — are systematically wrong-footed. The factor-leadership signal directly measures this.

**Practitioner usage.** Variants are used at every stat-arb desk. Goldman's "Hedge Fund VIP basket" tracking what crowded longs are doing is essentially this signal.

**Data needed.** Just constituent prices. Cheap.

**Honest critique.** This is a *crowding/momentum* signal repackaged. Daniel-Moskowitz (2016) show this kind of signal is exactly what predicts momentum crashes (when the trend reverses, momentum dies). For our problem we want to be *off* during trend, then back *on* when trend snaps. This requires the signal to flag both the start and the end of the trending regime. BOCPD on r_lead would be the right combination.

### 9. TED / FRA-OIS funding-liquidity gate

TED = 3M LIBOR − 3M T-bill (now SOFR-based). Two-regime threshold ~48bp (Boudt-Paulus-Rosenthal 2017). Pre-2008 canonical; replaced post-2012 by FRA-OIS or repo spreads. Catches Khandani-Lo style forced-unwind regimes (2007-08, 2020-03), not the dispersion mechanism. [Boudt-Paulus-Rosenthal 2017](https://www.sciencedirect.com/science/article/abs/pii/S0927539817300555)

### 10. Volatility-targeting / Carver-style position scaling

`w_t = σ_target / σ_realized,t`. Carver: "diversification across rules and speeds, not optimization." Barroso-Santa-Clara: vol-managed momentum 0.53 → 0.97 Sharpe. Uncontroversial small win for us. Compatible with any regime gate above. [Barroso-Santa-Clara](https://papers.ssrn.com/sol3/papers.cfm?abstract_id=2041429)

---

## MR vs. momentum hybrid

The core question for H3: when the regime says MR-on-baskets doesn't work, does it make sense to *flip* the same baskets to momentum, or just stand aside?

**The literature is split:**

### Pro-flip camp (academic)
- Wood-Roberts-Zohren 2021: explicit "slow momentum, fast reversion" hybrid. CPD module switches between modes. +67% Sharpe in 2015-2020.
- Sun-Bertschinger 2023 ("A regime-switching model of stock returns with momentum and mean reversion") — semi-Markov model with state termination probability inducing momentum-then-reversal patterns.
- Velocity-of-change literature (Sandbo-Zhang): regime feature = sign of trend on a longer timescale; flip MR → momentum when sign is persistent.

### Stand-aside camp (practitioner)
- Asness (2016) "Resisting the Siren Song of Factor Timing" — factor-timing has weak historical track record; Asness's recommendation is to diversify across factors and harvest unconditionally.
- Carver: "diversification across rules and speeds, not optimization" — i.e., run *both* MR and momentum systems unconditionally, let them average.
- Khandani-Lo implicit: the strategy was fine, the leverage was wrong. Right answer was de-lever, not flip.

**The empirical question: do the same baskets that mean-revert in regime A trend in regime B?**

This is *not* automatic. If the basket members are mean-reverting peers in normal times (because their fundamentals correlate), they are not necessarily trend-persistent peers in dispersion regimes. In dispersion regimes, the *mega-cap* trends and the *rest* fades — which means the basket-spread (mega-vs-rest constructed) trends in one direction. But our baskets are equal-weight peer groups built to capture cointegration, not equal-weight market-cap rebalanced. Flipping them to momentum is a different trade, on a different P&L, against different inventory.

**My read: stand-aside is the safer path.** A regime gate that turns MR off when CSV + factor-leadership flag adverse regimes, without flipping to momentum, is more defensible. Ground truth: we don't have evidence the same basket-spreads trend in adverse regimes. Wood-Roberts-Zohren's success is on TS-momentum portfolios, not on residual stat-arb baskets.

---

## Honest evaluation: walk-forward records

This is where the literature is most disappointing.

**Pros that survive walk-forward:**

- **Bulla et al. 2011** — only published, long-record (40 years US/JP/DE indices), out-of-sample, transaction-cost-aware paper showing Markov-switching beats unconditional. Vol -41%, excess return +18.5-201.6 bps. The *only* clean reference. Caveat: equity-index *returns* regime, not stat-arb-residuals regime. May not transfer.

- **Daniel-Moskowitz 2016** — momentum-crash forecasting using bear-market indicator B_{t-1} and forecasted variance σ²_M. They show dynamic momentum doubles Sharpe. Robust across DE/UK/JP/US, futures + commodities + bonds + FX. Strong evidence regime conditioning works *for momentum*. Direct transfer to stat-arb basket-MR is unclear.

- **Barroso-Santa-Clara 2015** — volatility-scaling momentum Sharpe 0.53 → 0.97. Works for *all* tested markets (US, FR, DE, JP, UK). Robust. But this is volatility scaling, not regime gating per se.

- **Wood-Roberts-Zohren 2021** — DMN+CPD on TSMOM, +33% Sharpe overall, +67% in 2015-2020. Code [public on GitHub](https://github.com/kieranjwood/slow-momentum-fast-reversion). Walk-forward designed in. But: TS-momentum on futures, not stat-arb on equities — may not transfer.

**Replication failures and skeptical findings:**

- The QSTrader HMM tutorial honestly reports modest improvement (Sharpe 0.37 → 0.48). Many similar tutorials inflate this with in-sample regime fitting.

- Asness 2016 surveys factor-timing literature and concludes: weak historical track record. This is the strongest skeptical voice.

- Multiple sources flag HMM "model mis-estimation": small-sample issues, unbalanced data, high state persistence. False signals are a recurring complaint.

- "Markov regime-switching strategies will fail if there is a regime higher than the current high regime or a regime lower than the current low regime in the future" — direct quote from QuantifiedStrategies.com. Translation: HMMs can't extrapolate regimes that haven't happened in training data. For our 2025-Q3 mega-cap concentration regime, an HMM trained on 2010-2020 might not classify 2025 correctly because the regime is novel.

**The honest summary:**
- Regime conditioning helps for momentum strategies (multiple lines of evidence converge).
- Regime conditioning is plausibly helpful for stat-arb but the published OOS evidence is thinner.
- Walk-forward replication of "Sharpe doubled with regime gate" claims is the exception, not the norm.

---

## Specific case study: would these signals have flagged 2025-Q3 ex-ante?

Reasoning concretely about the regime starting roughly Aug 2025 (based on autoresearch findings showing FAANG/entsw 0/8 weeks during 2025-Q3 → 2026-Q1):

### Signal 1: Cross-sectional dispersion (CSV / DSPX)
Cboe DSPX climbed and stayed elevated through 2024 and 2025 — described in practitioner reports as "calm index / busy constituents." Realized cross-sectional dispersion of S&P 500 in 2024-2025 was elevated above 2010-2019 averages. **Yes, this signal would have flagged adverse regime ex-ante**, particularly when combined with the trend in DSPX (rising = entering the regime, falling = exiting). Time-series momentum-of-DSPX would have given a more usable timing signal than DSPX level alone.

### Signal 2: Factor-leadership-trend
Hedge fund VIP basket (mega-cap crowded longs) outperformed S&P by 12 percentage points in 2023, with sustained leadership through 2024 to mid-2025. Long-mega-short-rest portfolio was in a multi-quarter persistent trend. **Yes, this signal would have flagged adverse regime** — and it would have flagged it *months before* our 2025-Q3 drawdown began.

### Signal 3: HMM on basket-spread P&L returns
~1.5 years of basket-spread P&L; not enough for stable HMM, especially 3-state. **A naïve HMM would likely have missed this regime** because the bad regime hadn't happened in training data — the canonical novel-regime failure mode.

### Signal 4: Realized / implied correlation
Implied correlation indices low through 2024 (idiosyncratic regime). Realized cross-pair correlation of our basket weekly P&L likely rose ahead of the drawdown. **Likely would have flagged crowding-style risk.**

### Signals 5-6: VIX, TED / funding spreads
2024-2025 was low-vol, no acute funding stress. **Neither would have flagged this regime.** Different failure mode than 2008/2020.

**Synthesis:** Two signals would have given ex-ante flags — (a) CSV trend, (b) factor-leadership trend — both data-cheap. HMM-on-basket-spread-P&L is the framework closest to H3 in spirit but the *least* likely to have caught this novel regime.

---

## Self-critique (5 weak claims marked)

Re-reading the draft against the standards in the prompt:

1. **Marked weak: "MR-on-spread is a short-vol-of-dispersion bet."** This framing is intuitive but I haven't found a paper that derives basket-spread MR returns as exposure to a dispersion variance/correlation premium explicitly. By analogy with index-vs-component dispersion trades it should hold (selling correlation = short dispersion), but the analogy needs a derivation we haven't done. Confidence: medium. Plausible but not proven.

2. **Marked weak: "Stand aside, don't flip to momentum."** The Wood-Roberts-Zohren paper offers the strongest published evidence for combined slow-momentum + fast-reversion, and it's robust across futures classes. I argue it doesn't transfer to residual stat-arb baskets, but I don't have a paper showing it FAILS to transfer either. Could be wrong. We could test by computing a 60-day basket-spread momentum signal and checking if it's predictive in adverse regimes.

3. **Marked weak: "The 2025 quant drawdown supports H3."** It strongly supports a regime mechanism, but the specific mechanism documented by GS/MSCI is *crowding-driven short-squeeze*, not *trending-mega-cap-dispersion*. These overlap but aren't identical. H3's specific causal story (mega-cap concentration → basket spreads diverge → MR fails) is more closely matched by the AQR 2018-2020 episode than by the 2025 episode.

4. **Marked weak: "The two signals (CSV + factor-leadership) would have flagged 2025-Q3 ex-ante."** This claim is in-sample because I am hand-picking signals that fit the regime I'm trying to detect. To be defensible OOS I need to backtest these signals against multiple historical adverse regimes (2007-08, 2018-2020, 2025) AND show they don't fire false positives in 2014-2018 (when the strategy presumably worked).

5. **Marked weak: "OOS replication of regime gates is mixed."** I cited Bulla et al., QSTrader, Wood-Roberts-Zohren as positive. I cited Asness skepticism as negative. The *count* of negative replications I found is small. There may be unpublished negative results (publication bias). I should not over-claim "mixed" — the published evidence is mostly positive, but selection bias is real.

## Cross-references with A, B, D

**A's draft (peer_sets_v1.md, read).** A argues the FAANG/entsw 0/4 problem is a peer-set-construction issue: GICS doesn't strip the right factors, so AAPL/AMZN/GOOGL/META end up in baskets that aren't truly residual-mean-reverting. A's recommended fixes are BARRA-style residuals, Avellaneda-Lee PCA, DBSCAN/OPTICS clustering — all *peer-set* fixes that would either reject the FAANG basket at admission or build a different one whose residuals do mean-revert.

**Tension with H3:** A's framing implies the problem is structural at the basket-construction layer. H3 argues the problem is regime-conditional at the model-class layer. **These are not mutually exclusive.** Both could be true: FAANG baskets might be poor regardless (A) AND the broader 6-out-of-8 sector weakness in this period might be regime (C). The 22/35 wins in 6 sectors and 0/8 in FAANG/entsw split is consistent with: A explains entsw and FAANG specifically; C explains the *sector-broad* underperformance.

**Test that distinguishes them:** if we had run the same MR-on-baskets in 2018-2019 (mega-cap concentration era #1), would FAANG/entsw have failed similarly? If yes, A's hypothesis is sufficient. If no, C is necessary.

**A says implicitly that regime-switching is *not* the fix for FAANG/entsw** — A says the right peer-set wouldn't have admitted those baskets at all. I agree. But the broader regime question remains: once we have good baskets (even with A's fixes), is the strategy regime-conditional or unconditional? My read: still regime-conditional, just with a higher base-rate Sharpe.

**B's draft (regime detection methodology). NOT PRESENT AT WRITE-TIME — file `B_*.md` does not exist in research_team folder.** Pending B's draft, my anticipated cross-checks:
- Does B's literature converge on HMM + BOCPD + dispersion as the dominant techniques? (Mine does.)
- Does B include the Bulla et al. 2011 honest-OOS evidence? (Should be the keystone.)
- Does B have a section on regime-detection FAILURES (false signals, mis-estimation)? Mine references QSTrader's modest Sharpe lift and the "novel regime" critique, but a fuller failure catalog is what I'd want from B.

**D's draft (failure-mode catalog). NOT PRESENT AT WRITE-TIME — file `D_*.md` does not exist in research_team folder.** Pending D's draft, my anticipated cross-checks:
- If D includes "stat-arb dies in trending regimes" as a documented failure mode with its own frequency, that's direct support for H3.
- If D's catalogue lists "junk rallies / short-squeezes" as separate from "trending regimes", that's a refinement — both apply to OpenQuant 2025.
- If D's catalogue is dominated by single-name structural-break issues (earnings, M&A), that supports A more than C.

**Anticipated conflicts:**
- A may say: regime gate is unnecessary if you fix the peer set. **My pushback:** even with perfect peers, MR-on-spread is a short-vol-of-dispersion bet, and that premium is genuinely regime-conditional. Bulla et al.'s 41% vol reduction holds even on equity indices that don't have peer-set issues.
- D may include "trending mega-cap regime" as a low-frequency failure mode, with caveat about insufficient samples to gate on it. **My pushback:** the same dispersion mechanism failed in 2018-2019 (AQR's value drawdown) and 2020 (COVID). It's not a one-off.

---

## Recommendation for OpenQuant

### What to ship (in order of confidence)

1. **CSV + factor-leadership two-signal regime filter.** Compute realized cross-sectional dispersion of S&P 500 daily; compute long-mega-short-rest 60-day momentum. Disable basket-MR entries when *both* are in their adverse regime (CSV trend-up persistent; factor-leadership trend strong). This is data-cheap (needs only constituent prices we already have), interpretable, and addresses H3 directly. **Confidence: medium-high.** Speculative magnitude estimate: based on Bulla et al. (vol -41%, +18-200 bps) and QSTrader HMM (Sharpe +0.11) we should expect a modest improvement in Sharpe and a meaningful drawdown reduction. Concrete numerical claims should wait for the backtest.

2. **Volatility-scaling on basket-spread P&L (Barroso-Santa-Clara style).** Independent of regime gate. Sharpe lift is well-established cross-asset (Barroso-Santa-Clara: 0.53 → 0.97 on momentum; smaller but positive on stat-arb in similar setups). Cheap to add.

3. **Stand aside, don't flip to momentum.** No empirical evidence in published literature that the same residual-stat-arb baskets give a useful trend signal in adverse regimes. The "MR↔momentum" framing in the prompt is appealing but not supported. Wood-Roberts-Zohren is on TS-momentum portfolios (futures), not residual baskets. *Speculative inference, but I have not found a paper that contradicts it.*

### What NOT to ship

1. **HMM-on-basket-spread-P&L as primary regime gate.** Insufficient training data; HMMs notoriously fail on novel regimes; small-sample mis-estimation is real. Maybe a stretch goal once we have 5+ years of basket P&L.

2. **VIX-only gate.** VIX-based gates are the most popular and the worst-fit for our dispersion-regime problem. The 2024-2025 episode was low-vol/high-dispersion, exactly the case where VIX gates fail.

3. **Markov-switching factor allocation à la Bulla.** This is a *whole-portfolio* asset-class shift; we're optimizing a single strategy.

### Implementation plan
- Extend the basket engine with a regime-gate input (boolean, daily). Default off → degrades to current behavior.
- Compute CSV daily from existing constituent close-prices.
- Compute factor-leadership signal from existing top-10 vs. rest 60-day cumulative return.
- Backtest: gate on/off, compare Sharpe, hit rate, drawdown sectorally.
- Walk-forward validation on Jul 2025 → Mar 2026 (out-of-sample for the question we're investigating); leave 2026-Q2 onward as held-out OOS for the gate decision itself.

---

## What's still unknown

1. **Stat-arb-on-residuals regime literature is thin.** Most regime-conditioning literature is on TS-momentum, factor portfolios, or asset-class rotation. Direct regime-conditioning of mean-reversion-on-residuals is rare. We may be slightly extrapolating.

2. **Whether the right gate is on dispersion *level* or dispersion *trend*.** Practitioner sources lean trend; academic sources tend to use level. The 2025-Q3 episode involved sustained high dispersion, which favors level. But the *transition* into the regime favors trend. Best is probably both (BOCPD on dispersion).

3. **OOS robustness of the gate.** With only one regime episode in our hands, we can't honestly walk-forward a regime gate. The gate parameters chosen will be in-sample-fit to the very episode they're supposed to detect. We need to be transparent about this in PR backtests.

4. **2018-2020 retrospective.** Did our (counterfactual) basket-MR strategy fail in 2018-2019 (AQR-style value drawdown) and in 2020 (COVID)? If we had run our system on those years and it was profitable, regime gating may be unnecessary and recent failure is noise. If it would have failed similarly, regime conditioning is necessary. **Worth running before committing to the gate.**

5. **Implied correlation / DSPX as additional signals.** DSPX is the cleanest forward-looking regime indicator but only available 2023-09 onward. Not enough history to walk-forward. Realized cross-sectional vol is the workable substitute.

6. **Cost of gating false positives.** A regime gate that's right 80% of the time still kills 20% of profitable opportunities. If our base-rate per-trade hit rate is ~55% (a guess), the gate needs to have meaningfully better discrimination to add net Sharpe.

7. **The peer-set fix vs. regime gate decision.** A's fixes are higher-confidence on the FAANG/entsw subset; C's fix is higher-confidence on the broader timing question. They are complementary but compete for engineering bandwidth. Sequence: fix peer-set first (eliminates known-bad baskets), then add regime gate (eliminates timing-bad regimes). Don't add the gate before fixing baskets, or the gate will be calibrated on poisoned data.

---

## Sources (all checked)

**Foundational papers**
- Khandani & Lo (2007/2011), "What Happened to the Quants in August 2007?": [NBER w14465](https://www.nber.org/system/files/working_papers/w14465/w14465.pdf)
- Avellaneda & Lee (2010), "Statistical Arbitrage in the U.S. Equities Market," Quant Finance 10(7): [PDF](https://math.nyu.edu/~avellane/AvellanedaLeeStatArb071108.pdf)
- Daniel & Moskowitz (2016), "Momentum Crashes," JFE 122(2): [NBER w20439](https://www.nber.org/papers/w20439)
- Barroso & Santa-Clara (2015), "Momentum Has Its Moments," JFE 116(1): [SSRN](https://papers.ssrn.com/sol3/papers.cfm?abstract_id=2041429)
- Asness/Moskowitz/Pedersen (2013), "Value and Momentum Everywhere," JF 68(3): [SSRN](https://papers.ssrn.com/sol3/papers.cfm?abstract_id=2174501)

**Regime-detection methodology**
- Adams & MacKay (2007), "Bayesian Online Changepoint Detection," [arXiv:0710.3742](https://arxiv.org/abs/0710.3742)
- Bulla et al. (2011), "Markov-switching asset allocation: Do profitable strategies exist?", J. Asset Mgmt: [Springer](https://link.springer.com/article/10.1057/jam.2010.27)
- Wood/Roberts/Zohren (2021), "Slow Momentum with Fast Reversion," [arXiv:2105.13727](https://arxiv.org/abs/2105.13727)
- Reus et al. (2020), "Regime-Switching Factor Investing with HMMs," JRFM 13(12): [MDPI](https://www.mdpi.com/1911-8074/13/12/311)
- Boudt/Paulus/Rosenthal (2017), "Funding liquidity, market liquidity and TED spread": [ScienceDirect](https://www.sciencedirect.com/science/article/abs/pii/S0927539817300555)

**Practitioner / market commentary**
- Asness (2016), "Resisting the Siren Song of Factor Timing": [AQR PDF](https://www.aqr.com/-/media/AQR/Documents/Insights/Perspectives/Resisting-the-Siren-Song-of-Factor-Timing.pdf)
- Asness three-quant-crises interview (Inst. Investor 2024): [link](https://www.institutionalinvestor.com/article/2dqsr456gmu55p19gxiio/corner-office/cliff-asness-has-steered-hedge-fund-aqr-through-not-one-not-two-but-three-quant-crises)
- Resonanz Capital (2025), "Understanding the 2025 Quant Unwind": [link](https://resonanzcapital.com/insights/crowding-deleveraging-a-manual-for-the-next-quant-unwind)
- Resonanz Capital, "Dispersion Trading and the DSPX Index": [link](https://resonanzcapital.com/insights/dispersion-trading-and-the-dspx-index)
- MSCI (2025), "Unraveling Summer 2025's Quant Fund Wobble": [link](https://www.msci.com/research-and-insights/blog-post/unraveling-summer-2025s-quant-fund-wobble)
- Bloomberg (2025), "Hedge Funds Are Hurting From a Garbage Rally": [link](https://www.bloomberg.com/opinion/articles/2025-08-04/hedge-funds-are-hurting-from-a-garbage-rally-who-s-to-blame)
- Hedgeweek (2026), "Quant hedge funds see worst drawdown since October": [link](https://www.hedgeweek.com/quant-hedge-funds-see-worst-drawdown-since-october-as-crowded-trades-unwind/)
- NY Ledger (2025), "Garbage rally" / GS prime services 4.2% Jun-Jul 2025 estimate: [link](https://thenyledger.com/markets/why-a-garbage-rally-powered-by-junk-stocks-could-explain-quant-hedge-funds-no-good-very-bad-summer/)
- Cboe S&P 500 Dispersion Index methodology: [PDF](https://www.spglobal.com/spdji/en/documents/methodologies/methodology-cboe-sp-500-dispersion-index.pdf)
- S&P "Dispersion: Measuring Market Opportunity": [PDF](https://www.spglobal.com/spdji/en/documents/research/research-dispersion-measuring-market-opportunity.pdf)
- Carver, Better System Trader podcast: [link](https://bettersystemtrader.com/026-robert-carver/)

**Code / implementations**
- [hmmlearn](https://github.com/hmmlearn/hmmlearn) (Python GaussianHMM, Baum-Welch)
- [statsmodels MarkovRegression](https://www.statsmodels.org/stable/generated/statsmodels.tsa.regime_switching.markov_regression.MarkovRegression.html) (Hamilton filter)
- [QSTrader regime-HMM tutorial](https://www.quantstart.com/articles/market-regime-detection-using-hidden-markov-models-in-qstrader/) (full code, OOS Sharpe 0.37→0.48)
- [Bayesian changepoint detection reference impl](https://github.com/hildensia/bayesian_changepoint_detection)
- [Slow Momentum Fast Reversion code](https://github.com/kieranjwood/slow-momentum-fast-reversion)
- [Hudson & Thames regime-switching pairs trading](https://hudsonthames.org/pairs-trading-with-markov-regime-switching-model/)

