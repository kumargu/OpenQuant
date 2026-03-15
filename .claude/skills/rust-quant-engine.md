---
name: rust-quant-engine
description: Use this skill when designing, implementing, reviewing, or optimizing the Rust core engine for a quant bot. Apply it for performance-critical math pipelines, deterministic decision logic, backtesting engines, market data processing, risk modules, benchmarks, testing strategy, and low-latency execution paths.
---

# Rust Quant Engine

## Purpose

This skill defines how the Rust engine for the quant bot must be built.

The Rust layer is the quantitative core. It is not a scripting convenience layer, and it is not a place for fuzzy AI judgment. Its job is to perform deterministic market-data processing, feature extraction, signal evaluation, risk checks, backtesting, simulation, state transitions, and performance-critical calculations with high reliability and strong observability.

This engine must be:
- mathematically grounded
- highly efficient
- memory-conscious
- deterministic
- well-benchmarked
- aggressively tested
- simple enough to reason about under stress
- strict about correctness in financial calculations
- explicit about assumptions and edge cases

The engine should favor truth, reproducibility, and speed over cleverness that cannot be measured.

---

## When to use

Use this skill whenever you are:

- designing the Rust trading engine architecture
- implementing market-data ingestion logic
- building candle, tick, or order-book processing pipelines
- implementing indicators or statistical features
- writing signal logic
- implementing risk controls
- designing the backtesting engine
- optimizing memory usage or allocation behavior
- adding concurrency or parallelism
- designing the trade state machine
- implementing metrics aggregation
- adding benchmarks or profiling
- defining how to test correctness and performance
- deciding what belongs in Rust vs Python vs Claude
- reviewing code for determinism, latency, safety, or numerical correctness

Use this skill especially when the engine must:
- run on large historical datasets
- process high event volume
- produce reproducible outputs
- support long-running live systems
- minimize overhead and garbage-like allocation patterns
- withstand future scaling and strategy growth

---

## Core role of the Rust engine

The Rust engine is where hard decisions become explicit.

This layer should own:
- market event representation
- feature and signal computation
- deterministic rule evaluation
- position and order state machines
- cost modeling
- risk calculation
- PnL and exposure tracking
- backtest simulation
- performance-sensitive analytics
- benchmarkable numerical logic
- high-integrity internal APIs

This layer should not depend on vague language interpretation for the core decision path.

Natural language can help generate ideas.  
Rust must decide, calculate, and verify.

---

## Design philosophy

### 1. Deterministic before intelligent

The engine must behave the same way for the same inputs.

The decision path should be deterministic, measurable, and replayable. A market event sequence should yield the same features, signals, fills, state transitions, and metrics every time unless randomness is explicitly modeled and seeded.

Determinism matters because:
- debugging becomes possible
- backtest and replay become trustworthy
- live incidents can be reproduced
- performance regressions can be isolated
- strategy changes can be compared fairly

If a result cannot be reproduced, it cannot be trusted.

---

### 2. Math first, stories later

The engine should be based on measurable quantities and explicit formulas.

Use:
- returns
- rolling windows
- volatility
- drawdown
- skew
- kurtosis where useful
- z-scores
- moving averages
- exponential smoothing
- volume imbalance
- spread measurements
- liquidity proxies
- regime tags from measurable inputs
- weighted scores with explainable formulas

Avoid letting vague intuition creep into the decision path.

A decision should be expressible as:
- thresholds
- scores
- rankers
- probabilities
- state transitions
- constrained optimization rules
- risk gates
- confidence formulas grounded in data

The engine should answer:
- what was computed
- from which inputs
- with which window
- using which formula
- at what timestamp
- under which assumptions

---

### 3. Performance is a feature, not a later optimization

This engine is performance-critical by design.

Performance must be considered from the beginning in:
- data structures
- memory layout
- ownership choices
- iterator patterns
- allocation strategy
- batching
- cache locality
- lock avoidance
- serialization format
- hot path branching
- trait object usage
- numeric type selection

The engine should avoid accidental slowness caused by:
- repeated heap allocation
- unnecessary cloning
- boxing in hot paths
- dynamic dispatch where static dispatch is sufficient
- branch-heavy inner loops
- converting data formats repeatedly
- copying historical windows unnecessarily
- string-heavy logic in compute paths
- per-event logging in hot loops without sampling or gating

A fast engine is not only about low latency. It is also about enabling:
- deeper backtests
- wider parameter sweeps
- faster iteration
- lower cloud cost
- more realistic simulation
- stronger confidence in scale behavior

---

