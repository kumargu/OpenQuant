# India Baseline

Date: 2026-06-01

This is the operating baseline for India basket work. The runtime path must stay
the same path used by live trading: runner TOML, broker adapter, refresh-data,
replay, and basket live.

## Files

- India runner profile: `config/runner.india.toml`
- India basket universe: `config/basket_universe_india.template.toml`
- India credentials: `.env.india`
- Local credential template: `.env.india.example`

US remains the default runtime when no runner TOML is supplied. Do not add a
separate US runner TOML unless a real new US runtime setting needs it.

## Capital

- Live India smoke capital: INR 10,000
- Replay/research capital: INR 500,000

The India universe TOML stores the live smoke default of `capital = 10000`.
Replay commands must pass `--capital 500000` explicitly so research output stays
comparable with the prior India runs.

## Kite Login

Use the maintained runner command to obtain and persist the daily Kite access
token. The only manual step should be completing the Kite login page when
required.

```bash
cargo build --release -p openquant-runner

engine/target/release/openquant-runner kite-login \
  --runner-config config/runner.india.toml \
  --wait-localhost
```

If localhost callback cannot be used, open the login URL printed by the command
and rerun with:

```bash
engine/target/release/openquant-runner kite-login \
  --runner-config config/runner.india.toml \
  --request-token <request_token>
```

## Data Refresh

Backfill and refresh India bars through the runner, not sidecar scripts:

```bash
engine/target/release/openquant-runner refresh-data \
  --runner-config config/runner.india.toml \
  --universe config/basket_universe_india.template.toml \
  --from 2025-01-01
```

## Replay Baseline

Replay must use the live basket path and INR 500,000 capital:

```bash
engine/target/release/openquant-runner replay \
  --engine basket \
  --runner-config config/runner.india.toml \
  --universe config/basket_universe_india.template.toml \
  --capital 500000 \
  --start 2026-01-01 \
  --end 2026-05-31 \
  --report-tsv outputs/india-baseline/replay_ytd.tsv \
  --basket-journal-path outputs/india-baseline/replay_ytd.sqlite3
```

## Live Smoke

Live starts small with the India universe default capital of INR 10,000. The
Kite profile uses `order_variety = "amo"` and `close_grace_minutes = 35`, so
session-close orders are submitted around 16:05 IST for next-session execution
and then reconciled at the next NSE open. This avoids submitting AMOs during
the 15:30-16:00 post-close interval, when Zerodha may still reject AMO orders.

```bash
engine/target/release/openquant-runner live \
  --engine basket \
  --runner-config config/runner.india.toml \
  --universe config/basket_universe_india.template.toml \
  --disable-leadership-overlay
```

Use `paper --execution noop` with the same runner/universe for a dry operational
check. Kite does not provide a paper endpoint, and the adapter fails closed for
`paper` execution.

## Current Gaps

- NSE/BSE holiday calendar is still weekday-only.
- Kite has a stop-market order helper for explicit broker-native emergency
  orders, but basket-level protective stops are not wired into the broker trait
  or basket state machine yet. Do not turn normal basket rebalances into `SL-M`
  orders by changing the TOML default order type.
- India basket membership is still a template and should be promoted only after
  repeated replay and live-smoke evidence.
