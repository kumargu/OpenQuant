# OpenQuant India Adaptation

This repo copy is the India-market staging branch of OpenQuant. The core runtime
can now run India refresh, replay, and small-capital Kite live smoke through the
same maintained runner path instead of sidecar scripts.

## What changed

- Runner market session logic can now boot in an India profile through environment variables instead of hard-coded NYSE hours.
- Core config now supports minute-precision timezone offsets, so IST (`UTC+05:30`) works for VWAP session resets and daily boundaries.
- India bootstrap artifacts were added:
  - `.env.india.example`
  - `config/runner.india.toml`
  - `config/basket_universe_india.template.toml`
- `openquant-runner refresh-data` can refresh broker minute-bar parquets through
  the maintained refresh pipeline. India uses Kite credentials from the runner
  TOML profile and `.env.india`.

## How to run in India mode

Create the local India credential file before launching India refresh or replay
runs:

```bash
cp .env.india.example .env.india
```

India-specific runtime settings are TOML-driven via `config/runner.india.toml`.
US mode continues to use the existing Alpaca/NYSE defaults when no runner TOML
is supplied.

Example India settings:

```toml
[broker]
provider = "kite"
env_file = ".env.india"

[market]
profile = "india"
tz = "Asia/Kolkata"
open = "09:15"
close = "15:30"
calendar = "weekdays"
```

Direct selection:

```bash
openquant-runner replay \
  --engine basket \
  --runner-config config/runner.india.toml \
  --universe config/basket_universe_india.template.toml \
  --capital 500000 \
  --start 2026-01-01 \
  --end 2026-05-31

openquant-runner paper --engine basket
```

India historical refresh:

```bash
openquant-runner refresh-data \
  --runner-config config/runner.india.toml \
  --universe config/basket_universe_india.template.toml \
  --from 2025-01-01
```

Kite access-token refresh:

```bash
openquant-runner kite-login \
  --runner-config config/runner.india.toml \
  --wait-localhost
```

India live smoke run uses the India TOML profile and keeps capital at the
universe default of INR 10,000. The profile uses Kite `amo` orders so the
session-close basket decision can reconcile at the next NSE open.

```bash
openquant-runner live \
  --engine basket \
  --runner-config config/runner.india.toml \
  --universe config/basket_universe_india.template.toml \
  --disable-leadership-overlay
```

## What is still missing

- Verified India baskets and overlay sectors.
- Exchange holiday calendar support for NSE/BSE. Current India profile uses weekday-only trading days as a placeholder.
- Symbol normalization layer between research symbols and broker tradingsymbols.
- Kite has a stop-market helper for explicit broker-native emergency orders,
  but protective stops are not wired into the basket broker trait yet. The
  basket runtime currently submits market/AMO rebalance orders and controls
  exits through basket state.

## Recommended next steps

1. Keep running weekly replay refreshes from `config/runner.india.toml` and `config/basket_universe_india.template.toml`.
2. Promote only stable India baskets by editing the India TOML universe, not the Rust runtime.
3. Replace weekday-only India calendar logic with a verified NSE holiday calendar.
4. Keep Kite live smoke small until the basket set has repeated replay and
   live-smoke evidence.
