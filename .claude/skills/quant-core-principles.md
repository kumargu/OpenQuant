---
name: quant-core-principles
description: Use this skill to ground all quant bot work in disciplined, evidence-first principles. Apply it when designing strategy logic, backtests, paper trading flows, risk systems, monitoring, journaling, or any AI-assisted trading workflow.
---

# Quant Bot Core Principles

## Purpose

This skill defines the non-negotiable principles for building a quant bot in a disciplined way.

The goal is not to build a gambling engine, a hype-driven AI trader, or a system that chases noise. The goal is to build a research-first trading system that can observe markets, test hypotheses, simulate trades, measure outcomes honestly, and only graduate to live trading if real evidence of edge exists.

This skill should keep the project anchored in truth, restraint, survival, and repeatability.

---

## When to use

Use this skill whenever you are:

- designing a quant bot architecture
- defining strategy rules
- planning market data collection
- building a backtesting engine
- setting up paper trading
- evaluating whether a strategy has edge
- defining risk management
- deciding how Claude should assist the workflow
- reviewing whether a new idea is disciplined or speculative
- writing journaling, monitoring, or governance logic
- deciding whether a system is ready for live trading

Use this skill especially when there is a risk of drifting into:
- overtrading
- vague AI-driven discretion
- backtest optimism
- gambler thinking
- weak risk controls
- premature live deployment

---

## Core mindset

You are not building a machine that tries to feel the market like a human gambler.

You are building a system that:
- collects evidence
- forms hypotheses
- tests those hypotheses honestly
- rejects weak ideas quickly
- keeps risk bounded
- avoids self-deception
- values no-trade as a valid outcome
- promotes only validated strategies forward

This is a research system first and a trading system second.

---

## Principles

### 1. Research comes before trading

Treat the project as a research engine before it becomes a money engine.

The first job of the system is to answer:

**Does this strategy show repeatable positive expectancy after realistic costs over a meaningful sample?**

At the beginning, profit is not the main success metric. Truth is.

A failed strategy is still useful if it was rejected honestly.

---

### 2. Do not aim for profits every day

The system must reject the idea that a good bot should make money every day.

Markets do not provide clean setups every day. Forcing daily trades leads to bad behavior:
- overtrading
- weak entries
- rule bending
- emotional drift
- losses disguised as activity

The bot must be comfortable doing nothing.

A valid outcome is:
- no signal
- no trade
- stay out

The real objective is:

**positive expectancy over a large sample, with controlled drawdowns and disciplined execution**

not daily green PnL.

---

### 3. Encode discipline into the system

Do not rely on willpower.

Discipline should be built into rules, state machines, limits, and checks.

The system should enforce:
- explicit entry conditions
- explicit exit conditions
- explicit stop logic
- position sizing rules
- max loss rules
- cooldown or kill-switch logic if needed
- exposure boundaries
- audit logs for intervention

Any override should be visible and reviewable.

If a trade cannot be explained through rules, it should not be trusted.

---

### 4. Every edge is a hypothesis until proven

Do not assume a pattern is real just because it looks good on a chart or sounds convincing.

Every strategy idea must be treated like a testable hypothesis.

Examples:
- opening spikes mean-revert
- breakout signals work only in trend regimes
- certain hours are structurally better
- volatility compression predicts directional expansion
- gap conditions improve follow-through odds

Each idea should pass through this flow:
1. define the hypothesis
2. define exact rules
3. test on historical data
4. include realistic costs
5. test on unseen periods
6. paper trade forward
7. compare expected vs actual behavior

The strategy must earn belief.

---

### 5. The system must tell the truth

This is one of the most important principles.

The bot must be optimized to produce honest results, not comforting results.

That means:
- losses must be logged clearly
- failed strategies must remain visible
- paper trades must be evaluated honestly
- costs must not be hidden
- metrics must not be cherry-picked
- weak evidence must not be rebranded as edge
- strategy edits must not erase past failures

A system that prevents self-deception is more valuable than a system that sounds intelligent.

---

### 6. Claude is an assistant, not the source of edge

Claude can be highly useful in a quant workflow, but it should not be the fuzzy discretionary brain that decides trades loosely.

Claude should help with:
- hypothesis generation
- market context summaries
- news tagging
- anomaly detection assistance
- journaling
- post-trade review
- rule compliance review
- experiment comparison
- operational guidance
- identifying possible regime shifts for later validation

Claude should not replace:
- hard rules
- statistical validation
- execution discipline
- risk management
- measured expectancy

Math and rules come first. Language comes second.

---

### 7. Keep critical logic deterministic

Core decision logic should be explicit, measurable, and reproducible.

