# OpenQuant — Claude Instructions

## After creating any PR

1. Start a `/loop 5m` to monitor the PR for review comments
2. When new comments arrive: assess, fix the code, commit, push, and reply to the comment
3. Continue monitoring until the PR is merged or the user cancels

This is mandatory — never create a PR without starting the monitor loop.

## PR requirements

Every PR that touches signals, risk, or strategy must include a backtest comparison table in the description. Run `python -m paper_trading.benchmark --compare` to generate it.

## Epic workflow — reviewer ↔ coder chain

Epics use GitHub labels (`epic/<name>`) to chain work between sessions:

### Coder session
- Poll for open issues with your epic label: `gh issue list --label "epic/<name>" --state open`
- Pick the next unassigned issue, work it, raise a PR referencing the issue
- After PR is merged, check for new issues tagged to the epic (reviewer may have filed follow-ups or bugs found during review)

### Reviewer session
- When filing follow-up issues or bugs found during review, **always tag with the epic label**: `gh issue edit <number> --add-label "epic/<name>"`
- This ensures the coder session picks them up automatically on its next poll
- Reference the parent epic issue in the body so context is traceable

### Active epics
- `epic/pair-discovery` — Pair discovery system (#117). Follow-ups: #129, #134, #136

## Reviewer mode

When reviewing PRs (e.g., from another Claude session or Codex), enforce these standards:

### Before merging — CI gate
```bash
gh pr checks <number>   # ALL checks must pass before merge
```

CI runs: `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test --workspace`, performance gates, Python bridge build. **Never merge with failing CI.**

### Code review standards
- **Rust-first**: All math, statistics, and trading logic in Rust. Python only for Alpaca API integration or ML libraries that require Python ecosystem
- **Mathematical correctness**: Verify formulas against cited research papers. If the implementation diverges from the literature, demand explicit justification with reasoning
- **No magic numbers**: Thresholds must be named constants or in config structs with `Default` impls
- **Structured logging**: Every significant decision must use `tracing::info!/warn!` with structured fields — pair promoted/demoted, regime change, cointegration break, scoring results
- **Tests**: Data-driven and flexible. Synthetic test data generators in shared `test_utils`. Cover edge cases (NaN, zero, empty, boundary conditions)
- **Benchmarks**: Criterion benchmarks for statistical computations and hot paths
- **No tech debt**: No dead code, no duplicate implementations, no TODO-later patterns. When replacing old code, delete it completely
- **Challenge the "why"**: Push back on code comments and implementation reasoning. Do independent research when benchmark results seem off

### Proactive reviewer responsibilities
- **Research ahead**: Between PRs, read the existing code that the next PR will modify. Study the relevant papers. Come prepared, don't just react to diffs
- **Diagnose CI failures**: If CI is broken on main, diagnose and flag it to the author so they can include the fix
- **Do independent research**: When an implementation claims to follow a paper (e.g., "NIG conjugate update per Murphy 2007"), verify the formulas yourself. When benchmark results seem off, research why before accepting

### Mid-sprint review (after every 3-4 merged PRs)
Stop, pull main, and verify everything actually works — don't just review diffs.

**Use agent team mode** — spawn parallel agents in worktrees to avoid conflicting with other active branches:
- **Test runner agent** (worktree): Pull main, run `cargo fmt --check`, `cargo clippy`, `cargo test --workspace`, run binaries end-to-end, verify output files
- **Data checker agent**: Inventory available data, check date ranges, assess what needs backfilling for forward testing
- **Forward test agent** (worktree): If data is available, run the full pipeline end-to-end with real historical data (pair-picker → active_pairs.json → PairsEngine → trade history)

**Analyze what was shipped**:
- **Architecture coherence**: Do shipped components fit together? Are interfaces consistent?
- **Integration gaps**: Functions defined but never called? Assumptions in module A that module B doesn't satisfy?
- **Test coverage**: Cross-module integration tests, or only unit tests per module?
- **Config separation**: Each subsystem owns its own config files. Don't mix unrelated params (e.g., shadow trading config should NOT be in `openquant.toml`)
- **Logging/observability**: Can we trace an entity's full lifecycle through logs?
- **Data readiness**: Do we have enough historical data to run the system with real prices?

Post a summary comment on the parent issue with findings and flag concerns before continuing.

### Testing and experimentation — protected main workflow

**This system manages real money. Main must never be contaminated by experimental changes.**

All testing, config tuning, and experimentation happens in isolated worktrees. Changes flow through a review cycle before reaching main:

```
Tester agent (worktree)                    Reviewer (main)                     Coder (branch)
  │                                          │                                   │
  ├─ Run forward tests on real data          │                                   │
  ├─ Try config/threshold changes            │                                   │
  ├─ Measure P&L impact                      │                                   │
  ├─ Report findings (no code changes        │                                   │
  │  to main — worktree is disposable)       │                                   │
  │                                          │                                   │
  └──→ Send results to reviewer ─────────────┤                                   │
                                             ├─ Quant review: are the            │
                                             │  findings statistically valid?    │
                                             │  Overfitting? Sufficient data?    │
                                             │                                   │
                                             ├─ Create GitHub issue with         │
                                             │  validated changes + evidence ────┤
                                             │                                   ├─ Implement via PR
                                             │                                   ├─ Backtest comparison
                                             ├─ Review PR, verify CI ────────────┤
                                             ├─ Merge only when validated        │
                                             │                                   │
```

**Rules**:
- Tester agents **always** use `isolation: "worktree"`. Never modify main directly
- Tester agents change configs, run backtests, tune thresholds freely in their worktree — it's disposable
- Tester sends back **findings and evidence** (P&L numbers, Sharpe ratios, comparison tables), not code changes
- Reviewer applies **quant research judgment**: Is the sample size sufficient? Is this overfitting? Does it hold OOS?
- Only after quant validation does the reviewer create a GitHub issue with the specific change + evidence
- Coder implements via PR with backtest comparison table (per PR requirements above)
- **No shortcutting**: tester cannot push to main, reviewer cannot apply tester's changes directly. The full cycle must complete

### Merge protocol
1. All review comments addressed
2. `gh pr checks <number>` — all green
3. Squash merge with descriptive subject

## Developer best practices

### Architecture
- **Rust owns all math and trading logic**: Statistics, signals, risk, scoring — zero Python for anything performance-sensitive or correctness-critical. Python is the data plumbing layer only (Alpaca API, bar fetching)
- **Minimize Python bridge dependency**: The Python↔Rust bridge (pybridge) is the most fragile boundary — methods can be missing, types can mismatch. Prefer standalone Rust binaries that read/write JSON over Python FFI where possible. When the bridge is used, test it explicitly
- **Separate crates for separate concerns**: Offline analysis tools (e.g., `pair-picker`) are standalone binaries, not linked into the trading engine. Communicate via JSON files, not shared state
- **Canonical identifiers everywhere**: When two components reference the same entity (a pair, a symbol, a strategy), use a single canonical ID format. Alphabetically ordered, consistent across all producers and consumers. Mismatched IDs cause silent data loss
- **Config structs with Default**: All tunable parameters in structs with `Default` impls. No magic numbers in function bodies. This makes parameters discoverable, documentable, and overridable without recompilation

### Correctness
- **This system manages real money — bugs lose money silently**: Every math operation should be defensively coded. Wrong results are worse than crashes because they go unnoticed
- **Guard NaN/infinity at system boundaries**: Check `is_finite()` and `> 0.0` before `ln()`, `clamp()`, or any math that can propagate NaN. NaN flows through arithmetic silently and corrupts everything downstream
- **Two-pass algorithms for numerical stability**: Single-pass variance formulas (sum_xx - n*mean^2) suffer catastrophic cancellation when values are large and variance is small. Use deviation-from-mean form
- **Time-series tests require contiguous observations**: Statistical tests like ADF assume consecutive data points. Filtering scattered indices creates multi-day gaps that invalidate the test's serial correlation assumptions and bias results (often toward false acceptance — fails unsafe)
- **Verify formulas against literature**: When implementing a statistical method, cite the source paper and verify your formula matches. Wrong signs, missing terms, or incorrect degrees of freedom can silently degrade strategy performance

### Testing
- **Test module seams, not just modules**: Unit tests per module are necessary but insufficient. When a PR bridges two components, include integration tests that verify the data contract between them (e.g., producer writes field X, consumer reads and uses field X correctly)
- **Test the bridge first**: If Rust code adds a new method, verify it's exposed in the Python bridge BEFORE merging. 122 Rust tests mean nothing if the method isn't callable from the orchestration layer
- **Test the rejection path**: Don't just test that good inputs pass — verify bad inputs are correctly rejected (invalid data, stale files, NaN prices, boundary conditions)
- **Shared test_utils module**: Deterministic data generators (`Lcg`, synthetic series) in a shared `#[cfg(test)]` module. Reuse across all test files to ensure consistent ground truth
- **Criterion benchmarks for critical paths**: Know your latency budget. Benchmark statistical computations, hot-path signal generation, and any operation that runs per-bar or per-trade

### Workflow
- **One implementation per concept**: Never have parallel implementations of the same logic in different languages or files. When Rust replaces Python, delete the Python entirely. Duplicate implementations drift and produce conflicting results
- **Reload preserves state**: When refreshing config from an updated file, update parameters (e.g., hedge ratio) on existing objects but preserve their runtime state (warm-up data, open positions). Don't reset what doesn't need resetting
- **Remove dead code aggressively**: If a function is defined but never called, delete it or wire it in. Dead code misleads readers and rots. No "keeping it for later"

## Live trading execution — single pipeline, no shortcuts

**This is the #1 rule: live trades must flow through the same code path as backtests.**

The PNC/USB (-$580) and HD/LOW losses came from bypassing the systematic pipeline — picking pairs ad-hoc that were never backtested, never in the portfolio, and never passed quality gates. Every dollar of loss was preventable by running the existing code.

### Daily workflow

There is exactly ONE command to run each trading day:

```bash
python3 scripts/live_pipeline.py run          # Full cycle: exit → scan → enter → monitor → eod
python3 scripts/live_pipeline.py run --dry    # Same cycle, no orders (signal watching)
```

This single command handles everything in order:
1. Close positions that reverted or hit max_hold
2. Scan the portfolio for new signals (all quality gates applied)
3. Place orders for validated signals
4. Monitor all positions with frozen z-scores and P&L
5. Log end-of-day summary

Individual steps are available (`monitor`, `scan`, `exit`, `eod`) but the `run` command is the primary entry point. Market-hours safety: if run outside 9:30-16:00 ET, it scans and monitors but does not place or close orders.

### Hard rules for live execution

1. **Portfolio gate**: Every live trade must exist in `trading/pair_portfolio.json`. No ad-hoc pair selection. If a pair isn't in the portfolio, it hasn't been backtested and MUST NOT be traded
2. **Same quality gates**: Live entry uses the same `scan_pair()` + quality thresholds as `capital_sim.py` — R²≥0.70, HL≤5.0, ADF≤-2.5, beta>0.1, beta stability, spread_std>0.005
3. **Win rate gate**: Block pair+direction combos with <40% historical win rate in the backtest
4. **Stability gate**: Reject pairs that failed `scan_pair()` on >5 of the last 10 trading days — sign of regime instability
5. **No manual overrides**: Claude MUST NOT place trades by calling Alpaca directly or assembling orders outside the pipeline. If the pipeline rejects a trade, the trade doesn't happen
6. **Single positions file**: `trading/live_positions.json` is the source of truth. Only `live_pipeline.py` writes to it

### What this prevents

| Past failure | Gate that blocks it |
|---|---|
| PNC/USB traded but never backtested | Portfolio gate — PNC/USB not in pair_portfolio.json |
| HD/LOW entered during unstable regime | Stability gate — rejected 12 of 15 prior days |
| PNC/USB SHORT had 20% win rate | Win rate gate — below 40% threshold |
| Ad-hoc pair selection bypassing quality checks | No manual overrides rule |

## Build commands

- Build engine: `cd engine && maturin develop --release`
- Run Rust tests: `cd engine && cargo test`
- Run pair-picker tests: `cd engine && cargo test -p pair-picker`
- Run benchmarks: `cd engine && cargo bench -p pair-picker`
- Run benchmark: `python -m paper_trading.benchmark --category crypto --days 7`
- Save baseline: `python -m paper_trading.benchmark --save-baseline`
