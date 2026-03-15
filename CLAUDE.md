# OpenQuant — Claude Instructions

## After creating any PR

1. Start a `/loop 5m` to monitor the PR for review comments
2. When new comments arrive: assess, fix the code, commit, push, and reply to the comment
3. Continue monitoring until the PR is merged or the user cancels

This is mandatory — never create a PR without starting the monitor loop.

## PR requirements

Every PR that touches signals, risk, or strategy must include a backtest comparison table in the description. Run `python -m paper_trading.benchmark --compare` to generate it.

## Build commands

- Build engine: `cd engine && maturin develop --release`
- Run Rust tests: `cd engine && cargo test`
- Run benchmark: `python -m paper_trading.benchmark --category crypto --days 7`
- Save baseline: `python -m paper_trading.benchmark --save-baseline`
