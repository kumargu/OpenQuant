# OpenQuant

A research-first quantitative trading system. Rust core engine, Python sidecar, pairs trading focus.

## Quick Start

```bash
# Build and run pairs trading
./run.sh pairs

# Check results
./run.sh summary

# View status
./run.sh status

# Tail logs
./run.sh logs
```

## Architecture

```mermaid
graph TB
    subgraph Config["⚙️ Configuration"]
        style Config fill:#f0f4ff,stroke:#b0c4de
        TOML["config/*.toml<br/><i>pairs · single · test</i>"]
        PAIRS_JSON["active_pairs.json<br/><i>GLD/SLV, GS/MS, AMD/INTC</i>"]
    end

    subgraph Data["📊 Market Data"]
        style Data fill:#f0fff0,stroke:#90ee90
        BARS["experiment_bars_*.json<br/><i>45 symbols · 1-min · 48 days</i>"]
        PRICES["pair_picker_prices.json<br/><i>daily closes for discovery</i>"]
    end

    subgraph Engine["🦀 Rust Engine"]
        style Engine fill:#fff5f0,stroke:#ffa07a
        RUNNER["openquant-runner<br/><i>standalone binary</i>"]

        subgraph Core["core crate"]
            style Core fill:#fff0f0,stroke:#ddd
            PAIR_ENGINE["PairsEngine<br/><i>spread · z-score · entry/exit</i>"]
            SINGLE_ENGINE["Engine<br/><i>mean-rev · momentum · VWAP</i>"]
            FEATURES["FeatureState<br/><i>GARCH · BOCPD · ADX · VWAP</i>"]
        end

        subgraph Runner["runner crate"]
            style Runner fill:#fff0f0,stroke:#ddd
            BAR_LOADER["Bar Loader<br/><i>filter · sort · deterministic</i>"]
            PNL["P&L Tracker<br/><i>entry/exit matching · costs</i>"]
            TEE["TeeWriter<br/><i>stderr + engine.log</i>"]
        end

        PICKER["pair-picker<br/><i>ADF · OLS · beta stability</i>"]
    end

    subgraph Sidecar["🐍 Python Sidecar"]
        style Sidecar fill:#f5f0ff,stroke:#b0a0d0
        ALPACA["alpaca_client.py<br/><i>bar fetching · orders</i>"]
        CANDIDATES["candidate_generator.py<br/><i>Claude API · pair ideas</i>"]
    end

    subgraph Output["📁 Output"]
        style Output fill:#fffff0,stroke:#daa520
        LOG["data/journal/engine.log<br/><i>append · run IDs · audit</i>"]
        INTENTS["order_intents.json"]
        RESULTS["trade_results.json"]
    end

    TOML --> RUNNER
    PAIRS_JSON --> PAIR_ENGINE
    BARS --> BAR_LOADER
    BAR_LOADER --> RUNNER
    RUNNER --> PAIR_ENGINE
    RUNNER --> SINGLE_ENGINE
    SINGLE_ENGINE --> FEATURES
    RUNNER --> PNL
    RUNNER --> TEE
    TEE --> LOG
    PNL --> RESULTS
    RUNNER --> INTENTS
    PRICES --> PICKER
    PICKER --> PAIRS_JSON
    ALPACA --> BARS
    CANDIDATES --> PAIRS_JSON
```

### Directory Layout

```
config/             ← TOML configs per mode (pairs, single, test)
data/               ← bar data, pair configs, journal logs
engine/crates/
  core/             ← spread computation, z-scores, entry/exit, risk
  runner/           ← standalone binary, bar loading, P&L, logging
  pair-picker/      ← statistical pair validation (ADF, OLS, beta)
  pybridge/         ← PyO3 bridge (optional)
paper_trading/      ← Python sidecar: Alpaca API, bar fetching
```

## Trading Modes

| Mode | Config | Description |
|------|--------|-------------|
| `pairs` | `config/pairs.toml` | Pairs trading only (GLD/SLV, GS/MS, AMD/INTC) |
| `single` | `config/single.toml` | Single-symbol mean-reversion + momentum |
| `test` | `config/test.toml` | Integration testing (pairs, no stale bar check) |

```bash
./run.sh pairs          # default
./run.sh single
./run.sh test
```

## Key Design Decisions

- **Rust-first**: All math, statistics, and trading logic in Rust. Python only for external APIs (Alpaca)
- **Deterministic**: Bars sorted by `(timestamp, symbol)`. Same config = same results every time
- **No overnight risk**: `last_entry_hour=14` blocks entries after 14:00 ET
- **Persistent logs**: `data/journal/engine.log` appends across runs with `run_id=git_commit-timestamp`
- **Config separation**: Pair identity (from JSON) vs trading params (from TOML)
- **Clean builds**: `./run.sh` always rebuilds from clean to avoid stale binaries

## Core Tenets

### Discipline
Research system first, trading system second. No strategy goes live without: observation → hypothesis → backtest → OOS validation → paper trade.

### Mathematics
Every decision reduces to measurable quantities. All evaluation is net of costs. Movement is judged relative to volatility. Complexity is earned after simpler math shows signal.

### Truth
- Backtests are filters, not proof
- Losses are logged clearly, never hidden
- The system discovers truth, not manufactures confidence

### Survival
- Risk management is the main strategy
- A dead strategy cannot improve

## Build Commands

```bash
./run.sh build          # clean build
./run.sh pairs          # build + run pairs
./run.sh clean          # clean artifacts + logs
./run.sh summary        # last run P&L
./run.sh status         # git, pairs, configs overview
./run.sh logs           # tail engine.log

# Manual
cd engine && cargo test --workspace
cd engine && cargo bench -p pair-picker
```

## Logging

Every run appends to `data/journal/engine.log` with:
- **Run ID**: `git_commit-timestamp` (maps logs to code)
- **Startup**: full config dump, pairs loaded, market hours
- **Every trade**: ENTRY/EXIT with timestamp, prices, z-score, bars held
- **P&L**: gross/net bps, dollar amount, exit reason
- **Summary**: total trades, win rate, $/day

```
grep "run_id=70d94da" data/journal/engine.log    # isolate one run
grep "STOP LOSS" data/journal/engine.log          # find risk events
grep "P&L summary" data/journal/engine.log        # all run summaries
```
