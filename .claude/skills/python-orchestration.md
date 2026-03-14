---
name: python-orchestration
description: Use this skill when designing, implementing, reviewing, or maintaining the Python orchestration layer. Apply it for data ingestion, monitoring, dashboards, alerts, scheduling, ETL workflows, deployment scripting, and any Python code that supports the Rust core without contaminating it.
---

# Python Orchestration Layer

## Purpose

This skill defines how the Python layer of the quant system must be built.

The Python layer is not the brain. The brain is the Rust engine. The Python layer is the nervous system: it connects, monitors, schedules, fetches, transforms, alerts, and presents. It handles the messy boundary between the deterministic core and the unpredictable external world.

This layer must be:
- well-organized but not over-engineered
- clear about what it owns and what it delegates
- reliable enough for production workflows
- flexible enough for research iteration
- observable
- testable at its boundaries
- never a place where trading decisions are made

Python is where the system breathes.
Rust is where the system thinks.

---

## When to use

Use this skill whenever you are:

- building data ingestion pipelines
- writing source adapters for market data, filings, news, or calendars
- building ETL workflows
- creating monitoring dashboards
- implementing alerting systems
- scheduling recurring jobs
- deploying or orchestrating the Rust engine
- building data quality dashboards
- creating research notebooks
- writing integration glue between services
- designing the Python project structure
- deciding what belongs in Python versus Rust
- reviewing Python code for reliability and clarity

Use this skill especially when there is a risk of:
- Python code becoming a second decision engine
- spaghetti scripts replacing structured orchestration
- monitoring gaps
- untested data pipelines
- deployment that depends on manual steps
- research notebooks becoming production code

---

## Core mindset

The Python layer should not compete with the Rust engine.

It should support it.

The division of responsibility is:

| Python owns | Rust owns |
|---|---|
| Data collection and ingestion | Canonical event processing |
| Source adapters | Feature computation |
| ETL and batch processing | Signal evaluation |
| Scheduling and orchestration | Risk gating |
| Monitoring and dashboards | Backtesting |
| Alerting | Paper trading core |
| Research notebooks | Portfolio accounting |
| Deployment scripts | Order state management |
| Data quality checks at ingestion | Performance-critical validation |
| Configuration management | Deterministic replay |

If you find yourself writing trading logic in Python, stop and move it to Rust.

If you find yourself writing a web scraper in Rust, stop and move it to Python.

---

## Principles

### 1. Python code must be organized, not scripted

The Python layer should not be a pile of loose scripts.

It should have:
- clear module structure
- explicit entry points
- configuration management
- dependency management
- testing
- logging

A reasonable structure:

```
python/
  ingestion/        # source adapters, data fetching
  etl/              # transformation, normalization, storage
  monitoring/       # dashboards, health checks, metrics
  alerts/           # alerting rules, notification delivery
  scheduling/       # job definitions, cron-like orchestration
  research/         # notebooks, exploratory analysis
  deployment/       # scripts for deploying and managing the engine
  config/           # configuration loading and validation
  tests/            # test suites
  utils/            # shared helpers (kept minimal)
```

Scripts are acceptable for one-off tasks. Production workflows should be structured.

---

### 2. Data ingestion must be reliable and observable

Data ingestion is one of the most important Python responsibilities.

Every ingestion pipeline should:
- fetch from the source
- validate the response
- transform into canonical or near-canonical form
- store reliably
- log success, failure, and anomalies
- report freshness and completeness

Ingestion failures should be:
- detected within minutes, not days
- retried with backoff where appropriate
- alerted on if persistent
- logged with enough detail to debug

A missing data ingestion should never silently cause the strategy to trade on stale inputs.

---

### 3. Source adapters must be thin and isolated

Each external data source should have its own adapter module.

An adapter should:
- handle authentication
- make API calls
- parse responses
- map fields to internal representations
- handle rate limiting
- handle pagination where needed
- emit validation flags
- be independently testable

An adapter should not:
- contain business logic
- compute features
- make trading decisions
- depend on other adapters

Adapters are translation boundaries.

---

### 4. Configuration must be centralized and validated

The Python layer should load configuration from explicit sources:
- environment variables for secrets
- configuration files for parameters
- command-line arguments for overrides

Configuration should be:
- validated at startup
- typed where possible
- documented
- never hardcoded in business logic

Secrets must never appear in:
- source code
- logs
- configuration files committed to version control
- error messages

---

### 5. Monitoring must cover the full pipeline

The Python layer should monitor:
- data ingestion health
- data freshness
- data quality metrics
- pipeline job success and failure
- Rust engine process health
- system resource usage
- error rates and types
- alert delivery health

Monitoring should answer:
- is data flowing?
- is it fresh?
- is it valid?
- is the engine running?
- are there errors?
- was I alerted when something broke?

A system without monitoring is a system that fails silently.

---

### 6. Alerting must be actionable

Alerts should be:
- specific about what failed
- clear about severity
- delivered through reliable channels
- not so frequent that they are ignored
- testable

Alert categories should include:
- data feed failure
- data staleness
- data quality anomaly
- engine process failure
- execution anomaly
- risk limit breach
- position reconciliation failure
- system resource warning

Alerts should tell the operator what happened and what to check, not just that "something is wrong."

---

