# OpenQuant — Claude Instructions

## After creating any PR

1. Start a `/loop 5m` to monitor the PR for review comments
2. When new comments arrive: assess, fix the code, commit, push, and reply to the comment
3. Continue monitoring until the PR is merged or the user cancels

This is mandatory — never create a PR without starting the monitor loop.

## PR requirements

Every PR that touches signals, risk, or strategy must include a backtest comparison table in the description. Run `python -m paper_trading.benchmark --compare` to generate it.

## Reviewer mode

When reviewing PRs (e.g., from another Claude session or Codex), enforce these standards:

### Before merging — CI gate
```bash
gh pr checks <number>   # ALL checks must pass before merge
```

CI runs: `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test --workspace`, performance gates, Python bridge build. **Never merge with failing CI.**

### Code review standards
- **Rust-first**: All math, statistics, and trading logic in Rust. Python only for Alpaca API integration or ML libraries that require Python ecosystem
- **Mathematical correctness**: Verify formulas against cited research papers. If the implementation diverges from the literature, demand explicit justification with reasoning
- **No magic numbers**: Thresholds must be named constants or in config structs with `Default` impls
- **Structured logging**: Every significant decision must use `tracing::info!/warn!` with structured fields — pair promoted/demoted, regime change, cointegration break, scoring results
- **Tests**: Data-driven and flexible. Synthetic test data generators in shared `test_utils`. Cover edge cases (NaN, zero, empty, boundary conditions)
- **Benchmarks**: Criterion benchmarks for statistical computations and hot paths
- **No tech debt**: No dead code, no duplicate implementations, no TODO-later patterns. When replacing old code, delete it completely
- **Challenge the "why"**: Push back on code comments and implementation reasoning. Do independent research when benchmark results seem off

### Proactive reviewer responsibilities
- **Research ahead**: Between PRs, read the existing code that the next PR will modify. Study the relevant papers. Come prepared, don't just react to diffs
- **Diagnose CI failures**: If CI is broken on main, diagnose and flag it to the author so they can include the fix
- **Do independent research**: When an implementation claims to follow a paper (e.g., "NIG conjugate update per Murphy 2007"), verify the formulas yourself. When benchmark results seem off, research why before accepting

### Mid-sprint review (after every 3-4 merged PRs)
Stop, pull main, and verify everything actually works — don't just review diffs.

**Use agent team mode** — spawn parallel agents in worktrees to avoid conflicting with other active branches:
- **Test runner agent** (worktree): Pull main, run `cargo fmt --check`, `cargo clippy`, `cargo test --workspace`, run binaries end-to-end, verify output files
- **Data checker agent**: Inventory available data, check date ranges, assess what needs backfilling for forward testing
- **Forward test agent** (worktree): If data is available, run the full pipeline end-to-end with real historical data (pair-picker → active_pairs.json → PairsEngine → trade history)

**Analyze what was shipped**:
- **Architecture coherence**: Do shipped components fit together? Are interfaces consistent?
- **Integration gaps**: Functions defined but never called? Assumptions in module A that module B doesn't satisfy?
- **Test coverage**: Cross-module integration tests, or only unit tests per module?
- **Config separation**: Each subsystem owns its own config files. Don't mix unrelated params (e.g., shadow trading config should NOT be in `openquant.toml`)
- **Logging/observability**: Can we trace an entity's full lifecycle through logs?
- **Data readiness**: Do we have enough historical data to run the system with real prices?

Post a summary comment on the parent issue with findings and flag concerns before continuing.

### Merge protocol
1. All review comments addressed
2. `gh pr checks <number>` — all green
3. Squash merge with descriptive subject

## Developer best practices

### Architecture
- **Rust owns all math and trading logic**: Statistics, signals, risk, scoring — zero Python for anything performance-sensitive or correctness-critical. Python is the data plumbing layer only (Alpaca API, bar fetching, orchestration)
- **Separate crates for separate concerns**: Offline analysis tools (e.g., `pair-picker`) are standalone binaries, not linked into the trading engine. Communicate via JSON files, not shared state
- **Canonical identifiers everywhere**: When two components reference the same entity (a pair, a symbol, a strategy), use a single canonical ID format. Alphabetically ordered, consistent across all producers and consumers. Mismatched IDs cause silent data loss
- **Config structs with Default**: All tunable parameters in structs with `Default` impls. No magic numbers in function bodies. This makes parameters discoverable, documentable, and overridable without recompilation

### Correctness
- **This system manages real money — bugs lose money silently**: Every math operation should be defensively coded. Wrong results are worse than crashes because they go unnoticed
- **Guard NaN/infinity at system boundaries**: Check `is_finite()` and `> 0.0` before `ln()`, `clamp()`, or any math that can propagate NaN. NaN flows through arithmetic silently and corrupts everything downstream
- **Two-pass algorithms for numerical stability**: Single-pass variance formulas (sum_xx - n*mean^2) suffer catastrophic cancellation when values are large and variance is small. Use deviation-from-mean form
- **Time-series tests require contiguous observations**: Statistical tests like ADF assume consecutive data points. Filtering scattered indices creates multi-day gaps that invalidate the test's serial correlation assumptions and bias results (often toward false acceptance — fails unsafe)
- **Verify formulas against literature**: When implementing a statistical method, cite the source paper and verify your formula matches. Wrong signs, missing terms, or incorrect degrees of freedom can silently degrade strategy performance

### Testing
- **Test module seams, not just modules**: Unit tests per module are necessary but insufficient. When a PR bridges two components, include integration tests that verify the data contract between them (e.g., producer writes field X, consumer reads and uses field X correctly)
- **Test the rejection path**: Don't just test that good inputs pass — verify bad inputs are correctly rejected (invalid data, stale files, NaN prices, boundary conditions)
- **Shared test_utils module**: Deterministic data generators (`Lcg`, synthetic series) in a shared `#[cfg(test)]` module. Reuse across all test files to ensure consistent ground truth
- **Criterion benchmarks for critical paths**: Know your latency budget. Benchmark statistical computations, hot-path signal generation, and any operation that runs per-bar or per-trade

### Workflow
- **One implementation per concept**: Never have parallel implementations of the same logic in different languages or files. When Rust replaces Python, delete the Python entirely. Duplicate implementations drift and produce conflicting results
- **Reload preserves state**: When refreshing config from an updated file, update parameters (e.g., hedge ratio) on existing objects but preserve their runtime state (warm-up data, open positions). Don't reset what doesn't need resetting
- **Remove dead code aggressively**: If a function is defined but never called, delete it or wire it in. Dead code misleads readers and rots. No "keeping it for later"

## Build commands

- Build engine: `cd engine && maturin develop --release`
- Run Rust tests: `cd engine && cargo test`
- Run pair-picker tests: `cd engine && cargo test -p pair-picker`
- Run benchmarks: `cd engine && cargo bench -p pair-picker`
- Run benchmark: `python -m paper_trading.benchmark --category crypto --days 7`
- Save baseline: `python -m paper_trading.benchmark --save-baseline`
