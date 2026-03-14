---
name: rust-backtesting-engine
description: Use this skill when designing, implementing, reviewing, or optimizing the Rust backtesting engine for the quant bot. Apply it for deterministic replay, realistic fill simulation, slippage and fee modeling, walk-forward validation, benchmark design, scenario testing, and large-scale historical evaluation.
---

# Rust Backtesting Engine

## Purpose

This skill defines how the Rust backtesting engine must be built.

The backtesting engine is one of the most important truth-telling components in the entire quant system. Its role is not to produce pretty equity curves. Its role is to simulate strategy behavior as honestly, deterministically, and efficiently as possible against historical data, while exposing the gap between imagined edge and actual edge.

The backtesting engine must be:
- deterministic
- realistic enough to be useful
- fast enough to support iteration
- explicit about assumptions
- easy to replay
- versionable
- well-tested
- benchmarked on meaningful workloads
- resistant to hidden optimism
- strict about cost and fill realism

A fast liar is worse than a slow truth-teller.  
This engine must try to be both fast and honest.

---

## When to use

Use this skill whenever you are:

- designing the backtesting engine architecture
- implementing event replay
- modeling fills, slippage, and fees
- simulating order and position lifecycle
- adding walk-forward testing
- building parameter sweep infrastructure
- validating historical strategies
- reviewing whether a backtest is too optimistic
- optimizing replay throughput
- designing benchmark suites for historical workloads
- validating that paper-trading logic and backtest logic are aligned
- building scenario and stress testing flows
- comparing strategy versions across time ranges

Use this skill especially when the backtester must:
- replay large datasets efficiently
- remain deterministic across runs
- support multiple strategy versions
- expose realistic performance net of costs
- avoid self-deception
- serve as the foundation for promotion into paper trading

---

## Core role of the backtesting engine

The backtesting engine exists to answer questions like:

- What would this strategy have done under explicit historical assumptions?
- What was the net expectancy after costs?
- How fragile is the strategy to slippage, latency, and parameter drift?
- Does the strategy behave similarly across different periods and regimes?
- Does the strategy survive out-of-sample validation?
- Is the strategy still worth paper trading?

The engine should not answer:
- “How good can we make this look?”
- “How do we make the chart smoother?”
- “How do we remove losses from the narrative?”

The engine is a validation machine, not a marketing machine.

---

## Core mindset

A backtest is not proof. It is a filter.

A strong backtest means:
- the hypothesis survived one layer of scrutiny

It does not mean:
- the strategy is real
- live trading will behave the same
- the edge is durable
- the system is ready to scale

This skill should keep the engine grounded in humility.

---

## Principles

### 1. Determinism is non-negotiable

Given the same:
- historical input data
- strategy version
- parameter set
- fee model
- slippage model
- fill model
- session rules
- random seed if randomness is used

the engine must produce the same outputs every time.

This includes:
- trades
- fills
- PnL
- drawdown
- equity curve
- exposure
- rejection counts
- performance metrics

Determinism matters because:
- bugs become reproducible
- optimization remains testable
- regressions can be caught
- strategies can be compared fairly
- research can be trusted

If a run cannot be replayed exactly, it is not dependable enough.

---

### 2. Replay should mirror decision timing honestly

The backtester must respect the timing structure of decisions.

The engine should define clearly:
- when data becomes visible
- when a feature is updated
- when a signal is allowed to fire
- when an order is considered placed
- when a fill is allowed to occur
- how delays or bar-close logic are modeled
- whether intrabar knowledge is available or forbidden

Avoid accidental lookahead through:
- using a full candle before it closes when the strategy would not know it yet
- ranking assets using future-complete values at decision time
- letting fills happen at prices that were not observable when the decision occurred
- computing indicators with future bars included silently

The engine must model what the strategy knew at the moment of action.

---

### 3. Fill realism matters more than convenient assumptions

A strategy can appear profitable only because the fill model is too generous.

