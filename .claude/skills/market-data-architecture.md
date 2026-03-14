---
name: market-data-architecture
description: Use this skill when designing the long-term market data architecture for a quant system. Apply it for source selection, canonical schemas, event normalization, feature pipelines, data quality checks, timing semantics, research-vs-production separation, and building a source-agnostic decision engine.
---

# Market Data Architecture

## Purpose

This skill defines how the market data layer for the quant system must be designed.

The goal is not to build a bot around one convenient feed, one website, or one app. The goal is to build a long-term market data architecture that can ingest data from different sources, normalize that data into a stable internal representation, and support disciplined decision-making through deterministic feature computation and strategy evaluation.

This system must be built so that:
- data sources can change without forcing a strategy rewrite
- research and production can share the same internal data model
- timing semantics are explicit
- data quality issues are visible
- the decision engine consumes clean, typed, source-agnostic events
- the architecture can evolve from prototype-grade sources to production-grade sources

We are not building around a vendor.  
We are building around a decision pipeline.

---

## When to use

Use this skill whenever you are:

- deciding how market data should enter the system
- designing canonical schemas for bars, ticks, quotes, filings, news, or events
- choosing between broker feeds, research feeds, unofficial feeds, or official feeds
- building source adapters
- designing normalization and validation logic
- handling timestamps, sessions, holidays, and timezones
- defining feature-generation inputs
- separating research data pipelines from production-grade pipelines
- deciding what data belongs in Python versus Rust
- planning data storage for backtests, paper trading, or live monitoring
- evaluating whether a new data source is good enough for a strategy
- creating a long-term roadmap from prototype to serious system

Use this skill especially when there is a risk of:
- tying strategy logic directly to vendor-specific formats
- building around UI products instead of data contracts
- ignoring timing semantics
- mixing noisy data collection with core decision logic
- overcomplicating the system before the strategy is clear
- assuming better data will magically create edge without a defined decision model

---

## Core mindset

The system should not begin with:

- where do we get prices?
- which app is easiest?
- which site shows a good chart?

The system should begin with:

- what decisions are we trying to make?
- what information is required to make those decisions?
- how quickly must that information be available?
- how accurate must it be?
- how should it be represented internally so strategies remain stable over time?

This is how durable quant systems are built.

The architecture must treat raw data as input material, not as truth in its original form.

---

## First principle

### Decisions come first, sources come second

Do not choose a data source before understanding the decisions the system will make.

For each strategy, define:
- what is being predicted or classified
- what horizon matters
- what latency matters
- what accuracy matters
- what features are needed
- what event types are needed
- what historical depth is needed
- what risk controls depend on this data

Examples:

A slow swing strategy may need:
- daily and intraday bars
- earnings dates
- filing events
- broad news context
- gap and volatility features

A medium-speed intraday strategy may need:
- reliable minute bars or quotes
- session structure
- spread and volume features
- event filters
- better fill realism

A high-speed strategy may need:
- quote and trade streams
- tighter clock discipline
- stronger execution assumptions
- much more care around latency and market microstructure

The data architecture must follow the strategy’s decision needs.

---

## Canonical model principle

### Build around an internal canonical data model

This is the most important architectural rule.

The system must define its own internal market data model.  
Every external source should be translated into that model.

Never let the vendor schema become the engine schema.

Canonical internal types may include:

- `InstrumentId`
- `VenueId`
- `Timestamp`
- `SessionId`
- `TradeEvent`
- `QuoteEvent`
- `BarEvent`
- `CorporateActionEvent`
- `EarningsEvent`
- `FilingEvent`
- `NewsEvent`
- `MarketStatusEvent`
- `FeatureSnapshot`
- `SignalInputSnapshot`

Optional later types may include:

- `OrderBookSnapshot`
- `OrderBookDelta`
- `VolatilitySurfacePoint`
- `FundingRateEvent`
- `EconomicCalendarEvent`

The system should define:
- required fields
- optional fields
- units
- timezone semantics
- precision expectations
- missing-value handling
- ordering expectations

A strategy should consume canonical events, not raw vendor payloads.

This allows:
- source replacement without rewriting strategies
- clean backtesting and replay
- consistent paper trading
- shared feature pipelines
- easier debugging
- stable storage formats

---

## Architecture shape

### The correct pipeline is:

**external sources → source adapters → normalization → validation → canonical events → storage / replay → feature engine → strategy engine**

This means:

1. collect raw data from one or more sources  
2. adapt source-specific fields into internal types  
3. normalize time, symbols, sessions, and units  
4. validate data quality and ordering  
5. produce canonical event streams  
6. persist them in a research-friendly and replay-friendly format  
7. feed those events into the feature engine  
8. allow the strategy and risk engine to operate only on canonical inputs

This layering is what keeps the system stable as it grows.

---

## Source philosophy

### Sources are replaceable; data contracts are not

Treat all external sources as adapters behind a stable contract.

Examples of source categories:

- broker APIs
- exchange or official market data feeds
- research-friendly historical sources
- fundamentals and filing sources
- news or event-enrichment sources
- watchlists or manual tracking tools
- internal derived datasets

Each source should be evaluated on:
- reliability
- historical depth
- timestamp quality
- field completeness
- legal or usage constraints
- cost
- refresh cadence
- symbol mapping complexity
- suitability for the intended strategy horizon

The system should not become emotionally attached to a source.

If a better source appears later, the adapter can change.  
The engine should remain intact.

---

## Research versus production principle

### Research-grade sources are acceptable early, but the system must still be built correctly

In early phases, the project may use easier or cheaper sources for:
- prototyping
- backtesting
- feature discovery
- paper-trading preparation
- dashboards and monitoring
- initial ETL work

That is fine.

But even in early phases:
- data should still flow through adapters
- canonical schemas should still exist
- validation should still exist
- timing should still be modeled honestly
- storage should still preserve event meaning

Do not build a shortcut architecture for the prototype if it will poison the long-term design.

Prototype sources can be temporary.  
Bad architecture tends to become permanent.

---

## Python versus Rust responsibility split

### Python should own flexibility  
### Rust should own determinism

A strong split looks like this:

#### Python responsibilities
- data collection
- API integration
- source adapters at the ingestion edge
- enrichment from filings, news, or calendars
- batch ETL
- data quality dashboards
- anomaly detection
- alerts and monitoring
- notebook-based exploration
- daily refresh workflows
- storage compaction or data export orchestration

#### Rust responsibilities
- canonical in-memory market/event models
- event replay
- deterministic feature computation
- signal evaluation
- risk gating
- backtesting
- paper trading core
- portfolio accounting
- performance-critical validation logic
- benchmarked data-path operations

The decision engine should not depend on Python-specific convenience objects in its critical path.

Python is where the data arrives and gets prepared.  
Rust is where the data becomes disciplined decision input.

---

## Event semantics

### Different event types should not be mixed carelessly

Not all data is the same.

The architecture should distinguish clearly between:

#### Price-derived market events
- trade prints
- quotes
- bars
- snapshots

#### Company and instrument events
- earnings
- splits
- dividends
- symbol changes
- corporate actions
- delistings

#### Information events
- filings
- news
- guidance changes
- analyst revisions if ever included later

#### Session and market-structure events
- market open
- market close
- half-day sessions
- halts
- resume events
- holiday calendars

Strategies should know what event families they depend on.

Do not hide fundamentally different event types behind one vague “data row” structure.

---

## Timing principles

### Timing must be explicit everywhere

Many quant mistakes come from time confusion rather than math errors.

The system must define clearly:
- event timestamp
- source timestamp
- ingestion timestamp
- exchange-local vs UTC representation
- session boundary rules
- decision timestamp
- fill timestamp in simulation or paper trading
- bar close versus bar availability timing
- whether an event is known at publication time or only at a later normalized availability time

Every important event should answer:
- when did this happen?
- when was it observed?
- when is the strategy allowed to act on it?

This is especially important for:
- earnings events
- news events
- filings
- bar-close strategies
- session-open logic
- overnight gaps
- market holidays
- delayed or stale data

If timing is vague, the backtest will lie.

---

## Session and calendar principles

### Session awareness is part of data correctness

The market data layer should understand:
- regular sessions
- pre-market and post-market if relevant
- half days
- holidays
- special closures
- timezone transitions
- overnight boundaries

The system should not treat time as a continuous naive number line without market context.

A bar at 09:30 and a bar at 02:00 are not equivalent events just because both have a timestamp.

Session awareness affects:
- feature interpretation
- gap detection
- warm-up handling
- risk rules
- daily resets
- event relevance
- slippage assumptions
- signal eligibility

---

## Symbol and instrument identity principles

### Stable internal identity matters more than source ticker text

External sources may disagree on:
- symbol naming
- exchange suffixes
- contract naming
- adjusted historical series
- corporate action handling
- instrument roll conventions

