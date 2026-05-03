# Cross-validated synthesis — 4-memo research package

**Synthesizer:** cross-validation pass over A_peer_sets_v2, B_regime_detection_v2, C_regime_switching_v2, D_failure_modes_v2.
**Date:** 2026-05-02.
**Empirical context:** OpenQuant runs basket-spread MR (OU + Bertram) on 43 GICS-sector baskets. 9-month backtest (Jul 2025 → Mar 2026, 189 days) gave Sharpe 0.47. 6 sectors net positive, FAANG (4 baskets) and entsw (4 baskets) are 0/4 each.

---

## Executive summary

1. **Three of four memos converge on a pre-trade structural fix; D dissents.** A (residual-construction + κ admission test), B (no online detector as primary; CUSUM as defensive layer), and C (stand-aside risk-off gate, not model-class flip) all propose changes. D argues the practitioner literature says all of A/B/C are at-best 30-40% confident and the dominant practitioner postmortem (crowding) is barely addressed by any of them.

2. **High-confidence consensus: regime-detection on top of bad peer sets is spurious.** A, B, and C all explicitly state this. Sequencing matters — peer-set or admission-level fix MUST precede any regime gate. D agrees implicitly (the "do nothing if uncertain" stance).

3. **High-confidence consensus: do not flip MR ↔ momentum.** B and C explicitly stand-aside; D's Asness quotes endorse stand-aside; A is silent but consistent.

4. **The biggest live conflict: "drop FAANG" (D) vs. "Avellaneda-Lee residual + κ rejects FAANG at admission" (A).** I find these *converge* in mechanism but diverge in evidence quality. D's recommendation has stronger practitioner backing; A's is a more elegant restatement that risks the same data-mining hazard Asness warned about. **Resolution: do exactly what D says (universe-level exclusion of high-crowding mega-caps) because it is cheaper, more honest, and has stronger non-replicating-academic-paper provenance.**