The engine must treat fill modeling as a core component, not as a cosmetic detail.

Possible models may include:
- next-bar open
- next-tick approximation
- midpoint with slippage
- bid/ask aware fills
- partial fills where relevant
- marketable vs passive order distinction
- latency-aware fill shifts
- bar-range constrained fill rules

The engine should be explicit about:
- whether fills occur at open, close, or another rule
- how stop orders are triggered
- how take-profit orders are evaluated
- how gaps are handled
- how crossing the spread is modeled
- how simultaneous stop and target collisions are resolved

A backtester that hides fill assumptions is not trustworthy.

---

### 4. Fees and slippage are first-class citizens

All meaningful evaluations must be net of realistic costs.

The backtester should support:
- commission
- exchange fees if relevant
- spread cost
- market impact approximations where reasonable
- fixed and variable fee structures
- asset-specific cost models
- time-of-day dependent slippage if useful
- volatility-sensitive slippage assumptions
- sensitivity testing under worse assumptions

The engine should make it easy to answer:
- what does this strategy look like before costs?
- what does it look like after realistic costs?
- how sensitive is it to slightly worse execution?

If profitability disappears under mild cost realism, the edge is weak.

---

### 5. Keep the simulation model explicit

The engine must not bury critical assumptions in scattered code.

Simulation assumptions should be clearly defined and versioned:
- fill rules
- latency model
- fee model
- slippage model
- session open/close rules
- overnight handling
- leverage assumptions if any
- borrowing or funding assumptions if relevant
- warm-up requirements
- exposure rules
- trade rejection rules

The engine should make it difficult to accidentally compare results produced under different hidden assumptions.

---

### 6. Strategy logic and simulation logic must remain separated

Do not mix strategy alpha logic with backtesting mechanics.

Prefer clear layers:
1. historical event stream
2. feature computation
3. signal generation
4. risk gating
5. order intent generation
6. simulation and fill engine
7. portfolio accounting
8. metrics aggregation

This separation improves:
- testability
- reuse
- debugging
- benchmarking
- auditability
- strategy iteration

A strategy should not need to know backtest internals beyond the contract it receives.

---

### 7. Portfolio accounting must be treated as critical logic

Incorrect accounting can create fake edge.

The backtester must use the same portfolio accounting logic defined in `rust-quant-engine` (section 6: explicit data models, and the `portfolio` module). This means the backtester should not reimplement PnL, exposure, or position math — it should consume the shared portfolio module with backtest-specific wrappers where needed.

Backtest-specific concerns on top of the shared portfolio logic:
- tracking capital curve and equity curve over the simulation
- accumulating session-level and regime-level performance slices
- comparing realized costs against cost model assumptions
- flagging periods where accounting precision may be affected by simulation simplifications

Portfolio math should be explicit, tested, and resistant to drift. See `rust-quant-engine` for the authoritative list of accounting behaviors.

---

### 8. Risk gates must operate in backtest exactly as they would in real operation

Do not treat the backtester as a place where risk discipline becomes optional.

The backtester must apply the same risk gates defined in `rust-quant-engine` (section 23: risk gates as hard barriers). The risk module should be shared, not reimplemented. The backtester's only additional responsibility is tracking rejection counts and reasons for post-backtest analysis.

A backtest that ignores real constraints is only measuring fantasy.

---

### 9. Paper trading and backtesting should share core decision logic

The system should avoid building one logic path for backtesting and a different one for paper trading.

The ideal arrangement is:
- same feature code
- same signal code
- same risk gates
- same position logic
- different data source and execution layer

This reduces drift between research confidence and forward behavior.

The more the code paths diverge, the less trustworthy the transition from backtest to paper trading becomes.

---

### 10. Simplicity in simulation rules is better than fake precision

It is acceptable to use simplified models when market microstructure data is unavailable, but the simplification must be honest.

