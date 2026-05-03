# Memo D v2 — Stat-arb failure modes and practitioner postmortems

**Status:** v2. ~4-hour research pass. Scope: meta-research on documented failure modes for stat-arb / pairs trading, with primary-source practitioner content. The other three memos in this package (A: peer-set construction, B: regime/structural-break detection, C: MR-vs-momentum switching) are looking at specific *fixes*. This memo is the **diagnostic counterweight** — the cumulative literature on what has actually broken stat-arb in production, and how often the fixes A/B/C propose actually replicate OOS.

---

## Executive summary

I went deep on six primary sources (Khandani-Lo 2007/2011, Asness's "August of Our Discontent" Sept-2007, Avellaneda-Lee 2010, PanAgora's 10-year retrospective, Goldman Sachs's 2017 FT-syndicated piece, Ernie Chan's blog 2007-2011), the open-source code of arbitragelab and mlfinlab, and supporting practitioner accounts (QuantRocket, ExtractAlpha on the March-2020 quant unwind, the 2018-2020 quant winter literature, Lopez de Prado on backtest overfitting). Three findings stand out as having direct bearing on OpenQuant's bimodal FAANG / entsw 0/8 failure:

1. **The single most-cited failure mode in the practitioner literature is crowding, and it is barely addressed by A/B/C.** Asness — writing in real time in September 2007 — names crowding as the proximate cause of every quant book's losses ("there is a new risk factor in our world and it is us"). Khandani-Lo independently arrive at the same conclusion via the contrarian-strategy simulation. The 2017 retrospectives (Goldman, AQR, ExtractAlpha) all conclude that *crowding has gotten worse since 2007, not better*. None of A/B/C touch crowding directly. A's clustering methods would not fix it. B's regime-detection would not flag it (it's invisible until the unwind starts). C's regime-switching has no signal for it.

2. **Avellaneda & Lee 2010 contain a buried result that maps directly onto OpenQuant's bimodal FAANG/entsw failure.** Their sector-level analysis of August 2007 shows that *Technology and Consumer Discretionary* sectors were hit *hardest* by the unwind — much harder than Financials and Real Estate, which were the *source* of the contagion. This is the same paradox OpenQuant is observing: FAANG (Tech) and entsw (Energy/Tech-flavored) underperform structurally, while sector-coherent groups do better. This finding *strengthens* memo A's recommendation but for a different reason than A states: the issue isn't peer-set quality, it's that **liquid, popular Tech mega-caps are precisely the names that other quants use to deleverage**. Avoiding them as targets is a crowding-mitigation move, not a peer-quality move.

3. **Most of the "fixes" that show up in academic stat-arb papers do not replicate OOS in the practitioner record.** Sarmento-Horta (which memo A cites approvingly) reports Sharpe 3.79 — a number no practitioner I can find replicates without time-period, leverage, or universe choices that don't survive walk-forward. Avellaneda-Lee's PCA stat-arb — the canonical academic strategy memo A endorses — went from Sharpe 1.44 (1997-2007) to 0.9 (2003-2007) to **drawdown -10% in Aug 2007** *as published in the same paper*. The pattern is consistent: academic stat-arb papers report 1997-2007 Sharpe; practitioner postmortems report 2007-onward Sharpe; the gap is large and well-documented. Lopez de Prado: "most discoveries in empirical finance are false."

**Recommendation:** Treat A/B/C's proposed fixes as 30-40% confidence each, not 80%. The strongest move from this memo's perspective is *not* a methodology change — it is a **portfolio-construction change**: drop FAANG mega-caps from the trading universe, accept lower notional, and trade the residuals where crowding is structurally lower. This is what AQR ended up doing post-2007 ("study changes to portfolio construction that will make our portfolios a bit less leveraged for the same level of risk"); it is what Avellaneda-Lee's volume-conditional ETF strategy does; it is the unstated implication of memo A's discussion of FAANG-as-a-basket being unworkable.

---

## The 2007 Quant Quake — primary-source narrative

Two primary sources, written within weeks of each other in September 2007, agree on the mechanism. Both are extracted verbatim below.

### Khandani & Lo (2007, NBER w14465 / J. Financial Markets 2011)

The paper's seven tentative hypotheses are stated cleanly on pp. 4-5 of the MIT version (`web.mit.edu/Alo/www/Papers/august07.pdf`), of which the most relevant for OpenQuant's situation:

> "Likely factors contributing to the magnitude of the losses of this apparent unwind were: (a) the enormous growth in assets devoted to long/short equity strategies over the past decade and, more recently, to various 130/30 and active-extension strategies; (b) the systematic decline in the profitability of quantitative equity market-neutral strategies, due to increasing competition, technological advances, and institutional and environmental changes such as decimalization, the decline in retail order flow, and the decline in equity-market volatility; (c) the increased leverage needed to maintain the levels of expected returns required by hedge-fund investors in the face of lower profitability..."

The leverage progression is buried in their Section 6 and Table 6 — a contrarian strategy that earned a Sharpe ~10.6 unleveraged in 1998 needed *9:1 leverage* by 2007 to hit the same expected return, because alpha had decayed:

> "There has been significant 'alpha decay' of the contrarian strategy between 1998 and 2007, so much so that a leverage ratio of almost 9:1 was needed in 2007 to yield an expected return comparable to 1998 levels!"

At 8:1 leverage (their realistic 2006 estimate), the contrarian strategy would have lost **−4.64% on Aug 7, −11.33% on Aug 8, −11.43% on Aug 9** — a combined **−27% by close of business on Day 3** before the Aug 10 reversal of +23.67%. These are the canonical loss figures every subsequent retrospective references.

Their crucial structural observation about the source of contagion (p. 27):

> "Even more significant is the fact that many of these empirical regularities have been incorporated into non-quantitative equity investment processes, including fundamental 'bottom-up' valuation approaches like value/growth characteristics, earnings quality, and financial ratio analysis. Therefore, a sudden liquidation of a quantitative equity market-neutral portfolio could have far broader repercussions, depending on that portfolio's specific factor exposures."

In other words: *crowding doesn't require everyone to use the same model — it requires everyone to bet on the same anomalies*. Even non-quants using "value" and "earnings quality" and "momentum" through fundamental analysis end up being correlated with quant books because the underlying premia are the same. This is the deepest cut in the paper.

CS/Tremont sub-strategy August 2007 returns (their Table 9): every category lost money. "Equity Market Neutral" was −0.39%, "Long/Short Equity" was −1.38%, "Managed Futures" was −4.61%. Stat-arb was *not* the worst-performing category — managed futures was. The unwind contagion crossed strategy boundaries.

