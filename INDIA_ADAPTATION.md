# OpenQuant India Adaptation

This repo copy is the India-market staging branch of OpenQuant. It is not live-ready yet, but the core runtime now has the minimum structure to stop assuming US cash equities everywhere.

## What changed

- Runner market session logic can now boot in an India profile through environment variables instead of hard-coded NYSE hours.
- Core config now supports minute-precision timezone offsets, so IST (`UTC+05:30`) works for VWAP session resets and daily boundaries.
- India bootstrap artifacts were added:
  - `.env.india.example`
  - `config/runner.india.toml`
  - `config/runner.us.toml`
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

Preferred runner configuration is now TOML-driven via `config/runner.toml`.

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

Recommended profile files:

- `config/runner.us.toml`
- `config/runner.india.toml`

Direct selection:

```bash
openquant-runner replay \
  --engine basket \
  --runner-config config/runner.india.toml \
  --universe config/basket_universe_india.template.toml \
  --start 2026-01-01 \
  --end 2026-05-31

openquant-runner paper --engine basket --runner-config config/runner.us.toml
```

India historical refresh:

```bash
openquant-runner refresh-data \
  --runner-config config/runner.india.toml \
  --universe config/basket_universe_india.template.toml \
  --from 2025-01-01
```

## What is still missing

- Zerodha/Kite live order placement.
- Zerodha/Kite streaming market-data adapter.
- Verified India baskets and overlay sectors.
- Exchange holiday calendar support for NSE/BSE. Current India profile uses weekday-only trading days as a placeholder.
- Symbol normalization layer between research symbols and broker tradingsymbols.

## Recommended next steps

1. Keep running weekly replay refreshes from `config/runner.india.toml` and `config/basket_universe_india.template.toml`.
2. Promote only stable India baskets by editing the India TOML universe, not the Rust runtime.
3. Replace weekday-only India calendar logic with a verified NSE holiday calendar.
4. Implement Kite live order placement and streaming only after the replay/paper basket set is stable.
