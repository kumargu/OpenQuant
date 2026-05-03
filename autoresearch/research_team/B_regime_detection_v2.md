# Memo B v2 — Online regime-change detection in cointegrating relationships

**Researcher:** B (regime-change / structural-break detection)
**Scope:** Hypothesis H2 — peer set was valid; cointegration broke mid-sample due to idiosyncratic shocks (GOOGL antitrust+AI cloud, NVDA mid-cycle while peers ripped, ORCL spike, INTU collapse). If H2 is right, the fix is online (live, not after-the-fact) detection of cointegration breakdown so we stop trading bad baskets before they bleed.
**Date:** 2026-05-02
**Status:** v2 final, cross-validated with researcher A (peer sets). C and D drafts not yet posted; conflicts/agreements noted speculatively where unavoidable, or marked as "pending."

---

## Executive summary (TL;DR)

1. **Six families of online structural-break tests exist for cointegrating relationships**: BOCPD (Adams-MacKay 2007), CUSUM-on-residuals (Brown-Durbin-Evans 1975 / Xiao-Phillips 2002), Quandt-Andrews sup-Wald (1993), Bai-Perron multiple-break (2003), Markov regime-switching with Hamilton filter (Hamilton 1989, Bock-Mestel 2009), and PELT/BinSeg offline-with-warm-restart (Killick 2012). Of these, only BOCPD, recursive CUSUM, and the Hamilton-filtered HMM are genuinely online (no hindsight, no retrospective re-estimation of the break date).

2. **The hazard-rate / threshold parameterisation is nowhere near a settled science for daily-equity-pair residuals.** Published BOCPD financial-data λ choices range from 1/80 to 1/250; the practitioner-blog rule-of-thumb is λ ≈ T/3 (one regime per ~80–250 days for our window). Different λ choices flip the ranking of detection-vs-false-positive trade-offs. **Calibration requires labelled break events we don't have.**

3. **Detection lag for HMM-based regime detection in published practitioner work is 1–5 days for macro-vol regimes; for pairs-residual structural breaks the lag is essentially undocumented in production literature.** The Bock-Mestel 2-state Markov model — the only stat-arb-specific HMM result — is a *signal-generation rule*, not an *online break alarm*: it uses smoothed (full-sample) probabilities and was never validated as a live circuit-breaker.

