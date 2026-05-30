# Checkpoint H: CVX and UNH NEE-Like Tests

Date: 2026-05-30

## Goal

Test whether `energy:CVX` and `hc_providers:UNH` are true **NEE-like** targets:

- bad direct target P&L
- but still useful to the capped basket portfolio

## Baseline

Current causal `buildout_core` replay:

- `cum_return=0.278804`
- `sharpe=2.999496`
- `max_dd=0.079028`

## Variant Results

### Remove `CVX` from `energy.traded_targets`

Replay result:

- `cum_return=0.172565`
- `sharpe=1.741676`
- `max_dd=0.117714`

Effect versus baseline:

- return drops by about `10.62` percentage points
- drawdown gets materially worse

Interpretation:

- `CVX` is a **strong NEE-like target**
- even though its direct target P&L is bad, removing it damages the capped book

Main replacements when `CVX` is removed:

- `gas_infra:WMB` enters `15` extra days
- `banks_regional:TFC` enters `8` extra days
- `hc_providers:CNC` enters `7` extra days
- `utilities:NEE` enters `4` extra days
- `utilities:AEP` enters `4` extra days
- `gas_infra:KMI` enters `4` extra days

This is consistent with the earlier sleeve work:

- `gas_infra` was a weak sleeve
- letting more `gas_infra` and marginal utility entries in is worse than
  carrying `CVX`

### Remove `UNH` from `hc_providers.traded_targets`

Replay result:

- `cum_return=0.259347`
- `sharpe=2.809403`
- `max_dd=0.108980`

Effect versus baseline:

- return drops by about `1.95` percentage points
- drawdown worsens

Interpretation:

- `UNH` is also NEE-like, but **much weaker than CVX**
- it still helps the capped portfolio, but the benefit is modest

Main replacements when `UNH` is removed:

- `gas_infra:KMI` enters `10` extra days
- `hc_providers:CNC` enters `9` extra days
- `gas_infra:WMB` enters `5` extra days
- `utilities:D` enters `4` extra days

Again, the replacements skew toward weaker or more crowded alternatives.

## Mathematical Reading

### `CVX`

- bad direct target P&L
- but clearly strong **portfolio value**
- most likely serving as a stabilizing cap winner that blocks weaker
  `gas_infra` / marginal utility substitutions

### `UNH`

- bad direct target P&L
- still slightly positive in portfolio value terms
- likely a weaker cap-allocation winner rather than a powerful standalone basket

## Conclusion

The generic detector is working:

- `NEE` was a real class example
- `CVX` is another strong member of that class
- `UNH` is a weaker member

So the next level of the system should not ask:

- “which targets have bad direct P&L?”

It should ask:

- “which bad-looking targets still improve capped portfolio economics by
  blocking weaker replacements?”

## Next Step

The next mathematical tool should be a **marginal replacement value score**:

- for each selected basket
- compare the realized economics of that selected basket
- against the strongest excluded basket that would have entered instead

That is the generic way to find more `NEE` / `CVX` type entries and improve the
cap-ranking rule without hand-tuning sleeves.
