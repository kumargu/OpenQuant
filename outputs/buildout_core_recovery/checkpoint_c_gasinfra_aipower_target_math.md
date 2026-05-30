# Buildout Core Recovery: Gas Infra and AI Power Target Math

## Question

Are `gas_infra` and `ai_power` weak because the **wrong stocks** are in the
basket target sets?

This checkpoint answers that mathematically using:

- current post-fix `buildout_core` replay on `2026-01-01..2026-05-27`
- per-sleeve target and peer contribution
- replay-start valid target set from the saved engine state
- surgical replay variants that remove or swap candidate targets

Current baseline:

- `buildout_core`: `+27.88%`
- `Sharpe`: `3.00`
- `max_dd`: `7.90%`

## Gas Infra

### Current sleeve math

Selected target counts:

- `KMI`: selected `26` sessions
- `WMB`: selected `14` sessions
- `EQT`: selected `0`
- `OKE`: selected `0`

Target P&L:

- `KMI`: `-249.13`
- `WMB`: `-49.03`
- `EQT`: `+121.14`
- `OKE`: `+86.86`

Peer-only members:

- `TRGP`: `+59.15`
- `EXE`: `+1.85`

Sleeve totals:

- target-side P&L: `-90.16`
- peer-side P&L: `+61.00`
- total sleeve P&L: `-29.17`

Replay-start valid gas-infra params in the actual state:

- `gas_infra:KMI`
- `gas_infra:WMB`

There were **no** replay-start valid params for:

- `gas_infra:EQT`
- `gas_infra:OKE`

Exact replay-start fit artifact (`--as-of 2026-01-01`) confirms why:

- `KMI`
  - valid
  - dominance score `0.400`
  - ADF p-value `0.511`
  - half-life `10.23` days
  - threshold `k=0.290`
- `WMB`
  - valid
  - dominance score `0.306`
  - ADF p-value `0.523`
  - half-life `12.80` days
  - threshold `k=0.265`
- `EQT`
  - invalid
  - reject reason: `dominance gate failed: score=0.831 > 0.600`
- `OKE`
  - invalid
  - reject reason: `dominance gate failed: score=0.799 > 0.600`

Replay-start component contributions show the deeper structure:

- `EQT`
  - target contribution `+0.831`
  - this is genuine target dominance, so rejection is mathematically consistent
- `OKE`
  - target contribution `+0.799`
  - again, genuine target dominance
- `WMB`
  - target contribution `+0.306`
  - largest peer contributions: `TRGP +0.271`, `EXE +0.214`
- `KMI`
  - target contribution `-0.097`
  - largest peer contributions: `TRGP +0.400`, `EXE +0.343`

This is the important generic finding:

- the current dominance gate is **not target-centric**
- it limits the largest absolute component contribution, regardless of whether
  that component is the target or a peer
- so `KMI` is valid even though the spread is mathematically driven more by
  peer components than by the target itself

### Causal replay tests

1. Remove gas-infra traded targets entirely:

- result: `+21.15%`
- change vs baseline: `-6.73 pts`

2. Keep only `EQT` and `OKE` as traded targets:

- result: `+21.15%`
- same as detector-only
- replay-start valid gas-infra params: none

### Finding

The obvious “wrong stocks” intuition is only partly right.

What is true:

- the **traded target choices that actually fired** (`KMI`, `WMB`) were bad
- the **non-selected alternatives** (`EQT`, `OKE`) look better on constituent P&L

What is not true:

- removing the whole sleeve does **not** improve the core book
- swapping to `EQT`/`OKE` under the current gate does **not** work, because those
  targets do not become replay-start valid baskets

### Classification

This does **not** point to “wrong sector members.”

It points to:

- target-expression / fit-validity problem
- likely interaction between target choice and dominance / fit admissibility
- not a clean case for deleting symbols from the basket membership list

## AI Power

### Current sleeve math

Selected target counts:

- `NEE`: selected `94` sessions
- `NRG`: selected `63` sessions
- `CEG`: selected `0`
- `VST`: selected `0`

Target P&L:

- `NEE`: `-409.19`
- `NRG`: `+130.95`
- `CEG`: `+99.77`
- `VST`: `-28.97`

Peer-only members:

- `EXC`: `+140.14`
- `ETR`: `+50.53`

Sleeve totals:

- target-side P&L: `-207.44`
- peer-side P&L: `+190.66`
- total sleeve P&L: `-16.78`

Replay-start valid ai-power params in the actual state:

- `ai_power:NEE`
- `ai_power:NRG`

Exact replay-start fit artifact (`--as-of 2026-01-01`) shows:

