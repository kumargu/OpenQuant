# Buildout Core Recovery: Step 0 + Checkpoint A

Date: 2026-05-30
Window: 2026-01-01..2026-05-27
Universe: `config/basket_universe_buildout.toml`
Mode: `buildout_core` (`--disable-leadership-overlay`)

## Baseline

- Replay report: `outputs/buildout_core_recovery/current_core_ytd/report.tsv`
- Result:
  - `cum_return=+1.6295%`
  - `ann_return=+7.6863%`
  - `sharpe=0.2851`
  - `max_dd=16.2692%`

## Step 0: Bar Integrity Audit

Audited source cache:

- `/Users/gulshan/quant-data/bars/v3_sp500_2024-2026_1min_adjusted`

Hand-picked symbols:

- `NVDA`, `AMD`, `MU`
- `NEE`, `NRG`, `EXC`
- `WMB`, `KMI`
- `CNC`, `ELV`
- `AAPL`, `META`

What was checked:

- parquet exists and is readable
- timestamps strictly increasing
- no duplicate in-session minutes on sampled days
- OHLC internal consistency (`high >= max(open, close, low)`, `low <= min(...)`)
- no NaN / inf / negative volume rows in sampled names
- first RTH minute and last RTH minute presence on key late-May dates

Conclusion:

- No evidence of wholesale 2026 cache corruption.
- No broken parquet files were found in the sampled buildout names.
- No refill was performed.

Important nuance:

- The cache includes extended-hours rows. Raw daily row counts are therefore not expected to be exactly `390`.
- The replay path filters to NYSE RTH correctly, so extended-hours presence is not itself a bug.

Useful data-quality finding:

- Several buildout names are sparse on the IEX minute feed during RTH, but they still usually have the session close minute.
- Example late-May RTH coverage:
  - `AAPL`, `NVDA`: full `390/390`
  - `AMD`, `META`, `MU`, `NEE`: mostly usable, small intra-day gaps
  - `NRG`, `CNC`, `ELV`: materially sparse intra-day, but still have the close minute on sampled late-May dates

Interpretation:

- This is a feed-density issue, not a corruption issue.
- It can matter for execution realism and sleeve quality, but it does not justify deleting and refilling the whole 2026 cache.

## Checkpoint A: Attribution

### 1. Churn is high

From `basket_session_closes` in:

- `outputs/buildout_core_recovery/current_core_ytd/journal.sqlite3`

Findings:

- `100/100` sessions submitted accepted orders
- average accepted orders per session: `5.75`
- average order gross notional per session: `$11,662.68`
- average admitted baskets per session: `5.00`
- selected basket set changed on only `52/99` day-to-day transitions
- average selected-basket Jaccard overlap: `0.794`

Interpretation:

- The book is trading every day even though the selected basket set is often fairly similar from one day to the next.
- A meaningful part of weakness is likely resize churn, not just basket identity churn.

### 2. The replay-start fitted universe is much narrower than the TOML suggests

Based on replay state / usage analysis:

- `58` configured traded targets
- only `15` targets valid in the replay-start fitted state

Sector usage summary:

- `chips`: `1/5` valid target fits, only `NVDA` active/admitted
- `hc_providers`: `3/6` valid target fits
- `energy`: `1/6` active admitted target in practice (`CVX`)
- `entsw`: active target `ADBE` but never admitted
- `utilities`: `3/6` valid target fits
- `banks_regional`: `1/6` valid target fits
- `faang`: no target usage in basket-only core
- `insurance`: `1/6` active admitted target in practice (`MET`)
- `electrification`: no valid or active target usage
- `ai_power`: `2/4` valid target fits (`NEE`, `NRG`)
- `gas_infra`: `2/4` valid target fits (`WMB`, `KMI`)
- `cyc_materials`: no valid or active target usage

Interpretation:

- `buildout_core` is structurally narrower and more concentrated than the configuration file implies.
- This is not merely a runtime-trading problem; the fitted opportunity set is small before trading begins.

### 3. Sleeve contribution is highly concentrated

Sector constituent attribution outputs live under:

- `outputs/buildout_core_recovery/current_core_ytd/*_analysis/`

Important caveat:

- These sector totals are close-to-close mark-to-market of held member exposure.
- They are useful for ranking drag sources, but totals can overlap where symbols belong to multiple conceptual lenses (`NVDA`, `NEE`).

Largest drags:

- `chips`: `-$2,255.53`
- `ai_power`: `-$300.72`
- `utilities`: roughly flat at `+$1.70`

Largest offsets:

- `hc_providers`: `+$868.35`
- `faang`: `+$592.20` (driven by overlapping `NVDA` exposure, not standalone FAANG target usage)
- `energy`: `+$201.80`
- `banks_regional`: `+$187.54`

### 4. Chips is the clearest sleeve-level problem

Top selected basket:

- `chips:NVDA:...` selected on `99/100` sessions

Observed held exposure in chips:

- long `NVDA`
- short peers such as `AMD`, `INTC`, `MU`, `ADI`

Member contribution summary:

- `NVDA`: `+$592.20`
- `INTC`: `-$972.50`
- `MU`: `-$853.93`
- `AMD`: `-$552.93`
- `ADI`: `-$289.71`
- `AVGO`: `-$178.67`

Interpretation:

- The chips spread is not failing because the target never works.
- It is failing because the peer short basket overwhelms the target gain.
- That makes chips the first sleeve to dissect in depth.

### 5. Basket admissions are concentrated in a small set of names

Most frequently selected baskets:

- `chips:NVDA`: `99`
- `ai_power:NEE`: `93`
- `ai_power:NRG`: `56`
- `utilities:NEE`: `51`
- `energy:CVX`: `45`
- `hc_providers:UNH`: `38`
- `banks_regional:TFC`: `31`

Interpretation:

- The buildout core is effectively being driven by a small recurring roster, not by the full configured universe.

## Ranked Causes After Checkpoint A

1. Sleeve quality problem in `chips`, with `ai_power` as the next drag.
2. Very narrow fitted opportunity set at replay start (`15/58` valid targets).
3. Excessive daily resize churn relative to how stable the admitted basket set actually is.

## Next Focus

1. Deep-dive the `chips` sleeve under current realistic next-open execution.
2. Explain why the replay-start fitted universe is only `15/58` targets and whether that should be improved at fit/admission level.
3. Measure whether resize churn can be reduced without reintroducing post-hoc target mutation.
