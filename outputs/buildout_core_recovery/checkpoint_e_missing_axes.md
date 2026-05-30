# Checkpoint E: What We Were Still Missing

Date: 2026-05-30

## Main Findings

1. Duplicate-target overlap was not the next fix.
   - Baseline `buildout_core` on the current checked-in buildout config:
     - `cum_return=0.278804`
   - Removing `NEE` from `utilities.traded_targets`:
     - `cum_return=0.122604`
   - Removing both `NEE` target expressions:
     - `cum_return=0.120243`
   - Conclusion:
     - the duplicate `NEE` target is not a simple redundancy bug
     - deleting overlap mechanically harms the basket book

2. The checked-in frozen fit artifact is not the replay contract.
   - Replay path in [`engine/crates/runner/src/main.rs`](/Users/gulshan/OpenQuant/engine/crates/runner/src/main.rs)
     builds a replay fit in memory from data strictly before `--start`.
   - Evidence:
     - replay succeeds from a universe TOML with **no adjacent `.fits.json`**
     - selected basket IDs still reflect the replay-start `frozen_at`
   - Conclusion:
     - fit-validity analysis must be tied to the replay-start in-memory fit path
     - the checked-in `config/basket_universe_buildout.fits.json` is relevant to
       live/paper startup, but not to replay selection

3. Frozen-fit timing is not the current performance driver.
   - A causal replay-start universe frozen at `2026-01-01` reproduced the same
     replay P&L as the current checked-in buildout config:
     - core: `cum_return=0.278804`
     - overlay: `cum_return=0.531404`
   - Conclusion:
     - the earlier `2026-05-21` stamp on basket IDs was confusing, but not the
       reason the post-fix core looks strong

4. Cap pressure is still extreme and persistent.
   - Across the current causal `buildout_core` replay:
     - average `active_baskets = 12.82`
     - average `excluded_baskets = 7.82`
     - max `active_baskets = 14`
     - max `excluded_baskets = 9`
   - Most frequently excluded baskets:
     - `gas_infra:WMB` excluded `84` days
     - `entsw:ADBE` excluded `80` days
     - `gas_infra:KMI` excluded `74` days
     - `insurance:MET` excluded `67` days
     - `utilities:AEP` excluded `65` days
     - `utilities:D` excluded `63` days
   - Most frequently selected baskets:
     - `ai_power:NEE` selected `94` days
     - `ai_power:NRG` selected `63` days
     - `utilities:NEE` selected `55` days
     - `energy:CVX` selected `51` days
     - `hc_providers:UNH` selected `40` days
     - `banks_regional:TFC` selected `38` days
   - Conclusion:
     - the remaining problem is likely not “too few valid baskets”
     - it is “which five survive the cap, and how redundant are they?”

## What This Leaves

After bounding:

- chips traded-spread construction
- blanket target-centrality rules
- duplicate-target overlap deletion
- checked-in frozen-fit timing

the most likely remaining missing factor is **cap selection quality**.

The current replay repeatedly compresses roughly `10-14` admitted baskets into
the `5`-slot active cap. That means the next mathematical question is not:

- “is this one stock wrong?”

It is:

- “is the selected basket set too redundant under the cap?”
- “are excluded baskets systematically better than selected baskets?”
- “is the ranking rule choosing the wrong five when the admitted set is crowded?”

## Next Step

Build a cap-selection attribution pass that compares:

- selected baskets vs excluded baskets
- basket-set redundancy / overlap among the selected five
- realized basket economics of excluded names that were crowded out

The goal is to determine whether the next improvement should be:

- a diversity / crowding-aware admission rule
- a better cap-ranking score
- or a narrower candidate universe if the extra admitted baskets are mostly noise
