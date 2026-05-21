# Regime Overlay Playbook

This note captures the reusable lesson from the `buildout` research cycle:

- the trading universe and the overlay do different jobs
- the best overlay is not always the same thing as the best new regime basket
- regime work should be replayed in layers instead of promoted all at once

## Core idea

Treat a new regime in three separate pieces:

1. `trading universe`
   The sectors and symbols we actually want the basket engine to trade.
2. `overlay detector`
   The fast leadership sectors that tell us when the tape is favorable.
3. `overlay action`
   What we do when leadership is present:
   - stay `basket_only`
   - `suppress_shorts`
   - add `add_capped_long_sleeve`

Do not force these three pieces to be the same.

## What we learned from `buildout`

The `buildout` universe improved the basket book in `2026-01-02` to
`2026-04-30`, but the
best overlay signal still came from `faang,chips`, not from the newer
`ai_power,gas_infra` sectors.

That means:

- the new regime thesis was useful for basket construction
- the old leadership complex was still better for timing the overlay
- the overlay worked because it measured market leadership well, not because it
  matched the narrative perfectly

This is the main pattern to reuse.

## Why overlays matter

The basket engine is good at spread selection and relative-value behavior.
The overlay adds a second layer that can lean into broad leadership when the
tape is trending hard.

That combination can outperform either piece on its own:

- `basket_only` keeps us grounded in the basket engine
- the overlay lets us participate when leadership is obvious
- the current adaptive overlay implementation (`rule_v1` internally) is useful
  because it behaves like a governor, not a daily toggle

The overlay should be chosen for signal quality, not story purity.

## How to choose overlay sectors

Good overlay sectors usually have these properties:

- highly liquid names
- clean institutional leadership
- fast breadth expansion when the regime is working
- strong trend persistence over 5d to 20d windows
- broad market signaling value, not just isolated stock moves

Good examples:

- `faang`
- `chips`

Possible but weaker candidates:

- downstream beneficiaries with slower price discovery
- utilities and materials that move later than the real leadership complex

In practice, the sectors that explain the regime best are not always the ones
that monetize first.

## How to build the next regime

When we research a new regime, follow this order:

1. Build the thesis basket.
   Keep sectors narrow, liquid, and easy to replay.
2. Run `basket_only`.
   Verify the universe itself adds value before introducing overlay help.
3. Test overlay candidates separately.
   Include both:
   - thesis-native candidates
   - existing market leadership sectors that may still time better
4. Compare against fixed mechanisms.
   Every overlay idea should be judged against:
   - `basket_only`
   - fixed `suppress_shorts`
   - fixed `add_capped_long_sleeve`
   - `rule_v1`
5. Promote in layers.
   A new universe can be good even if its own overlay idea is not ready.

## Promotion rules

Promote a new regime only when we can say which layer improved:

- universe only
- overlay detector only
- both

Do not promote a new regime because the narrative is strong.
Promote it because replay shows:

- better return
- acceptable drawdown
- understandable behavior
- repeatable results

## Monday research loop

The weekly automation should do the first half of regime work:

- scan fresh news and trend leadership
- identify sectors with shifting fundamentals
- turn them into candidate baskets
- classify each idea as:
  - `universe_candidate`
  - `overlay_candidate`
  - `watch_only`

The automation should not silently promote anything. Its job is to narrow the
research queue so manual replay work starts from stronger hypotheses.