### 4. Correctness beats premature micro-optimization

The engine must be fast, but not at the expense of correctness.

Never optimize ambiguous or incorrect logic.

The sequence must be:
1. define correct behavior
2. test it thoroughly
3. benchmark it
4. profile the real bottleneck
5. optimize only the hot path
6. prove the optimization preserved correctness

Do not guess about performance. Measure it.

Do not guess about correctness. Test it.

---

## Architectural principles

### 5. Separate hot path from orchestration

The engine should distinguish clearly between:
- hot compute path
- state management
- persistence
- monitoring
- orchestration
- reporting
- strategy configuration
- external integrations

The hottest path should remain lean and predictable.

For example:
- event decode and normalization
- feature update
- signal evaluation
- risk check
- order or paper-trade simulation
- position update

That path should avoid:
- JSON parsing
- heavy logging
- filesystem writes
- network chatter
- expensive trait indirection
- nonessential allocation

Keep noncritical work outside the core loop.

---

### 6. Prefer explicit data models

Use domain-specific types that make mistakes harder.

Examples:
- `Price`
- `Quantity`
- `SpreadBps`
- `TimestampNanos`
- `PnL`
- `Volatility`
- `SignalScore`
- `RiskBudget`
- `PositionSide`
- `OrderState`
- `TradeId`
- `StrategyId`

Avoid primitive obsession where `f64`, `i64`, and `String` are used everywhere without semantic meaning.

Typed models improve:
- readability
- compile-time safety
- correctness under refactoring
- unit testing
- API clarity

The engine should make invalid states hard to represent.

---

### 7. State machines over ad hoc condition trees

Trading logic becomes fragile when built as scattered conditionals.

Use explicit state machines for:
- order lifecycle
- position lifecycle
- backtest fill lifecycle
- risk trip states
- engine startup and shutdown stages
- feed health transitions
- strategy enable and disable states

State machines reduce hidden transitions and make edge cases easier to test.

A clear state machine is better than dozens of loosely connected booleans.

---

### 8. Minimize allocation in the critical path

The engine should be careful with memory churn.

Prefer:
- preallocated buffers where useful
- reusable workspace structs
- ring buffers for rolling windows
- contiguous storage
- slice-based APIs
- stack-friendly temporary data when possible
- zero-copy parsing or borrowing when safe
- object pools only when measurement justifies them

Be suspicious of:
- `clone()` in tight loops
- repeated `Vec` growth in hot paths
- string formatting during compute
- converting between collections repeatedly
- unnecessary owned copies of event payloads

Allocation discipline matters because it reduces:
- latency spikes
- cache misses
- backtest cost
- unpredictable runtime behavior

---

### 9. Simplicity in the engine beats framework cleverness

Avoid overengineering the core.

Do not create abstraction towers that make the engine harder to optimize or reason about.

Prefer:
- clear modules
- direct data flow
- explicit ownership
- predictable trait boundaries
- minimal macro magic in critical code
- small, composable interfaces

A quant engine should feel like a precise machine, not a flexible framework that can become anything.

---

## Numerical principles

### 10. Be deliberate about numeric types

Choose numeric types intentionally.

Use `f64` where floating-point throughput and range matter, but be explicit about precision assumptions. Where money, fees, or exact accounting matter, consider fixed-point or integer-scaled representations if precision guarantees are important.

Be consistent in:
- price representation
- PnL accumulation
- basis-point conversions
- fee calculations
- slippage calculations
- percentage returns
- risk sizing

Do not casually mix:
- raw prices
- percentages
- ratios
- basis points
- scaled integer values

The engine should make these distinctions obvious.

---

### 11. Incremental math should be preferred where valid

For rolling statistics and event streams, prefer incremental updates rather than full recomputation where mathematically sound.

Examples:
- rolling mean
- rolling variance
- EWMA
- cumulative returns
- running drawdown
- rolling max/min with efficient structures
- online z-score inputs
- rolling volume imbalance

This reduces computation cost and supports:
- live streaming efficiency
- larger historical runs
- more stable latency

But incremental formulas must be tested thoroughly against trusted reference implementations.

---

### 12. Keep formulas transparent

Every feature or signal formula should be explainable.

Document:
- the formula
- the inputs
- the units
- the assumptions
- the numerical edge cases
- the warm-up behavior
- the handling of NaN, zero-volume, missing data, or market gaps

A strategy is easier to validate when the engine math is not mysterious.

---

## Performance principles

### 13. Benchmark everything important

Every performance-critical component should have benchmarks.