The architecture should use stable internal identifiers such as:
- `InstrumentId`
- `AssetClass`
- `VenueId`
- `PrimaryListingId`
- optional mapping metadata

Never assume a raw ticker string is enough to identify the tradable object safely.

This becomes especially important later for:
- multiple venues
- futures rolls
- corporate actions
- ADRs
- symbol changes
- duplicate ticker namespaces across exchanges

---

## Data quality principles

### The system must verify data quality, not assume it

Every ingestion pipeline should include validation checks.

Checks may include:
- timestamp monotonicity
- duplicate detection
- missing interval detection
- zero or negative price detection where invalid
- zero-volume anomalies
- spread sanity checks
- bar OHLC consistency checks
- quote crossed-market detection if relevant
- symbol mapping failures
- session mismatch detection
- stale update detection
- missing event fields
- impossible corporate action values
- outlier jump flags for manual review

Bad data should not silently pass into the feature engine.

The pipeline should either:
- reject it
- repair it explicitly
- quarantine it
- mark it with quality flags

A strategy should know when its inputs were degraded.

---

## Storage principles

### Store raw enough for replay, normalized enough for reuse

The architecture should preserve the ability to:
- re-run normalization
- debug data issues
- replay historical sequences
- benchmark feature pipelines
- compare source behavior
- build consistent backtests and paper-trading feeds

A healthy approach is to retain multiple layers when feasible:

- raw vendor payloads or compact raw extracts for debugging
- normalized canonical event records
- derived feature snapshots where useful
- aggregated summaries for dashboards

Do not only store the final chart-ready form if you may later need to inspect event semantics or rebuild assumptions.

Storage should support:
- research
- replay
- auditing
- migrations
- source comparison

---

## Feature pipeline principle

### Features are derived from canonical data, not from source-specific hacks

The feature engine should operate on internal event types.

Examples of features:
- returns
- rolling mean and variance
- realized volatility
- momentum scores
- gap size
- rolling z-scores
- spread averages
- volume imbalance
- relative volume
- session trend classification
- event proximity flags
- earnings-window markers
- filing recency markers

If features depend on source quirks, they will become brittle.

Features should be defined in terms of:
- canonical inputs
- explicit windows
- explicit warm-up requirements
- explicit units
- explicit missing-data handling

This is how the same logic can survive a source migration.

---

## Event enrichment principle

### Enrichment should remain separate from the core market tape

News, filings, and event summaries can be useful, but they should not be mixed carelessly into price event streams.

A clean design is:
- market event stream
- event-enrichment stream
- alignment layer that joins them by instrument and time

Examples:
- attach an “earnings within 2 days” flag
- attach a “recent filing” flag
- attach a “major news item within session” flag
- attach a “macro event proximity” flag

That way:
- the base price feed remains clean
- enrichment can be revised or replaced
- strategy logic remains explainable
- event timing stays explicit

LLMs may help summarize enrichment, but the structured event representation should remain numerical or categorical by the time it reaches the engine.

---

## Latency and freshness principle

### The required freshness depends on the strategy horizon

Do not overbuy data quality before the strategy deserves it, but do not understate freshness requirements.

Ask:
- Is end-of-day refresh enough?
- Do we need minute-level updates?
- Do we need near-real-time quotes?
- Do we need event publication timestamps with precision?
- Do we need stale-data alarms within seconds?

A long-term architecture should allow:
- research data at slower cadence
- production data at tighter freshness
- the same canonical contracts for both

This makes the system upgradeable.

---

## Source adapter principle

### Adapters should be thin, testable, and replaceable

Every data source should enter through a source adapter.

An adapter should do only what is necessary to transform external data into canonical form:
- map symbols
- parse timestamps
- convert units
- map field names
- attach source metadata
- preserve source identifiers where useful
- handle missing or malformed fields
- emit validation flags

Adapters should not contain strategy logic.

Adapters should not contain feature logic.

Adapters should not contain decision logic.

They are translation boundaries, not intelligence layers.

---

## Metadata principle

### Every canonical event should carry enough provenance to be audited

Useful metadata may include:
- source name
- source event ID if available
- ingestion timestamp
- normalization version
- validation flags
- adjustment status
- session label
- timezone normalization info

This helps answer:
- where did this value come from?
- was it adjusted?
- was it delayed?
- did validation raise any concerns?
- which normalization rules produced it?

Without provenance, debugging becomes guesswork.

---

## Decision alignment principle

### Data should serve decisions, not overwhelm them

