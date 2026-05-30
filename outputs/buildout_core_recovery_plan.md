# Buildout Core Recovery Plan

## Goal

Make `buildout_core` reliably positive under the current realistic replay contract:

- decisions at session close
- fills at next regular-session open
- no dependence on the leadership overlay to rescue a weak basket book

This plan treats `buildout_overlay` as a separate success layer. The first job is to make the basket book itself healthy.

## Current Baseline

As of 2026-05-30:

- current checked-in `buildout_core`, `2026-01-01..2026-05-27`:
  - `cum_return=0.278804`
  - `sharpe=2.999496`
  - `max_dd=0.079028`
  - replay report:
    `/Users/gulshan/OpenQuant/outputs/buildout_core_recovery/post_chips_detector_only/core/report.tsv`
- current checked-in `buildout_overlay`, same window:
  - `cum_return=0.531404`
  - `sharpe=4.161767`
  - `max_dd=0.083447`
  - replay report:
    `/Users/gulshan/OpenQuant/outputs/buildout_core_recovery/post_chips_detector_only/overlay/report.tsv`
- prior gate-era reference on older replay assumptions:
  - materially better than current core
  - not directly apples-to-apples because later replay work removed optimistic same-close fills

## North Star

Primary metric:

- increase `buildout_core` cumulative return on realistic replay

Guardrails:

- do not worsen max drawdown materially without a strong return gain
- track Sharpe, turnover, order count, and gross notional behavior
- do not mix overlay changes into core diagnosis

## Reasoning Rule

After every completed step or checkpoint:

- stop and write the plain-English finding
- distinguish outright sector strength from spread quality
- state whether the evidence points to:
  - data quality
  - admission quality
  - sleeve construction
  - execution sensitivity
  - or a broader engine issue
- only then choose the next step

Decision discipline:

- do not let a better replay number by itself decide the direction
- explain *why* the number moved
- prefer sleeve-local conclusions over global conclusions when the evidence is concentrated

## Investigation Order

### 0. Bar Integrity Audit

Question:

- are the minute bars feeding replay internally consistent and free of obvious corruption?

Why this comes first:

- if bar data is wrong, every admission, sleeve, and execution conclusion can be false

Method:

- hand-pick a small set of symbols across the most important buildout sleeves
- include both winners and losers
- include a few names that showed suspicious behavior in replay or paper
- verify selected days by checking:
  - open/high/low/close consistency
  - split-adjustment sanity
  - missing-minute / duplicate-minute anomalies
  - first regular-session minute and close minute correctness
  - cross-source spot checks for a few exact timestamps

Suggested starter set:

- `NVDA`, `AMD`, `MU`
- `NEE`, `NRG`, `EXC`
- `WMB`, `KMI`
- `CNC`, `ELV`
- `AAPL`, `META`

Deliverables:

- one short integrity note per symbol/day checked
- list of confirmed-good bars
- list of suspicious files or timestamps, if any

Decision rule:

- if any corruption is found, stop strategy diagnosis and repair data first

### 1. Churn And Turnover Attribution

Question:

- is the core book mostly losing because it trades too much?

Deliverables:

- turnover by day
- order count by day
- gross notional change by day
- realized drag estimate from delayed next-open execution

Decision rule:

- if churn explains most of the weakness, target trading cadence and transition logic first

### 2. Admission Quality Audit

Question:

- are we admitting weak baskets while better baskets are excluded?

Deliverables:

- every admitted basket in 2026 YTD
- forward P&L after admission
- hold-path P&L while active
- comparison against excluded baskets on the same day

Decision rule:

- if admitted baskets are weak on entry, fix basket admission before touching execution policy

### 3. Target/Peer Construction Audit

Question:

- are some buildout sleeves structurally bad even before execution?

Check:

- weak target names
- poor peer sets
- overly correlated peers
- sleeves that mostly create hedge noise instead of clean spreads

Deliverables:

- per-sleeve contribution
- per-sleeve churn
- per-sleeve hit rate

Decision rule:

- if a small number of sleeves are dragging the book, fix or remove those sleeves before changing global logic

### 4. Execution Sensitivity Audit

Question:

- how much of the remaining weakness is caused by realistic fills rather than bad signals?

Replay matrix:

- old optimistic close-fill contract
- current next-open contract
- same target logic and same date range

Decision rule:

- if the strategy collapses only under realistic fills, the book is too execution-sensitive and must trade less or hold stronger signals longer

### 5. Minimal Fixes Only

Only after attribution:

- if churn is the problem: reduce churn
- if admission quality is the problem: tighten admissions
- if sleeves are the problem: fix sleeve construction
- if execution sensitivity is the problem: reduce transition frequency or make entries more selective

Rule:

- test one change at a time
- no broad rewrites
- no post-hoc portfolio mutation layers like the removed band

## Checkpoints

### Checkpoint A

Produce one attribution table with:

- churn cost
- admission quality
- sleeve quality
- execution realism cost

Exit condition:

- we can rank the top two causes of weak core performance

### Checkpoint B

Implement the smallest high-value fix for the top cause.

Exit condition:

- realistic `buildout_core` improves on the same YTD replay

### Checkpoint C

Re-test `buildout_overlay` after the core fix.

Exit condition:

- core improves without breaking overlay behavior

## Anti-Goals

Do not:

- optimize around one historical artifact like the old `17%` run without causal reproduction
- rely on overlay to hide a weak basket book
- add features that mutate the target book after construction without re-optimizing the portfolio
- mix multiple strategy ideas into one replay and call the result “improvement”

## Next Concrete Task

After the chips detector-only fix, audit replay-start fit breadth:

- explain why the replay-start state still contains only a small fitted basket set
- separate:
  - targets removed intentionally
  - targets invalid because of dominance gate
  - targets invalid because OU fit itself failed
- decide whether the next improvement should come from:
  - fit/admission reform
  - sleeve redesign in a second sector
  - or leaving the narrower fitted universe alone