Benchmark:
- event parsing
- rolling window updates
- indicator calculations
- signal evaluation
- risk checks
- state transitions
- backtest throughput
- order book update handling if relevant
- serialization and deserialization where relevant
- memory allocation behavior in hot paths

Benchmarks should cover:
- small workloads
- realistic workloads
- worst-case workloads
- steady-state workloads

Use benchmarks not to chase vanity numbers, but to:
- catch regressions
- compare implementations
- guide optimizations
- validate scale assumptions

Performance claims without benchmarks are guesses.

---

### 14. Profile before optimizing

Use profiling to find true bottlenecks.

Do not assume the slowest-looking function is the real problem.

Investigate:
- CPU hotspots
- allocation hotspots
- branch-heavy loops
- lock contention
- serialization costs
- cache-unfriendly access patterns
- synchronization overhead
- repeated conversions
- unnecessary copying

Only optimize what is proven to matter.

---

### 15. Favor cache-friendly layouts

Data layout matters.

Prefer structures and access patterns that support contiguous access and predictable iteration. Be mindful of:
- array-of-structs vs struct-of-arrays tradeoffs
- alignment
- window storage pattern
- reuse of contiguous buffers
- avoiding pointer-heavy graphs in hot loops

The engine should think in terms of throughput, locality, and stable iteration patterns.

---

### 16. Concurrency should be intentional, not decorative

Do not add threads just because the engine is performance-critical.

Concurrency should be used where it is justified by workload and contention profile.

Good candidates:
- partitioned backtests
- parallel parameter sweeps
- independent symbol processing
- separate monitoring or persistence paths
- batch analytics on immutable data

Be careful with:
- shared mutable state
- lock-heavy design
- hidden contention
- non-deterministic ordering
- parallel code that harms cache locality more than it helps throughput

Single-threaded fast code is often better than poorly designed concurrent code.

---

## Testing principles

### 17. Testing is not optional; it is a core feature

This engine must be highly tested.

Testing should cover:
- mathematical correctness
- edge conditions
- state transitions
- warm-up windows
- missing data handling
- empty input handling
- extreme values
- precision edge cases
- risk limit enforcement
- fill logic
- PnL accounting
- deterministic replay
- serialization round-trips if relevant
- configuration parsing for engine-critical settings

A trading engine that is fast but weakly tested is unsafe.

---

### 18. Use layered testing

Use multiple layers of validation.

#### Unit tests
Test isolated formulas, state transitions, and helpers.

#### Property tests
Assert invariants over large randomized input spaces.

Examples:
- position exposure should remain bounded
- realized plus unrealized PnL accounting should remain internally consistent
- rolling metrics should match reference behavior
- state machines should never enter invalid states
- risk gates should never allow forbidden transitions

#### Golden tests
Validate deterministic outputs on known historical sequences or synthetic datasets.

#### Differential tests
Compare optimized implementations against simple reference implementations.

#### Integration tests
Test end-to-end flow from market event stream to signals, simulated trades, and metrics.

#### Stress tests
Run high-event-volume scenarios, long datasets, and edge-market conditions.

Each test layer catches a different class of failure.

---

### 19. Build simple reference implementations first

For any complex optimized computation, start with a simple correct version.

Then:
1. test the simple version
2. use it as the reference
3. implement the optimized version
4. compare outputs across many datasets
5. benchmark both versions

This is especially useful for:
- rolling stats
- fill simulation
- feature extraction
- scoring systems
- risk checks
- portfolio aggregation

A slow reference implementation is extremely valuable.

---

### 20. Fuzz and property-test parsers and state logic

Where the engine accepts external or semi-structured data, use fuzzing and property tests.

Focus on:
- event decoders
- candle aggregation
- feed normalization
- risk input parsing
- strategy parameter parsing
- state machine boundaries

The engine should not panic or silently corrupt internal state because of malformed or partial input.

---

## Signal and decision principles

### 21. Decision logic must be rule-based and explainable

The engine should not “feel” the market.

Decision logic should be encoded as:
- threshold logic
- weighted scores
- rank-based filters
- risk gates
- state-conditioned transitions
- mathematically defined regime filters
- deterministic selection rules

Each decision should be explainable in terms of:
- feature values
- thresholds crossed
- score components
- rejected risk checks
- regime gates passed or failed

If the engine cannot explain why it acted, it is too vague.

---

### 22. Separate feature generation from strategy logic

Do not tightly couple raw market math with final decision policy.

Prefer layering:
1. raw event processing
2. feature computation
3. regime tagging
4. signal scoring
5. risk filtering
6. order or paper-trade action

