---
name: strategy-lifecycle
description: Use this skill when managing the lifecycle of a trading strategy from hypothesis through retirement. Apply it for promotion decisions, decay detection, versioning governance, performance review cadence, kill criteria, and the disciplined process of deciding whether a strategy should advance, hold, or be retired.
---

# Strategy Lifecycle Management

## Purpose

This skill defines how trading strategies are governed through their lifecycle.

The quant-core-principles skill defines a readiness ladder: observation, hypothesis, backtest, out-of-sample, paper trading, tiny live, scaling. This skill defines the governance process that moves strategies up and down that ladder — and eventually retires them.

Without lifecycle governance:
- strategies drift without review
- dead strategies consume resources
- promising strategies are never promoted
- failing strategies are never killed
- versioning becomes chaotic
- decisions are made on feelings instead of evidence

This skill exists to make strategy management systematic, evidence-based, and auditable.

---

## When to use

Use this skill whenever you are:

- deciding whether a strategy should advance to the next stage
- evaluating whether a live strategy should be paused or retired
- reviewing strategy performance on a regular cadence
- detecting strategy decay
- deciding whether a parameter change requires a new version
- defining review criteria for each stage
- building dashboards or reports for strategy oversight
- handling a strategy that has stopped performing
- deciding how to manage multiple concurrent strategies
- defining what "good enough" means for promotion

Use this skill especially when there is a risk of:
- promoting strategies based on hope
- letting failing strategies run too long
- skipping required validation stages
- losing track of what version is running
- making changes without review
- confusing activity with progress

---

## Core mindset

A strategy is not a permanent fixture. It is a hypothesis under continuous evaluation.

Every strategy should be able to answer at any time:
- what stage am I in?
- what evidence supports me being at this stage?
- what would cause me to be promoted?
- what would cause me to be demoted or retired?
- when was I last reviewed?
- what version am I on?

If a strategy cannot answer these questions, it is not governed.

---

## The lifecycle stages

### Stage 1: Observation
**Purpose:** Collect data and notice patterns without trading conclusions.

**Entry criteria:** None. Any market observation can begin here.

**Activities:**
- collect market data
- describe patterns
- note anomalies
- form initial questions

**Promotion criteria:**
- a specific, testable hypothesis has been formulated
- the hypothesis includes exact conditions, not vague narratives

**This stage has no risk.** No capital, no trades, no decisions.

---

### Stage 2: Hypothesis
**Purpose:** Define a narrow, testable idea with exact rules.

**Entry criteria:** A pattern observation that can be reduced to measurable conditions.

**Activities:**
- define entry conditions precisely
- define exit conditions precisely
- define the expected behavior
- specify the instrument, timeframe, and session scope
- specify the risk parameters
- document the reasoning

**Promotion criteria:**
- the hypothesis is specific enough to backtest
- entry, exit, and risk rules are explicit
- the expected edge is stated

**Kill criteria:**
- the hypothesis cannot be reduced to testable rules
- it depends on vague language like "looks strong" or "feels right"

---

### Stage 3: Historical testing (backtest)
**Purpose:** Test the hypothesis against historical data with realistic assumptions.

**Entry criteria:** A fully specified hypothesis with explicit rules.

**Activities:**
- run backtests with realistic costs, slippage, and fill assumptions
- evaluate in-sample performance
- examine trade distribution, not just total return
- check for concentration of profits in few trades
- check for regime dependence

**Promotion criteria:**
- positive net expectancy after costs
- sufficient sample size
- no obvious overfitting signals
- reasonable drawdown characteristics
- performance is not dependent on a single period or few outlier trades

**Kill criteria:**
- negative expectancy after costs
- profitability disappears under mild cost sensitivity
- insufficient trade count
- all profit concentrated in one cluster
- strategy requires unrealistic execution assumptions

**Review cadence:** After each significant backtest run.

---

### Stage 4: Out-of-sample validation
**Purpose:** Test on data the strategy has never seen.

**Entry criteria:** Passed in-sample backtest with honest metrics.

**Activities:**
- run on held-out time periods
- run walk-forward validation
- test parameter sensitivity
- test under degraded assumptions
- compare in-sample and out-of-sample behavior