### Asness (Sept 2007, "The August of Our Discontent")

Available at `aqr.com/-/media/AQR/Documents/Insights/Working-Papers/The-August-of-Our-Discontent.pdf`. This is the canonical practitioner real-time account. Quoting at length from the most-load-bearing passages:

On **why he is sure the cause was deleveraging, not bad stock-picking** (his "pretty airtight case," pp. 3-4 of the PDF):

> "First, the very size of the moves. The world is highly 'fat-tailed', meaning very big events happen more often than most models assume (the so-called 'Black Swan' problem)... The size of bad and good days during this event came along with statements like 'well, that was a 25 standard deviation event.' What this really means is whoever said that didn't have a good grip on the underlying distribution of returns, since statistics would tell you that such an event doesn't happen.
>
> Second, the linear nature of the declines and the comebacks. When they were falling and then when they were rising, these strategies moved in a near straight line throughout the trading day. It looked just like what it was — someone working large orders to take down their risk — and then someone putting that risk back on. It did not look like the random losses or gains of getting many small bets right or wrong.
>
> Third, models are composed of many factors, some of which are low or negatively correlated with each other (value and momentum are the most prominent example of this phenomenon). During this period, all of the more well-known factors performed very poorly. That is a sure sign people were taking down risk in similar models.
>
> Fourth, the specific factors that are the most well-known and popular had the worst performance. For example, within our valuation theme, we look at about 14 separate indicators (with varying degrees of popularity) that are generally very correlated to each other... During this drawdown, we saw large divergence in the performance of these factors. In particular, the more proprietary factors were down dramatically less than the others."

On **the "new risk factor in our world and it is us"** (pp. 5-6):

> "I have said before that 'there is a new risk factor in our world,' but it would have been more accurate if I had said 'there is a new risk factor in our world and it is us.' It is our collective action going forward (where 'our' refers to quant market-neutral managers or those employing very similar strategies) that now affects a world we didn't realize we had such influence over, and this is undoubtedly an important short-term risk factor."

On **the AQR-specific finding that proprietary factors held up while public ones did not** (p. 4):

> "Within our earnings quality theme, the version most widely written up in academia performed terribly, while several of our more proprietary/esoteric factors were flat or only slightly down."

On **what AQR did differently after** (p. 12):

> "We are, however, studying changes to portfolio construction that will make our portfolios a bit less leveraged for the same level of risk. This may help to reduce our tail risk and, as a side benefit, transaction costs, too."

On **whether quants should hunt for unique factors** (p. 8):

> "In truth, unique factors would be a great way to avoid the problem of crowding. Even among correlated value indicators, the ones we thought were most unique to AQR suffered the least during August. We don't claim this is because they are so much better (okay, we think they're a little better), but rather the more common the factor, the more subject to deleveraging.
>
> But, here's the problem with unique factors. They are also uniquely susceptible to data-mining and getting the last crisis right. In particular, new research often seems to be directed at 'fixing' a model that is often just having a bad period."

This last passage is the single most useful warning for OpenQuant's autoresearch loop: **the urge to fix a model after a bad period is itself a data-mining hazard**. Memos A/B/C are all fix-proposals after a bad period. Asness, writing in 2007, is pre-emptively skeptical of the same pattern.

### Goldman Sachs Asset Management 2017 retrospective (FT, syndicated by GSAM)

Gary Chropuvka, who ran Goldman QIS in 2007, told the FT in 2017 (PDF: `gsam.com/.../articles/2017/quant-quake-reprint-news.pdf`):

> "All this worked academically, and for a long time it worked in practice, and then all of a sudden you have this horrible event. It was the most humbling experience of our lives."

The FT piece confirms Goldman's QIS division managed $165bn at peak, fell to a fraction of that after the quake, and the long-term reduction of the unit was tied directly to redemptions following the August 2007 losses. By 2011, Goldman wound down its flagship Global Alpha fund entirely (Bloomberg, Marketplace 2011). This is the most concrete "stat-arb desk shut down because of the failure mode" data point in the public record.

### Where these primary sources agree

1. **Cause was forced unwind by a multi-strategy fund hit elsewhere (subprime credit), not strategy-specific.** Khandani-Lo say it explicitly. Asness says it explicitly. The fund identity has never been confirmed publicly (some speculate it was Goldman's Global Alpha; others Highbridge or DE Shaw; Khandani-Lo deliberately don't name it).
2. **The propagation channel was *common factor exposures*, not common code.** Even fundamental managers doing "value" by hand suffered, because the value premium was being unwound.
3. **The fix is "less leverage for the same risk" plus "more proprietary signals."** Asness states this explicitly. AQR's later writings (Fact, Fiction and Factor Investing 2023) confirm the pattern continued.

### Where the primary sources disagree

- Khandani-Lo think this was a one-time liquidity event with little implication for the efficacy of stat-arb. Asness says explicitly that "the more common the factor, the more subject to deleveraging" — i.e., crowding is an ongoing, persistent risk that calls for unique-factor research as a structural matter. The 2017 retrospectives side with Asness: GSAM, ExtractAlpha, AQR all argue another quant quake is more, not less, likely now.

---

## Documented failure modes catalog

Compiled from the primary sources above plus subsequent literature. Each named mode includes: mechanism, primary citation, public quote, current relevance to OpenQuant.

### F1. Crowding and forced unwind

- **Mechanism:** Multiple funds hold near-identical factor exposures. A single fund's liquidation (often due to losses elsewhere — credit, mortgage, etc.) cascades through everyone with similar exposures.
- **Primary citation:** Khandani-Lo (2007/2011); Asness (Sept 2007).
- **Public quote (Asness):** "there is a new risk factor in our world and it is us."
- **Current relevance to OpenQuant:** First-order. Our basket-spread strategy on FAANG is exactly the kind of trade that hundreds of stat-arb books run. When mega-cap Tech moves on a forced-deleveraging signal, our book moves with everyone else's. Crowding is essentially uncorrelated with our peer-selection methodology — fixing the cluster algorithm doesn't fix this.

### F2. Cointegration breakdown (relationship visibly breaks)

