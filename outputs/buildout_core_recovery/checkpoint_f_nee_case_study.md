# Checkpoint F: NEE Case Study

Date: 2026-05-30

## Question

Why does keeping `NEE` help `buildout_core` even though `NEE` itself has strongly
negative direct target P&L?

## Main Result

There are **two different `NEE` stories**:

1. `utilities:NEE` looks mathematically legitimate as a **basket expression**
2. `ai_power:NEE` looks more like a **cap-allocation / ranking winner** than a
   clean standalone sleeve winner

Those should not be treated as the same thing.

## Evidence

### 1. Utilities basket: negative target, but non-fictional basket math

From [`utilities_analysis/constituent_summary.csv`](/Users/gulshan/OpenQuant/outputs/buildout_core_recovery/current_core_ytd/utilities_analysis/constituent_summary.csv):

- `NEE`: `-515.46`
- `D`: `+331.595`
- `EXC`: `+75.055`
- `SO`: `+42.86`
- `DUK`: `+35.24`
- `AEP`: `+32.41`

Net utilities constituent total:

- about `+1.70`

Utilities exposure episodes from [`utilities_analysis/exposure_episodes.csv`](/Users/gulshan/OpenQuant/outputs/buildout_core_recovery/current_core_ytd/utilities_analysis/exposure_episodes.csv):

- early short episodes were mildly negative
- long main episode `2026-01-22..2026-05-27`: `+54.61`

Interpretation:

- `NEE` is a bad standalone leg
- but the **utilities basket is not fictional**
- the sleeve remains roughly flat-to-positive because the rest of the spread
  offsets the `NEE` drag

### 2. Ai-power basket: direct sleeve math is weaker

From [`ai_power_analysis/constituent_summary.csv`](/Users/gulshan/OpenQuant/outputs/buildout_core_recovery/current_core_ytd/ai_power_analysis/constituent_summary.csv):

- `CEG`: `+61.22`
- `VST`: `-32.67`
- `NEE`: `-515.46`
- `NRG`: `+88.855`
- `EXC`: `+75.055`
- `ETR`: `+22.285`

Net ai-power constituent total:

- about `-300.715`

Interpretation:

- `ai_power:NEE` does **not** look like a clean positive sleeve expression on
  direct constituent P&L
- yet removing it still hurts total core replay

So the value of `ai_power:NEE` is likely not “this sleeve is great in isolation.”
It is more likely:

- the basket wins scarce cap slots against even worse alternatives
- or it changes which other baskets get crowded out

### 3. Overlap deletion confirms that naive removal is wrong

Replay variants:

- baseline core: `+27.88%`
- remove `utilities:NEE`: `+12.26%`
- remove both `NEE` target expressions: `+12.02%`

Interpretation:

- overlap deletion is not the fix
- one or both `NEE` expressions are doing useful work in the capped book

### 4. Cap-allocation evidence

In the baseline causal replay:

- `ai_power:NEE` selected `94` days
- `utilities:NEE` selected `55` days
- both selected together `54` days

Even with that, removing `utilities:NEE` still hurts badly. This means:

- `utilities:NEE` is not just duplicate clutter
- it is surviving because the cap/ranking process often still prefers it to the
  next-best alternatives

One concrete crowding clue:

- there are `28` days where `utilities:NEE` is selected while
  `hc_providers:UNH` is excluded

So some of the NEE story is not about `NEE` itself. It is about which other
baskets get displaced when `NEE` is present.

## Conclusion

The mathematically correct lesson is:

- do **not** judge `NEE` from standalone target P&L
- split the problem into:
  - **basket-expression value** for `utilities:NEE`
  - **cap-allocation / displacement value** for `ai_power:NEE`

This is exactly the kind of thing the company should learn generically:

- some entries are worth keeping because the **basket math** is valid
- others may be worth keeping because, under a cap, they still beat the
  available alternatives

## Next Step

The next quantitative test should be **marginal replacement value**:

- on each day `NEE` is selected, compare the realized next-day or hold-window
  economics of that selected `NEE` basket against the best excluded alternative
  it displaced

That will tell us whether:

- `NEE` is intrinsically good basket math
- or whether the book is simply starved for better capped alternatives
