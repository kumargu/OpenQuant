---
name: rust-engine
description: Use when writing, reviewing, or optimizing Rust code in the engine/ workspace. Covers crate structure, maturin builds, numerical stability, NaN guards, criterion benchmarks, testing patterns, and performance-critical paths.
---

# Rust Engine

## Trigger

Activate when working on any code under `engine/crates/` — core, pair-picker, journal, metrics, pybridge.

## Build & Test

```bash
cd engine && maturin develop --release    # build Python bridge
cd engine && cargo test --workspace       # all tests
cd engine && cargo test -p pair-picker    # single crate
cd engine && cargo bench -p pair-picker   # criterion benchmarks
cd engine && cargo fmt --all -- --check   # formatting
cd engine && cargo clippy --workspace -- -D warnings  # lint
```

## Crate Layout

- `core` — trading engine: features, signals, risk, portfolio, backtest simulation
- `pair-picker` — offline pair discovery: cointegration, scoring, universe screening
- `journal` — SQLite trade journal, structured logging
- `metrics` — counters, histograms, performance tracking
- `pybridge` — PyO3 bindings exposing Rust to Python

## Non-Negotiable Rules

1. **All math in Rust** — Python is the pipe, Rust is the brain
2. **Guard NaN/infinity at boundaries** — check `is_finite()` and `> 0.0` before `ln()`, `clamp()`, or any math that can propagate NaN
3. **Two-pass algorithms for variance** — deviation-from-mean form, never single-pass `sum_xx - n*mean^2`
4. **No magic numbers** — all thresholds in config structs with `Default` impls, documented in `openquant.toml`
5. **Structured logging** — `tracing::info!/warn!` with structured fields for every significant decision
6. **Reference tests (reftests)** — expected values from Python/numpy, seeded PRNG, tight tolerance (1e-8)
7. **Criterion benchmarks** — for hot paths, statistical computations, per-bar operations
8. **Deterministic replay** — same inputs → same outputs, always
9. **Contiguous time-series** — statistical tests (ADF) require consecutive observations; never filter scattered indices
10. **Cite sources** — when implementing a statistical method, cite the paper and verify formula matches

## Performance

- Prefer incremental/rolling updates over full recomputation
- Minimize allocation in hot paths — preallocated buffers, ring buffers, slice-based APIs
- Profile before optimizing — `cargo bench` to find real bottlenecks
- Cache-friendly layouts — contiguous storage, predictable iteration
- No `clone()` in tight loops, no string formatting during compute

## Testing Layers

- **Unit tests**: isolated formulas, state transitions
- **Property tests**: invariants over randomized inputs (exposure bounded, PnL consistent)
- **Golden/reftests**: deterministic outputs on known data, cross-validated with Python
- **Integration tests**: data contracts between producer and consumer modules
- **Bench gate tests**: CI performance gates (`#[ignore]`, run with `--release -- --ignored`)

## Config Pattern

```rust
#[derive(Debug, Clone)]
pub struct FooConfig {
    pub threshold: f64,
    pub window: usize,
}

impl Default for FooConfig {
    fn default() -> Self {
        Self { threshold: 0.05, window: 20 }
    }
}
```

Every config key in `openquant.toml` must have a comment explaining: what it does, effect of changing it, typical range.