- `NEE`
  - valid
  - dominance score `0.405`
  - ADF p-value `0.264`
  - half-life `4.27` days
  - threshold `k=0.290`
- `NRG`
  - valid
  - dominance score `0.519`
  - ADF p-value `0.060`
  - half-life `2.77` days
  - threshold `k=0.309`
- `CEG`
  - invalid
  - reject reason: `dominance gate failed: score=0.828 > 0.600`
- `VST`
  - invalid
  - reject reason: `dominance gate failed: score=1.265 > 0.600`

Replay-start component contributions show:

- `CEG`
  - target contribution `+0.828`
  - genuine target dominance, so rejection is mathematically consistent
- `VST`
  - target contribution `+1.265`
  - even stronger target dominance
- `NRG`
  - target contribution `+0.519`
  - largest peer contribution: `VST +0.258`
- `NEE`
  - target contribution `+0.118`
  - largest peer contribution: `VST +0.405`

So `NEE` is valid **not** because the target is strong inside the spread, but
because no single component breaches the global `0.600` limit. The basket is
mathematically peer-influenced more than target-driven.

### Causal replay tests

1. Remove `NEE` from the traded target set:

- result: `+22.40%`
- change vs baseline: `-5.48 pts`

Replay-start valid ai-power params after the change:

- `ai_power:NRG` only

2. Keep `NRG` as the only traded target:

- result: `+22.40%`
- same as removing `NEE`

### Finding

`NEE` is a bad **direct target P&L** contributor, but it is **not** a simple
bad stock to remove from the basket design.

Mathematically:

- `NEE` target P&L is strongly negative
- but removing `NEE` still makes the total core book worse

That means the sleeve is not failing because “NEE is the wrong stock” in a
simple membership sense. `NEE` still contributes useful fit structure,
competition, or hedge interaction to the overall capped portfolio.

### Classification

This points to:

- sleeve-level target-expression quality
- overlap / interaction with `utilities`
- and possibly target-selection frequency

It does **not** justify deleting `NEE` from the theme by hand.

## Overall Conclusion

The next decision should **not** be:

- remove `NEE`
- replace `KMI/WMB` with `EQT/OKE`
- or rewrite the sector member lists by hand

The mathematical conclusion is narrower:

1. `gas_infra`
   The currently selected targets are poor, but the sleeve itself is still
   helpful to the whole book. The problem is **which gas-infra baskets are
   fit-valid and selected**, not the existence of gas-infra as a theme.

2. `ai_power`
   `NEE` looks bad on direct target attribution, but removing it hurts core.
   The problem is **not** simply “wrong stock in basket.”

3. Generic gate implication
   The current dominance gate is solving one problem well:
   - reject baskets where one component completely dominates the spread

   But it does **not** solve a second problem:
   - accepted baskets where the target itself is only a weak or even negative
     variance contributor, and peers do most of the work

   That suggests the next mathematical hypothesis is not “remove bad stocks,”
   but:
   - add a **target-centrality diagnostic** or threshold alongside the existing
     max-component dominance gate

Broader replay-start valid-basket ranking supports this as a generic issue, not
just a gas-infra / ai-power anomaly. Examples of valid but peer-dominated
baskets at `2026-01-01`:

- `hc_providers:ELV`
  - target contribution `0.021`
  - max peer contribution `0.436`
- `utilities:D`
  - target contribution `0.008`
  - max peer contribution `0.361`
- `gas_infra:KMI`
  - target contribution `0.097`
  - max peer contribution `0.400`
- `ai_power:NEE`
  - target contribution `0.118`
  - max peer contribution `0.405`
- `banks_regional:TFC`
  - target contribution `0.107`
  - max peer contribution `0.233`

So the next mathematical test should be generic:

- does requiring some minimum target-centrality improve the capped core book?

## Tooling Note

To make this exact instead of heuristic, the runner now supports:

- `openquant-runner freeze-basket-fits --as-of YYYY-MM-DD`

That command builds a frozen fit artifact using data strictly before the given
date, which lets us inspect replay-start fit validity directly.

## Plain-English Finding

For both sleeves, the evidence points away from “wrong stocks in the basket”
and toward “wrong traded expression of the theme.”

That means the next mathematical step should be:

- inspect replay-start fit validity and dominance behavior **within** these
  sleeves
- explain why the engine makes `KMI/WMB` tradable but not `EQT/OKE`
- explain why `NEE` remains structurally useful despite bad direct target P&L

The next mathematical step after this report should be:

- test a generic **target-centrality** filter or score
- ask whether accepted baskets should require the target to contribute at least
  some minimum share of spread variance
- verify that on the full core replay before changing any sleeve by hand