### 7. Scheduling must be explicit and recoverable

Recurring jobs should be:
- defined explicitly, not buried in ad hoc cron entries
- logged on start and completion
- monitored for missed runs
- idempotent where possible
- recoverable after failure

Common scheduled jobs:
- data ingestion (market data, filings, news, calendars)
- data quality checks
- monitoring sweeps
- report generation
- storage compaction
- backup routines

The system should know when a job was last run and whether it succeeded.

---

### 8. Research notebooks should remain separate from production

Notebooks are valuable for:
- exploratory analysis
- feature discovery
- strategy prototyping
- visualization
- post-trade review
- data inspection

Notebooks should not:
- become production pipelines
- contain hardcoded credentials
- duplicate production logic
- be the only place where important analysis lives

If a notebook produces a useful result, extract the logic into a proper module before depending on it.

---

### 9. Testing must cover boundaries and integrations

Python tests should focus on:
- adapter response parsing
- data transformation correctness
- configuration validation
- alert rule evaluation
- scheduling behavior
- integration with the Rust engine interface
- error handling paths

Use:
- unit tests for logic
- integration tests for pipeline stages
- mock external APIs for adapter tests
- fixture data for transformation tests

The Python layer is glue, and glue must hold.

---

### 10. Deployment should be repeatable

Deploying the system should not require manual steps beyond what is documented.

The Python layer should support:
- dependency installation from lock files
- environment setup
- engine process management
- health verification after deployment
- rollback if needed

Document every deployment step.
Automate every deployment step that can be automated.

---

## Python-to-Rust interface principles

### 11. The interface between Python and Rust must be narrow and well-defined

The Python layer communicates with the Rust engine through explicit interfaces.

Possible interface mechanisms:
- CLI invocation with structured arguments
- file-based data exchange with defined formats
- IPC or socket communication
- FFI bindings through PyO3 if justified later
- shared database or message queue

The interface should:
- use typed, versioned data formats
- validate inputs on both sides
- handle errors explicitly
- be testable independently

Do not let the Python-Rust boundary become an undocumented mess of ad hoc file drops.

---

### 12. Data format between Python and Rust must be explicit

When Python prepares data for the Rust engine:
- the format must be documented
- the schema must be versioned
- validation must occur before handoff
- the Rust engine should reject malformed input clearly

Common formats:
- Parquet for columnar market data
- CSV for simple tabular data in early stages
- JSON for configuration and metadata
- binary formats for high-throughput paths later

Choose formats based on:
- volume
- read/write performance
- schema enforcement
- tooling support
- debugging convenience

---

## Dependency management principles

### 13. Dependencies must be pinned and minimal

The Python layer should:
- use a lock file for reproducible installs
- minimize the dependency tree
- avoid pulling in large frameworks for small tasks
- audit dependencies periodically
- separate research dependencies from production dependencies

Heavy dependencies for research notebooks are acceptable.
Heavy dependencies in production pipelines should be justified.

---

### 14. Logging must be structured and consistent

All Python modules should use structured logging with:
- timestamps
- log level
- module name
- message
- relevant context fields

Logs should be:
- machine-parseable
- greppable
- rotated or managed to avoid disk fill
- never used to store secrets

---

## Operating instructions

When applying this skill:

1. Keep trading decisions in Rust, not Python.
2. Organize Python code into clear modules, not loose scripts.
3. Make every ingestion pipeline observable and alertable.
4. Keep source adapters thin and independently testable.
5. Centralize and validate configuration.
6. Monitor the full pipeline, not just the engine.
7. Keep research notebooks separate from production code.
8. Test boundary and integration code thoroughly.
9. Define the Python-Rust interface explicitly.
10. Make deployment repeatable and documented.

---

## Guardrails

Never:
- put trading logic in Python
- let notebooks become production pipelines
- hardcode secrets in source code
- ignore ingestion failures
- let monitoring gaps persist
- use unstructured logging in production paths
- let the Python-Rust interface become ad hoc
- deploy with manual undocumented steps
- let research dependencies bloat the production environment
- treat the Python layer as less important than the Rust layer

The Python layer is the immune system. If it fails, the system cannot detect or recover from problems.

---

## Output style

When using this skill, produce guidance that is:
- practical and operations-focused
- clear about responsibility boundaries
- explicit about monitoring and alerting
- organized around reliability
- honest about the role of Python in the system

Prefer language like:
- "keep this in Python, keep that in Rust"
- "make the adapter testable independently"
- "add monitoring for this pipeline"
- "validate configuration at startup"
- "extract this from the notebook into a module"
- "define the data handoff format explicitly"

Avoid language like:
- "just write a quick script"
- "we can monitor this later"
- "the notebook is fine for production"
- "put the signal logic in Python for convenience"
- "deployment is self-explanatory"

---

## Definition of done

A Python orchestration component should be considered complete only when:

- it has a clear module location
- it is tested at its boundaries
- it logs its behavior
- it handles errors explicitly
- it is monitored and alertable
- configuration is externalized
- secrets are not exposed
- it does not contain trading logic
- it is documented enough to operate
- it integrates with the Rust engine through a defined interface

---

## Final principle

A strong Python orchestration layer is not the one with the most features.

It is the one that keeps the system running, observable, and honest while the Rust engine does the thinking.