**Promotion criteria:**
- out-of-sample performance is consistent with in-sample
- walk-forward results do not show systematic degradation
- the strategy is not overly sensitive to small parameter changes
- performance survives degraded cost assumptions

**Kill criteria:**
- out-of-sample performance collapses
- walk-forward shows decay
- small parameter shifts destroy profitability
- performance only exists under optimistic assumptions

**Review cadence:** After each validation round.

---

### Stage 5: Paper trading
**Purpose:** Run forward in real time without capital.

**Entry criteria:** Passed out-of-sample validation.

**Activities:**
- run the strategy on live data with simulated execution
- compare behavior with backtest expectations
- check for data-feed issues, timing bugs, and operational problems
- measure signal frequency and compare with historical expectations
- track simulated execution quality

**Minimum duration:** Long enough to observe a meaningful sample of trades. The exact duration depends on the strategy's expected trade frequency, but it should not be rushed.

**Promotion criteria:**
- behavior matches backtest expectations within reasonable bounds
- no operational issues
- simulated execution quality is acceptable
- signal frequency is consistent with historical patterns
- risk controls function correctly in real time

**Kill criteria:**
- behavior diverges significantly from backtest
- operational failures that cannot be resolved
- signal frequency is drastically different from expected
- risk controls fail or are bypassed

**Review cadence:** Weekly during paper trading.

---

### Stage 6: Tiny live deployment
**Purpose:** Trade with the smallest practical size under strict limits.

**Entry criteria:** Successful paper trading with consistent behavior.

**Activities:**
- deploy with minimum position size
- strict daily loss limits
- compare real fills with paper-trading fills
- measure actual slippage and execution quality
- monitor for behavioral divergence
- full logging and alerting

**Minimum duration:** Long enough to accumulate enough real trades to compare against expectations.

**Promotion criteria:**
- real execution quality is close to paper-trading expectations
- slippage and costs are within acceptable bounds
- strategy behavior matches expectations
- no operational failures
- risk controls function correctly

**Kill criteria:**
- execution quality is materially worse than assumed
- behavioral divergence from paper trading
- operational failures
- risk limit breaches
- costs erode expected edge

**Review cadence:** Weekly during tiny live.

---

### Stage 7: Scaling
**Purpose:** Gradually increase size if behavior remains stable.

**Entry criteria:** Successful tiny live with stable performance.

**Activities:**
- increase position size incrementally
- monitor for capacity effects
- track execution quality at larger size
- watch for market impact
- continue periodic review

**Scaling rules:**
- increase only after sufficient observation at current size
- never more than one step at a time
- revert to smaller size if performance degrades
- monitor execution quality at each size level

**Kill criteria:**
- performance degrades at larger size
- execution quality worsens materially
- market impact becomes significant
- any criteria from stage 6 fail at the new size

**Review cadence:** After each size increase, then ongoing periodic review.

---

## Demotion and retirement

### Demotion

A strategy can be demoted to a previous stage when:
- performance degrades below acceptable thresholds
- market regime changes in a way that invalidates the edge
- execution quality deteriorates
- operational issues arise
- a strategy modification requires re-validation

Demotion is not failure. It is governance.

### Retirement

A strategy should be retired when:
- edge has decayed and shows no sign of recovery
- the market structure that supported the edge has changed
- costs have increased beyond what the strategy can absorb
- the strategy has been demoted multiple times without recovery
- a better strategy has replaced its function
- continued operation is not justified by expected value

Retired strategies should:
- be documented with retirement reasoning
- have their final performance recorded
- remain in version history
- not be restarted without going through the full promotion process again

---

## Decay detection

### What is decay?

A strategy decays when its edge diminishes over time, often because:
- the market regime it exploited has changed
- other participants have discovered the same edge
- execution costs have increased
- liquidity conditions have shifted
- structural changes in market microstructure

### How to detect decay

Monitor for:
- declining rolling expectancy
- increasing drawdowns
- lower win rate over time
- longer losing streaks
- signal frequency changes
- regime shift indicators
- performance divergence from historical baselines
- cost sensitivity increasing

### Decay response protocol