For example:
- fixed slippage may be acceptable for early research
- next-bar-open fills may be acceptable for coarse strategies
- bar-based stop handling may be acceptable if documented

What is not acceptable is pretending that a crude model is highly realistic.

Prefer:
- simple and honest
over
- elaborate and misleading

---

## Data and replay principles

### 11. Historical data integrity must be treated seriously

The backtester is only as trustworthy as its data.

The engine should define how it handles:
- missing bars
- duplicate events
- out-of-order timestamps
- bad prices
- zero-volume bars
- market holidays
- half sessions
- symbol changes or contract rolls if relevant
- adjusted vs unadjusted data
- timezone normalization
- daylight saving transitions if relevant
- corrupted rows

Do not silently smooth over broken data.

Broken inputs should either:
- be repaired explicitly
- be rejected explicitly
- be flagged clearly in results

---

### 12. Time handling must be explicit

Time confusion is a common source of backtest bugs.

The engine should standardize:
- canonical timestamp format
- timezone handling
- session open and close logic
- overnight boundaries
- daily reset timing
- warm-up start timing
- holiday handling
- event ordering rules for equal timestamps

The engine should make it obvious:
- when a trade decision occurred
- when a fill occurred
- when a stop was triggered
- which session a metric belongs to

---

### 13. Warm-up periods must be modeled honestly

Indicators and rolling windows need warm-up history.

The engine should define:
- when features become valid
- whether warm-up bars are excluded from trading
- whether warm-up data is included in the evaluation period or only used for initialization
- how strategies behave before minimum data requirements are met

Do not allow the backtest to use “fully warmed” indicators from a point where they would not yet exist in a real run.

---

### 14. Event ordering should be deterministic and documented

If multiple actions occur at similar timestamps, define the ordering explicitly.

Examples:
- bar close update vs signal generation
- stop trigger vs target trigger within the same bar
- daily reset vs opening trade
- fill processing vs risk limit updates
- portfolio mark-to-market vs end-of-day metric snapshot

Undocumented ordering rules create hidden behavior and inconsistent results.

---

## Validation principles

### 15. In-sample performance is only the first gate

A strategy must not be judged primarily on how good it looks in the development window.

The engine should support:
- train/test splits
- out-of-sample windows
- walk-forward validation
- rolling retraining windows if relevant
- parameter stability analysis
- regime-specific evaluation

The question is not:
- “Can we make it work somewhere?”

The real question is:
- “Does it survive beyond the period that inspired the idea?”

---

### 16. Walk-forward testing should be easy to run

The engine should make walk-forward validation a first-class workflow.

Useful capabilities include:
- multiple sequential training and testing windows
- fixed or rolling calibration logic
- stable evaluation summaries across windows
- window-by-window metrics
- strategy degradation visibility over time

Walk-forward testing matters because it better reflects the way strategies confront changing markets.

---

### 17. Sensitivity analysis should be normal, not optional

If a strategy only works at a razor-thin parameter choice, it is fragile.

The backtester should support testing around:
- threshold values
- window lengths
- stop distances
- take-profit distances
- slippage assumptions
- fee assumptions
- session filters
- volatility filters

A robust strategy should not collapse when a small parameter changes slightly.

---

### 18. Scenario testing should be supported

The backtester should be able to stress strategies under difficult historical conditions.

Examples:
- crash periods
- high-volatility days
- thin liquidity windows
- strong trend regimes
- severe mean-reversion periods
- gap-heavy sessions
- news-heavy intervals
- operationally degraded assumptions such as worse slippage

A strategy should be understood not only at its average, but also at its ugly edges.

---

### 19. Benchmarks should include realistic historical workloads

Benchmarking the backtester only on toy datasets is misleading.

Performance benchmarks should cover:
- short datasets
- medium datasets
- long multi-year datasets
- high-event-volume days
- multi-symbol runs if relevant
- parameter sweep workloads
- walk-forward workloads
- cost-heavy and feature-heavy strategies

