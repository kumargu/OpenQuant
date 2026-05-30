# Chips Sleeve Reasoning

Date: 2026-05-30

## Question

Why did the chips sleeve hurt even though semis were strong outright?

## Plain-English Finding

The problem was not "chips are bad."

The problem was:

- the basket book traded a **spread**
- not a long-only semiconductor sleeve

Under the original buildout configuration, the only effective chips basket in replay was:

- long `NVDA`
- short peers such as `AMD`, `INTC`, `MU`, `ADI`, `AVGO`

That spread lost money even while semis were strong overall.

## Evidence

### 1. Chips was the largest negative sleeve

From current realistic-fill core attribution:

- total chips sleeve contribution: about `-$2,255.53`

Member-level contribution:

- `NVDA`: about `+$592.20`
- `INTC`: about `-$972.50`
- `MU`: about `-$853.93`
- `AMD`: about `-$552.93`
- `ADI`: about `-$289.71`
- `AVGO`: about `-$178.67`

### 2. The replay-start fit set only admitted one chips target

In the replay-start state, chips contributed only:

- `chips:NVDA:2026-05-21:6b8cb6c3`

All other configured chips traded targets were rejected.

### 3. The valid chips fit already looked structurally weak as a mean-reversion basket

For the valid `NVDA` fit:

- `adf_pvalue = 0.8932`
- `half_life_days = 90.81`
- `threshold_k = 0.1507` (at the clip floor)

Interpretation:

- this does not look like a convincing mean-reverting spread
- it looks more like a persistent leader-vs-pack relationship
- that is dangerous when the strategy then shorts the rest of the sector against the leader

### 4. Removing chips traded exposure fixed the core and improved overlay too

Current realistic replay matrix:

- baseline `buildout_core`: `+1.63%`
- no `chips` traded sleeve: `+27.88%`
- no `ai_power`: `-4.68%`
- no `chips` and no `ai_power`: `+21.45%`
- no dominance gate: `+4.97%`

Overlay:

- chips detector-only buildout overlay: `+53.14%`

## Reasoned Conclusion

This is best explained as a **sleeve construction problem**, not a general basket-engine failure.

Why:

- outright semiconductor strength was real
- the spread construction was the wrong expression of that strength
- removing traded chips exposure improved both basket-only core and overlay
- other sleeves remained useful, so the failure was concentrated rather than universal

## Action Taken

Kept `chips` as a detector sector for overlay logic, but set:

- `chips.traded_targets = []`

in:

- `/Users/gulshan/OpenQuant/config/basket_universe_buildout.toml`

## Next Question

If chips ever comes back as a traded sleeve, it should only do so after a new design answers:

1. Should chips be traded as a spread at all?
2. If yes, should it be long-only relative strength instead of long-target/short-peer?
3. If it remains a spread, how should peers be chosen so the sleeve does not short broad semi strength?