1. flag the strategy for review
2. compare recent performance with historical baselines
3. analyze whether the decay is regime-specific or structural
4. if regime-specific: consider pausing and monitoring
5. if structural: consider demotion or retirement
6. do not increase size to "make up for" declining performance
7. do not modify parameters without creating a new version and re-validating

---

## Versioning governance

### Every parameter change is a new version

A strategy version includes:
- strategy ID
- version number
- parameter set
- entry rules
- exit rules
- risk parameters
- cost model assumptions
- creation date
- promotion history

### Version change rules

- changing a threshold creates a new version
- changing a window length creates a new version
- adding or removing a feature creates a new version
- changing risk parameters creates a new version
- changing the cost model creates a new version
- cosmetic changes (logging, naming) do not require a new version

### Version comparison

When a new version is created:
- run both versions on the same data
- compare performance metrics side by side
- document why the change was made
- document expected impact versus actual impact
- the new version starts at the appropriate validation stage, not at the stage the old version reached

---

## Review cadence

### Mandatory reviews

| Stage | Review frequency |
|---|---|
| Observation | No formal review |
| Hypothesis | Before promotion |
| Backtest | After each significant run |
| Out-of-sample | After each validation round |
| Paper trading | Weekly |
| Tiny live | Weekly |
| Scaling | After each size change, then monthly |

### Review content

Each review should cover:
- current performance versus expectations
- trade distribution quality
- risk limit utilization
- execution quality (for live stages)
- decay indicators
- operational health
- whether promotion, hold, demotion, or retirement is appropriate

### Review output

Each review should produce:
- a decision: promote, hold, demote, or retire
- reasoning for the decision
- action items if any
- next review date

---

## Multi-strategy management

### When running multiple strategies

- each strategy has its own lifecycle
- each strategy has its own risk budget
- aggregate exposure limits must be respected
- correlated strategies should be identified
- portfolio-level risk must be monitored
- one strategy's failure should not kill the others
- resource allocation should favor strategies with stronger evidence

### Capacity planning

Before adding a new strategy:
- verify that aggregate risk limits allow it
- verify that infrastructure can support it
- verify that monitoring can cover it
- verify that the operator can review it

Do not run more strategies than you can govern.

---

## Operating instructions

When applying this skill:

1. Every strategy must know what stage it is in.
2. Promotion requires evidence, not hope.
3. Demotion is governance, not failure.
4. Retirement is a valid and important outcome.
5. Decay must be monitored actively.
6. Parameter changes create new versions.
7. Reviews must happen on schedule.
8. Multi-strategy management requires aggregate risk awareness.
9. Do not skip stages.
10. Do not increase size to recover from losses.

---

## Guardrails

Never:
- promote a strategy without the required evidence
- skip paper trading
- increase size after a losing period as a recovery strategy
- modify parameters without versioning
- let a failing strategy run because "it will come back"
- run more strategies than you can monitor and review
- treat past performance as a guarantee
- let ego prevent retirement of a strategy you created
- restart a retired strategy without full re-validation
- confuse a parameter tweak with a fix

Never ask:
- "can we just go live?"
before asking:
- "has it passed every required stage?"

---

## Output style

When using this skill, produce guidance that is:
- governance-oriented
- evidence-focused
- systematic
- honest about decay and failure
- practical about review cadence
- disciplined about versioning

Prefer language like:
- "what stage is this strategy in?"
- "what evidence supports promotion?"
- "has decay been checked?"
- "this parameter change requires a new version"
- "schedule the next review"
- "retirement is the right decision here"

Avoid language like:
- "it will probably be fine"
- "just push it live"
- "the backtest looked great so it is ready"
- "we can review it later"
- "it is not really a new version"
- "it will recover"

---

## Definition of done

A strategy lifecycle process should be considered complete only when:

- every strategy has a documented stage
- promotion criteria are explicit for each stage
- kill criteria are explicit for each stage
- decay detection is active
- versioning governance is enforced
- reviews happen on schedule
- retirement is treated as a normal outcome
- multi-strategy risk is monitored
- the process is auditable

---

## Final principle

A strong strategy lifecycle process does not keep strategies alive because they once worked.

It keeps strategies alive only while the evidence says they still do.