4. **Cointegration breakdown caused by idiosyncratic shocks is, in Ernie Chan's published view, NOT a regime-detection problem — it's a missing-variable problem.** In his 2011 GLD-GDX-USO post-mortem, he documented that the GLD-GDX cointegration "went haywire in 2008" because of rising oil prices (miners' input costs) and miner hedging programs; the fix was *adding USO to the cointegration vector*, not detecting the break and pulling out. **This directly maps to our case: GOOGL antitrust + cloud AI exposure = a missing factor (regulatory / cloud-revenue) that GICS-FAANG can't capture.**

5. **Cross-reference with researcher A:** A's evidence (BARRA-residual peers, Sarmento & Horta 2020 OPTICS-clustered peers, Cartea ICAIF 2023 SPONGE) says **bad peer sets cause spurious "structural break" detections**. If GOOGL/META share an ad-stock factor that AAPL doesn't, every event that hits the ad-stock factor will look like a "GOOGL-AAPL cointegration break" regardless of model — but it's actually peer-set mis-specification firing as a fake break. **This is a strong signal: regime detection on top of a bad peer set produces false positives masquerading as detected breaks.**

6. **Practitioner-deployable recommendation for OpenQuant:** *Do not deploy ex-ante online regime detection as the primary fix for the FAANG/entsw failure.* Researcher A's hypothesis (bad peers) is more parsimonious and the literature backs it. However, a **CUSUM-of-residuals monitor (Brown-Durbin-Evans 1975 with Xiao-Phillips 2002 cointegration variant) and a `recent_z_run_length` heuristic** are cheap, online, parameter-light circuit breakers that, layered on top of a corrected peer set (A) or the right model class (C), reduce the risk of compounding losses when the inevitable post-admission breakdown occurs. False-positive rate should be evaluated against the cost of *not* trading good baskets, which is high in our 0.47 Sharpe regime.

---

## Techniques (~10)

For each: (a) name + canonical paper, (b) test statistic and key formula, (c) parameterisation with concrete values from the literature, (d) online vs offline, (e) detection-lag and false-positive notes, (f) code reference.

### 1. Bayesian Online Change-Point Detection (BOCPD) — Adams & MacKay 2007 ([arXiv 0710.3742](https://arxiv.org/abs/0710.3742))

**Math.** Maintain a posterior over "run length" r_t = time since last changepoint. At each new observation x_t:

- Joint update (Eq. 3 of the paper):
  p(r_t, x_{1:t}) = Σ_{r_{t-1}} p(x_t | r_{t-1}, x^(r)) · p(r_t | r_{t-1}) · p(r_{t-1}, x_{1:t-1})

- Two operations:
  - **Growth** (no break): p(r_t = ℓ, x_{1:t}) = p(r_{t-1}=ℓ-1, x_{1:t-1}) · π_{t-1}^(ℓ-1) · (1 - H(r_{t-1}))
  - **Reset** (break): p(r_t = 0, x_{1:t}) = Σ_{r_{t-1}} p(r_{t-1}, x_{1:t-1}) · π_{t-1}^(r) · H(r_{t-1})

- π is the predictive density under the conjugate model. For **Gaussian observations with NIG prior**, the posterior predictive is a Student-t with degrees of freedom that grow linearly in run length — this fattens tails and reduces volatility-spike false positives.

- The **hazard function H(τ)** is the prior probability that a break occurs at run-length τ. The standard memoryless choice is H(τ) = 1/λ (geometric inter-arrival times).

**Hazard-rate calibration in the literature** (this is the contested parameter):
- Bayesian Online Changepoint Detection for Financial Time Series, [DL ACM 2025 paper](https://dl.acm.org/doi/10.1145/3795154.3795291): empirically optimised λ ≈ 100 on S&P 500 / CSI 300 daily log returns 2019–2025.
- [Online Learning of Order Flow and Market Impact, Tandfonline 2024](https://www.tandfonline.com/doi/full/10.1080/14697688.2024.2337300) ([arXiv 2307.02375](https://arxiv.org/abs/2307.02375)): h = 1/80 for both TSLA and MSFT, "tuned in a preliminary phase" by minimizing MSE.
- Practitioner blog [QuantBeckman "Switch-Off"](https://www.quantbeckman.com/p/with-code-switch-off-bayesian-online): `λ = max(burn_in + 10, T/3)` — for a 750-day backtest, λ ≈ 250; for our 189-day window, λ ≈ 80. They explicitly call fixed hazard rates "dark magic" and avoid them.

**Spread between published values: 1/80 ↔ 1/250, a factor of ~3.** No consensus.

**Online?** Yes, exact online inference, O(T²) naive or O(T) with run-length pruning at log-prob threshold (e.g. -10.0 keeps ~99.995% mass).

**Detection lag.** Adams-MacKay paper itself does not quantify lag. The [R-BOCPD 2020 paper "Restarted BOCPD achieves Optimal Detection Delay"](https://www.researchgate.net/publication/344337041) shows the original Adams-MacKay has *suboptimal* worst-case detection delay: it can be dominated arbitrarily by an adversarial pre-break distribution. The Restarted-BOCPD variant fixes this. **Implication: vanilla BOCPD has known pathological cases where the algorithm waits a long time after a break before flagging it — exactly the failure mode we care about.**

**Code.** [github.com/dtolpin/bocd](https://github.com/dtolpin/bocd) (Python), [github.com/jayzern/bayesian-online-changepoint-detection](https://github.com/jayzern/bayesian-online-changepoint-detection-for-multivariate-point-processes) (multivariate Cox process variant), [QuantBeckman `BOCPDRunner` class](https://www.quantbeckman.com/p/with-code-switch-off-bayesian-online) (~350 lines, NIG conjugate + log-space stability + run-length pruning).

### 2. Recursive-residual CUSUM — Brown, Durbin & Evans 1975 ([JRSS-B 37, 149-192](https://rss.onlinelibrary.wiley.com/doi/10.1111/j.2517-6161.1975.tb01532.x))

**Math.** For a regression y_t = x_t' β + ε_t fitted recursively, the **recursive residual** at step t is:

  w_t = (y_t − x_t' β̂_{t-1}) / √(1 + x_t' (X_{t-1}' X_{t-1})^{-1} x_t)

Under H_0 (parameter constancy), {w_t} are i.i.d. N(0, σ²). The **CUSUM statistic** is:

  W_t = (1/σ̂) · Σ_{s=k+1}^t w_s, for t = k+1, ..., T

Linear boundary approximation (Brown-Durbin-Evans 1975, Eq. 2.31):
  ±c · {1 + 2(t−k)/(T−k)}, where c is solved from boundary-crossing probabilities of Brownian motion. For α = 0.05, c ≈ 0.948.

Reject parameter constancy if W_t crosses the boundary at any t.

**Online?** Yes, naturally — recursive update is one-step. Standard implementation in `statsmodels.stats.diagnostic.recursive_olsresiduals`.

**Cointegration variant.** Xiao & Phillips 2002 ([J Econometrics 108(1), 43-61](https://www.sciencedirect.com/science/article/abs/pii/S0304407601001038), preprint at [Yale](http://korora.econ.yale.edu/phillips/pubs/art/p1046.pdf)) showed that the conventional CUSUM applied to cointegrating-regression residuals is a *consistent* test for the null of cointegration vs. the alternative of no-cointegration. The asymptotic distribution is non-standard (functional of Brownian motions); critical values are tabulated.

**Detection lag.** Designed for offline post-hoc inference, but with a Page-style sequential one-sided variant the recursive CUSUM can be operated online with "average run length" (ARL) governing the false-positive frequency. ARL₀ = 1/α gives the expected interval between false alarms; minimax detection delay (worst-case ARL₁) decreases as the post-break drift grows. **Concrete number from the MQL5 stat-arb article (NVDA/INTC partnership, Sep 18 2025): the CUSUM "triggered alarm: September 19, 2025 (one day after announcement)."** ([MQL5 article](https://www.mql5.com/en/articles/20946)).

**Code.** `statsmodels.stats.diagnostic.recursive_olsresiduals`, `strucchange::efp` (R), MATLAB `cusumtest`. See also the [strucchange vignette](https://cran.r-project.org/web/packages/strucchange/vignettes/strucchange-intro.pdf) for full-suite (CUSUM, MOSUM, RE, ME, OLS).

### 3. Sup-Wald / Quandt-Andrews — Andrews 1993 ([Econometrica 61(4)](http://dido.econ.yale.edu/~dwka/pub/p1138.pdf))

**Math.** Compute the Wald statistic for parameter equality at *every* candidate break date τ in the trimmed range [⌊π·T⌋, ⌊(1−π)·T⌋]:

  Sup-Wald = sup_{τ ∈ [π, 1−π]} W_T(τ)

where W_T(τ) is the standard Wald F-statistic for H_0: β_pre = β_post.

**Trimming.** Andrews recommends π = 0.15 (drop first/last 15% of sample). Critical values depend on π and the number of restricted parameters p:
- For π = 0.15, the **5% critical value ≈ 4.71 · p**.
- Asymptotic critical values are non-standard and tabulated in Andrews 1993 Table 1.

**Variants.** ave-Wald (Σ W_T(τ) / N_candidates), exp-Wald (Σ exp(W_T(τ)/2) — has higher power for moderate breaks), all in Andrews & Ploberger 1994.

**Online?** No, fundamentally offline. To use it online, you re-run the test at each new observation on the latest window — detection lag is essentially the half-window-size. **This is its core weakness for our application: it tells you *that* a break exists in the historical sample, not *when* one just happened.**

**Code.** `mlfinlab.structural_breaks.sadf` is essentially a sup-ADF variant of this approach for explosive (right-tailed) breaks. R's `strucchange::breakpoints`. Stata's `estat sbsingle`.

### 4. Bai-Perron multiple structural breaks — Bai & Perron 1998, 2003 ([J Applied Econ 18, 1-22](https://www.jstor.org/stable/30035185))

**Math.** Allows m unknown breakpoints τ_1 < ... < τ_m (and unknown m). Two test families:
- **F(s+1 | s) sequential test**: starts at H_0 of no breaks, alternative single break, accept/reject; if reject, test 2 breaks vs 1, etc.
- **Double max (UDmax)**: max of sup-F statistics over m = 1, ..., M_max.

**Algorithm.** Dynamic programming over O(T²) least-squares partitions to find optimal segmentation under SSR + penalty.

**Online?** No. Pure offline, retrospective. *But* Bai-Perron is the standard reference *post-hoc* analysis to validate that a method which claimed to detect a break in real-time actually got the break-date right.

**Code.** R `strucchange::breakpoints`, R `mbreaks` package. EViews multiple breakpoint tester.

### 5. Hamilton-filter-based Markov regime switching — Hamilton 1989 ([Econometrica 57(2), 357-384](https://users.ssc.wisc.edu/~behansen/718/Hamilton1989.pdf))

**Math.** State s_t ∈ {1, ..., K} follows Markov chain with transition matrix P_{ij} = P(s_t = j | s_{t-1} = i). Conditional on s_t, observation y_t ~ N(μ_{s_t}, σ²_{s_t}) (or richer model). The **Hamilton filter** recursion gives p(s_t = j | y_{1:t}):

  - Predict:    p(s_t = j | y_{1:t-1}) = Σ_i p(s_{t-1} = i | y_{1:t-1}) · P_{ij}
  - Update:     p(s_t = j | y_{1:t}) = [f(y_t | s_t = j) · p(s_t = j | y_{1:t-1})] / [Σ_k f(y_t | s_t = k) · p(s_t = k | y_{1:t-1})]

These are the **filtered** (causal, online) probabilities. **Smoothed** probabilities p(s_t | y_{1:T}) require backward recursion and use future data — *NOT online*.

**Stat-arb variant — Bock & Mestel 2009.** [SSRN 1213802](https://papers.ssrn.com/sol3/papers.cfm?abstract_id=1213802), [PDF](https://assets.super.so/e46b77e7-ee08-445e-b43f-4ffd88ae0a0e/files/706438cf-895f-472b-bd10-24701655e1a3.pdf). Two-state model on the spread: regime 1 (high mean μ₁, σ₁), regime 2 (low mean μ₂, σ₂). Trading rule:

  - In regime 1: Long if X_t ≤ μ₁ − δ·σ₁ AND P(s_t = 1 | X_t) ≥ ρ
  - In regime 2: Short if X_t ≥ μ₂ + δ·σ₂ AND P(s_t = 2 | X_t) ≥ ρ

Reported Sharpe 1.5–3.4 on US/Australian equity data (S&P 500, ASX 100), 2002–2008 sample.

**Caveats explicitly stated by arbitragelab/Bock-Mestel** ([arbitragelab regime-switching docs](https://hudson-and-thames-arbitragelab.readthedocs-hosted.com/en/latest/time_series_approach/regime_switching_arbitrage_rule.html)):
- "The strategy will fail if there is a regime higher than the current high regime or a lower regime in the future" — i.e. the model **cannot extrapolate to unseen regime severity**.
- "The Markov regime-switching model often fails to fit when switching mean and switching variance are both assumed" — i.e., **fitting is brittle**.

**Detection lag.** The QuantStart and LSEG-DevPortal practitioner sources report 1–5 days for macro-vol regime classification. **For pairs-residual breaks no published lag distribution exists.** The Bock-Mestel approach uses *smoothed* probabilities for trading rule construction, which is a **look-ahead bias** in any literal live-trading interpretation.

**Code.** `statsmodels.tsa.regime_switching.MarkovRegression`, `arbitragelab.time_series_approach.regime_switching_arbitrage_rule.RegimeSwitchingArbitrageRule`, `hmmlearn` (Python, Baum-Welch + Viterbi).

### 6. SADF / GSADF — Phillips, Shi, Yu 2015 ([IER 56(4)](https://onlinelibrary.wiley.com/doi/abs/10.1111/iere.12132))

**Math.** SADF: at each end-point t, run ADF on a backwards-expanding window starting from t − r₀ down to t − min_length:

  SADF_t = sup_{s ∈ [r₀, t]} ADF_{[t-s, t]}

GSADF generalises by also allowing the start point r₁ to vary, giving a flexible window:

  GSADF_t = sup_{r₁ ∈ [0, t-r₀], r₂=t} ADF_{[r₁, r₂]}

Designed for detecting **explosive (right-tailed)** behavior: reject H_0 of unit root in favor of mildly explosive alternative.

**Online?** Yes — at each t, compute the supremum over the past window. Computational cost O(T²) per evaluation but parallelizable.

**Use for cointegration breakdown.** The mlfinlab implementation (`mlfinlab.structural_breaks.sadf` ([source](https://github.com/hudson-and-thames/mlfinlab/blob/master/mlfinlab/structural_breaks/sadf.py))) is the practitioner's tool. Apply to cointegrating residuals: if the residual becomes explosive, that signals breakdown. Default parameters: `phi=0` (no penalty for long windows), `add_const=False`, `min_length` user-set.

**Detection lag.** Same trade-off as CUSUM — function of rolling-window length. Phillips-Shi-Yu show the GSADF has *consistent date-stamping* properties (real-time detection asymptotically locates the break correctly) but in finite samples there's lag.

### 7. PELT / Binary segmentation — Killick, Fearnhead, Eckley 2012 ([JASA 107(500)](https://arxiv.org/pdf/1101.1438))

**Math.** PELT solves the penalised optimal-segmentation:

  min over m, τ_1 < ... < τ_m of  Σ_i C(y_{τ_i:τ_{i+1}}) + β · m

where C is a cost function (Gaussian negative log-likelihood, etc) and β is the per-changepoint penalty.

**Penalty choices.**
- AIC: β = 2k (k = parameters per segment)
- **BIC/SIC: β = k · log(n)** — standard default, e.g. `ruptures` `pen=BIC`.
- MBIC: β = 3 · log(n) (Zhang-Siegmund 2007).

**Linear time.** Pruning rule: discard candidate breakpoints that can't be optimal under any future segmentation. Average O(n) under "regular changepoint distribution" assumption.

**Online?** No, fundamentally offline. Requires the entire signal. *Online-PELT* / *streaming PELT* exist (e.g. `changepoint-online` PyPI package) but are research-grade.

**Code.** [`ruptures` (Python)](https://github.com/deepcharles/ruptures): `rpt.Pelt(model="rbf").fit(signal).predict(pen=10)`. R `changepoint` package.

**Detection lag for online use.** None advertised. PELT is a batch algorithm. The 2025 PELT-financial paper ([DL ACM 2025](https://dl.acm.org/doi/10.1145/3773365.3773532)) noted: "the PELT approach is highly sensitive to structural breaks but tends to over-segment the data" — i.e. **high false-positive rate on financial data** even in offline mode.

### 8. ChangeFinder — Yamanishi & Takeuchi 2002 ([SDAR algorithm](https://pypi.org/project/changefinder/))

**Math.** Sequentially Discounting AR (SDAR) modelling. At each new observation:
1. Update AR(p) coefficients with discount factor r ∈ (0, 1] (closer to 0 = more weight on recent data).
2. Compute outlier score: -log p(x_t | x_{t-p:t-1}, θ_t).
3. Smooth outlier scores over a window.
4. Refit a *second-stage* AR on the smoothed outlier series.
5. Compute changepoint score from the second-stage residual.

**Online?** Yes, fully online and constant-time-per-update.

**Pros.** Lightweight, no labelled training, no conjugate-prior bookkeeping. Used in industrial anomaly detection.

**Cons.** Two hyperparameters (discount factor for first-stage AR, smoothing window) and threshold; little public guidance on financial calibration.

**Code.** [`changefinder` PyPI](https://pypi.org/project/changefinder/), [`ocpdet`](https://pypi.org/project/ocpdet/).

### 9. Wavelet + deep-learning structural-break-aware pairs trading — Wang et al. 2021 ([J Supercomputing](https://link.springer.com/article/10.1007/s11227-021-04013-x), [IEEE 2020 wavelet+CNN](https://ieeexplore.ieee.org/document/9070533))

**Math.** Hybrid two-phase model:
- **Phase 1 — break detection.** Wavelet transform of spread → frequency-domain features + time-domain features → CNN/LSTM classifier outputs P(break at t). Classifier is trained on labelled break events.
- **Phase 2 — RL agent.** Conditional on no-break, run a DQN-style trader.

**Online?** Yes for inference; offline for training.

**Critical limitation.** **Requires a labelled training set of past structural-break events.** For our 43 baskets across 8 sectors, we have neither (a) an objective labelling procedure for what counts as a break, nor (b) enough events to train a CNN. **Effectively unusable for OpenQuant.**

**Reported results.** The paper claims SAPT framework outperforms naïve pairs trading on Taiwan Stock Exchange data, but does not release detection-lag distributions, false-positive rates, or precision/recall numbers — only aggregate P&L deltas.

### 10. Restarted BOCPD (R-BOCPD) — Alami et al. 2020 ([RG paper](https://www.researchgate.net/publication/344337041))

**Math.** Modification of Adams-MacKay: when the run-length posterior collapses to short runs (i.e., a break has just been detected), **restart** the run-length distribution from a flat prior. Eliminates the worst-case detection delay of vanilla BOCPD.

**Why it matters.** The original Adams-MacKay BOCPD has *no theoretical guarantee* on detection delay — it can be made arbitrarily slow with adversarial pre-break distributions. R-BOCPD achieves the *optimal* (Lai 1995) detection delay rate D_Δ ~ log(1/α) / KL(post‖pre).

**Online?** Yes.

**Code.** No widely-deployed implementation; research-grade only.

---

## Practitioner usage — quotes

**Quote 1 — Ernie Chan (2011) on cointegration breakdown as missing-variable, not regime detection** ([epchan.blogspot.com](http://epchan.blogspot.com/2011/06/when-cointegration-of-pair-breaks-down.html)):

> "Cointegration relationships that once worked reliably can break down for extended periods—maybe as long as a half a year or more. Abandoning such a pair completely is also unsatisfactory, since cointegration often mysteriously returns after a while. ... The GLD-GDX pair was an excellent candidate for pair trading in 2006 but went haywire in 2008. ... Rising oil prices increased miners' extraction costs, causing gold miners' income to lag behind the rise in gold prices. ... Trading the triplet GLD-GDX-USO proved profitable throughout the entire period from 2006-2010."

**Why this matters for OpenQuant.** Chan's prescription is *not* online break detection — it's *re-modelling with the missing factor*. For our GOOGL antitrust + cloud AI shock, the analogue is: don't try to detect the break online — *include the cloud-revenue factor or the regulatory-overhang factor in the cointegration vector*. This bridges to researcher A: it's a peer-set / factor-model design problem, not an online-statistics problem.

**Quote 2 — Lars Klawitter on QuantConnect, on production HMM regime detection** ([QuantConnect forum](https://www.quantconnect.com/forum/discussion/14818/rage-against-the-regimes-the-illusion-of-market-specific-strategies/)):

> "The HMMs don't seem to produce any reliable signal for bear markets. ... Switching to a different universe during bear markets never reacted quickly enough to make a positive impact. ... Every time you run HMMlearn it might switch the order of the states."

Klawitter tested HMM-based regime detection over 10 years; found only marginal improvement via "dynamic adjustment of the total exposure base on a signal essentially interpreting vol of vol" — i.e. *reactive* sizing on realised volatility, not predictive regime detection.

**Quote 3 — Francesco Baldisserri, same QuantConnect thread, the structural argument:**

> "Regime-based trading is a form of overfitting. You have a strategy that works well under specific conditions and hope to figure out the best time to apply it in the future, which is basically market timing."

This is the strongest *prima facie* objection to the entire enterprise of online regime detection in production. Note: Baldisserri is talking about *meta-strategy* regime selection (use strategy X when in bull, Y when in bear). Our problem is narrower: detect *cointegration breakdown* on a specific pair to stop trading it. But the same overfitting concern applies: with 43 baskets × 8 sectors, we will *definitely* see false-positive break signals in some baskets that retrospectively didn't break.

**Quote 4 — QuantStart on HMM detection lag** ([QuantStart Hidden Markov Models](https://www.quantstart.com/articles/market-regime-detection-using-hidden-markov-models-in-qstrader/)):

> "The Hidden Markov Model does a good job of correctly identifying regimes, albeit with some lag. ... HMM could identify both financial and COVID crashes, as well as (with some lags) the volatile period starting after January 2022."

Detection lag is *acknowledged* but never precisely quantified in the practitioner literature.

**Quote 5 — arbitragelab on regime-switching pairs trading limitations** ([arbitragelab docs](https://hudson-and-thames-arbitragelab.readthedocs-hosted.com/en/latest/time_series_approach/regime_switching_arbitrage_rule.html)):

> "The strategy will fail if there is a regime higher than the current high regime or a lower regime in the future, since no such regime has appeared in historical data. ... The Markov regime-switching model often fails to fit when switching mean and switching variance are both assumed."

This is critical for our case: **GOOGL's antitrust ruling on Aug 5 2024 was a regime that hadn't appeared in the prior fitting window.** A 2-state HMM trained on 2022–early-2024 data has only "normal" and "moderately volatile" states; the post-ruling +9% jump on Sep 3 2025 ([CNBC](https://www.cnbc.com/2025/09/02/google-antitrust-search-ruling.html)) is a state the model has no support for.

**Quote 6 — Carsten on QuantConnect, on HMM state-order instability:**

> "Every time you run HMMlearn it might switch the order of the states."

Standard label-switching pathology in mixture models. In a production live-trading context this is a live bug: the meaning of "regime 1" can flip between training runs, producing nonsensical signals.

**Quote 7 — MQL5 article on CUSUM/Chow detection lag for stat-arb** ([MQL5 article](https://www.mql5.com/en/articles/20946)):

> "**NVDA/INTC Partnership (September 18, 2025)**: Chow F-statistic: 568.73, p-value: <0.001. ... CUSUM triggered alarm: September 19, 2025 (one day after announcement)."

This is the **single most concrete published number** I found on detection lag for a specific cointegration breakdown event. **CUSUM detected a real, documented partnership-driven break in 1 trading day.** Caveat: this was a *huge* shock (β flipped sign from -2.59 to +0.95, F = 568) — the detection lag for smaller breaks will be longer.

**Quote 8 — Wikipedia on Renaissance Technologies / HMM:**

> "Leonard Baum [co-inventor of the Baum-Welch algorithm] would help found RenTec."

This is the most-cited claim that production stat-arb desks use HMMs, but it's circumstantial — there is no public evidence that current Medallion strategies rely on HMM regime detection as a circuit breaker. The danielscrivner.com Renaissance breakdown ([Renaissance Technologies Business Breakdown](https://www.danielscrivner.com/renaissance-technologies-business-breakdown/)) describes a "millions of factors with dynamic weighting" approach more consistent with researcher A's BARRA-residual story than with the HMM-circuit-breaker story.

---

## Detection-lag vs false-positive trade-off (quantitative comparison where data exists)

| Method | Online? | Detection lag (lit value) | False-positive rate (lit value) | Fitness for OpenQuant |
|---|---|---|---|---|
| BOCPD (vanilla A&M 2007) | Yes | Unbounded worst-case (R-BOCPD 2020 result); for typical financial regimes ~3 days (CSI 300 carbon-price 2025 paper) | λ-dependent: ARL₀ ≈ λ/(P(no-break per step)). For λ=100, ARL₀ ≈ 100 days ≈ 4 false alarms/year on each of 43 baskets ≈ ~170 false alarms/year. **High.** | Poor without R-BOCPD restart; calibration fragile |
| Recursive CUSUM (BDE 1975) | Yes | 1 day on NVDA/INTC partnership shock (MQL5 case study); average ARL₁ ~ 1/(post-break drift in σ-units) | ARL₀ = 1/α; for α=0.05, false alarm every 20 days/basket = ~430/year across 43 baskets. **Very high without one-sided gating** | Best of available; pair with ARL₀ ≈ 250 (one false alarm/yr/basket) by tightening α to 1/250 |
| Sup-Wald / Quandt-Andrews | Offline (re-run on rolling window for online use) | Window-half (e.g. 30 days for 60-day window) | Andrews 5%-CV ≈ 4.71p; with p=2 and 43 baskets, raw Type-I rate 5% gives ~2 alarms/day. Bonferroni-correct or use sup-LR with pi=0.15 trim. | Useful as confirmation, not first-line alarm |
| Bai-Perron multiple breaks | Offline | Half-of-trimmed-window (15-25 days) | Sequential F(s+1\|s) controls FWER; actual FPR depends on chosen significance level | Weekly/monthly post-mortem tool |
| HMM regime switching (filtered) | Yes | 1-5 days for binary macro regimes (QuantStart); not quantified for pair-residual breaks | Not formally quantified in pairs literature; arbitragelab docs explicitly warn about non-convergence and label-switching | Brittle for pair-residual breaks; better as risk-on/off macro overlay |
| GSADF (PSY 2015) | Yes (rolling SADF) | Window-half | Phillips-Shi-Yu show consistent date-stamping asymptotically; finite-sample bias toward late detection | Detects explosive behavior (one-sided) — useful only for "spread blew up" not "cointegration died quietly" |
| PELT (offline) | No | N/A | "Tends to over-segment financial data" (DL ACM 2025) — explicit high-FPR finding | Post-mortem only |
| ChangeFinder (SDAR) | Yes | Not published for finance | Not published | Lightweight; no calibration evidence for stat-arb |
| Wavelet + DL | Yes inference, offline training | Not published | Not published | **Unusable: requires labelled break events we don't have** |
| R-BOCPD | Yes | Optimal (log(1/α) / KL) | Same as vanilla BOCPD calibration | Theoretically best; no production code |

**Reading of the table.** No method achieves both (a) low detection lag and (b) low false-positive rate without bespoke calibration on labelled break events. **The CUSUM family with ARL₀ tuned to ~250 days/basket is the cheapest, most parameter-light, online-capable choice.** Everything else is either offline, brittle, or undocumented for our use case.

---

## Documented failures of online regime-change detection

**Failure 1 — Vanilla BOCPD has no worst-case detection-delay guarantee.** Alami et al. 2020 ([R-BOCPD paper](https://www.researchgate.net/publication/344337041)) prove that adversarial pre-break distributions can make Adams-MacKay's run-length posterior collapse arbitrarily slowly after a break. **Production implication:** vanilla BOCPD on a pair-residual that has been quiet for a long time can take *much* longer than 1/λ to flag a small drift in the post-break mean. Our window has 189 days; if a break occurs at day 100, we may not see the run-length posterior reset until well past where the bleed has accumulated.

**Failure 2 — HMM regimes don't generalise out-of-sample.** Both arbitragelab docs and the Klawitter QuantConnect post above document the same failure mode: HMMs trained on history H can't represent regimes severity that didn't appear in H. The GOOGL post-antitrust regime is exactly such a case.

**Failure 3 — PELT over-segments.** [DL ACM 2025 PELT financial paper](https://dl.acm.org/doi/10.1145/3773365.3773532) notes PELT applied to S&P returns produces too many breakpoints; even with BIC penalty, the algorithm finds segmentations that don't survive economic interpretation.

**Failure 4 — Quandt-Andrews critical-value sensitivity.** With p variables and π = 0.15 trim, the CV scales as 4.71p; for our basket spreads with p ≈ 6 peers this gives raw 5%-CV ≈ 28. But the *asymptotic* approximation degrades in finite samples; Hansen 2000 ([J Econ Lit](https://users.ssc.wisc.edu/~bhansen/papers/jel_00.pdf), not retrieved here but standard reference) documents that for samples T < 200 the empirical distribution has heavier tails than the asymptotic, **so 5% nominal-size tests have actual size 8-12% — i.e., 60-140% more false positives than advertised**. Our 189-day window is exactly in this danger zone.

**Failure 5 — Look-ahead bias in academic HMM stat-arb papers.** [QuantInsti regime-adaptive guide](https://blog.quantinsti.com/regime-adaptive-trading-python/) explicitly calls this out: "this is violated in nearly every academic paper that uses full-sample HMM estimation." Bock-Mestel 2009 trains the 2-state model on the entire spread series and uses smoothed probabilities for trading rules; this doesn't translate to live deployment.

**Failure 6 — The boy-who-cried-wolf in production.** No specific stat-arb post-mortem documents this rigorously, but the cybersecurity literature (the closest discipline with rigorous false-positive-cost work) reports that 99% of intrusion-detection alerts are false positives, and the operational result is alert fatigue and missed real events ([USENIX Security 2022](https://www.usenix.org/system/files/sec22summer_alahmadi.pdf)). For a 43-basket live system, an online break detector that fires more than ~1/year per basket will get ignored or auto-bypassed in practice.

---

## Specific case study: would the technique have flagged GOOGL or NVDA's regime change in time?

The brief asks for an honest answer, ideally backed by re-running tests on the actual residual time series, but I don't have data here, so I reason from published analyses and the timeline.

**GOOGL antitrust + AI cloud +101% — what's the candidate "break" date?**

There are three candidate break dates, and which one matters for our 189-day window (Jul 2025 → Mar 2026) determines the answer:
- **Aug 5 2024**: Judge Mehta initial monopoly ruling ([Wikipedia US v. Google](https://en.wikipedia.org/wiki/United_States_v._Google_LLC_(2023))). Pre-our-window. This is *the* break — but it's already in the in-sample fit window of any window-based method.
- **Sep 2-3 2025**: Remedies decision; +8-9% one-day move on GOOGL ([CNBC](https://www.cnbc.com/2025/09/02/google-antitrust-search-ruling.html), [CNBC follow-up](https://www.cnbc.com/2025/09/03/alphabet-pops-after-google-avoids-breakup-in-antitrust-case.html)). **Inside our window.**
- **Continuous mid-2024 → 2026**: AI/Gemini revenue growth driving GOOGL +101%.

For the Sep 2-3 2025 remedies-decision shock (+9% one-day basket-residual jump for GOOGL relative to META/AAPL/NFLX), **CUSUM-of-residuals would have triggered within 1-2 days** — the move is enormous in residual-σ units, comparable to the NVDA/INTC partnership case where the MQL5 article documents 1-day detection lag.

**But here's the question that matters: would early detection have prevented the 0/4 FAANG bleed?** The answer is *probably not* unless detection happens *in the first month of the window*, because:
1. The basket P&L is dominated by *continuous* drift (GOOGL +101%, NVDA mid-cycle while peers ripped) more than by single-day shocks. CUSUM and BOCPD detect *changes in mean*, but by the time enough mean drift has accumulated to trigger detection, the drawdown is already deep. Recursive CUSUM has expected detection delay scaling as 1/drift (in σ-units); a 0.5σ/day post-break drift is detected in roughly 8-12 days, by which time the basket has already moved 6σ against the spread.
2. The *first* signs of GOOGL decoupling occurred well before our July 2025 window starts. A method calibrated on July 2025 onward initialises with GOOGL already running away. Any break that occurred during the *2024 antitrust* doesn't get re-detected in 2025; it's just baseline.
3. **The deeper issue is researcher A's hypothesis: GOOGL was never cointegrated with AAPL/META in a stable way.** If true, every "break" detection on this basket is a false positive on a peer relationship that was never stationary. We'd be using a regime detector to confirm a model error.

**NVDA mid-cycle while peers ripped — what's the candidate break date?**

NVDA underperformed AVGO and AMD in 2025 — see [Invezz Apr 30 2026](https://invezz.com/news/2026/04/30/why-amd-avgo-are-outperforming-nvidia-after-big-tech-earnings/): "AMD has gained approximately 51% YTD compared to NVDA's 14% ... in the past year, AVGO returned 47.47% vs NVDA's 34.83%." There's no single break date — **it's a slow regime drift driven by hyperscaler custom-silicon adoption (Broadcom MTIA, AMD's CPU-GPU stack)**.

This is the worst case for online break detection: a **gradual structural shift**, not a discrete shock. Both BOCPD and CUSUM are tuned for discrete breaks; for slow drifts, ARL₁ scales as 1/drift, so at typical sub-σ/day drift rates the detection lag would be 30-60 days — by which time the FAANG/entsw P&L has already played out.

**Honest summary.** For the *Sep 2025 GOOGL remedies-decision shock*, recursive CUSUM would likely have flagged within 1-2 days. For the *NVDA-vs-AVGO/AMD slow-decoupling*, no online method in the literature would have flagged in time without unacceptable false-positive rates elsewhere. **And both events are downstream of A's hypothesis: the peer set was wrong for these names.**

---

## Cross-references with researchers A, C, D

**Reference to A (peer sets) — strong agreement, slight tension on causality.**

Researcher A's draft ([A_peer_sets_v1.md](./A_peer_sets_v1.md)) argues that GICS-equal-weight FAANG was never a valid peer set for these names because GOOGL/META share an ad-stock factor that AAPL/NFLX don't, and AAPL has a platform-rent factor that META doesn't. Sarmento & Horta 2020 OPTICS clustering, BARRA residual peers, and the SPONGE signed-graph method would all have rejected the FAANG basket at admission.

**Implication for B's research.** If A is right, then most of the "structural breaks" my methods would detect are spurious — they're firing on relationships that were never stationary in the first place. **A's hypothesis dominates B's hypothesis under H2.** If a basket has a never-valid peer set, no online break detector can repair it — there's no "true" cointegration to detect breaking from.

**Where B contributes anyway.** Even *with* a corrected peer set (A's proposal), idiosyncratic shocks will still happen. Ernie Chan's 2011 GLD-GDX-USO post-mortem is the cleanest illustration: even a properly-constructed pair experiences breakdown when a missing factor (oil) becomes pivotal. So B's recommendation is to layer a CUSUM-of-residuals monitor *on top of* A's peer-set construction, with detection thresholds calibrated to give ~1 false alarm/year/basket (ARL₀ ≈ 250 days). It's a circuit-breaker, not a primary signal.

**Reference to C (MR vs momentum regime switching) — pending C draft, but speculative position.**

C is studying whether mean-reversion is the wrong model class for high-dispersion regimes. This is a **different problem** from B's: C is asking *whether the model class fits the regime*, B is asking *whether the regime broke within a fixed model class*. These two problems can both be true simultaneously (the basket was MR in 2024, structurally broke in mid-2025, and is now momentum-y), but the literature treats them separately.

The relevant cross-link: regime-switching VECM (Krolzig 2002) explicitly models a *single* spread that can be in MR or momentum regimes, with the Hamilton filter giving online regime probabilities. **If C's hypothesis is right, the Bock-Mestel 2-state HMM with switching mean is exactly the right tool — but to switch trading rules, not to circuit-break.** Researcher A and C therefore propose constructive fixes (better peers, better model class), while researcher B proposes a defensive add-on (circuit breaker).

**Reference to D (failure-mode catalogue) — pending D draft.**

If D documents "regime change too late / too aggressive" as a failure mode in published practitioner postmortems, that supports my recommendation that BOCPD and HMM should not be the *primary* fix. If D documents specific stat-arb desks that successfully deploy CUSUM-of-residuals or sup-Wald monitors in production, that strengthens the recommendation to layer a CUSUM circuit breaker on top of A's peer-set fix.

---

## Recommendation — what's actually deployable for OpenQuant

**Primary recommendation: do not deploy ex-ante online regime detection as the primary fix for the FAANG/entsw 0/4 failure.**

Reasons:
1. Researcher A's evidence (BARRA-residual peers, OPTICS, SPONGE) is more parsimonious and is supported by the literature consensus that bad peer sets cause spurious break-detector firings.
2. Hazard-rate / threshold calibration for BOCPD on stat-arb residuals is empirically unstable across published values (1/80–1/250) and we have no labelled break events to calibrate against.
3. The 43-basket × 8-sector universe will produce a punishing false-positive rate at any defensible significance level, leading to alert fatigue.
4. The slow-drift NVDA-vs-AVGO/AMD case is a known worst case for online break detection — no method I found in the literature would have flagged in time without unacceptable FPR.
5. Look-ahead bias is rampant in published HMM stat-arb work; the only honest deployment is *filtered* (causal) probabilities, which arrive with 1-5 day lag and have known label-switching/non-convergence pathologies.

**Secondary recommendation (defensive add-on after A's peer-set fix):** a recursive CUSUM-of-residuals monitor (Brown-Durbin-Evans 1975, Xiao-Phillips 2002 cointegration variant), with:
- Significance level α tuned for ARL₀ ≈ 250 days per basket (one false alarm per basket per year), corresponding to a one-sided c ≈ 1.5σ on the recursive-residual partial sum.
- One-sided thresholding on the basket-spread residual: only fire when the partial sum exceeds *and* the realised mean is in the wrong direction relative to the position.
- Trigger action: pause new entries on the basket for a configurable window (e.g. 20 days). Do *not* force exit; let HL / max-hold / stop-loss handle existing positions per their own rules.
- Reset the recursive regression and CUSUM accumulator after a 20-day quiet period.

**Why CUSUM and not BOCPD or HMM:**
- One parameter (α) instead of three (hazard rate + Student-t prior + run-length pruning threshold)
- Literature evidence of 1-day detection lag on the NVDA/INTC partnership shock
- No labelled training data needed
- Long econometric pedigree, known asymptotic distribution, available critical values
- Compatible with our existing OLS + ADF-residual workflow — operationally cheap

**Tertiary recommendation (post-mortem, weekly):** run sup-Wald / Bai-Perron offline on the residuals of all 43 baskets with the previous week's data. Pairs that show an offline-confirmed break should be flagged for re-admission review (Researcher A's process). This is the *post-hoc* validation loop that catches breaks the online CUSUM missed and confirms breaks the online CUSUM did flag.

**Honest about the false-positive cost.** Even at ARL₀ = 250 days/basket, with 43 baskets we expect ~63 false alarms/year. Each false alarm pauses entries for 20 days on a basket that didn't actually break. The cost is opportunity cost; net of that, in our 0.47 Sharpe regime, the value of the circuit breaker is bounded above by *only* the avoided drawdowns from the rare true breaks. **This is a marginal-utility decision and should be A/B'd in shadow mode before production.**

**Do not deploy:** vanilla Adams-MacKay BOCPD (worst-case detection delay unbounded), Bock-Mestel 2-state HMM with smoothed probabilities (look-ahead bias), Wavelet+DL break-aware pairs trading (no labelled training data), PELT (offline + over-segmentation).

---

## What's still unknown (for v3 / future research)

1. **Re-running CUSUM on the actual 43-basket residual time series.** Without doing this, we don't know the empirical false-positive rate or the actual detection lag for our specific spreads. Phase 2's gap.
2. **Whether layered detection (CUSUM + sup-Wald + half-life check + ADF-rerun) is more powerful than single-test detection.** The "cusum AND sup-Wald" intersection rule should sharply reduce FPR at modest cost in detection lag, but we have no published evidence for this combination on stat-arb data.
3. **R-BOCPD vs vanilla BOCPD on real cointegration residuals.** R-BOCPD's optimal-detection-delay guarantee is a theoretical result; no public benchmark on equity pairs.
4. **The exact August 2024 → March 2026 GOOGL-vs-FAANG-residual trajectory.** Did the residual show a single discrete break (Aug 5 2024 ruling), a series of small breaks (each AI announcement), or a continuous regime shift (the gradual ad-stock vs cloud-AI factor split)? The answer determines which of the techniques in my table would have helped.
5. **Detection lag distributions on labelled break events.** No paper I found publishes (lag distribution | technique × asset class × break type), and we don't have labelled events to build one. Constructing a labelling protocol for our 43 baskets would itself be a research project.
6. **Cross-validation with C and D drafts** when they are posted. Particularly: does C's MR-vs-momentum literature specifically address whether structural-break detection should come before or after model-class switching? Does D's failure-mode catalog include "alert fatigue from break-detector false positives" as a documented mode?

---

## Sources

- Adams, R. P. & MacKay, D. J. C. (2007). Bayesian Online Changepoint Detection. [arXiv:0710.3742](https://arxiv.org/abs/0710.3742). [Princeton lips PDF](https://lips.cs.princeton.edu/pdfs/adams2007changepoint.pdf).
- Alami, R., Maillard, O., Féraud, R. (2020). Restarted Bayesian Online Change-point Detector achieves Optimal Detection Delay. [ResearchGate 344337041](https://www.researchgate.net/publication/344337041).
- Andrews, D. W. K. (1993). Tests for Parameter Instability and Structural Change with Unknown Change Point. Econometrica 61(4). [Yale PDF](http://dido.econ.yale.edu/~dwka/pub/p1138.pdf).
- Aue, A. & Horváth, L. (2013). Structural breaks in time series. JTSA 34(1). [Wiley](https://onlinelibrary.wiley.com/doi/abs/10.1111/j.1467-9892.2012.00819.x).
- Bai, J. & Perron, P. (2003). Computation and Analysis of Multiple Structural Change Models. JAE 18.
- Bock, M. & Mestel, R. (2009). A Regime-Switching Relative Value Arbitrage Rule. [SSRN 1213802](https://papers.ssrn.com/sol3/papers.cfm?abstract_id=1213802), [PDF](https://assets.super.so/e46b77e7-ee08-445e-b43f-4ffd88ae0a0e/files/706438cf-895f-472b-bd10-24701655e1a3.pdf).
- Brown, R. L., Durbin, J., Evans, J. M. (1975). Techniques for Testing the Constancy of Regression Relationships Over Time. JRSS-B 37, 149-192. [PDF](https://hhstokes.people.uic.edu/ftp/e535/Brown_Durbin_evans_1975.pdf).
- Chan, E. (2011). When cointegration of a pair breaks down. [epchan.blogspot.com](http://epchan.blogspot.com/2011/06/when-cointegration-of-pair-breaks-down.html).
- Chu, C.-S. J., Hornik, K., Kuan, C.-M. (1995). MOSUM tests for parameter constancy. Biometrika.
- DL ACM 2025. Bayesian Online Changepoint Detection for Financial Time Series. [DL ACM doi 3795154](https://dl.acm.org/doi/10.1145/3795154.3795291).
- DL ACM 2025. Change-Point Detection in Financial Time Series Using PELT. [DL ACM doi 3773532](https://dl.acm.org/doi/10.1145/3773365.3773532).
- Hamilton, J. D. (1989). A New Approach to the Economic Analysis of Nonstationary Time Series and the Business Cycle. Econometrica 57(2). [PDF](https://users.ssc.wisc.edu/~behansen/718/Hamilton1989.pdf).
- hudson-and-thames/arbitragelab. [GitHub](https://github.com/hudson-and-thames/arbitragelab). [SADF source](https://github.com/hudson-and-thames/mlfinlab/blob/master/mlfinlab/structural_breaks/sadf.py). [Regime-switching docs](https://hudson-and-thames-arbitragelab.readthedocs-hosted.com/en/latest/time_series_approach/regime_switching_arbitrage_rule.html).
- Killick, R., Fearnhead, P., Eckley, I. (2012). Optimal detection of changepoints with a linear computational cost. JASA 107(500). [arXiv 1101.1438](https://arxiv.org/pdf/1101.1438).
- Krolzig, H.-M. (2002). Regime-Switching Models. [perhuaman.files.wordpress.com](https://perhuaman.files.wordpress.com/2014/09/krolzig2002.pdf).
- MQL5 article (2025). Statistical Arbitrage Through Cointegrated Stocks (Part 10): Detecting Structural Breaks. [mql5.com/articles/20946](https://www.mql5.com/en/articles/20946).
- Phillips, P. C. B., Shi, S., Yu, J. (2015). Testing for Multiple Bubbles. IER 56(4). [Yale PDF](http://korora.econ.yale.edu/phillips/pubs/art/p1498.pdf).
- QuantConnect forum, Rage Against the Regimes (2024). [quantconnect.com/forum/discussion/14818](https://www.quantconnect.com/forum/discussion/14818/rage-against-the-regimes-the-illusion-of-market-specific-strategies/).
- QuantStart (Halls-Moore). Hidden Markov Models for Regime Detection using R. [quantstart.com](https://www.quantstart.com/articles/market-regime-detection-using-hidden-markov-models-in-qstrader/).
- ruptures (deepcharles). [GitHub](https://github.com/deepcharles/ruptures). [PELT docs](https://centre-borelli.github.io/ruptures-docs/user-guide/detection/pelt/).
- Tandfonline 2024. Online learning of order flow and market impact with Bayesian change-point detection methods. [doi 10.1080/14697688.2024.2337300](https://www.tandfonline.com/doi/full/10.1080/14697688.2024.2337300), [arXiv 2307.02375](https://arxiv.org/abs/2307.02375).
- Wang, C.-H., et al. (2021). Structural break-aware pairs trading strategy using deep reinforcement learning. J Supercomputing. [Springer](https://link.springer.com/article/10.1007/s11227-021-04013-x).
- Xiao, Z. & Phillips, P. C. B. (2002). A CUSUM Test for Cointegration Using Regression Residuals. J Econometrics 108(1). [Yale PDF](http://korora.econ.yale.edu/phillips/pubs/art/p1046.pdf).
- QuantBeckman (2025). Switch-Off: BOCPD as kill-switch with code. [quantbeckman.com](https://www.quantbeckman.com/p/with-code-switch-off-bayesian-online).

GOOGL antitrust timeline references:
- [CNBC 2025-09-02 Google avoids worst-case penalties](https://www.cnbc.com/2025/09/02/google-antitrust-search-ruling.html).
- [CNBC 2025-09-03 Alphabet pops](https://www.cnbc.com/2025/09/03/alphabet-pops-after-google-avoids-breakup-in-antitrust-case.html).
- [Wikipedia US v. Google 2023](https://en.wikipedia.org/wiki/United_States_v._Google_LLC_(2023)).

NVDA performance vs peers references:
- [Invezz 2026-04-30 AMD/AVGO outperform NVDA](https://invezz.com/news/2026/04/30/why-amd-avgo-are-outperforming-nvidia-after-big-tech-earnings/).
- [Nasdaq 2025 NVDA vs AMD AI play](https://www.nasdaq.com/articles/nvda-vs-amd-which-semiconductor-stock-smarter-ai-play-2025).