Benchmarks should report:
- events per second
- wall time
- memory footprint trends
- allocation-sensitive regressions where useful

Backtest throughput matters because it drives research velocity.

---

### 20. Profile real workloads before optimizing

Do not optimize based on intuition.

Use profiling on representative runs to find:
- hot feature computations
- allocation hotspots
- portfolio accounting bottlenecks
- branch-heavy simulation rules
- slow replay decoding
- lock contention
- cache-unfriendly access patterns
- result aggregation overhead
- serialization bottlenecks

Then optimize only what actually matters.

---

## Testing principles

The backtesting engine follows the layered testing philosophy defined in `rust-quant-engine` (sections 17-20). The same principles apply: unit tests, property tests, golden tests, differential tests, integration tests, and stress tests. Below are the backtester-specific testing concerns.

### 21. Use a slower reference simulator where helpful

For complex optimized simulation logic, keep a simpler trusted reference implementation.

Use it to validate backtest-specific behavior:
- fill simulation correctness
- stop and target trigger logic within bars
- session boundary handling
- warm-up period exclusion
- order timing rules
- multi-step lifecycle transitions during replay

Then compare the optimized engine against the reference across synthetic datasets, historical fixtures, randomized inputs, and edge-case scenarios.

---

### 22. Synthetic datasets should be used deliberately

Do not rely only on real market data to test behavior.

Use synthetic fixtures to force backtest-specific edge conditions:
- gaps through stops
- equal high and low collisions
- repeated same-price bars
- missing intervals
- zero-volume segments
- immediate reversals
- large spread expansions
- stop and target both touched in one bar
- empty sessions
- pathological timestamp orderings

Synthetic datasets make hard cases easier to verify precisely.

---

### 23. Historical fixtures should be stable and versioned

When using real historical slices for golden tests or performance regression tests, fix them as controlled fixtures.

Track:
- source
- symbol or instrument
- date range
- normalization assumptions
- data version
- timezone assumptions

This helps avoid subtle changes caused by upstream data refreshes or cleaning differences.

---

## Output and metric principles

### 25. The engine must produce more than just total return

A backtest summary should include enough information to judge the quality of the result.

Useful outputs include:
- total return
- net PnL
- trade count
- win rate
- average win
- average loss
- expectancy
- max drawdown
- profit factor
- exposure over time
- time in market
- turnover
- rejection counts by reason
- session-level behavior
- regime-tagged metrics if available
- cost breakdown
- slippage sensitivity

A single headline metric can hide a broken strategy.

---

### 26. Output should make weak strategies obvious

The system should not make fragile results look clean.

The backtester should expose:
- concentration of profits in very few trades
- long flat or decaying phases
- heavy dependence on one period
- performance only before costs
- unstable parameter dependence
- regime dependence
- poor out-of-sample survival
- sensitivity to slightly worse slippage

It should be easy to tell when a strategy is too brittle.

---

### 27. Results must be versioned and attributable

Every backtest result should be tied to:
- strategy version
- parameter set
- data range
- cost model version
- fill model version
- engine version
- random seed where applicable
- benchmark or run mode

Without attribution, comparison becomes unreliable.

---

## Performance principles

### 28. Throughput matters because research speed matters

A good backtester should allow rapid iteration without sacrificing realism.

High throughput helps with:
- more validation windows
- larger historical coverage
- quicker rejection of weak ideas
- broader robustness testing
- better engineering productivity

But speed must not come from:
- silently dropping fees
- simplifying fills without disclosure
- removing risk checks
- skipping warm-up realism
- hiding data-quality issues

The backtester should be fast and explicit.

---

### 29. Memory behavior should be understood

Large-scale backtests can quietly become memory-heavy.

The engine should be careful about:
- unnecessary cloning
- repeated materialization of full intermediate results
- keeping too much history in memory when streaming suffices
- string-heavy result payloads
- per-event heap churn
- overly generic containers in hot replay paths

