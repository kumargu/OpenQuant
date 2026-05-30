# Benchmarks

This file tracks current, reproducible benchmark references. Older branch
experiments and stale pre-picker basket numbers were removed because they are
not reliable yardsticks for the current basket engine.

## Buildout Regime

Universe names used below:

- `standard_core` = `config/basket_universe.toml` (the frozen standard basket universe; this used to be named `basket_universe_v1.toml`)
- `buildout_core` = `config/basket_universe_buildout.toml` with leadership overlay disabled
- `buildout_overlay` = the same buildout universe with the adaptive overlay enabled

For the AI buildout regime, the default evaluation pair is:

- `buildout_core`
- `buildout_overlay`

Current causal 2026 YTD references after the chips detector-only change
and next-session-open replay contract:

- `buildout_core`: `+27.88%`, Sharpe `3.00`, max DD `7.90%`
- `buildout_overlay`: `+53.14%`, Sharpe `4.16`, max DD `8.34%`

Source artifacts:

- [`/Users/gulshan/OpenQuant/outputs/buildout_core_recovery/post_fix_audit/core/report.tsv`](/Users/gulshan/OpenQuant/outputs/buildout_core_recovery/post_fix_audit/core/report.tsv)
- [`/Users/gulshan/OpenQuant/outputs/buildout_core_recovery/post_fix_audit/overlay/report.tsv`](/Users/gulshan/OpenQuant/outputs/buildout_core_recovery/post_fix_audit/overlay/report.tsv)

Definitions:

- `buildout_core` = `config/basket_universe_buildout.toml` with leadership overlay disabled
- `buildout_overlay` = the same buildout universe with the adaptive overlay enabled

Why both must be run:

- `buildout_core` tells us whether the basket universe itself improved
- `buildout_overlay` tells us whether the money-making combined expression
  improved

Do not evaluate buildout changes with `buildout_core` alone. The overlay is a
material part of the regime edge and must be treated as a default companion
run, not as an optional extra.

## Basket Overlay Picker

Current basket leadership work should be judged against fixed mechanism
benchmarks, not `basket_only` alone. The basket core remains the foundation;
the rule picker is evaluated as a conservative allocator among:

- `basket_only`
- basket core + `suppress_shorts`
- basket core + `add_capped_long_sleeve`

Replay command:

```bash
scripts/run_basket_overlay_benchmark.py --prefix overlay_bench_spare_budget
```

Shared replay settings:

- sectors: `faang,chips`
- leadership on threshold: `ret5d >= 0.02`
- leadership breadth threshold: `breadth5d >= 0.56`
- long-only sleeve budget: `leadership_long_only_leverage = 1.0`
- capital: `10000`
- active basket cap: `5`

| Window | Basket only | Fixed suppress | Fixed sleeve | Rule v1 | Best fixed | Rule v1 vs best fixed |
|--------|----------|----------------|--------------|---------|------------|-----------------------|
| wide Q3 2025 | -4.19%, DD 17.09% | -2.22%, DD 19.57% | +10.51%, DD 12.08% | +13.02%, DD 12.08% | Fixed sleeve | +2.52%, DD +0.00% |
| wide Q4 2025 | -5.00%, DD 13.09% | -5.00%, DD 13.09% | -2.17%, DD 10.95% | +4.42%, DD 7.25% | Fixed sleeve | +6.60%, DD -3.70% |
| wide 2026 YTD | -9.20%, DD 15.85% | -9.20%, DD 15.85% | +52.40%, DD 8.15% | +52.40%, DD 8.15% | Fixed sleeve | +0.00%, DD +0.00% |
| strong Q1 2025 | +27.05%, DD 3.37% | +33.74%, DD 2.57% | +8.12%, DD 9.52% | +38.42%, DD 2.48% | Fixed suppress | +4.68%, DD -0.08% |

## Acceptance Notes

The rule picker should continue to clear these bars before promotion:

- do not degrade the best fixed mechanism materially in strong leadership windows
- do not beat `basket_only` by merely taking more drawdown
- preserve basket-core behavior when leadership is absent
- keep replay outputs deterministic and restart-stable
- record picker decisions so regressions can be attributed to mode selection,
  not guessed from PnL alone

Current validation:

```bash
cargo test -p openquant-runner
python3 -m py_compile scripts/run_basket_overlay_benchmark.py
```
