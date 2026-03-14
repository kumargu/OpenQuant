---
name: quant-mathematical-foundations
description: Use this skill when defining, reviewing, or implementing the mathematical foundations of the quant system. Apply it for feature design, scoring logic, risk math, regime detection, expectancy measurement, portfolio accounting, and any decision logic that must be driven by explicit, testable mathematics rather than intuition.
---

# Quant Mathematical Foundations

## Purpose

This skill defines the mathematical foundations required by the quant system.

The goal is not to collect random indicators or decorate charts with formulas. The goal is to identify the core mathematics needed to transform market data into disciplined, testable decision inputs that align with the rest of the system architecture.

This skill must align with:

- `quant-core-principles`
- `rust-quant-engine`
- `rust-backtesting-engine`
- `market-data-architecture`

This means the math must be:

- explicit
- deterministic
- testable
- performance-conscious
- net-of-cost aware
- source-agnostic
- useful for decision-making
- explainable under replay
- compatible with paper trading and backtesting
- grounded in measurable quantities rather than stories

We are not building a chart-reading machine.  
We are building a decision system driven by mathematics.

---

## When to use

Use this skill whenever you are:

- deciding what quantitative features to compute
- defining formulas for signals
- designing scoring systems
- defining regime-detection logic
- building risk calculations
- implementing portfolio accounting
- selecting rolling statistics
- deciding which math belongs in Rust hot paths
- deciding whether a feature is useful or decorative
- designing backtest metrics
- defining how a strategy should judge opportunity
- turning raw market data into structured features
- deciding whether a model input is mathematically meaningful

Use this skill especially when there is a risk of:
- relying on visual chart intuition
- using vague technical-analysis language without formulas
- adding too many indicators without purpose
- confusing feature count with edge
- ignoring cost, volatility, or execution realism
- mixing units or semantics carelessly
- building logic that cannot be benchmarked or audited

---

## Core mindset

Mathematics is the language of disciplined market reasoning.

The system should not ask:
- does this chart look bullish?
- does this candle look strong?
- does this setup feel good?

The system should ask:
- what is the recent return distribution?
- how unusual is this move relative to recent volatility?
- how does current volume compare with baseline?
- is spread or execution cost acceptable?
- is the market in a trend or range regime?
- is expected reward materially better than expected cost and risk?
- does this setup show positive expectancy over enough samples?

The system must prefer measurable quantities over narratives.

---

## First principle

### Every decision must reduce to measurable quantities

No final decision should depend on vague pattern language.

Each strategy decision should ultimately be expressible in terms of:
- values
- windows
- thresholds
- scores
- probabilities
- risk limits
- constraints
- state transitions

This means any final action should be explainable using:
- which features were computed
- their values at decision time
- their formulas
- their valid ranges
- their thresholds or score weights
- the risk gates that passed or failed

If a decision cannot be explained mathematically, it is too vague for the core engine.

---

## Categories of math needed

The mathematical foundations of the system should be thought of in layers.

### 1. Price and return mathematics
These quantify movement.

### 2. Volatility and dispersion mathematics
These quantify uncertainty and unusualness.

### 3. Volume and participation mathematics
These quantify market participation and conviction.

### 4. Time and session mathematics
These quantify market context in time.

### 5. Regime mathematics
These quantify the type of market environment.

### 6. Signal scoring mathematics
These turn features into decisions.

### 7. Risk mathematics
These determine whether a decision is tradable.

### 8. Portfolio and accounting mathematics
These measure outcome honestly.

### 9. Validation mathematics
These determine whether a strategy is actually good.

Each of these layers should be explicit.

---

# 1. Price and return mathematics

## Purpose

Price alone is often not the right decision variable.  
The system usually needs transformations of price.

## Required concepts

### Returns
The engine should support:
- simple return
- log return where useful
- cumulative return over windows
- rolling return over configurable windows

Examples:
- 1-bar return
- 5-bar return
- session-to-date return
- overnight gap return
- return from session open
- return from rolling high or low

Returns are often more meaningful than raw prices because they normalize movement.

### Range
The engine should support measures such as:
- bar high-low range
- normalized range
- range relative to rolling average range
- intraday realized range
- open-to-high and open-to-low excursions
- close location within bar range

Range helps quantify expansion, compression, and intrabar behavior.

### Distance measures
Useful distance features include:
- distance from moving average
- distance from VWAP if supported
- distance from recent high
- distance from recent low
- distance from session open
- distance from breakout threshold
- distance in basis points or volatility-normalized units