Collecting more data is not the same as making better decisions.

Before adding any new field or source, ask:
- what exact decision will this improve?
- how will it be transformed into a measurable feature?
- how will we test whether it helps?
- does it add noise, latency, or fragility?
- can the strategy explain why it needs this information?

A strong data architecture is selective.

It does not hoard information just because it exists.

---

## Progressive maturity model

### Stage 1: Structured prototype
At this stage:
- use accessible sources
- design canonical schemas
- build adapters
- store normalized events
- build replayable datasets
- keep strategy logic source-agnostic

Goal:
- prove the architecture shape
- begin feature research
- avoid hardcoding vendor assumptions

### Stage 2: Research discipline
At this stage:
- add validation checks
- add event-enrichment streams
- improve symbol mapping
- harden time/session handling
- benchmark replay throughput
- ensure backtests and paper trading consume the same canonical forms

Goal:
- make the research stack trustworthy

### Stage 3: Production seriousness
At this stage:
- improve live data quality
- tighten freshness monitoring
- strengthen failure handling
- reduce reliance on fragile or unofficial feeds where necessary
- align live and historical semantics closely
- define clear SLAs for data availability internally

Goal:
- support real decision-making without source fragility dominating behavior

---

## Guardrails

Never:
- tie strategy logic directly to a vendor response format
- let ticker strings alone define tradable identity
- hide timing semantics
- let enrichment blur the underlying market event truth
- build the prototype in a way that prevents later source replacement
- mix data collection concerns into the performance-critical engine
- assume a prettier source UI means better data quality
- believe that adding more feeds automatically creates edge
- let stale or low-quality data silently pass into signals
- treat manual watchlists as the core system backbone

Never ask:
- “Which site is easiest to scrape?”
before asking:
- “What decision are we trying to support?”

---

## Operating instructions

When applying this skill:

1. Start from the strategy decision, not the source.
2. Define the event types required.
3. Design canonical schemas before writing ingestion shortcuts.
4. Build every source behind a thin adapter.
5. Normalize time, identity, units, and sessions explicitly.
6. Add validation and provenance at ingestion time.
7. Keep enrichment separate from the raw market event stream.
8. Feed only canonical typed events into the feature and strategy engines.
9. Keep Python at the ingestion and monitoring edge.
10. Keep Rust as the deterministic engine core.

---

## Suggested module split

A healthy structure may include:

- `sources/`
  - vendor and broker adapters
  - filing and news adapters
  - calendar adapters

- `canonical/`
  - typed internal event models
  - instrument identity models
  - session and timestamp models

- `normalize/`
  - time normalization
  - symbol mapping
  - adjustment logic
  - unit conversion

- `validate/`
  - quality checks
  - anomaly flags
  - data repair or quarantine logic

- `store/`
  - raw payload retention
  - canonical event storage
  - replay datasets
  - compaction/export

- `enrichment/`
  - news alignment
  - filing alignment
  - earnings alignment
  - event proximity flags

- `replay/`
  - deterministic historical playback
  - session-aware iteration
  - event ordering

- `features/`
  - derived measures from canonical events

- `monitoring/`
  - pipeline health
  - freshness alerts
  - missing data checks
  - source drift detection

This is only a suggested shape, but the boundary discipline matters.

---

## Output style

When using this skill, produce guidance that is:
- architectural
- long-term oriented
- source-agnostic
- explicit about timing
- careful about quality and provenance
- practical for staged implementation
- skeptical of shortcuts that hardcode vendor assumptions

Prefer language like:
- “design the canonical schema first”
- “make the source adapter replaceable”
- “be explicit about event timing”
- “validation should happen before features”
- “data should serve the decision model”
- “separate enrichment from the market tape”
- “keep the strategy engine source-agnostic”

Avoid language like:
- “just wire the API directly into signals”
- “we can clean the timestamps later”
- “ticker strings are enough”
- “more data automatically means better edge”
- “the UI source is good enough for production because it looks correct”

---

## Definition of done

A market-data architecture component should be considered complete only when:

- the decision need is clearly defined
- the required event types are known
- the canonical schema is explicit
- source adapters are thin and testable
- timing semantics are documented
- identity mapping is deliberate
- validation and provenance are included
- storage supports replay and debugging
- enrichment is separated cleanly
- the strategy engine can consume the data without knowing the original vendor format

---

## Final principle

A strong market-data architecture is not the one with the most feeds.

It is the one that turns messy external information into clean, timed, typed, trustworthy inputs for disciplined decisions.