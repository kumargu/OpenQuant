# Performance Baseline

Measured on Apple M4 (2026-03-15) with `cargo bench`.

## Hot Path (criterion)

| Benchmark | Baseline | CI Gate |
|---|---|---|
| feature_update | 9ns | 500ns |
| signal_eval_no_fire | 7ns | — |
| signal_eval_buy_fire | 9ns | — |
| risk_check_pass | 1.7ns | — |
| on_bar_no_signal | 66ns | 500ns |
| backtest_1k | 69µs | 500µs |
| backtest_10k | 681µs | 5ms |

## Metrics Overhead (criterion)

| Benchmark | Baseline | With Cached Handles |
|---|---|---|
| noop_counter | 2.1ns | — |
| active_counter | 14.6ns | 1.6ns |
| active_histogram | 17.1ns | 5.2ns |
| on_bar 6 metrics | 105ns | 17ns |

## CI Gate Thresholds

Gates run as `cargo test --test bench_gate --release` in CI.
Thresholds are ~7x baseline to accommodate slower CI runners
and prevent flakiness. A gate failure means something got
catastrophically slower — investigate with `cargo bench`.