Distance measures help capture overextension and reversion opportunity.

---

# 2. Volatility and dispersion mathematics

## Purpose

Movement must be judged relative to context.

A 0.5% move may be large in one regime and meaningless in another.

## Required concepts

### Rolling volatility
The engine should support:
- rolling standard deviation of returns
- exponential volatility estimates
- realized volatility over windows
- intraday volatility estimates
- session-specific volatility baselines

### Range-based volatility proxies
Useful approximations include:
- average true range style calculations
- rolling average range
- gap-adjusted range measures
- range compression and expansion ratios

### Z-scores and normalization
The engine should support:
- z-score of return
- z-score of range
- z-score of volume
- z-score of spread or liquidity proxies
- z-score of momentum features

Z-scores are important because they transform raw values into relative unusualness.

### Drawdown and adverse excursion
The engine should support:
- rolling drawdown
- max adverse move after entry
- max favorable move after entry
- session drawdown
- strategy equity drawdown
- local drawdown from recent peak

These are essential for both signal evaluation and risk control.

---

# 3. Volume and participation mathematics

## Purpose

Price movement without context can be misleading.

Volume and participation often help distinguish noise from meaningful action.

## Required concepts

### Raw and relative volume
The engine should support:
- raw traded volume
- rolling average volume
- relative volume ratio
- volume z-score
- session-normalized volume
- volume compared to same time-of-day baseline

### Volume trend
Useful measures include:
- rolling increase or decrease in volume
- acceleration of volume
- volume bursts
- participation drop-off
- cumulative session volume compared with baseline expectation

### Price-volume relationships
Useful derived math includes:
- return weighted by relative volume
- breakout strength adjusted by volume
- reversion signals penalized during high participation trends
- move significance conditioned on volume regime

Volume should not be treated as decoration.  
It should help qualify whether a move is worth trusting.

---

# 4. Time and session mathematics

## Purpose

Market behavior depends strongly on time.

The same price move can mean different things at the open, midday, or close.

## Required concepts

### Time-of-day features
The engine should support:
- minutes since session open
- minutes until close
- session bucket
- opening window flags
- lunch-hour flags
- closing window flags

### Session-relative metrics
Useful measures include:
- return since open
- high/low since open
- session range so far
- session volume compared to expected
- current volatility compared to session baseline
- time elapsed since last major move

### Gap mathematics
The engine should support:
- overnight gap size
- gap normalized by recent volatility
- gap fill percentage
- continuation vs reversal behavior after a gap
- open relative to prior high/low/close

Time context is not optional.  
The math should know where the market is within the session.

---

# 5. Regime mathematics

## Purpose

A strategy may work in some market conditions and fail in others.

The system needs math that helps classify regime.

## Required concepts

### Trend versus range
Possible mathematical tools include:
- slope of rolling returns
- net move divided by total path length
- directional persistence
- ratio of drift to volatility
- moving-average separation
- breakout follow-through frequency over a rolling window

### Compression versus expansion
Useful measures include:
- recent range compression ratio
- low-volatility window detection
- realized volatility contraction
- sudden volatility expansion
- range percentile relative to recent history

### Participation regime
Useful classifications include:
- high-volume session
- low-volume session
- normal participation
- news-heavy or event-heavy session if event flags exist

### Stress regime
Useful measures include:
- spread widening proxy
- abnormal volatility
- abnormal gap frequency
- sudden instability in intraday ranges
- drawdown-heavy market behavior

Regime math should remain quantitative and explainable.  
Do not let “trend day” or “choppy market” become vague labels.

---

# 6. Signal scoring mathematics

## Purpose

Features must eventually become a tradable or non-tradable judgment.

## Required concepts

### Threshold logic
The simplest valid decision math is threshold-based.

Examples:
- return > threshold
- relative volume > threshold
- volatility between lower and upper bounds
- spread proxy < threshold
- no trade if drawdown or stress regime too high

This is often the best place to begin.

### Weighted scoring
A signal may be defined as a weighted combination of standardized features.

Example shape:
- momentum score
- reversion score
- breakout score
- regime compatibility score
- cost penalty
- risk penalty

Then:
- total score = weighted sum of components
- trade only if total score exceeds threshold

Weighted scores are acceptable if:
- the components are well-defined
- the weights are versioned
- sensitivity is tested
- the score remains explainable

### Ranking mathematics
If multiple opportunities exist, the system may rank them by:
- expected move proxy
- cost-adjusted score
- volatility-adjusted score
- conviction score
- probability estimate from later models if used