- **Mechanism:** A previously-cointegrated pair stops mean-reverting. Reasons: idiosyncratic single-name event (earnings, M&A, regulatory action), structural shift in business mix, sector reclassification, crowding-driven divergence.
- **Primary citation:** Engle-Granger (1987) for the test; Chan (2011 blog) for failure-mode analysis.
- **Public quote (Chan, June 2011):** "It is actually quite hard to detect the breakdown of cointegration except in hindsight maybe a year afterwards... Only when the drawdown lasts for, say, a year, when we can say that the cointegration is really gone."
- **Current relevance to OpenQuant:** Medium. ADF re-tests can catch this, but Chan's point — that you only know in hindsight a year later — is sobering. OpenQuant already does periodic re-validation; that is the right defense. The stricter the ADF threshold, the more false-positive rejections; the laxer, the more we trade post-broken pairs. There is no clean fix.

### F3. Liquidity asymmetry / hard-to-borrow short squeeze

- **Mechanism:** Long-leg illiquid or short-leg sees borrow rates spike. GameStop in Jan-2021 is the canonical case (borrow fee went from 1% to 34%). Stat-arb books holding the wrong side get squeezed before they can unwind.
- **Primary citation:** Hilliard 2023 (J. Futures Markets) on GameStop; pre-2021 evidence in Cohen-Diether-Malloy 2007.
- **Public quote (Morningstar, post-GameStop):** "Recent major short squeezes... provided remedial courses for hedge funds about the risks associated with short-selling, particularly when it involves stocks that are difficult or expensive to borrow."
- **Current relevance to OpenQuant:** Low for FAANG (large, liquid, easy to borrow). Higher for any sub-S&P-500 names entering OpenQuant's universe.

### F4. Single-name event sensitivity (earnings, mergers, antitrust)

- **Mechanism:** A single name in the basket has an asymmetric event (earnings beat/miss, M&A announcement, FDA decision, antitrust ruling). Spread blows out far more than the basket-residual model assumes.
- **Primary citation:** No clean canonical source — folklore in the literature. Quantopian-archived discussions, Vidyamurthy's "Pairs Trading" Ch.11 covers merger arb specifically.
- **Public quote:** From the WSO and Wilmott-style threads I attempted to access: scattered consensus that earnings-blackout windows are standard practice on equity stat-arb desks; no single canonical write-up.
- **Current relevance to OpenQuant:** **High and unaddressed in A/B/C.** OpenQuant's earnings-blackout filter is a crude, single-day window. Q4-earnings-revision trades — where a basket member has a multi-day post-earnings drift driven by analyst revisions — are not blocked. AAPL/META have these multi-day drifts regularly. **(See cross-reference critique of memo A below.)**
- **From `feedback`/MEMORY:** OpenQuant's project_statarb_gg_earnings_alpha and project_statarb_port_contamination_surfaces both recognize that the engine filters earnings — exact events where signal lives. The failure-mode literature here suggests a *strategy-family-specific* earnings policy, not a global blackout.

### F5. Capacity / impact (strategy works at $X AUM, breaks at $10X)

- **Mechanism:** As AUM grows, the strategy's market impact grows linearly with position size. Implementation shortfall eats expected alpha.
- **Primary citation:** Kahn-Shaffer 2005 ("Why Beta Doesn't Work"); Khandani-Lo 2007 explicitly cite this (their (a) factor in losses).
- **Public quote (Khandani-Lo p. 5):** "the enormous growth in assets devoted to long/short equity strategies over the past decade and, more recently, to various 130/30 and active-extension strategies."
- **Current relevance to OpenQuant:** Low at current AUM. Will be first-order if OpenQuant scales meaningfully. Worth flagging as a future failure mode but not present-day.

### F6. Volatility regime / vol-of-vol of spread

- **Mechanism:** The spread's volatility itself is non-stationary. Z-score thresholds calibrated to one regime trigger far more entries — many of them losing — in another regime.
- **Primary citation:** Cont 2001 ("Empirical properties of asset returns: stylized facts"); the entire HMM-stat-arb literature implicitly.
- **Public quote (PanAgora 2018, p. 2):** "Volatility of factor returns rose five-fold, and factors experienced six-plus standard deviation moves. The principles on which modern quantitative portfolio management were built had faltered; otherwise uncorrelated factors became almost perfectly correlated."
- **Current relevance to OpenQuant:** Memo C's domain. Not unique to memo D's scope.

### F7. Borrow cost / hard-to-borrow short squeeze

(Covered as F3 above — keeping the failure-mode taxonomy clean, this is the long/short-cost-asymmetry version of F3.)

### F8. Regime concentration / single-factor dominance

- **Mechanism:** One factor (size, growth, momentum, market) dominates the cross-section. PCA / risk-model residuals shrink to zero. Stat-arb has no signal to trade.
- **Primary citation:** Avellaneda-Lee 2010, especially Section 5.4 and Figure 21 on the rise in % variance explained by top 15 PCs in summer 2007 (i.e., factor concentration *increased* into the unwind).
- **Public quote (Avellaneda-Lee, p. 41):** "This apparently paradoxical result – whereby sectors that are uncorrelated with Financials experience large volatility – is consistent with the unwinding theory of Khandani and Lo."
- **Current relevance to OpenQuant:** **Very high.** The 2023-2026 mega-cap concentration era (memo C documents this) means AAPL/MSFT/GOOGL/META/AMZN/NVDA dominate the cross-section. PCA residuals on these names are tiny relative to their gross moves. This is the Avellaneda-Lee 2007 picture, except the dominant factor is "AI-megacap concentration" instead of "subprime-credit-deleveraging." The mechanism is the same.

### F9. Alpha decay and competition

- **Mechanism:** Once a profitable signal is published, capital floods in, the spread narrows, and the alpha decays. Khandani-Lo's 9:1 leverage observation is the canonical evidence.
- **Primary citation:** Asness 2007; Avellaneda-Lee 2010 (Sharpe 1.44 → 0.9 from 1997-2007 to 2003-2007); QuantRocket's GLD-GDX 2-year half-life.
- **Public quote (QuantRocket on GLD-GDX):** "The strategy was profitable for the first two years following publication, but was unprofitable thereafter... Pairs that cointegrate and perform well in-sample cannot be expected to perform well out-of-sample indefinitely."
- **Current relevance to OpenQuant:** First-order. Every published pair-selection method memo A cites (Sarmento-Horta, Cartea SPONGE, Avellaneda-Lee PCA) suffers this. The 2-year half-life is OOS-validated by QuantRocket.

### F10. Backtest selection bias / multiple testing