The critical engine should preferably handle:
- signal generation
- backtesting
- risk calculations
- order state transitions
- PnL calculations
- metrics aggregation
- rule evaluation

The more important the logic, the less it should depend on ambiguity.

Determinism matters because you need to know why the bot acted.

---

### 8. Paper trading is mandatory

No strategy should go live just because it looked good in backtest.

Paper trading is the bridge between historical confidence and live uncertainty.

Paper trading helps expose:
- hidden execution assumptions
- setup instability
- real-time drift
- alerting gaps
- data feed issues
- signal frequency mismatch
- behavioral mismatch between backtest and live conditions

A strategy that fails paper trading is not ready.

---

### 9. Protect small capital like large capital

Do not treat small capital casually.

Good habits should begin at the smallest size.

Even early-stage or tiny deployments must still respect:
- risk caps
- consistent sizing
- max drawdown limits
- no revenge trading
- full logging
- strict invalidation rules

Sloppy small-scale behavior becomes catastrophic large-scale behavior.

---

### 10. Risk management is the main strategy

Signals matter, but risk management matters more.

The system should treat risk as a first-class component:
- risk per trade
- max exposure
- max correlated exposure
- stop logic
- daily loss limits
- volatility-aware sizing
- spread and liquidity filters
- stale data detection
- broker failure handling
- abnormal behavior kill switches

The first duty of the bot is survival.

A dead strategy cannot improve.

---

### 11. Repeatability matters more than brilliance

One beautiful trade means very little.

The system should optimize for:
- repeatable setups
- consistent execution
- understandable behavior
- enough sample size
- controlled downside
- measurable expectancy

Do not fall in love with outlier wins.

You are trying to build a process that still makes sense after 100 trades, not a story around 3 lucky ones.

---

### 12. Backtests are useful but dangerous

Backtests are not proof. They are filters.

Be suspicious of:
- lookahead bias
- survivorship bias
- hidden leakage
- unrealistic fills
- ignored slippage
- parameter overfitting
- time-window cherry-picking
- rewriting the hypothesis after seeing outcomes

Prefer:
- simple rules
- walk-forward logic
- out-of-sample testing
- robustness checks
- degraded assumption testing
- net performance, not gross

Backtests should reduce illusions, not create them.

---

### 13. Version all strategy changes

If the rules change, that is a new strategy version.

Track:
- version ID
- parameter set
- date of change
- reason for change
- expected impact
- before/after comparison

Without versioning, you will slowly rewrite history and mistake drift for improvement.

Every strategy should have a lineage.

---

### 14. Journaling is part of the system

Journaling is not optional overhead.

It should record:
- why the setup qualified
- what regime seemed active
- what the system expected
- what actually happened
- whether execution matched the plan
- whether abnormal conditions affected the trade
- whether the signal behaved as designed
- what lesson was learned, if any

Claude can help turn raw logs into usable post-trade insight, but the facts must come from data and rules.

---

### 15. Simplicity beats cleverness early

Do not build a giant multi-factor, multi-timeframe, AI-heavy machine on day one.

Early-stage complexity creates confusion.

Prefer:
- one market
- one timeframe
- one setup family
- one risk model
- one evaluation loop

A simple strategy that survives scrutiny is more valuable than a sophisticated one you cannot explain.

Complexity should be earned.

---

### 16. Regime matters

A strategy may work only in some market conditions.

Study context such as:
- trending vs ranging
- high vs low volatility
- open vs midday vs close
- event-heavy vs quiet sessions
- broad market strength or weakness
- liquidity and spread conditions
- overnight gap context

Claude may help describe context, but regime filters should ultimately be grounded in measurable data.

A major source of edge is learning when **not** to apply a strategy.

---

### 17. Monitoring is part of safety

A trading system can fail operationally even if the strategy is sound.

Monitor for:
- broken feeds
- stale data
- delayed updates
- duplicate orders
- process crashes
- abnormal spread changes
- unexpected PnL swings
- logging failures
- timezone issues
- broker/API faults

Operational failures can damage capital faster than bad signals.

---

### 18. Separate research mode from live mode

Research mode can be flexible.
Live mode cannot.

Research mode may include:
- trying variants
- replaying data
- comparing thresholds
- verbose logs
- quick experiments

Live mode should include:
- fixed rules
- fixed risk logic
- strict configuration
- controlled deployment
- immediate observability
- minimal hidden changes

A useful operating rule is:

**exploration can be messy, execution cannot**

---

### 19. Do not attach ego to a strategy

Never confuse a strategy idea with your identity.

The system must remain free to conclude:
- this does not work
- this only works in some regimes
- this decayed
- this is too fragile
- this was overfit
- this is not worth trading after costs

You are not here to defend strategies. You are here to test them.

---

