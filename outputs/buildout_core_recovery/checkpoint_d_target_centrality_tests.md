# Buildout Core Recovery: Target Centrality Tests

## Goal

Test whether the new target-centrality signal should be used as:

1. a hard fit gate, or
2. a soft admission-ranking penalty under the active-basket cap.

Baseline:

- current `buildout_core`: `+27.88%`
- `Sharpe`: `3.00`
- `max_dd`: `7.90%`

## New Math Added

The fit artifact now preserves:

- `dominance_score`
- `dominance_contributions` for every component

including dominance-gate rejects.

The runner also now supports:

- `freeze-basket-fits --as-of YYYY-MM-DD`

so replay-start fit validity can be inspected exactly.

## Hard Gate Sweep

Added an optional validator gate:

- `target_centrality_gate_enabled`
- `target_centrality_min`

Interpretation:

- reject a basket if the target's absolute spread-variance contribution is too small

### 2026 YTD replay results

- `target_centrality_min=0.05`: `+24.30%`, `Sharpe 2.57`, `max_dd 8.08%`
- `target_centrality_min=0.10`: `+6.69%`, `Sharpe 0.75`, `max_dd 11.58%`
- `target_centrality_min=0.12`: `+18.37%`, `Sharpe 2.00`, `max_dd 6.91%`
- `target_centrality_min=0.15`: `+18.37%`, `Sharpe 2.00`, `max_dd 6.91%`
- `target_centrality_min=0.20`: `+16.85%`, `Sharpe 1.76`, `max_dd 10.32%`

### Finding

Every tested hard threshold underperformed the current baseline.

Reason:

- the gate removes not only problematic peer-dominated baskets like `KMI` and `NEE`
- it also removes useful baskets like `ELV` and `D`
- so blanket target-centrality rejection is too blunt

## Soft Admission Ranking Test

Added a new admission score option:

- `signal-score-target-centrality`

Interpretation:

- rank active baskets by `abs(signal_score * target_centrality_abs)`

instead of raw `abs(signal_score)`.

### 2026 YTD replay result

- baseline `signal-score`: `+27.88%`, `Sharpe 3.00`, `max_dd 7.90%`
- `signal-score-target-centrality`: `+19.78%`, `Sharpe 1.84`, `max_dd 9.13%`

Selection shifted toward:

- more `gas_infra`
- more `insurance`
- more `entsw`
- less `energy`
- less `banks_regional`

### Finding

Soft centrality weighting also hurts.

Reason:

- target centrality alone is not a sufficient proxy for basket quality
- some peer-dominated baskets still help the capped book
- blanket downranking distorts the admission mix away from stronger sleeves

## Overall Conclusion

Target centrality is a **useful diagnostic**, but not a good standalone rule.

What the math supports:

- keep centrality in the artifact and observability stack
- use it to explain why a basket behaves the way it does
- use it to generate hypotheses

What the math does **not** support:

- hard reject all low-centrality baskets
- or mechanically downrank them under the cap

## Next Step

Use target centrality as one feature in a more complete basket-quality model,
not as a one-dimensional filter.

The next mathematical question should be:

- why do some peer-dominated baskets still help total core P&L while others hurt?
- which additional variables separate them?

Likely candidates:

- ADF strength
- half-life
- threshold quality
- sector overlap / cap crowding
- realized sleeve contribution
