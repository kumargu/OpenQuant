# Checkpoint G: NEE-Like Detector

Date: 2026-05-30

## Question

If `NEE` can be useful even with bad direct target P&L, how do we find **other**
names like that mathematically?

## Definition

A **NEE-like** name is not:

- “a stock we like”

It is:

- a target with weak standalone target P&L
- that still improves either:
  - the net basket expression
  - or the capped portfolio by displacing worse alternatives

## Generic Detector

For each target basket, compute:

1. **Direct target drag**
   - target selected days
   - target direct P&L
   - target P&L on portfolio-down days

2. **Basket-offset support**
   - sum of non-target constituent P&L inside the sleeve
   - sleeve episode P&L while the basket is active
   - target-vs-peer contribution split

3. **Cap-allocation value**
   - selected days under the active cap
   - excluded days for close substitutes
   - marginal replacement value versus the best excluded basket on those days

4. **Crowding role**
   - whether the same target appears in multiple sleeves
   - whether it survives because it is intrinsically good basket math or because
     it wins a crowded ranking contest

## What NEE Taught Us

### `utilities:NEE`

- direct target P&L is bad
- but the utilities sleeve still offsets that drag well enough to remain roughly
  flat-to-positive
- this is a real example of:
  - **bad target leg**
  - **acceptable basket**

### `ai_power:NEE`

- direct sleeve math is weaker
- it still survives selection frequently
- this suggests its value is more likely:
  - cap-ranking / displacement value
  - not pure standalone sleeve quality

## Current Candidate Scan

From the current post-fix core replay, the strongest high-frequency negative-leg
candidates are:

- `utilities:NEE`
- `ai_power:NEE`
- `energy:CVX`
- `hc_providers:UNH`

Important distinction:

- `NEE` is **already proven** to matter because removal tests were run
- `CVX` and `UNH` are only **candidates so far**
- they are not yet proven NEE-like until we run the same marginal removal /
  replacement tests

## Immediate Next Test

For each candidate:

1. remove that target expression only
2. replay under the same causal setup
3. compare:
   - total core return
   - drawdown
   - which baskets enter when the candidate disappears

That will separate:

- **true NEE-like targets** that add portfolio value despite bad direct P&L
- from **fake NEE-like targets** that simply look important because they trade a lot