Ranking should still remain explicit and testable.

---

# 7. Risk mathematics

## Purpose

A setup may be mathematically interesting and still not be worth trading.

Risk math determines whether the trade is allowed.

## Required concepts

### Position sizing
The system should support sizing based on:
- fixed notional
- risk per trade
- volatility-adjusted position size
- stop-distance-adjusted size
- capital fraction constraints
- portfolio exposure constraints

### Expected reward versus expected risk
Useful math includes:
- projected reward-to-risk ratio
- expected move versus stop distance
- cost-adjusted opportunity score
- trade rejection when expected edge is too close to execution cost

### Exposure control
The engine should support:
- gross exposure
- net exposure
- single-position exposure
- per-symbol exposure
- sector or correlated exposure later if needed
- concurrent trade limits

### Strategy risk state
The engine should track:
- daily drawdown
- rolling drawdown
- losing streak effects if policy uses them
- cooldown triggers
- kill-switch conditions
- abnormal volatility rejection
- stale or low-confidence signal rejection

Risk math is not a side module.  
It is one of the main decision layers.

---

# 8. Portfolio and accounting mathematics

## Purpose

The system must measure performance honestly.

## Required concepts

### PnL accounting
The engine should support:
- realized PnL
- unrealized PnL
- fee-adjusted PnL
- slippage-adjusted PnL
- mark-to-market valuation
- capital curve
- equity curve
- cash tracking where relevant

### Trade-level statistics
Useful metrics include:
- average win
- average loss
- median win
- median loss
- holding time
- max favorable excursion
- max adverse excursion
- trade expectancy
- trade efficiency

### Portfolio-level statistics
Useful metrics include:
- total return
- volatility of returns
- max drawdown
- exposure over time
- turnover
- capital utilization
- streak statistics
- session-level and regime-level performance slices

Accounting math must be internally consistent and heavily tested.

---

# 9. Validation mathematics

## Purpose

The final question is not whether a strategy can produce trades.  
It is whether it has believable edge.

## Required concepts

### Expectancy
This is one of the most important formulas in the system.

The engine should support:
- expectancy per trade
- expectancy after costs
- expectancy by regime
- expectancy by session bucket
- expectancy by signal-strength decile if useful

### Profitability quality metrics
Useful metrics include:
- win rate
- profit factor
- payoff ratio
- average trade return
- median trade return
- drawdown-adjusted return
- stability across windows
- sensitivity to costs
- sensitivity to parameter variation

### Distribution awareness
The system should not rely only on averages.

It should also examine:
- dispersion of trade outcomes
- concentration of profits
- tail losses
- skewness if useful
- dependence on very few outlier trades
- regime-specific breakdowns

### Robustness mathematics
Useful checks include:
- parameter sensitivity
- degraded slippage assumptions
- alternate fee assumptions
- walk-forward consistency
- out-of-sample survival
- confidence intervals later if helpful

A strategy should be judged by robustness, not only by headline return.

---

## Mathematical building blocks for v1

For a strong first version, the system does not need exotic mathematics.

A disciplined v1 should prioritize the following building blocks.

### Price and return v1
- simple returns over multiple windows
- cumulative return
- gap size
- distance from session open
- distance from rolling mean
- bar range
- close position within range

### Volatility v1
- rolling standard deviation
- rolling average range
- volatility-normalized return
- z-score of return
- z-score of range

### Volume v1
- rolling average volume
- relative volume
- volume z-score
- session volume versus expected baseline

### Session v1
- minutes since open
- minutes until close
- opening window
- midday window
- closing window
- return since session open
- session range so far

### Regime v1
- trend slope proxy
- drift-to-volatility ratio
- range compression ratio
- high-volatility versus low-volatility classification
- high-volume versus normal-volume classification

### Risk v1
- fixed risk-per-trade
- volatility-adjusted sizing
- max daily drawdown
- max open exposure
- cost filter
- minimum score threshold
- no-trade in stress regime

### Evaluation v1
- net expectancy
- max drawdown
- win rate
- average win/loss
- profit factor
- exposure time
- performance by session bucket
- performance by regime bucket

This is already enough to build a serious first engine.

---

## What not to prioritize early

The first version should avoid unnecessary mathematical complexity.

Do not rush into:
- deep neural sequence models
- candle-pattern classification as a foundation
- highly nonlinear feature forests with no explanation
- dozens of overlapping indicators
- overfit parameter grids
- pseudo-scientific pattern language
- optimization without stable baseline math

Complexity should be earned only after simpler math has shown signal.

