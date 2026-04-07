# autoresearch — research mode

This is an autonomous research session. You do NOT modify any code. You do NOT run experiments. You READ code, REASON about it, and RESEARCH gaps.

## Setup

1. **Create branch**: `git checkout -b autoresearch/research-<tag>` (read-only branch, no commits needed)
2. **Read this file** and `autoresearch/research_brain.md` for context
3. **Initialize research_log.tsv** with header row
4. **Confirm and go**

## The research loop

LOOP FOREVER:

1. Pick the next code file from the **reading list** below
2. Read it THOROUGHLY — every function, every constant, every comment, every edge case
3. For EACH significant piece of logic, ask yourself:
   - What is this doing? Is the math correct?
   - What does the academic literature say about this approach?
   - Is there a better known method? (cite paper, year, key finding)
   - What edge cases could break this?
   - What assumptions is this making? Are they valid?
   - Does this interact correctly with other components?
   - Is there a bug hiding here?
4. Write findings to `autoresearch/research_log.tsv`
5. After completing a file, write a detailed section in `autoresearch/alpha_prep.md`
6. Move to the next file

## What you CAN do
- Read any file in the repository
- Read `autoresearch/research_brain.md` for prior findings
- Use your knowledge of quant finance literature (Gatev, Avellaneda, de Prado, Chan, Vidyamurthy, etc.)
- Use your knowledge of statistics (ADF, Kalman, GARCH, OU process, copulas, etc.)
- Use your knowledge of Rust best practices (numerical stability, NaN guards, etc.)
- Write to `autoresearch/research_log.tsv` and `autoresearch/alpha_prep.md`

## What you CANNOT do
- Modify ANY source code
- Run ANY commands (no cargo build, no replay, no tests)
- Create PRs or commit code changes
- Skip a file — you must analyze every file in the reading list

## Reading list (in order)

### Phase 1: The Engine (how we trade)
1. `engine/crates/core/src/pairs/mod.rs` — PairState, spread computation, z-score, entry/exit logic, frozen exit context, regime gate, P&L tracking. THIS IS THE MOST IMPORTANT FILE.
2. `engine/crates/core/src/pairs/engine.rs` — PairsEngine, position management, max concurrent pairs, trade recording
3. `engine/crates/core/src/pairs/active_pairs.rs` — pair config loading, ClosedPairTrade, trading history

### Phase 2: The Pair Picker (how we select pairs)
4. `engine/crates/pair-picker/src/pipeline.rs` — validation gates, pass/fail criteria
5. `engine/crates/pair-picker/src/scorer.rs` — scoring formula, priority scoring, max hold computation
6. `engine/crates/pair-picker/src/regime.rs` — regime detection, robustness scoring, threshold adjustment
7. `engine/crates/pair-picker/src/stats/adf.rs` — ADF test implementation
8. `engine/crates/pair-picker/src/stats/halflife.rs` — AR(1) half-life estimation
9. `engine/crates/pair-picker/src/stats/ols.rs` — OLS/TLS regression
10. `engine/crates/pair-picker/src/stats/beta_stability.rs` — rolling beta CV, structural break

### Phase 3: The Runner (how we execute)
11. `engine/crates/runner/src/main.rs` — replay/live/paper modes, bar feeding, warmup
12. `engine/crates/runner/src/alpaca.rs` — API client, bar fetching, order placement
13. `engine/crates/runner/src/stream.rs` — WebSocket connection, live bar handling

### Phase 4: Config and Data
14. `config/pairs.toml` — all parameters, their values, their comments
15. `trading/active_pairs.json` — current pair set, their properties

## research_log.tsv format

Tab-separated, one finding per row:

```
file	function	finding_type	severity	description
```

- file: source file path
- function: function or section name
- finding_type: `bug`, `gap`, `optimization`, `correctness`, `literature`, `edge_case`
- severity: `critical`, `high`, `medium`, `low`
- description: detailed finding with paper references

Example:
```
file	function	finding_type	severity	description
pairs/mod.rs	on_price	gap	high	No Hurst exponent check — Ramos-Requena (2024) shows H<0.5 predicts 2x faster reversion
pairs/mod.rs	on_price	bug	critical	NaN propagation if entry_price_a is zero (division by zero in ret_a computation)
pipeline.rs	validate	literature	medium	Lin et al. (2006) minimum profit condition would reject 88% of non-trading pairs
```

## alpha_prep.md structure

For each file analyzed, write:

```markdown
## [filename]

### What it does
[1-2 sentence summary]

### Findings
1. [finding with severity and paper reference]
2. [finding]
...

### Alpha opportunities
- [specific actionable improvement with expected impact]
```

## NEVER STOP

Once you begin, do NOT pause to ask the human if you should continue. Analyze EVERY file in the reading list. If you finish the list, go deeper — read test files, read the pair-picker tests, look for implicit assumptions in test data that might not hold in production.

The human might be asleep. You work until interrupted.

## Key questions to ask about every piece of code

1. **Is the math right?** Check formulas against cited papers. Wrong signs, missing terms, and incorrect degrees of freedom are common.
2. **Is it numerically stable?** Single-pass variance? Division without zero-check? Large values in ln()?
3. **Does it handle edge cases?** NaN, zero, empty, boundary conditions, negative prices, missing data.
4. **Is the statistical test valid?** Does ADF assume contiguous data? Is the sample size sufficient? Are the critical values correct for the test variant used?
5. **Is there a better method?** Kalman vs OLS? Johansen vs Engle-Granger? Hurst vs ADF? What does 2024-2025 literature recommend?
6. **Does it match what we claim?** Do comments match implementation? Do config comments match actual behavior?
7. **What would a quant at Two Sigma do differently?** Think like a senior quant reviewing this code.
