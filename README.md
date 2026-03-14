# OpenQuant

A research-first quantitative trading system. Rust core engine, Python orchestration, Claude-assisted evaluation.

## Core Tenets

### Discipline

This is a research system first and a trading system second. No strategy goes live without passing through observation, hypothesis, backtest, out-of-sample validation, and paper trading — in that order. No-trade is always a valid outcome. Risk limits override signal strength, every override is logged, and the system is built to be comfortable doing nothing when the evidence is not there.

### Mathematics

Every decision in the system must reduce to measurable quantities — values, windows, thresholds, scores, and constraints. Features must have explicit formulas, defined units, and documented warm-up rules. All evaluation is net of costs, never gross. Movement is always judged relative to context: a 0.5% move means nothing without volatility normalization. Complexity is earned only after simpler math has shown signal.

### Truth
- Backtests are filters, not proof
- Losses are logged clearly, never hidden
- Weak evidence is never rebranded as edge
- The system exists to discover truth, not manufacture confidence

### Survival
- Risk management is the main strategy
- Protect small capital like large capital
- A dead strategy cannot improve
- Sloppy small-scale behavior becomes catastrophic large-scale behavior

### Process
- Strategies earn promotion: observe → hypothesize → backtest → validate OOS → paper trade → tiny live → scale
- Every parameter change is a new version
- Repeatability matters more than brilliance
- Focus on decision quality, not prediction fantasy

## Architecture

- **Rust** — deterministic engine: features, signals, risk gates, backtesting, portfolio accounting, execution
- **Python** — orchestration: data ingestion, monitoring, alerts, dashboards, scheduling
- **Claude skills** — structured reasoning: hypothesis critique, evaluation, journaling, self-learning

## Skills

System specs live in `.claude/skills/`:

| Skill | Purpose |
|---|---|
| `quant-core-principles` | Non-negotiable principles and readiness ladder |
| `quant-mathematical-foundations` | Feature design, scoring, risk math, validation math |
| `rust-quant-engine` | Rust engine architecture and performance guidelines |
| `rust-backtesting-engine` | Deterministic replay, fill simulation, walk-forward |
| `market-data-architecture` | Canonical schemas, source adapters, data quality |
| `execution-broker-layer` | Order management, fill reconciliation, failover |
| `python-orchestration` | Ingestion, monitoring, alerts, deployment |
| `strategy-lifecycle` | Promotion, decay detection, versioning, retirement |
| `claude-eval-self-learn` | Evaluation, journaling, pattern recognition, learning |
