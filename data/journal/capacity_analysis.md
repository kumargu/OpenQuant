# Capacity Analysis: Why $26/day is the ceiling on $10K

## Finding

With 13,481 S&P 500 same-sector pairs, there are **183 actionable signals per day** 
(|z|>1.0, R²≥0.70, HL≤5d, ADF≤-2.5). The strategy is NOT signal-limited.

## The constraint is CAPITAL, not signals

- $10K capital, $5K/leg = 1 trade at a time
- Avg hold: 5 days
- 22 trading days / 5 days per trade = 4.4 trades max per month
- Avg P&L per trade: ~$50-100 (0.5-1.0% on $10K)
- 4 trades × $75 avg = $300/month = **$14/day**

We got $26/day because some trades were winners at +$100-230, pulling the average up.

## Signal funnel (Day 273, typical)

- 13,481 total pairs
- 1,223 pass basic scan
- 407 pass quality gate (R²≥0.70, HL≤5, ADF≤-2.5)
- 204 have |z|>1.0 (actionable signal)
- We can trade: 1 (capital constrained)

## To hit $100/day

$100/day at 1% per trade = $10K per trade = need $10K capital that's FULLY deployed.
But trades take 5 days on average, so we need 5 × $10K = $50K to always have a trade running.

Alternatively: 2-3 concurrent trades at $5K each = $10-15K per trade = need $30-50K.

## Conclusion

The strategy works. The pairs selection works. The priority scoring works.
$10K is simply too small to generate $100/day with market-neutral pairs trading.

$50K → ~$130/day (at current 0.26%/day RoCC)
$100K → ~$260/day