This separation improves:
- reuse
- testing
- debugging
- benchmarking
- strategy iteration
- auditability

---

### 23. Risk gates should be hard barriers

Signal logic should not bypass risk logic.

Risk checks must be first-class and enforceable:
- max notional
- max loss
- cooldown
- spread filter
- volatility filter
- liquidity minimum
- stale data rejection
- correlated exposure cap
- trade frequency cap
- session boundary restrictions

A good engine does not merely produce signals. It prevents invalid trades.

---

## Backtesting principles

### 24. Backtest realism matters

The backtester should be engineered with realistic assumptions.

Include:
- fees
- spread
- slippage
- latency assumptions where relevant
- partial fills if modeled
- market session boundaries
- warm-up periods
- missing or bad data handling
- realistic order timing assumptions

A high-speed backtester that lies is worse than a slower one that tells the truth.

---

### 25. Backtest engine must be deterministic and versionable

Backtests should be replayable exactly with:
- strategy version
- parameter snapshot
- data range
- random seed if randomness exists
- cost model version
- fill model version

This makes comparisons meaningful and avoids accidental self-deception.

---

### 26. Optimize for throughput without hiding assumptions

The backtesting engine should be fast enough to support:
- broad historical coverage
- repeated validation
- parameter sensitivity analysis
- walk-forward testing
- regression testing

But its speed should never come from removing realism silently.

---

## Observability principles

### 27. Hot-path observability must be low-overhead

The engine needs observability, but hot-path logging must be carefully controlled.

Prefer:
- counters
- histograms
- sampled diagnostics
- structured events at important boundaries
- compile-time or runtime logging levels
- per-batch summaries instead of per-event logs where possible

Do not flood the hot path with verbose string-heavy logs.

---

### 28. Metrics should expose both correctness and performance

Track:
- events processed
- feature update counts
- signal counts
- trade counts
- rejected-risk counts
- benchmark throughput
- allocation-sensitive metrics if useful
- latency percentiles where relevant
- backtest wall time
- memory footprint trends
- deterministic replay mismatches
- panic or recovery counts

The engine should make regressions visible.

---

## API and module principles

### 29. APIs should be narrow and hard to misuse

The core modules should expose small, intentional APIs.

Prefer:
- typed inputs
- typed outputs
- minimal side effects
- explicit error paths
- constructors that validate invariants
- immutable inputs where possible
- hidden internal mutation behind clear boundaries

A clean API protects the engine from accidental complexity.

---

### 30. Configuration must not leak chaos into the core

Configuration should be parsed and validated at the edges.

The hot path should receive already-validated typed settings.

Avoid:
- string lookups in compute loops
- dynamic config interpretation in hot paths
- weakly typed runtime parameter maps
- ambiguous defaults hidden deep in engine logic

The engine should fail fast on invalid configuration.

---

## Suggested module split

A reasonable shape for the Rust engine is:

- `market_data`
  - event types
  - parsers
  - normalizers
  - candle/tick/book aggregation

- `features`
  - rolling stats
  - indicators
  - derived measures
  - regime feature inputs

- `signals`
  - scoring models
  - threshold logic
  - strategy rules
  - selection logic

- `risk`
  - sizing
  - exposure checks
  - kill switches
  - trade gating

- `execution_sim`
  - paper fills
  - cost models
  - slippage models
  - order state transitions

- `portfolio`
  - positions
  - PnL
  - exposure
  - realized/unrealized accounting

- `backtest`
  - replay engine
  - result aggregation
  - scenario execution
  - walk-forward support

- `metrics`
  - counters
  - summaries
  - benchmark hooks
  - profiling helpers

- `bench`
  - criterion or equivalent benchmark suites
  - dataset fixtures
  - throughput and regression checks

- `test_utils`
  - synthetic datasets
  - golden outputs
  - reference implementations
  - deterministic fixtures

This structure is only a suggestion, but the separation of concerns should remain sharp.

---

## Data-driven development (PR model)

### 31. Every change proves itself with data

No strategy, signal, or risk change merges without a backtest comparison against baseline. PRs must include a before/after metrics table showing impact.

The baseline is the backtest result from `main` branch across the diversified symbol universe (tech, oil/gas, energy, metals, pharma, crypto).

