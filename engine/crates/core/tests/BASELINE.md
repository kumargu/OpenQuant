# Performance Baseline

Measured on Apple M4 (2026-03-15) with `cargo bench`.

## Hot Path (criterion)

| Benchmark | Baseline | CI Gate |
|---|---|---|
| feature_update | 8.9ns | 2µs |
| signal_eval_no_fire | 7.0ns | — |
| signal_eval_buy_fire | 8.9ns | — |
| risk_check_pass | 1.7ns | — |
| risk_check_killed | 90ns | — |
| exit_check_no_trigger | 2.2ns | — |
| exit_check_stop_loss | 13.9ns | — |
| on_bar_no_signal | 64ns | 5µs |
| on_bar_journaled | 69ns | — |
| backtest_1k | 67µs | 5ms |
| backtest_10k | 673µs | 50ms |

## Metrics Overhead (criterion)

| Benchmark | Baseline | With Cached Handles |
|---|---|---|
| noop_counter | 2.1ns | — |
| active_counter | 14.6ns | 1.6ns |
| active_histogram | 17.1ns | 5.2ns |
| on_bar 6 metrics | 105ns | 17ns |

## CI Gate Thresholds

Gates run as `cargo test --test bench_gate --release` in CI.
Thresholds are ~30-70x baseline to accommodate slower CI runners
and prevent flakiness. A gate failure means something got
catastrophically slower — investigate with `cargo bench`.