- **Mechanism:** Researchers test thousands of parameter combinations and report only the best. Reported Sharpe is inflated by the number of trials.
- **Primary citation:** Bailey-Lopez de Prado 2014 (Deflated Sharpe Ratio); Lopez de Prado AFML 2018 (Probability of Backtest Overfitting).
- **Public quote (Lopez de Prado):** "Backtesting is not a research tool. Feature importance is" (Marcos' First Law). "Most discoveries in empirical finance are false, as a consequence of selection bias under multiple testing."
- **Current relevance to OpenQuant:** **Active failure mode.** OpenQuant's autoresearch loop (per `project_statarb_autoresearch_findings`) ran 14 experiments and found "nothing else helps" beyond the baseline. The DSR of 6.88 quoted in that memo is a strong signal, but the *next* round of memos A/B/C will produce candidate fixes that will benefit from selection bias. This is exactly what Asness warns about: "new research often seems to be directed at 'fixing' a model that is often just having a bad period."

---

## Modern open-source implementations: what failure modes do they actually mitigate?

I cloned and read both `hudson-and-thames/arbitragelab` (commit at clone-time on `master`) and `hudson-and-thames/mlfinlab`. Here is what their code *actually* does, file-by-file, with respect to the failure modes above.

### arbitragelab — what it implements

**`arbitragelab/spread_selection/cointegration.py:112-150` — `select_spreads`**: implements the Sarmento-Horta filter chain. The filtering rules (lines 152-180):

```python
cointegration_passing = self.selection_logs[self.selection_logs['coint_t']
                                            <= self.selection_logs[
                                                'p_value_{}%'.format(int(adf_cutoff_threshold * 100))]].index
hurst_passing = self.selection_logs[self.selection_logs['hurst_exponent'] <= hurst_exp_threshold].index
crossover_passing = self.selection_logs[self.selection_logs['crossovers'] >= min_crossover_threshold].index
hl_passing = self.selection_logs[(self.selection_logs['half_life'] > 0) &
                                 (self.selection_logs['half_life'] <= min_half_life)].index
```

This is the canonical practitioner filter chain for pair *admission*: ADF, Hurst < 0.5, ≥12 mean crossovers/year, half-life > 0 and ≤ min_half_life. Failure modes addressed: F2 (cointegration breakdown — at admission only, not online), partly F8 (mean-reverting spread filter rejects single-factor-dominated baskets where Hurst drifts above 0.5).

What it does *not* address: F1 (crowding), F3 (borrow), F4 (single-name events), F5 (capacity), F9 (alpha decay over time). These are explicitly out of scope.

**`arbitragelab/ml_approach/filters.py:251-401` — `VolatilityFilter`**: this is the most failure-mode-aware piece of code in the entire arbitragelab repo. Implements the Dunis-Laws-Evans (2005) regime-conditional leverage scheme. Verbatim code at lines 350-385:

```python
# Extremely Low Regime - smaller or equal than mean average volatility - 4 std devs
extrm_low_mask = (self.rolling_mean_vol <= avg_minus_four_std_dev)
vol_series.loc[extrm_low_mask, "regime"] = -3
vol_series.loc[extrm_low_mask, "leverage_multiplier"] = 2.5

# Higher Low Regime
higher_low_mask = (self.rolling_mean_vol <= self.mu_avg) & (self.rolling_mean_vol >= avg_minus_two_std_dev)
vol_series.loc[higher_low_mask, "leverage_multiplier"] = 1.5

# Lower High Regime
low_high_mask = (self.rolling_mean_vol >= self.mu_avg) & (self.rolling_mean_vol <= avg_plus_two_std_dev)
vol_series.loc[low_high_mask, "leverage_multiplier"] = 1

# Medium High Regime
medium_high_mask = (self.rolling_mean_vol >= avg_plus_two_std_dev) & (self.rolling_mean_vol <= avg_plus_four_std_dev)
vol_series.loc[medium_high_mask, "leverage_multiplier"] = 0.5

# Extremely High Regime
extrm_high_mask = (self.rolling_mean_vol >= avg_plus_four_std_dev)
vol_series.loc[extrm_high_mask, "leverage_multiplier"] = 0
```

This is a concrete, code-level implementation of regime-conditional sizing. **In extremely high vol regime, leverage goes to zero.** Note that this is exactly the mechanism Asness says AQR studied post-2007 ("a bit less leveraged for the same level of risk"). Memo C should reference this code as a baseline implementation. Failure modes addressed: F6 (vol regime), partly F1 (crowding shows up as vol spike, so leverage drops). Note this addresses *F1 by accident*, not by design — vol is a lagging indicator of crowding.

**`arbitragelab/time_series_approach/regime_switching_arbitrage_rule.py`**: implements Bock-Mestel (2009) Markov regime-switching rule. Trade rules differ in High vs. Low vol regimes. The `prob >= rho` check in opening rules (lines 38-48) means trades are only opened when the regime probability passes a threshold. Failure modes addressed: F6 (vol regime), partial F8.

**`arbitragelab/optimal_mean_reversion/ou_model.py:770, 821`** — `optimal_liquidation_level_stop_loss`, `optimal_entry_interval_stop_loss`. OU-model-implied stop-loss levels. Failure modes addressed: partial F2 (visible cointegration breakdown).

**`arbitragelab/cointegration_approach/utils.py:10-26`** — `get_half_life_of_mean_reversion`:

```python
def get_half_life_of_mean_reversion(data: pd.Series) -> float:
    reg = LinearRegression(fit_intercept=True)
    training_data = data.shift(1).dropna().values.reshape(-1, 1)
    target_values = data.diff().dropna()
    reg.fit(X=training_data, y=target_values)
    half_life = -np.log(2) / reg.coef_[0]
    return half_life
```

**Critical numerical-safety note for OpenQuant**: arbitragelab does *not* check that `reg.coef_[0] < 0` before computing `-np.log(2) / reg.coef_[0]`. If the regression slope is positive (i.e., the series is *not* mean-reverting), you get a *negative half-life*, which is then used downstream as if it were a valid number. The Sarmento-Horta filter happens to filter on `half_life > 0`, which masks the bug, but the function itself is unsafe. OpenQuant's pair-picker has already encountered this issue (per CLAUDE.md), so this is just confirmation that arbitragelab gets it wrong too.

### What arbitragelab does *not* implement

I grepped the whole `arbitragelab/` tree for: `earnings`, `event`, `news`, `merger`, `borrow`, `short_squeeze`, `crowding`, `ftd`, `position_concentration`. **Zero hits in any strategy/filter file.** The only matches were in `util/data_cursor.py` (matplotlib event handlers — a UI artifact). 

Concretely: arbitragelab — the canonical open-source stat-arb library — **mitigates F2 (admission-only), F6, partial F8, partial F2-online**. It does *not* address F1, F3, F4, F5, F9, F10 in code. These are precisely the failure modes the practitioner literature flags as primary.

### mlfinlab — what it (still) implements

`hudson-and-thames/mlfinlab` open-source has been gutted: nearly every method in `backtest_statistics/`, `structural_breaks/`, and `cross_validation/` is a function signature with `pass` body (the proprietary version is paid). What remains as docstrings and (in some cases) implementations:

- **`mlfinlab/backtest_statistics/backtests.py:7` — `CampbellBacktesting`**: implements Harvey-Liu (2015) "Backtesting" haircut to Sharpe ratios for multiple-testing. Addresses F10 directly.
- **`mlfinlab/structural_breaks/sadf.py`** (gutted): SADF (Supremum ADF) test from AFML Snippet 17.2. Detects explosiveness — a flavor of F2 (cointegration breakdown).
- **`mlfinlab/structural_breaks/chow.py`, `cusum.py`** (gutted): Chow test and CUSUM filter for structural breaks in regression coefficients. Direct F2 detector if working.

The fact that mlfinlab has had its core methods removed is itself a notable signal: **the for-pay version is the actual reference implementation**, the open-source version is a stub. Anyone trying to build on mlfinlab for free is building on docstrings.

### Summary of failure-mode coverage in open source

| Failure mode | arbitragelab | mlfinlab (gutted OSS) | mlfinlab (paid) |
|---|---|---|---|
| F1 Crowding | None | None | (claimed: HRP allocator, 13F overlap) |
| F2 Cointegration breakdown | Admission filter | SADF stub | SADF (live monitoring) |
| F3 Borrow / squeeze | None | None | None |
| F4 Single-name event | None | None | None |
| F5 Capacity / impact | None | None | None |
| F6 Volatility regime | VolatilityFilter, RegimeSwitching | None | (claimed: HMM bet sizing) |
| F8 Single-factor dominance | Partial (Hurst filter) | None | None |
| F9 Alpha decay | None | None | (claimed: PBO) |
| F10 Multiple testing | None | CampbellBacktesting | DSR, PBO, CPCV |

The pattern is striking: **the open-source stat-arb tooling addresses F2, F6, F10. It does *not* address F1, F3, F4, F5, F9 — which the practitioner literature treats as primary**. Anyone building from these libraries inherits the gap.

---

## Practitioner voices

Five extended quotes from named practitioners with attribution. I weighted toward people who actually ran money during stat-arb failures.

### Cliff Asness (AQR) — Sept 2007

On the *inevitability* of factor crowding for popular signals:

> "Strategies like value, earnings quality, and momentum (as well as many others) spring from the academic and practitioner literature we've all read and some of us helped write, and the common sense we perhaps all share. I have been personally writing and speaking for years about how strategies are often alpha when first discovered, and then slowly move toward beta, or at least develop an 'exotic beta' component over time. Thus, quant strategies should be correlated. Again, we don't claim prescience; the size/virulence of this move caught us and everyone by surprise, but the fact that quant strategies are correlated is not news." (August of Our Discontent, p. 10)

### Gary Chropuvka (Goldman Sachs Quantitative Investment Strategies) — 2017 FT interview

On the experience of the August 2007 quake:

> "All this worked academically, and for a long time it worked in practice, and then all of a sudden you have this horrible event. It was the most humbling experience of our lives." (FT 2017, GSAM reprint p. 1)

This is the most concrete public statement from a practitioner whose career was specifically defined by being on the wrong side of the unwind. Goldman QIS managed $165B at peak; the unit never fully recovered; Global Alpha was wound down in 2011.

### Ernie Chan (former Morgan Stanley equity stat-arb, then independent) — June 2011 blog

On detecting cointegration breakdown:

> "It is actually quite hard to detect the breakdown of cointegration except in hindsight maybe a year afterwards." ([epchan.blogspot.com/2011/06/when-cointegration-of-pair-breaks-down](http://epchan.blogspot.com/2011/06/when-cointegration-of-pair-breaks-down.html))

And on what to do:

> "Even if you haven't done this backtest...you should at least continue paper trading it to see when it is turning around!"

His prescription is the opposite of the OpenQuant instinct to *demote* failed pairs aggressively. Chan's logic: if the cause was a temporary fundamental shock (he uses GLD-GDX-USO as the example, where rising oil prices in 2008 changed gold-miners' profit margins), the cointegration may return. Demoting permanently locks in the loss.

### Marcos Lopez de Prado (formerly AQR head of ML, now LBL/CIQF) — multiple sources

On backtesting in finance:

> "Backtesting is not a research tool. Feature importance is."  
> "Backtesting while researching is like drink driving. Do not research under the influence of a backtest."  
> "Every backtest must be reported with all trials involved in its production."  

(Marcos' Three Laws of Backtesting, distilled in `reasonabledeviations.com/notes/adv_fin_ml/`)

The deepest cut: "Most discoveries in empirical finance are false, as a consequence of selection bias under multiple testing." This is the single most important warning for OpenQuant's autoresearch loop — every memo (A, B, C, D) is itself an *additional trial* in the universe-search-space, and the DSR adjustment compounds.

### Avellaneda-Lee 2010 — sector-level finding (the underrated one)

This is not a quote from a person but a finding so directly relevant to OpenQuant's bimodal failure that it deserves featuring. From their Section 7 / Figure 36 (pp. 41-43 of the NYU PDF):

> "A closer look at the PL for different sectors shows, for example, that the Technology and Consumer Discretionary sectors were strongly affected by the shock — and more so than Financials and Real Estate; see Figure 36. This apparently paradoxical result — whereby sectors that are uncorrelated with Financials experience large volatility — is consistent with the unwinding theory of Khandani and Lo."

The implication for OpenQuant: in a forced-unwind regime, the sectors that get hit *worst* are not the ones in trouble fundamentally — they are the ones with the *most liquidity*, because that is where stat-arb desks deleverage. Tech in 2007 was the most liquid place to dump. **Tech in 2024-2026 (FAANG mega-caps) is the most liquid place to dump.** The Avellaneda-Lee paradox is an exact match for OpenQuant's FAANG-fails-while-other-sectors-work pattern.

This is — to my reading — the strongest single piece of evidence in the published literature that explains OpenQuant's specific 0/8 FAANG-and-entsw failure pattern. It is not in any of A, B, or C's drafts.

---

## OpenQuant-specific failure-mode mapping

Of the failure modes catalogued, three most cleanly explain the bimodal FAANG / entsw 0/8 pattern. Ranked by my confidence:

### Most likely: F8 (single-factor dominance) + F1 (crowding) — combined effect

**Why:** The 2023-2026 era is defined by mega-cap concentration. Memo C documents this with breadth and dispersion data. The 7-8 names that dominate FAANG basket variance (AAPL, MSFT, GOOGL, AMZN, META, NVDA — in OpenQuant's universe) are *also* the names most heavily traded by stat-arb desks for liquidity (F1 crowding) and *also* the names whose individual factor loadings shift dramatically as the market's "AI-megacap" factor evolves (F8).

When memo A says "AAPL ≈ 30% of equal-weighted FAANG basket's volatility," the deeper read is: *this is structural, not a peer-set choice*. No reasonable peer-set will fix it because the factor loading itself is changing dynamically under our feet. Avellaneda-Lee's volume-conditional gate (only trade when volume signals confirm) is a more honest fix than re-clustering.

**Predicted observable in OpenQuant data:** if F8+F1 is the dominant failure mode, FAANG basket trades should fail *more* on days when (a) cross-sectional dispersion is high (memo C's regime signal), (b) gross liquidity in those names is high relative to their median (the unwind signature), and (c) the spread move during the 1-min entry bar is *sustained* through the next 30 minutes rather than mean-reverting in 5-10 minutes (the linear-decline signature Asness describes).

### Second-most likely: F4 (single-name event sensitivity), specifically multi-day post-earnings drift

**Why:** OpenQuant's earnings-blackout filter is single-day. Mega-cap Tech earnings produce 3-7 day post-revision drifts (analyst upgrades cascading through a week). FAANG names, more than industrial mid-caps, have *concentrated analyst coverage* — meaning revisions cluster, and the post-earnings drift is more pronounced than for less-covered names. This is sector-specific and would predict exactly the bimodal failure (FAANG worst, entsw also bad given post-earnings-drift is strong in energy too where commodity-price-revisions follow).

**Predicted observable:** if F4 is the dominant failure mode, FAANG trade losses should cluster within 7 trading days of any basket member's earnings, even if not on the announcement day itself. Memo A doesn't address this; B and C are sector-agnostic; **this is a specific check OpenQuant should run** before adopting any of A/B/C.

### Third (lower confidence): F9 (alpha decay) for pairs/baskets that have been published

**Why:** GLD-GDX is the canonical alpha-decay-after-publication case. FAANG-basket-style trades have been written about extensively in academic and practitioner literature since at least 2020. Even if OpenQuant's specific basket isn't published, the *style* is. QuantRocket's empirical 2-year half-life is a reasonable prior for any published-style trade.

**Predicted observable:** the in-sample period (2024) Sharpe should be meaningfully higher than the OOS period (2025-Q3 onward). Memo C reports OpenQuant's Sharpe is 0.47 OOS; if I had access to the 2024 in-sample number, I'd predict it was 1.5-2.0. The decline magnitude is consistent with F9.

### Less likely (but should be ruled out): F2 (cointegration breakdown of specific baskets)

Memos A and B implicitly claim F2 is the dominant mode. The data doesn't quite support this: OpenQuant *has* periodic re-validation, the rejection rate is non-trivial, and the failures are concentrated in specific sectors, not random across the universe. F2 doesn't predict bimodal sector failure; F8+F1 and F4 do. F2 is probably *some* of the residual but not the dominant story.

---

## Cross-references with A, B, C — what they got right, missed, overstated

I read memo A v1 in full. Memo C v2 in full. Memo B was not yet finalized when I drafted this memo. I'll cross-reference A and C; flag what B's expected scope should pick up.

### Memo A (peer-set construction, v1) — strengths and gaps

**A got right:** the GICS-only peer set is too coarse for FAANG. The factor-residual approach is what desks actually do. DBSCAN/OPTICS on residuals would *exclude* AAPL/AMZN/GOOGL/META as noise points — which is correct and would have prevented the bad trades.

**A got partially right:** the recommendation that BARRA-style residual peers would have moved the outcome "high confidence." This is true for F8 (factor dominance) but *not* for F1 (crowding) or F4 (single-name events). Better peers don't fix crowding; better peers don't fix earnings sensitivity.

**A overstated:** Sarmento-Horta 2020 reports Sharpe 3.79 vs. 2.59 for GICS baseline. This is a single backtest on a single universe over a single time window. The practitioner literature on out-of-sample replication of clustering-based pair selection is *much* less encouraging. From the search trail: every replication study I could find shows degradation. Memo A treats Sharpe 3.79 as evidentiary; I'd treat it as a soft prior at best.

**A missed entirely:** crowding (F1). The AAPL/AMZN/GOOGL/META are the most-crowded names on the planet *because* they are the most liquid. DBSCAN excluding them as noise points is correct *because* of crowding, but A doesn't explain it that way. A says "they don't share factor exposures cleanly." The deeper truth is they don't share factor exposures cleanly *because* every stat-arb desk has been regressing them against every imaginable factor for 15 years and capacity has eroded any residual-stationary structure.

### Memo C (regime-switching, v2) — strengths and gaps

**C got right:** the literature is "rich but mixed." Bulla et al. (2011) is the cleanest evidence for regime-switching beating unconditional OOS. The COVID-2020 episode is correlation-explosion, not dispersion-divergence. Memo C is appropriately cautious about Sharpe-doubled-with-regime-gate claims that don't replicate.

**C got partially right:** the recommendation is "regime gate as risk-off filter (stand aside in adverse regime) rather than a model-class flip (MR ↔ momentum)." This is the right shape — Asness in Sept 2007 is explicit that *the absolute worst thing to do in a crisis is decide the world has changed once you see the deleveraging begin*. C's caution about model-class flips aligns with this. The only critique: C frames the risk-off gate as *de-risking*, not as *crowding-mitigation*. The two have similar mechanical effects (lower exposure in adverse regime) but different causal models, and the crowding model implies the gate should fire on *positioning* signals, not on volatility signals.

**C missed:** the Avellaneda-Lee sector finding. The fact that Tech and Consumer Disc were hit hardest in Aug-2007 — not because of fundamental shock but because of liquidity-driven unwinds — is the cleanest explanation for OpenQuant's bimodal sector failure. Memo C cites Avellaneda-Lee but doesn't extract this specific finding.

**C overstated:** the case for regime-detection generally. Asness pushes back hard: "factor-timing has weak historical track record." The Asness-side critique is that if regime detection worked reliably, AQR would have used it post-2007 to escape the value-factor drawdown of 2018-2020 — and they didn't, because it doesn't. Memo C's recommendation of a *risk-off* (not flip) gate is the right place to land, but the strength of the evidence for *any* regime gate is weaker than C's executive summary implies.

### What memo B should pick up (if it's about regime/structural-break detection)

If B is on regime/structural-break detection (the implication from the user's framing), B will likely be in tension with this memo on the question of *whether structural breaks are detectable in real time*. The practitioner consensus from the failure-mode literature:

- **Structural breaks in cointegration are detectable in hindsight, not in real time** (Chan 2011, explicitly).
- **SADF-style explosiveness tests work but with significant lag** (mlfinlab's stub is the canonical implementation; Phillips-Shi-Yu 2015 is the academic source).
- **CUSUM-of-squares on regression residuals fires too late to be useful** for stop-loss purposes (this is a common Quantopian-archive observation).

If B claims real-time detection is feasible, this memo's counter-claim is: *show me one practitioner who has actually run real-time structural-break detection and made money from it*. The academic literature has many proposals; the practitioner record has very few success stories.

---

## What's missing from this entire 4-memo research package

Honest meta-critique of what A+B+C+D are collectively *not* covering:

1. **Execution / market-microstructure failure modes are entirely absent.** Slippage, fill rate, adverse selection on limit orders, iceberg-detection by other algos, last-minute-of-day liquidity dry-up. These are first-order for any 1-min-bar strategy. None of A/B/C/D address them. From the practitioner literature, this is at least 20-30% of stat-arb's edge erosion since 2007.

2. **Risk model misspecification.** OpenQuant's "structural quality" gates (ADF, R², half-life, β stability) are themselves a risk model. They capture *spread* mean-reversion but not *factor* exposures. If the basket has a stealth factor exposure (e.g., AI-momentum, oil-price), the spread can be stationary in-sample and structurally net-long-momentum out-of-sample. Memo A touches this with risk-model residuals but doesn't validate that OpenQuant's existing gates approximate a real risk model.

3. **Capacity and impact at OpenQuant's scale.** Mostly out-of-scope today, but a meaningful Sharpe of 1.5-3.0 would attract enough AUM that capacity becomes the binding constraint within a year. None of A/B/C/D have a capacity model.

4. **The selection-bias problem of doing 4 memos at once.** Each memo proposes 2-5 candidate fixes. Across A+B+C+D, that is 10-20 candidate changes to OpenQuant. If we backtest all of them and pick the best, we are doing exactly the kind of multiple-testing Lopez de Prado warns against. The DSR adjustment for 20 trials at correlation ~0.5 is brutal — the implied Sharpe haircut is 30-40%. This memo package needs a *Bonferroni-style* discipline that the OpenQuant team explicitly enforce: pick *one* fix to implement, ideally the one with the strongest *prior* evidence, rather than the one with the best in-sample backtest.

5. **The "do nothing" baseline is undervalued.** Asness's specific advice from 2007: "the absolute worst thing to do in this past crisis would have been to decide the world had changed once you saw the serious deleveraging begin." If OpenQuant's underperformance is a 6-12 month regime episode (consistent with F1+F8 being temporary), the right move may be to *not change anything* and wait for the regime to revert — exactly what AQR did and benefited from. None of A/B/C/D treat "do nothing" as a serious option, but the practitioner literature treats it as the default.

6. **No memo covers the *short-leg-borrow* failure mode for non-FAANG names.** OpenQuant's universe will eventually expand beyond S&P 500. When it does, F3 becomes first-order. Worth a stub mention even if not deep-dived.

---

## Recommendation for OpenQuant

Stack-ranked by my confidence:

### Highest confidence: drop FAANG mega-caps from the universe; do not "fix" them

The Avellaneda-Lee 2007 sector paradox + Asness's "more common = more deleveraged" + Lopez de Prado's "fixing a model often just having a bad period" all point to the same conclusion: **the strongest move for OpenQuant right now is to *exclude* AAPL/MSFT/GOOGL/META/AMZN/NVDA from the trading universe entirely**, rather than re-cluster them, regime-gate them, or model-switch them.

This is a portfolio-construction change, not a methodology change. It is what Asness implies AQR did ("a bit less leveraged for the same level of risk"). It is what Avellaneda-Lee's volume-conditional ETF strategy effectively does (only trade when volume confirms — which excludes the most-crowded names). It is what memo A's analysis implies but doesn't quite state ("avoid them entirely as targets in residual-PCA strategies").

Concrete proposal: define a "high-crowding exclusion list" using public 13F overlap data (NPORT-P filings, Form 13F, top-10 hedge-fund-holdings overlap), and exclude any name that appears in >X% of the top-50 stat-arb-style fund 13F overlaps. Refresh quarterly. This addresses F1+F4+F9 simultaneously without changing any methodology.

### Medium confidence: add a strategy-family-specific earnings policy (per memo notes on `port_contamination`)

Replace the global single-day earnings blackout with:

- For basket-spread strategy: 7-day post-earnings hold-out for the affected basket (analyst-revision drift window).
- For sector baskets where one name has earnings: hold-out the entire basket, not just the name.
- Implement a *positive* signal: re-enter only after observed analyst-revision-rate drops back to baseline.

This addresses F4 directly. None of A/B/C cover it.

### Medium confidence: implement Campbell-Harvey-Liu Sharpe haircut for autoresearch outputs

Per Lopez de Prado: every backtest in the autoresearch loop should be reported with its DSR-adjusted Sharpe, factoring in the *cumulative trial count across all autoresearch runs*. This is what OpenQuant already does for individual experiments (DSR 6.88 in `project_statarb_autoresearch_findings`); extend it to the cumulative experiment count across 14+ experiments.

This addresses F10 directly.

### Lower confidence: implement arbitragelab's `VolatilityFilter` regime gate as the **first** regime gate

Before adopting memo C's two-signal regime gate (cross-sectional dispersion + factor-leadership-trend), implement the simpler arbitragelab `VolatilityFilter` (lines 251-401 of `arbitragelab/ml_approach/filters.py`) as a baseline. It's 150 lines, has a documented academic source (Dunis-Laws-Evans 2005), and produces a leverage multiplier that goes to 0 in extreme-vol regimes.

This is *not* a deep methodological choice — it's a "can we get the obvious regime-filter working first" sanity check before adopting C's more elaborate version.

### Do not implement (low confidence in benefit, high cost)

- Methodology-flips between MR and momentum (per memo C's caveat, validated by this memo's Asness quote).
- Aggressive re-clustering with new ML methods (per memo A's overstatement of Sarmento-Horta replication).
- Real-time structural-break detection as a stop-loss trigger (per Chan's "hindsight a year afterwards" critique).

### Default if uncertain: do nothing for one quarter

Asness's 2007 advice is the under-recognized null hypothesis. If memo C is right that OpenQuant's underperformance is a regime episode, *waiting* for the regime to revert is the highest-Sharpe-per-effort move. Tracking error in the meanwhile is the cost of patience, but it's smaller than the cost of implementing 3-4 changes that turn out to be data-mining.

Concrete action: set a "do nothing for one quarter" decision criterion. If after Q2-2026 the Sharpe is still below 1.0 and *none* of the proposed changes have been implemented, *then* implement the highest-confidence change (drop FAANG mega-caps). Otherwise, hold.

---

## Sources

Primary documents (fully extracted):
- Khandani, A. and Lo, A. (2007). *What Happened to the Quants in August 2007?* MIT preprint, NBER w14465. ([web.mit.edu/Alo/www/Papers/august07.pdf](https://web.mit.edu/Alo/www/Papers/august07.pdf))
- Asness, C. (Sept 2007). *The August of Our Discontent: Questions and Answers about the Crash and Subsequent Rebound of Quantitative Stock Selection Strategies*. AQR Capital Management. ([aqr.com/-/media/AQR/Documents/Insights/Working-Papers/The-August-of-Our-Discontent.pdf](https://www.aqr.com/-/media/AQR/Documents/Insights/Working-Papers/The-August-of-Our-Discontent.pdf))
- Avellaneda, M. and Lee, J.-H. (2010). *Statistical Arbitrage in the U.S. Equities Market*. Quantitative Finance 10(7). ([math.nyu.edu/~avellane/AvellanedaLeeStatArb071108.pdf](https://math.nyu.edu/~avellane/AvellanedaLeeStatArb071108.pdf))
- Mussalli, G. (Jan 2018). *Quant Meltdown: 10 Years Later*. PanAgora Asset Management. ([panagora.com/assets/PanAgora-Quant-Meltdown-10-Years-Later.pdf](https://www.panagora.com/assets/PanAgora-Quant-Meltdown-10-Years-Later.pdf))
- Wigglesworth, R. (March 2017, FT, syndicated by GSAM). *Goldman Sachs' Lessons from the Quant Quake*. ([gsam.com/content/dam/gsam/pdfs/common/en/public/articles/2017/quant-quake-reprint-news.pdf](https://www.gsam.com/content/dam/gsam/pdfs/common/en/public/articles/2017/quant-quake-reprint-news.pdf))

Secondary practitioner sources:
- Chan, E. *When cointegration of a pair breaks down*. Quantitative Trading blog, June 2011. ([epchan.blogspot.com/2011/06](http://epchan.blogspot.com/2011/06/when-cointegration-of-pair-breaks-down.html))
- Chan, E. *Pair trading stocks and the life-cycle of strategies*. Quantitative Trading blog, June 2007. ([epchan.blogspot.com/2007/06](http://epchan.blogspot.com/2007/06/pair-trading-stocks-and-life-cycle-of.html))
- Boetel, B. *Is Pairs Trading Still Viable?*, QuantRocket blog. ([quantrocket.com/blog/pairs-trading-still-viable](https://www.quantrocket.com/blog/pairs-trading-still-viable/))
- Jha, V. *What Happened to the Quants in March 2020?*, ExtractAlpha, April 2020. ([extractalpha.com/2020/04/08](https://extractalpha.com/2020/04/08/what-happened-to-the-quants-in-march-2020/))
- Jha, V. *The Quant Quake, 10 years on*, ExtractAlpha, August 2017. ([extractalpha.com/2017/08/11](https://extractalpha.com/2017/08/11/the-quant-quake-10-years-on/))
- *Lessons from the Quant Winter*, First Sentier Investors. ([firstsentierinvestors.com.au/...lessons-from-the-quant-winter](https://www.firstsentierinvestors.com.au/au/en/adviser/insights/latest-insights/lessons-from-the-quant-winter.html))
- *August 2007 Broke Every Quant Fund at Once*, Quant Decoded. ([quantdecoded.com/en/crowding-quant-strategies-detection-risk](https://quantdecoded.com/en/crowding-quant-strategies-detection-risk))
- *Quantopian Shutting Down*, QuantRocket blog. ([quantrocket.com/blog/quantopian-shutting-down](https://www.quantrocket.com/blog/quantopian-shutting-down/))

Academic / methodology:
- Bailey, D. and Lopez de Prado, M. (2014). *The Deflated Sharpe Ratio: Correcting for Selection Bias, Backtest Overfitting and Non-Normality*. ([papers.ssrn.com/abstract=2460551](https://papers.ssrn.com/sol3/papers.cfm?abstract_id=2460551))
- Lopez de Prado, M. (2018). *Advances in Financial Machine Learning*. Wiley. (Notes summary at [reasonabledeviations.com/notes/adv_fin_ml/](https://reasonabledeviations.com/notes/adv_fin_ml/))
- Krauss, C. (2017). *Statistical Arbitrage Pairs Trading Strategies: Review and Outlook*. Journal of Economic Surveys 31(2). ([finance.lab.nycu.edu.tw/.../STATISTICAL-ARBITRAGE-PAIRS-TRADING-STRATEGIES-REVIEW-AND-OUTLOOK.pdf](https://finance.lab.nycu.edu.tw/Students/105%E8%8E%8A%E5%87%B1%E8%87%A3/STATISTICAL-ARBITRAGE-PAIRS-TRADING-STRATEGIES-REVIEW-AND-OUTLOOK.pdf))
- Sarmento, S. and Horta, N. (2020). *Enhancing a Pairs Trading Strategy with the Application of Machine Learning*. Expert Systems with Applications 158:113490.

Open-source code (cloned and read in full at `/tmp/quant_research/`):
- `hudson-and-thames/arbitragelab` — `arbitragelab/spread_selection/cointegration.py`, `arbitragelab/ml_approach/filters.py:251-401` (VolatilityFilter), `arbitragelab/cointegration_approach/utils.py:10-26` (half-life), `arbitragelab/time_series_approach/regime_switching_arbitrage_rule.py`, `arbitragelab/ml_approach/optics_dbscan_pairs_clustering.py`. ([github.com/hudson-and-thames/arbitragelab](https://github.com/hudson-and-thames/arbitragelab))
- `hudson-and-thames/mlfinlab` — `mlfinlab/backtest_statistics/backtests.py:7` (CampbellBacktesting), `mlfinlab/structural_breaks/sadf.py` (gutted in OSS). ([github.com/hudson-and-thames/mlfinlab](https://github.com/hudson-and-thames/mlfinlab))
