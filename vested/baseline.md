# Vested Baseline

## Goal

Run the existing OpenQuant basket engine as much as possible, then adapt the final basket target book to a Vested-compatible long-only cash account.

The baseline is not a new strategy engine. It is an adapter layer for accounts that cannot express short legs or accept API orders through Vested.

## Baseline Mode

Use:

```bash
./engine/target/debug/openquant-runner paper --engine basket --paper-vested
```

The preset applies:

- universe: `config/basket_universe_buildout.toml`
- capital: `10000`
- active baskets: `5`
- Vested projection: `DropShorts`
- Vested regime gate: enabled with the module defaults
- state/output root: `data/paper/vested_model`

Replay uses the same model:

```bash
./engine/target/debug/openquant-runner replay --engine basket --replay-vested --start 2026-01-01 --end 2026-06-02
```

## Projection

The baseline projection is `DropShorts`:

1. Let the normal basket engine choose signed basket positions.
2. Remove negative target notionals because Vested cannot short.
3. Scale remaining positive notionals to the cash-account capital.
4. Convert to whole shares using the existing basket runner share conversion.

Other projection modes are available for research through:

```bash
--vested-projection peer-mirror
--vested-projection short-penalty
```

These are intentionally not exposed with extra tuning flags yet. Any parameter tuning should first clear walk-forward replay checks.

## Regime Gate

The Vested regime gate is exposure-only. It can scale the projected long-only book to cash when the recent strategy equity series is weak. It does not change basket state, selected baskets, or basket admission.

Default gate:

- minimum observations: `21`
- minimum 20d return: `0.0`
- maximum 20d drawdown: `0.05`
- risk-off exposure scale: `0.0`

This preserves a clean separation:

- basket engine decides what the long/short signal wants;
- Vested adapter decides how much of the long-only projection is safe to express.

## Execution

`--paper-vested` submits projected long-only orders to Alpaca paper so we can observe order behavior and fills without touching live money.

`--live-vested` uses live market data but keeps Alpaca execution in noop mode. Real Vested execution remains browser/manual until Vested exposes a supported API.

Orders submitted after the U.S. close can remain accepted/open in Alpaca paper and fill at the next regular session open. The basket runner may wait during reconciliation before writing pending-open state; this is expected, not a strategy failure.

## Files

Committed:

- `vested/mod.rs`: adapter design notes and module boundary
- `vested/vested.rs`: adapter implementation
- `vested/baseline.md`: current operating baseline

Generated and not committed:

- `data/paper/vested_model/*`
- `data/live/vested_model/*`
- `data/replay/vested_model/*`
- Vested picks TSVs, journals, pid files, and engine logs
