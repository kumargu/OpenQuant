# Chips Detector-Only Matrix

Date: 2026-05-30
Window: 2026-01-01..2026-05-27
Execution model: realistic next-open fills

## Baseline

- Current `buildout_core`: `+1.6295%`
- Current `buildout_overlay`: previously replayed at roughly `+38.41%`

## Targeted Variant Matrix

All variants were run on current code with isolated replay state.

### Basket-only core

- remove `chips` traded sleeve entirely:
  - `cum_return=+27.8804%`
  - `sharpe=2.9995`
  - `max_dd=7.9028%`
- remove `ai_power` sector:
  - `cum_return=-4.6818%`
  - `sharpe=-0.3836`
  - `max_dd=13.1755%`
- remove both `chips` and `ai_power`:
  - `cum_return=+21.4491%`
  - `sharpe=2.7590`
  - `max_dd=4.3487%`
- disable dominance gate:
  - `cum_return=+4.9688%`
  - `sharpe=0.5403`
  - `max_dd=19.0993%`

### Overlay

- keep `chips` sector present but set `chips.traded_targets = []`:
  - `cum_return=+53.1404%`
  - `sharpe=4.1618`
  - `max_dd=8.3447%`

## Interpretation

1. `chips` traded exposure is the dominant drag in `buildout_core`.
2. `ai_power` should not be removed; doing so makes core worse.
3. Fit breadth matters, but the dominance gate is not the main driver by itself.
4. The clean near-term change is:
   - preserve `chips` as a detector sector
   - disable `chips` traded targets
   - leave the rest of the buildout universe intact

## Decision

Adopt `chips` as detector-only in `config/basket_universe_buildout.toml` and
continue investigation on:

- replay-start fit breadth (`15/58` valid targets)
- whether a future redesigned chips sleeve can be reintroduced safely