Prefer:
- streaming aggregation where possible
- preallocated buffers
- compact typed structures
- reusable workspaces
- selective retention of detailed traces

Keep rich traces optional, not mandatory in every run.

---

### 30. Parallelism should be applied where it naturally fits

Parallelism can help in:
- independent symbol backtests
- parameter sweeps
- walk-forward partitions
- scenario batch execution

Be careful with:
- shared mutable portfolio state
- lock-heavy metric collection
- non-deterministic reductions
- parallelism that changes result ordering silently

Parallel speedup is useful, but reproducibility must remain intact.

---

## Suggested architecture stance

A strong architecture often separates:

- `replay`
  - event readers
  - timestamp normalization
  - deterministic sequencing

- `features`
  - rolling stats
  - indicator updates
  - regime inputs

- `signals`
  - strategy rules
  - scoring
  - action intents

- `risk`
  - hard gates
  - sizing
  - exposure controls

- `sim`
  - order intents
  - fill engine
  - slippage model
  - fee model
  - order and position state transitions

- `portfolio`
  - cash
  - positions
  - realized/unrealized PnL
  - exposure tracking

- `metrics`
  - run summaries
  - drawdown
  - expectancy
  - trade distributions
  - regime metrics

- `validation`
  - walk-forward
  - sensitivity sweeps
  - scenario runs
  - comparison tooling

- `bench`
  - throughput benchmarks
  - memory-sensitive workloads
  - regression checks

This split is not mandatory, but the boundaries should remain clear.

---

## Operating instructions

When applying this skill:

1. Always ask what the strategy knew at decision time.
2. Always ask how the fill is modeled.
3. Always include fees and slippage.
4. Always treat replay ordering as explicit logic.
5. Always separate signal logic from simulation mechanics.
6. Always require deterministic re-runs.
7. Always compare optimized simulation against a simpler reference where useful.
8. Always push toward out-of-sample and walk-forward validation.
9. Always expose assumptions in the result metadata.
10. Always prefer honest simplification over hidden optimism.

---

## Guardrails

Never:
- use future information implicitly
- hide cost assumptions
- allow undefined fill behavior
- compare runs with different hidden simulation rules
- treat in-sample performance as proof
- optimize throughput by stripping realism silently
- let the backtester and paper-trader diverge without reason
- accept a strategy that only works under perfect fills
- present a fragile backtest as robust
- let pretty charts override ugly metrics

Never let the backtest become a machine for confirming hope.

---

## Output style

When using this skill, produce guidance that is:
- skeptical in a healthy way
- explicit about assumptions
- deterministic in mindset
- performance-aware
- grounded in replay realism
- honest about what a backtest can and cannot prove
- clear about gaps between historical testing and live trading

Prefer language like:
- “make the fill rule explicit”
- “treat this as a hypothesis, not proof”
- “test this out of sample”
- “version the simulation assumptions”
- “compare this against a slower reference”
- “benchmark on realistic workloads”
- “model what the strategy knew at the time”
- “show net-of-cost results”
- “paper trading should still be required”

Avoid language like:
- “the equity curve looks good so it is ready”
- “we can ignore slippage for now”
- “backtest profits are enough evidence”
- “the exact fill details do not matter”
- “we will tune until it looks smooth”
- “out-of-sample can come later”

---

## Definition of done

A backtesting-engine component should be considered complete only when:

- the timing model is clearly defined
- fill behavior is documented
- costs are modeled explicitly
- deterministic replay is verified
- unit tests exist
- property, golden, or differential tests exist where appropriate
- benchmarks exist for realistic workloads
- result metadata captures assumptions
- outputs include more than one vanity metric
- optimized behavior has been checked against a simpler reference where needed
- the component is honest enough to reject weak strategies rather than decorate them

---

## Final principle

A strong Rust backtesting engine is not the one that produces the smoothest historical profits.

It is the one that makes weak strategies fail early, clearly, and reproducibly.