---

## Feature design rules

Every mathematical feature should answer these questions:

1. What does it measure?
2. Why might it matter for the decision?
3. What are its units?
4. What is its valid range?
5. What is its warm-up requirement?
6. How is missing data handled?
7. Is it stable across data sources once normalized?
8. Is it cheap enough for the intended runtime path?
9. Can it be benchmarked?
10. Can it be explained in a trade review?

If a feature cannot answer these questions, it is not ready.

---

## Mathematical hygiene rules

The system must be careful about:
- mixing basis points, percentages, and raw prices
- mixing session-relative and rolling-window values without clarity
- comparing features computed on incompatible scales
- using raw volume across instruments without normalization where needed
- ignoring warm-up behavior
- silently allowing NaN or invalid values
- changing formulas without versioning
- using future information through careless aggregation

The math layer must be clean.

---

## Testing expectations

The mathematical layer must be tested heavily.

This includes:
- unit tests for formulas
- differential tests against simpler reference implementations
- property tests for invariants
- replay consistency tests
- edge-case tests for zero volume, flat prices, gaps, missing data, and extreme moves
- benchmark coverage for hot-path features
- versioned behavior when formulas change

A mathematical feature is not complete just because it compiles.

---

## Rust alignment

This skill must align with `rust-quant-engine`.

That means:
- formulas should be deterministic
- units should be explicit
- rolling computations should be incremental where appropriate
- hot-path allocation should be minimized
- optimized math should be checked against simpler reference versions
- feature generation should remain separate from strategy policy
- risk math should act as hard gates
- all critical math should be benchmarkable

The math layer should feel like a library of precise building blocks, not a pile of ad hoc indicators.

---

## Backtesting alignment

This skill must align with `rust-backtesting-engine`.

That means:
- features must respect decision-time availability
- no future information should leak into feature computation
- regime math should be based on past and current information only
- reward and expectancy calculations must be net of costs
- risk and position-sizing formulas must behave the same under replay
- evaluation math must expose fragility, not hide it

Math that backtests unfairly is bad math for this system.

---

## Market-data alignment

This skill must align with `market-data-architecture`.

That means:
- features should consume canonical events
- formulas should not depend on vendor-specific quirks
- time and session semantics must be explicit
- volume, price, and event inputs must be normalized before feature computation
- event-enrichment features must preserve timing semantics
- instrument identity and source provenance should remain stable outside the formulas

The math should work on internal truth, not source chaos.

---

## Operating instructions

When applying this skill:

1. Reduce each candidate feature to an explicit formula.
2. Prefer normalized values over raw values where context matters.
3. Start with simple math before advanced modeling.
4. Use rolling and incremental statistics where valid.
5. Keep regime features quantitative, not descriptive.
6. Keep risk math as a first-class part of the decision.
7. Measure expectancy net of cost.
8. Reject decorative indicators that do not improve decision quality.
9. Version formula changes.
10. Benchmark hot-path computations.

---

## Guardrails

Never:
- let candles become the main intelligence layer
- use math that cannot be explained
- add indicators just because they are common
- ignore cost and volatility context
- treat raw price as enough without normalization
- let regime labels become vague storytelling
- use future data implicitly in feature calculation
- let risk math be optional
- trust untested formulas in the engine
- confuse mathematical complexity with edge

Never ask:
- what indicator should we add?
before asking:
- what decision problem are we solving?

---

## Output style

When using this skill, produce guidance that is:
- mathematical
- explicit
- engineering-friendly
- careful about units and timing
- aligned with deterministic implementation
- skeptical of decorative complexity
- focused on usefulness in actual decisions

Prefer language like:
- “define the formula explicitly”
- “normalize by volatility”
- “measure this relative to baseline”
- “treat this as a regime feature”
- “make the score additive and explainable”
- “version the threshold and weights”
- “compute expectancy net of cost”

Avoid language like:
- “this indicator is popular”
- “the chart looks strong”
- “we can infer this visually”
- “we should just train on candles”
- “more features must be better”

---

## Definition of done

A mathematical component should be considered complete only when:

- its purpose is clear
- the formula is explicit
- the units are explicit
- the warm-up rules are explicit
- missing-data behavior is defined
- it aligns with canonical market inputs
- it is tested
- it is benchmarked if performance-critical
- it can be explained in strategy review
- it improves actual decision quality rather than cosmetic complexity

---

## Final principle

A strong quant system does not win because it has the most indicators.

It wins because its mathematics turns market data into clear, disciplined, testable decisions.