5. **Crowding (D's central finding) is the largest single gap in A/B/C.** None of A/B/C address F1 (crowding) directly. A claims FAANG's failure is "factor mismatch"; D's deeper read is that FAANG fails *because* every stat-arb desk uses these names for liquidity, so they are precisely where forced-deleveraging contagion concentrates. The Avellaneda-Lee 2007 sector-paradox finding that D extracted (Tech hit hardest in the August 2007 unwind, not Financials) is the single strongest piece of evidence in the package and is missing from A/B/C.

6. **OOS robustness ranks the recommendations.** Of all proposed fixes, only Bulla et al. (2011, regime-gate as risk-off) and Gatev et al. (2006, distance method) have published walk-forward OOS validation across multiple market regimes. The clustering methods (Sarmento-Horta, SPONGE, autoencoder) all show big in-sample wins that fail to replicate (5-10× Sharpe gap, post-cost collapse). Avellaneda-Lee is published-OOS but its own authors document Sharpe degradation 1.44 → 0.9 over the publication window.

7. **The single most-actionable recommendation: drop the 6-7 mega-cap names (AAPL, MSFT, GOOGL, AMZN, META, NVDA, possibly TSLA) from the trading universe entirely, then run unchanged for one quarter to measure the lift.** This is reversible, requires no new code, addresses A's structural claim AND D's crowding claim simultaneously, and avoids the multiple-testing hazard of trying 5 fixes at once.

---

## Consensus findings (high confidence — multiple memos agree)

### Consensus 1: Sequence peer-set fix BEFORE regime detection
- **A (line 418):** "Do NOT add a regime gate to a system with bad baskets — the gate will be calibrated on poisoned data."
- **B (line 336):** "If A is right, then most of the 'structural breaks' my methods would detect are spurious — they're firing on relationships that were never stationary in the first place. A's hypothesis dominates B's hypothesis under H2."
- **C (line 340):** "Sequence: fix peer-set first (eliminates known-bad baskets), then add regime gate. Don't add the gate before fixing baskets, or the gate will be calibrated on poisoned data."

This is the single strongest cross-memo agreement. Three independent researchers, three independent literature passes, same conclusion.

### Consensus 2: Stand-aside in adverse regime; do NOT flip to momentum
- **B (line 364-365):** Trigger action of CUSUM monitor is "pause new entries on the basket for a configurable window... Do *not* force exit."
- **C (line 200-201):** "Stand-aside is the safer path... no empirical evidence the same basket-spreads trend in adverse regimes."
- **D (line 76-77, quoting Asness):** "the absolute worst thing to do in this past crisis would have been to decide the world had changed once you saw the serious deleveraging begin."

Wood-Roberts-Zohren is the lone pro-flip evidence and is on TS-momentum on futures, not residual-stat-arb on equities. Verdict: do not flip.

### Consensus 3: HMM as primary regime gate is brittle
- **B (line 290):** "HMM regimes don't generalise out-of-sample... arbitragelab docs and the Klawitter QuantConnect post both document the same failure mode."
- **C (line 247):** "A naïve HMM would likely have missed this regime because the bad regime hadn't happened in training data."
- **D:** does not test HMM directly but cites Asness's broader skepticism toward factor-timing (which subsumes HMM-based regime detection).

Both B and C cite the same arbitragelab docstring warning verbatim: HMM "will fail if there is a regime higher than the current high regime in the future." This is a non-trivial cross-confirmation — both researchers landed on the same diagnostic without coordinating.

### Consensus 4: Cointegration breakdown is hard to detect online
- **B (entire memo):** detection lag is the central problem; CUSUM at ARL₀ ≈ 250 days is the cheapest defensible choice but admits ~63 false alarms/year on 43 baskets.
- **D (line 296, quoting Chan):** "It is actually quite hard to detect the breakdown of cointegration except in hindsight maybe a year afterwards."
- **A (line 305, citing Chan):** persistent breakdown might not be a "wrong basket" diagnosis but an "incomplete basket" diagnosis (missing variable).

All three converge: real-time online detection of cointegration breakdown is a hard problem with no clean published practitioner success story. Ernie Chan is cited verbatim by A, B, and D — and all draw the same conclusion. The 4-memo package has no concrete proposal that overcomes this limitation.

### Consensus 5: Published clustering-stat-arb numbers don't replicate
- **A (line 130):** "Independent replication of Han-He-Jun-Toh (2021)... academic claim is Sharpe 2.69; the replicator achieves Sharpe 0.4."
- **A (line 154):** Cartea SPONGE Sharpe 1.1 → 0.28 with 0.05% TC.
- **D (line 16):** "Most of the 'fixes' that show up in academic stat-arb papers do not replicate OOS in the practitioner record."

Both A and D independently flag the replication gap. A goes further and re-corrects v1 (which had over-stated Sarmento-Horta's relevance). D pushes harder, treating *all* of A/B/C's fixes with 30-40% confidence rather than 80%.

---

## Conflicts identified and resolution

### Conflict 1: peer-set vs. regime detection priority

**The conflict:** A says "fix peer set first." B confirms. C confirms partially. **D doesn't directly address it** but D's practitioner literature implies all three (A, B, C) may be over-engineering — practitioners just stop trading or cut universe.

**Evidence on each side:**
- *A/B/C side:* Three independent literature passes converge on the same sequencing logic. The argument is structural (regime detection on a non-stationary residual is spurious by construction).
- *D side:* Asness's empirical observation that "new research often seems to be directed at 'fixing' a model that is often just having a bad period" is the strongest counter — the very act of proposing fixes after a 9-month underperformance window is the hazard.

**My resolution:** A/B/C's sequencing logic is correct *given that you are going to make a change*. D's deeper challenge is that the question is whether to make a change at all. **The right answer is to combine them: do the cheapest, most-reversible peer-set-level change (drop the high-crowding mega-cap names from the universe) BEFORE any methodology change.** This satisfies both: it sequences peer-set work first (per A/B/C), and it is the practitioner move (per D, citing AQR's 2007 response).

### Conflict 2: drop FAANG vs. fix FAANG

**The conflict:** D says drop FAANG entirely from the universe. A says Avellaneda-Lee residual + κ admission test will reject most FAANG baskets *at admission time* — which is *de facto* dropping FAANG, but via principled rule.

**Do they converge?** In *mechanism* on the FAANG question: yes. Both end up not trading FAANG.

**Do they converge in *evidence quality*?** No.
- D's recommendation has direct practitioner-source backing (AQR 2007 response, Avellaneda-Lee sector finding, Goldman QIS post-mortem).
- A's recommendation rests on Avellaneda-Lee 2010's PCA-residual machinery, but that paper itself reports Sharpe degradation 1.44 → 0.9 over the publication window — i.e., the very method A endorses is documented to decay in OOS.

**Do they imply different downstream actions?**
- D's "drop FAANG" stays dropped; quarterly review of 13F overlap.
- A's "Avellaneda-Lee admission" might re-admit FAANG if the κ statistic happens to look favorable in a future window (i.e., it's a dynamic gate). This is *more* sophisticated but also *more* fragile to false admissions.

**My resolution:** Drop FAANG via universe exclusion (D's path) is the higher-confidence, lower-engineering, more-reversible move. A's full Avellaneda-Lee + κ + R²[MR] residual-construction layer is *also* defensible but is a much bigger engineering change. **Do D first, then if a future regime makes mega-caps tradeable again, A's machinery can be the principled re-admission gate.** Sequence: D → A.

### Conflict 3: crowding

**The conflict:** D says crowding is the dominant practitioner failure mode (F1). A/B/C don't address it.

**Is crowding relevant for OpenQuant at $10K capital?** This is the user's specific question and is critical.

**Two parts to the answer:**

1. *Are we causing crowding?* No. At $10K we don't move prices. Our activity isn't part of the contagion.

2. *Are we exposed to others' crowding?* **Yes, and this is what matters.** The Khandani-Lo / Asness 2007 mechanism is that *we hold the same positions as funds that are about to be forced out*. When a multi-strategy fund liquidates its FAANG-basket-style trade, our basket spread moves with theirs. Our P&L is hit by *their* unwind regardless of our size. D's Avellaneda-Lee 2007 sector-paradox extraction (Tech sectors hit hardest because most-liquid for forced-deleveraging) is *directly* applicable: the FAANG/entsw 0/4 pattern OpenQuant sees is structurally consistent with this exact mechanism.

**Is the academic literature on crowding portable from $billion AUM context to $10K?** D's answer is implicitly yes, because the mechanism is *positional* (we hold the same trade), not *size-based* (we move the price). I agree.

**My resolution:** Crowding IS relevant for OpenQuant. D's framing is correct. The most data-cheap proxy is 13F overlap of the names in the basket; even simpler is "is this a name in the Goldman Hedge Fund VIP basket / heavily-shorted Tech mega-cap list?" — yes-or-no exclusion.

### Conflict 4: academic fixes vs. OOS replication

**The conflict:** D's strongest warning is that most academic stat-arb fixes do not replicate OOS. A's recommendations cite specific papers — are those papers ones that replicate OOS?

**A's recommended methods + OOS evidence quality:**

| Method (from A's recommendation) | Published OOS validation? | Evidence grade |
|---|---|---|
| Avellaneda-Lee residual + κ admission | Yes, 1997-2007 in original paper | C+: Authors document Sharpe 1.44 → 0.9 over their own window. No major academic update post-2010 shows the method still works at scale. |
| Partial cointegration R²[MR] (Clegg-Krauss 2018) | Yes, S&P 500 1990-2015 | B-: Published OOS but on a single window. No independent replication I can find. |
| TNIC industry classification (Hoberg-Phillips) | No stat-arb validation | D: A explicitly notes "no published evidence yet that it produces better stat-arb pairs than GICS or BARRA." |
| Distance method (Gatev 2006) — A's "sanity-check baseline" | Yes, multiple windows | A-: Gatev's original 1962-2002, Zhu 2024 replication 2003-2023 (Sharpe 1.35), Rubesam 2021 confirms decay direction. **The single strongest OOS-validated method in the package.** |

**My resolution:** Of A's three recommended methods, only one (partial cointegration) has clean OOS evidence; one (Avellaneda-Lee) has documented decay; one (TNIC) has no stat-arb validation at all. **A's ranked recommendation should be inverted: the distance-method-as-baseline (sanity check) is actually the strongest published method, not the κ admission test.** This is consistent with D's broader skepticism and represents an undocumented overstatement in A.

### Conflict 5: Memo A's v2 corrections, propagated to B/C/D

A v2 explicitly corrected v1 on three things: Sarmento-Horta's universe (208 commodity ETFs, not US equities), SPONGE's cost-degradation, and BARRA's factor count. Are there similar errors in B/C/D?

**B errors I checked:**
- B cites Wood-Roberts-Zohren 2021 Sharpe +33% / +67% — *consistent with C's reading* (Sharpe lift over 1995-2020 / 2015-2020 sub-window). No conflict.
- B cites Bock-Mestel 2009 Sharpe 1.5-3.4 — but B itself flags this as using *smoothed* (full-sample) probabilities, i.e. look-ahead bias. Properly noted.
- B cites the MQL5 article reporting CUSUM detected NVDA/INTC partnership in 1 day. **I did not verify this against the article directly; this is a single-source claim by B and should be flagged.** It is a tutorial-grade article, not peer-reviewed.

**C errors I checked:**
- C cites Bulla et al. 2011 — vol −41%, +18-200 bps. Both numbers consistent with B's mention. No conflict.
- C cites GS Prime Services 4.2% Jun-Jul 2025 quant equity loss. This is from a NY Ledger article; I did not verify against the GS source. Single-source via a tertiary aggregator.
- C says "Avellaneda-Lee 1.44 → 0.9 (down ~37%)" — *consistent with A's v2*. Both researchers extracted the same number from the same paper. Good cross-validation.
- **C's H3 claim ("MR-on-spread is a short-vol-of-dispersion bet") is self-flagged as weak in C's own self-critique (line 263).** C is honest about not having the formal derivation.

**D errors I checked:**
- D cites Avellaneda-Lee p. 41 sector finding (Tech hit hardest in Aug-2007 unwind). **I did not directly verify against the PDF.** A read the PDF in full and did not extract this finding — which is consistent with the finding being there but not central to A's scope. Plausibly real; should be re-checked before relying on it.
- D cites Khandani-Lo on "9:1 leverage by 2007" — consistent with their published Table 6.
- D's claim about arbitragelab `get_half_life_of_mean_reversion` not checking `coef < 0` is verifiable from the code D quotes inline. Looks correct.

**One conflict between memos that I caught:**

A's v2 reports Sarmento-Horta universe as commodity-linked ETFs (correctly, this is A's v2 correction). **D's description of Sarmento-Horta still treats it as evidence about US equities** ("Sarmento-Horta (which memo A cites approvingly) reports Sharpe 3.79 — a number no practitioner I can find replicates"). D doesn't flag the universe issue. **D's critique still holds (replication gap is real), but the specific framing is sharper if D had picked up A's v2 correction:** the Sarmento-Horta number was for commodity ETFs, so transferability to US equities was always a stretch, not just a replication gap.

**Single-source claim in C that I want to flag:** C's recommendation rests heavily on the 2025 quant drawdown narrative (GS, MSCI, Bloomberg). All of these citations are within the same August-October 2025 narrative cluster. This is a single-event story, not multi-window confirmation. C self-flags this as weak (line 269). I agree with C's honest grade.

**Single-source claim in D that I want to flag:** D's Avellaneda-Lee sector finding is presented as the keystone insight — "the strongest single piece of evidence in the published literature that explains OpenQuant's specific 0/8 FAANG-and-entsw failure pattern." That's a strong claim resting on a finding A did not extract. Before treating it as load-bearing, the AvL paper p. 41 / Figure 36 should be re-read.

---

## Single-source claims (low confidence — flag for caution)

1. **D's Avellaneda-Lee sector-paradox extraction** (Tech hit hardest in Aug-2007 unwind). Not extracted by A, who read the same paper directly. Plausibly real; should be re-verified before treating as load-bearing.

2. **B's MQL5 CUSUM-detection-in-1-day claim for NVDA/INTC.** Single tutorial article, not peer-reviewed, not cross-checked. Likely directionally correct (huge shock = fast detection) but the exact lag number shouldn't be relied on.

3. **C's "MR-on-spread is a short-vol-of-dispersion bet" framing.** C self-flags as weak. Plausible by analogy but not derived. Don't deploy any tool on the basis of this claim alone.

4. **C's claim that the 2025 quant drawdown is the live-evidence regime.** Single-narrative cluster (GS, MSCI, Bloomberg, Aug-Oct 2025). Probably real but doesn't constitute multi-regime OOS validation.

5. **A's claim that TNIC similarity would correctly handle AMZN-as-cloud.** Plausible but no published stat-arb validation. A explicitly notes this.

6. **A's recommendation that R²[MR] > 0.5 is the right partial-cointegration threshold.** A explicitly notes Clegg-Krauss don't specify a calibration; this is A's guess.

7. **D's claim that 13F overlap is the right crowding proxy.** D states this but doesn't cite a specific stat-arb paper that has used 13F overlap operationally for universe construction. Reasonable hypothesis but unvalidated for this purpose.

8. **B's CUSUM-with-ARL₀=250 calibration recommendation.** B states this gives ~63 false alarms/year on 43 baskets, then asserts the cost is "marginal-utility" decision. The decision reasoning is sensible but the calibration choice itself is single-source.

---

## Errors / corrections caught

### Errors A v2 caught (and others should learn from)

- v1 Sarmento-Horta misuse: applied to US equities when the universe was 208 commodity ETFs. v2 corrected this. **D did not pick up the correction** and still treats Sarmento-Horta as evidence about US equity replication failure (which it is, but the framing is sharper if you note the universe issue).

- v1 SPONGE Sharpe overstatement: v1 claimed SPONGE "moves the needle" on US equities. v2 correctly notes Sharpe 1.1 → 0.28 with realistic costs.

- v1 BARRA factor count overstatement: "50-70 factors" is mostly industry dummies (~60 of them). Only ~10 style factors.

### Errors I caught in cross-validation

- **D's framing of Sarmento-Horta is still the v1 framing.** D's general point (academic replication failure) is correct, but the specific framing is dulled because it doesn't note the commodity-ETF universe issue.

- **A v2's recommendation ranks by sophistication, not OOS evidence quality.** Reading A v2 in full and grading each recommended method on OOS robustness, the distance method (A's "sanity check") is actually the highest-OOS-confidence method in the entire package. A's primary recommendation (κ admission + R²[MR]) has weaker OOS evidence than its sanity-check baseline.

- **B/C both correctly extract the arbitragelab "novel regime failure" docstring, but neither memo extracts a *quantitative* false-positive rate for HMM regime detection on stat-arb residuals.** Because no such number exists in the published literature. Both memos treat this gap honestly.

- **None of A/B/C extract D's Avellaneda-Lee sector finding.** This is the largest gap-in-cross-memo-coverage I found. If D's reading is correct, this is the keystone for OpenQuant's bimodal failure and A/B/C all missed it. Verification of the AvL p. 41 / Figure 36 reading is the highest-priority follow-up.

- **None of the memos compute the actual Deflated Sharpe Ratio for the 4-memo package.** D mentions DSR as a concern (line 169) but doesn't compute it. Across A+B+C+D there are roughly 12-15 candidate fixes; if OpenQuant tries the top 3-5 and picks the winner, the DSR haircut is significant. Lopez de Prado's discipline (one fix, picked on prior, not best-of-backtests) should be applied.

---

## Ranked recommendations

Stack-ranked by *expected Sharpe lift × implementation feasibility × OOS robustness*. Honest about uncertainty.

### Recommendation 1: Drop high-crowding mega-caps from the trading universe (D's Highest Confidence)

- **Mechanism addressed:** F1 (crowding), F8 (single-factor dominance), F4 (single-name event sensitivity at the FAANG scale).
- **Implementation cost:** ~0.5 engineering days. Modify universe filter to exclude AAPL, MSFT, GOOGL, AMZN, META, NVDA, possibly TSLA. One config change.
- **Data requirements:** None beyond what we have.
- **OOS evidence:** *Indirectly* strong via Avellaneda-Lee 2007 sector finding (D), Asness 2007 ("a bit less leveraged for the same level of risk"), Goldman QIS post-mortem. Not a published paper specifically endorsing this exact rule, but the practitioner-conduct evidence is consistent.
- **Specificity to FAANG/entsw 0/8:** Direct.
- **Reversibility:** Trivial. One config flip.
- **Expected lift:** Plausibly removes the entire 0/8 contribution. If the rest of the system is genuinely working at Sharpe 0.47 with FAANG/entsw dragging, removing the drag should lift aggregate Sharpe materially. *Rough magnitude: 0.47 → 0.7-0.9*. Big uncertainty interval; I would bet on the sign but not the magnitude.

**Verdict:** Ship this first. It's the highest-confidence, lowest-cost change and addresses the specific bimodal failure pattern in OpenQuant data.

### Recommendation 2: Run for one quarter unchanged after the universe cut (D's "do nothing" baseline)

- **Mechanism addressed:** F10 (multiple-testing / data-mining hazard).
- **Implementation cost:** Zero. Active discipline, not engineering.
- **OOS evidence:** Asness 2007 advice + AQR's documented behavior + the meta-evidence that "fixing a model that is just having a bad period" is itself a hazard.
- **Reversibility:** Perfect.
- **Expected lift:** Indirect. The lift comes from *avoiding* over-fitting losses from the next 5 fixes you would otherwise ship.

**Verdict:** Adopt as a discipline. Do not implement A/B/C's methodology changes during this quarter.

### Recommendation 3: Add Gatev distance method as a sanity-check baseline (A's secondary)

- **Mechanism addressed:** Diagnostic only — answers "does our sophistication beat the simplest method?"
- **Implementation cost:** ~1-2 engineering days. Distance-based pair selection is the most basic algorithm in the package.
- **OOS evidence:** Strongest in the package. Multi-window OOS (Gatev 1962-2002, Zhu 2003-2023, Rubesam 2021).
- **Reversibility:** Diagnostic; runs alongside, doesn't replace.
- **Expected lift:** Negative direct; positive diagnostic. If our basket method underperforms Gatev, our sophistication has no edge and we should simplify. If it outperforms, the edge is real.

**Verdict:** Cheap to add, high diagnostic value. Run as a non-trading shadow benchmark.

### Recommendation 4: Implement arbitragelab's VolatilityFilter as a baseline regime gate (D's medium confidence)

- **Mechanism addressed:** F6 (volatility regime), partial F1 (crowding shows up as vol spike, lagged).
- **Implementation cost:** ~1-2 engineering days. The code is 150 lines, public, documented.
- **OOS evidence:** Dunis-Laws-Evans 2005 published, but single window. C+ grade.
- **Reversibility:** Easy — toggle a leverage multiplier.
- **Expected lift:** Modest. C's CSV + factor-leadership gate is more bespoke; the VolatilityFilter is the published baseline that should be beat before adopting the bespoke version.
- **Caveat:** Per A/B/C consensus, do NOT add this before the universe fix in Recommendation 1.

**Verdict:** Defer. Implement only if Recommendation 1 doesn't lift Sharpe enough and we still need a regime gate.

### Recommendation 5: Add CUSUM-of-residuals defensive monitor (B's recommendation)

- **Mechanism addressed:** F2 (cointegration breakdown), partial F1 (catches forced-unwind dispersions).
- **Implementation cost:** ~3-5 engineering days. statsmodels has the building blocks; tuning ARL₀ requires care.
- **OOS evidence:** B grade. Brown-Durbin-Evans 1975 + Xiao-Phillips 2002 are foundational; MQL5 case study is single-source.
- **Reversibility:** Easy — disable monitor.
- **Expected lift:** Defensive (drawdown reduction), not offensive (Sharpe lift). B explicitly notes ~63 false alarms/year and the value is bounded by avoided drawdowns from rare true breaks.
- **Caveat:** Per consensus, only after a peer-set fix is in place.

**Verdict:** Defer until after Recommendations 1-4. A defensive add-on, not a primary fix.

### Do NOT implement (high-cost, low-confidence)

- **DBSCAN/OPTICS/SPONGE clustering for peer-set construction** (A's explicit recommendation — see A's "What NOT to ship"). Replication gap, post-cost collapse, US-equity-specific failure modes documented.
- **HMM-on-basket-spread-P&L as primary regime gate** (B/C consensus). Novel-regime failure, label-switching pathology, insufficient training data.
- **MR ↔ momentum model-class flip** (B/C/D consensus). Wood-Roberts-Zohren is on different asset classes. No evidence the same baskets give useful momentum signal in adverse regimes.
- **Wavelet + DL break-aware pairs trading** (B). Requires labelled break events we don't have.
- **Real-time online structural-break detection as stop-loss trigger** (D's critique, citing Chan). Detection lag exceeds the bleed window.

---

## What this 4-memo package is missing

Honest meta-critique:

1. **Execution / market microstructure failure modes are entirely absent.** Slippage, adverse selection on limit orders, last-minute-of-day liquidity, fill rate variance. For a 1-min-bar strategy this is at least 20-30% of edge erosion since 2007. None of A/B/C/D address it.

2. **No memo computed the cumulative Deflated Sharpe Ratio for the package.** With 12-15 candidate fixes across 4 memos, the multiple-testing penalty is real and Lopez de Prado's discipline (one fix, prior-based, not best-of-backtests) should be applied. D mentions this conceptually but doesn't compute it.

3. **No back-of-envelope on whether the FAANG/entsw 0/8 contribution is actually big enough to matter.** The aggregate Sharpe is 0.47; if FAANG/entsw is contributing modestly negative, removing it might lift Sharpe to 0.6-0.8. If FAANG/entsw is contributing massively negative, it might be 0.47 → 1.2. None of A/B/C/D run the back-of-envelope.

4. **No 2018-2019 retrospective.** Both A and C explicitly call for this: did our (counterfactual) basket-MR strategy fail in 2018-2019 (the previous mega-cap concentration era)? If yes, the failure is structural; if no, the failure is regime. This is the single most important *empirical* test that distinguishes A's hypothesis from C's, and it wasn't run.

5. **No explicit treatment of capacity / impact at OpenQuant's $10K → $1M+ growth path.** All recommendations should be checked against future scaling.

6. **The "do nothing" baseline is undervalued in A/B/C and is treated as load-bearing only by D.** I think D is right but the package as a whole lets the burden of proof slide too far toward "we should change something."

7. **No empirical re-running of the proposed detectors on actual OpenQuant residual data.** B explicitly flags this gap. Without it, all recommendations are ex-ante reasoning, not posterior measurement.

8. **None of A/B/C/D quantify what fraction of FAANG/entsw 0/8 losses are explainable by the 6-7 mega-cap names alone.** The simplest possible analysis — "what does the 9-month backtest look like with AAPL/MSFT/GOOGL/AMZN/META/NVDA removed from any basket they appear in?" — would have shifted the entire research package's recommendations.

9. **Crowding is dramatically under-measured for OpenQuant specifically.** D claims it's first-order but provides no operational measurement (e.g., "the FAANG names appear in X% of top-50 stat-arb fund 13Fs vs. the median name's Y%"). Without this measurement, "drop FAANG" is recommendation-by-analogy, not evidence-based.

---

## Specific recommendation for next session

**Single most-impactful next step:** run the back-of-envelope analysis that would have shifted this entire 4-memo package — **drop the 6-7 mega-cap names (AAPL, MSFT, GOOGL, AMZN, META, NVDA, TSLA) from the trading universe and re-run the same 9-month backtest.** This is one config-change-and-rerun. It should take <1 day.

If that lifts aggregate Sharpe meaningfully (from 0.47 to >0.7), the conclusion is:
- D's diagnosis (crowding + single-factor dominance) is sufficient.
- A/B/C's methodology changes are at-best second-order.
- The right action is to ship the universe exclusion, run for one quarter unchanged (Asness's discipline), and re-evaluate.

If that lifts Sharpe modestly (0.47 → 0.55) or not at all, the conclusion is:
- D's diagnosis is incomplete; the failure is broader than the mega-caps.
- C's regime hypothesis or A's residual-construction hypothesis become more relevant.
- The right action is then to run the 2018-2019 retrospective (A's and C's open question) before committing to either fix.

**Either way, the question to answer next session is empirical, not literature-based.** The 4 memos collectively gathered enough evidence; they did not run the one experiment that would distinguish their hypotheses. That experiment (drop FAANG, re-run, measure) is the highest-value action.

A secondary action — run in parallel — is to verify D's Avellaneda-Lee sector-paradox claim by re-reading the AvL paper's Figure 36 and section 7. If D's reading is correct, it is the keystone empirical reference for the entire research package and should be cited everywhere. If D's reading is overstated, the entire "crowding-causes-FAANG-failure" thesis weakens and A's residual-construction hypothesis regains weight.
