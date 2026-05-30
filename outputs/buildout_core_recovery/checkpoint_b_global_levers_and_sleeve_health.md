# Buildout Core Recovery: Global Lever Sweep + Sleeve Health

## Scope

This checkpoint asks a narrow question:

- after the `chips` detector-only fix, is the next improvement in a **generic global lever**
  like the dominance gate, active-basket cap, admission ranking, or gate policy?
- or is the next improvement in **generic sleeve-health detection** and then a second sleeve fix?

All numbers below use the current checked-in `buildout` config with realistic next-session-open fills over `2026-01-01..2026-05-27`, unless noted otherwise.

Current baseline:

- `buildout_core`: `+27.88%`
- `Sharpe`: `3.00`
- `max_dd`: `7.90%`

## Global Lever Sweep

### 1. Dominance gate breadth sweep

Results:

- `dominance_max=0.50`: `+16.15%`, `Sharpe 1.72`, `max_dd 7.76%`
- `dominance_max=0.60` (current): `+27.88%`, `Sharpe 3.00`, `max_dd 7.90%`
- `dominance_max=0.70`: `+8.31%`, `Sharpe 0.88`, `max_dd 16.94%`
- `dominance_max=0.80`: `+19.22%`, `Sharpe 1.96`, `max_dd 9.23%`
- `dominance_max=1.00`: `+10.70%`, `Sharpe 1.18`, `max_dd 12.91%`
- `dominance_gate=false` on the current chips-detector-only config: `+7.33%`, `Sharpe 0.89`, `max_dd 10.27%`

Reasoning:

- The current `0.60` setting is not obviously too strict. It is the best point in the tested neighborhood.
- Loosening the gate expands replay-start fit breadth sharply, but those added baskets are lower quality and hurt the capped portfolio.
- This means the narrow fit set is not, by itself, the next fix.

Classification:

- points away from **fit/admission reform** as the immediate next improvement
- points toward **quality of admitted sleeves under the cap**

### 2. Active-basket cap sweep

Results:

- `cap=4`: `+20.22%`, `Sharpe 1.74`, `max_dd 15.27%`
- `cap=5` (current): `+27.88%`, `Sharpe 3.00`, `max_dd 7.90%`
- `cap=6`: `+16.16%`, `Sharpe 1.78`, `max_dd 11.52%`
- `cap=7`: `+10.12%`, `Sharpe 1.30`, `max_dd 9.70%`
- `cap=8`: `+10.22%`, `Sharpe 1.44`, `max_dd 7.60%`

Reasoning:

- The cap absolutely binds, but changing it in either direction makes the core book worse.
- Lower cap over-concentrates and raises drawdown.
- Higher cap admits too many weaker baskets and dilutes the stronger ones.

Classification:

- points away from **basket-count tuning** as the immediate next fix

### 3. Admission ranking metric

Results:

- `signal-score` admission: same as baseline
- `raw-z-score` admission: identical to baseline

Reasoning:

- Under the current buildout state, both ranking metrics end up selecting the same admitted baskets.
- So portfolio ranking is not the current bottleneck.

Classification:

- points away from **admission-score ranking tweaks**

### 4. Gate policy sweep

Results:

- `BertramFrozen` (current): `+27.88%`, `Sharpe 3.00`, `max_dd 7.90%`
- `rolling-s-score-v1` default: `+8.31%`, `Sharpe 1.40`, `max_dd 5.75%`
- `rolling-s-score-v1` with `raw-z-score` entry: `+4.95%`, `Sharpe 0.99`, `max_dd 4.84%`
- `rolling-s-score-v1` with `raw-z-score` and `entry_confirmation_bars=2`: `+5.56%`, `Sharpe 1.18`, `max_dd 3.52%`

Reasoning:

- The rolling gate variants reduce volatility and drawdown, but they give up too much return.
- For current buildout core, the legacy Bertram gate is still clearly better.

Classification:

- points away from **gate-policy replacement** as the immediate next fix

## Sleeve Health Ranking

The next remaining problem is sleeve quality.

Current sector P&L on the post-fix core replay:

- `hc_providers`: `+1006.84`
- `insurance`: `+217.84`
- `utilities`: `+209.75`
- `energy`: `+159.47`
- `banks_regional`: `+134.43`
- `gas_infra`: `-29.17`
- `ai_power`: `-16.78`
- `chips`: `0.0` by design

Generic spread-health finding:

- `gas_infra` and `ai_power` are the next sleeves with **negative total sleeve P&L**
- in both, the **target side is the drag**
- this is the same type of clue that exposed `chips` as a bad traded expression

Target-vs-peer decomposition:

- `gas_infra`: target P&L `-90.16`, peer P&L `+61.00`
- `ai_power`: target P&L `-207.44`, peer P&L `+190.66`

Key target drags:

- `gas_infra`
  - `KMI`: `-249.13`
  - `WMB`: `-49.03`
- `ai_power`
  - `NEE`: `-409.19`
  - `VST`: `-28.97`

Key target offsets in the same sleeves:

- `gas_infra`
  - `OKE`: `+86.86`
  - `EQT`: `+121.14`
- `ai_power`
  - `NRG`: `+130.95`
  - `CEG`: `+99.77`

Reasoning:

- This looks less like a whole-engine failure and more like **specific traded-target expressions inside otherwise viable themes**.
- That is exactly the generic pattern we want to catch programmatically:
  - theme may be valid
  - spread expression may still be poor
  - bad target choices can dominate sleeve outcome

Classification:

- points toward **generic sleeve-health logic**
- points toward a **second sleeve deep dive**, starting with `gas_infra`, then `ai_power`

## Decision

The next right thing is **not** another broad parameter change.

The next right thing is:

1. preserve the conclusion that the current global knobs are locally well set
2. build a generic sleeve-health framework around:
   - target-side vs peer-side contribution
   - target concentration
   - target validity breadth
   - outright theme strength vs spread quality
3. use that framework first on `gas_infra`, then `ai_power`

## Plain-English Finding

After fixing chips, the buildout core is no longer weak because of obvious global settings.

The remaining weakness is more surgical:

- the current dominance gate is helping, not hurting
- the current basket cap is helping, not hurting
- the current Bertram gate is helping, not hurting
- the next gains should come from finding the next bad **spread expression**, not from more top-level tuning