### 20. Focus on decision quality, not prediction fantasy

The project should not be built around the fantasy of predicting the market perfectly.

The real objective is to improve decision quality by:
- filtering weak setups
- waiting for predefined alignment
- controlling losses tightly
- measuring results honestly
- adapting slowly
- staying out when edge is unclear
- preserving capital during uncertainty

A quant bot does not need perfect prediction.
It needs a small edge, risk control, and consistency.

---

## Operating instructions

When applying this skill:

1. Always reduce vague trading ideas into explicit hypotheses.
2. Always ask how the idea would be measured.
3. Always include fees, slippage, and execution realism.
4. Always prefer narrow scope over broad ambition.
5. Always treat paper trading as a required stage.
6. Always surface the possibility that there is no edge yet.
7. Always value staying out as a legitimate action.
8. Always prioritize risk and survival over activity.
9. Always challenge optimism that is not backed by data.
10. Always preserve auditability when strategies or parameters change.

---

## Guardrails

Never encourage:
- gambler thinking
- doubling down after losses
- forcing daily trades
- moving to live trading based on weak evidence
- hand-wavy AI discretion without rules
- hiding costs in performance summaries
- rewriting history after strategy failure
- using backtests as final proof
- confusing activity with progress

Never describe a strategy as strong unless the evidence is clear and measured.

Never let “interesting” substitute for “validated.”

Never let the desire to make money override risk discipline.

---

## Initial scope discipline

### Define the asset class and market before building

The system must anchor its first implementation in a specific, narrow scope.

Before writing any engine code, define:
- which asset class (equities, futures, crypto, options, etc.)
- which market or exchange
- which instruments or universe (single stock, index constituents, specific pairs, etc.)
- which session structure (regular hours, extended hours, 24/7, etc.)
- which timeframe family (daily, intraday minutes, tick-level, etc.)
- which execution assumptions (market orders, limit orders, latency expectations)
- which cost structure (commissions, spread, exchange fees, funding rates if applicable)

This matters because:
- data architecture depends on the asset class
- session and calendar logic depends on the market
- fee and slippage models depend on the venue
- risk limits depend on the instrument type
- backtesting realism depends on all of the above

A system that tries to be universal on day one will be correct for nothing.

Start with one market, one asset class, one timeframe family. Prove the architecture there. Generalize only after the narrow scope is working and validated.

The initial scope choice should be documented and referenced by all other skills.

---

## Suggested architecture stance

A good split for this style of system is:

- **Rust** for deterministic core logic, signal evaluation, backtesting, state transitions, and metrics
- **Python** for monitoring, orchestration, dashboards, alerts, and lightweight analysis
- **Claude skills** for structured reasoning, journaling, hypothesis critique, market-context summarization, experiment review, and governance support

The language model layer should steer process quality, not replace the quant core.

---

## Readiness ladder

A strategy should move through these stages:

### Stage 1: Observation
Collect market data and describe patterns without trading conclusions.

### Stage 2: Hypothesis
Write a narrow, testable idea with exact conditions.

### Stage 3: Historical testing
Backtest with realistic assumptions and identify whether expectancy appears positive.

### Stage 4: Out-of-sample validation
Test on unseen data or later windows.

### Stage 5: Paper trading
Run forward in real time without capital.

### Stage 6: Tiny live deployment
Trade with the smallest practical size under strict limits.

### Stage 7: Scaling
Only scale if behavior remains stable through enough real samples.

The strategy earns promotion. It is never assumed ready.

---

## Non-negotiable rules

### Rule 1
No real-money trading without successful paper-trading evidence.

### Rule 2
Every evaluation must include realistic fees, slippage, and execution assumptions.

### Rule 3
No-trade is a valid output.

### Rule 4
Manual overrides must be logged.

### Rule 5
Risk limits override signal strength.

### Rule 6
Strategy changes must be versioned.

### Rule 7
If the evidence is unclear, assume no edge yet.

### Rule 8
The system exists to discover truth, not manufacture confidence.

---

## Output style

When using this skill, produce guidance that is:
- calm
- disciplined
- engineering-oriented
- skeptical in a healthy way
- explicit about uncertainty
- resistant to hype
- focused on measurable process
- honest about risk and limitations

Prefer language like:
- “this is only a hypothesis”
- “this needs out-of-sample validation”
- “paper trading should come first”
- “no-trade is a valid decision”
- “the evidence is not strong enough yet”
- “the strategy needs net-of-cost evaluation”

Avoid language like:
- “this will definitely work”
- “AI can feel the market”
- “small daily profit is easy”
- “backtest looks great, so go live”
- “we should trade every day”

---

## Final principle

A good quant bot is not the one that trades the most.

It is the one that lies the least.