The development cycle:
1. Cache historical bars locally (don't re-fetch every run)
2. Run backtest with `main` branch config → baseline
3. Make your change on a feature branch
4. Run backtest with new code → candidate
5. Compare: did metrics improve or stay neutral?
6. PR includes the comparison table, per-category and aggregated

---

### 32. One hypothesis per PR

Each PR is a testable hypothesis:
- "raising min_relative_volume to 1.3 should reduce false entries"
- "adding RSI confirmation should improve win rate in low-vol regimes"
- "tighter stop loss at 1.5% should reduce avg loss without cutting winners"

The PR description states the hypothesis. The backtest data confirms or rejects it. No bundling of multiple signal changes in one PR.

---

### 33. Metrics hierarchy

What must improve (or not regress):
- **Primary**: expectancy, profit factor, Sharpe
- **Secondary**: win rate, max drawdown
- **Neutral**: trade count (more trades isn't inherently better or worse)

A PR that improves win rate but tanks profit factor is a regression. Evaluate holistically.

---

### 34. SQLite journal is the audit trail

Every bar processed gets logged with full feature state and decision outcome:
- Features snapshot (z-score, volatility, volume, etc.)
- Signal output (fired or not, score, reason)
- Risk gate result (passed or blocked, rejection reason)
- Fill result (price, slippage)
- Engine version (git SHA) tagged on every record

The journal enables post-hoc analysis: "why did we lose on this trade?", "what signals did we miss?", "which regime are we weakest in?"

---

### 35. Dual-runtime architecture

Two async runtimes, separated by concern:
- **Trading runtime** (hot path): bar processing, feature computation, signal evaluation, risk gates — synchronous, zero-alloc, deterministic
- **Data runtime** (Tokio): journal writes, benchmark runs, analytics queries, dashboard serving — async, non-blocking, can tolerate latency

The trading hot path must never block on I/O. Journal writes happen on the data runtime via channel.

---

### 36. Claude monitors PRs after creation

After creating a PR, Claude should automatically poll for review comments using `/loop 5m` and iterate on feedback without waiting for the user to relay comments. The loop should:
1. Fetch new review comments via `gh api`
2. Assess whether the comment is actionable
3. Fix the code, commit, and push
4. Continue polling until the PR is merged or the user stops the loop

This replaces manual polling scripts — Claude IS the watcher. No separate infrastructure needed.

---

## Operating instructions

When applying this skill:

1. Reduce all strategy math to explicit formulas or deterministic rules.
2. Ask whether the hot path allocates unnecessarily.
3. Ask whether the module is benchmarked.
4. Ask whether the optimized version has a slower reference version.
5. Ask whether state transitions are explicit and testable.
6. Ask whether the logic is reproducible on replay.
7. Ask whether numeric representation is deliberate.
8. Ask whether APIs make invalid states hard to represent.
9. Ask whether concurrency is justified by evidence.
10. Ask whether performance claims are backed by benchmark data.

---

## Guardrails

Never:
- put fuzzy LLM judgment in the final decision path
- optimize before testing correctness
- accept performance claims without measurements
- accept benchmarks without realistic workloads
- accept backtest speed by hiding cost realism
- allow silent precision drift in accounting-sensitive logic
- tolerate excessive cloning or allocation in hot paths without reason
- use abstraction layers that make the core impossible to profile
- let risk checks become optional
- merge state transitions into unreadable condition trees

Never treat “works on sample data” as enough for a performance-critical engine.

Never let convenience weaken determinism.

---

## Output style

When using this skill, produce guidance that is:
- precise
- engineering-heavy
- performance-aware
- skeptical of hidden cost
- explicit about math and state
- practical for implementation
- focused on correctness first, then measured optimization
- honest about tradeoffs

Prefer language like:
- “make the data flow explicit”
- “measure this with benchmarks”
- “use a simple reference implementation first”
- “avoid allocation in the hot loop”
- “separate feature computation from decision policy”
- “verify deterministic replay”
- “use state machines for lifecycle transitions”
- “profile before optimizing”
- “keep risk checks as hard gates”

Avoid language like:
- “just let AI infer it”
- “we can optimize later without measuring”
- “this abstraction is probably fine”
- “precision does not matter much”
- “it feels fast enough”
- “this backtest is good enough without cost realism”

---

## Definition of done for engine-critical code

A piece of Rust engine code should be considered complete only when:

- the behavior is clearly defined
- invariants are documented
- unit tests exist
- property or differential tests exist where appropriate
- benchmarks exist for hot paths
- edge cases are covered
- allocations are understood
- logs and metrics are appropriate
- API misuse is minimized by design
- the implementation is simple enough to explain
- correctness has not been traded away for superficial speed

---

## Final principle

The Rust quant engine should not be admired because it is clever.

It should be trusted because it is correct, measurable, fast, and hard to fool.