# Pair-Picker Consumer Contract Inventory

Everything the rest of the workspace imports from the `pair-picker` crate.
This is the surface that must be preserved (or deliberately broken and
rewritten) when we delete the old pipeline.

## External consumers (must be handled)

### `engine/crates/runner/src/pair_picker_service.rs` (148 lines)
The runtime integration layer. This file wraps the pair-picker for the
`openquant-runner` binary.

**Imports:**
- `pair_picker::graph::RelationshipGraph` — sector/industry relationship filter
- `pair_picker::pipeline::validate_candidates_with_config` — the main entry
- `pair_picker::pipeline::InMemoryPrices` — price data provider
- `pair_picker::pipeline::PipelineConfig` — threshold configuration
- `pair_picker::types::ActivePair` — output type (written to `active_pairs.json`)
- `pair_picker::types::ActivePairsFile` — output file wrapper

**Public API it exposes to the rest of runner:**
- `generate_pairs_with_config(pipeline_cfg, ...) -> Result<Vec<ActivePair>, String>`
- `write_active_pairs(pairs, path)`
- `to_pair_configs(pairs: &[ActivePair]) -> Vec<PairConfig>` — converts pair-picker type to core engine's `PairConfig`

**Blast radius on delete:** Breaks `runner/src/main.rs` which calls all three
public methods above. **Must be rewritten, not stubbed** — the runner needs
*some* way to produce `active_pairs.json`, whether from the new picker or
from a hard-coded curated list.

### `engine/crates/runner/src/main.rs`
The runner CLI binary.

**Imports:**
- `pair_picker::pipeline::PipelineConfig`

**Usage:**
- `PipelineConfig::default()` / `::metals()` / `::force()` — three presets
  selected by `--pipeline` CLI flag
- Passed to `pair_picker_service::generate_pairs_with_config(...)`

**Blast radius:** Minimal — just needs *some* config type. Can be replaced
with a new config struct from the new picker, or deleted entirely if the
new picker reads its config from a file.

### `engine/crates/pybridge/src/lib.rs`
Python bridge — ONLY in `#[cfg(test)]` block at line 723 (test helper).

**Imports:**
- `pair_picker::pipeline::{InMemoryPrices, validate_pair}`
- `pair_picker::types::PairCandidate`

**Blast radius:** Test-only. Easy to delete or rewrite.

## Internal consumers (free to rewrite)

All inside the `pair-picker` crate itself — will be rewritten as part of the
new picker design, no external coordination needed:

- `engine/crates/pair-picker/src/main.rs` — the binary shell (keep structure, replace body)
- `engine/crates/pair-picker/src/scorer.rs` — doc-comment references only
- `engine/crates/pair-picker/tests/integration_pipeline.rs` — old pipeline tests (delete, write new ones)
- `engine/crates/pair-picker/benches/stats_bench.rs` — benchmarks against old ADF/half-life (delete, write new benches against Bertram)

## The minimum viable contract to preserve

For the workspace to keep compiling after the delete, we need at least these
exposed from `pair-picker`:

| Item | Module | Why |
|------|--------|-----|
| `PairCandidate` | `types` | Input format, widely used |
| `ActivePair` | `types` | Output format for `active_pairs.json` |
| `ActivePairsFile` | `types` | JSON wrapper |
| `RelationshipGraph` | `graph` | Orthogonal sector filter, reusable |
| *Some* `PipelineConfig`-like struct | new module | Runner passes it to the picker |
| *Some* `generate_pairs(...)` function | new module | What `pair_picker_service` calls |

Everything else (`pipeline::*`, `stats::*`, `scorer::*`, `thompson::*`,
`regime::*`, `lockfile::*`) can be deleted without breaking external
compilation — we just have to also rewrite `pair_picker_service.rs` at the
same time, because it calls `validate_candidates_with_config`.

## Delete/stub plan (no code yet — just the sequence)

```
Step 1 — preserve skeleton
  Keep:   src/types.rs (PairCandidate, ActivePair, ActivePairsFile, MaxHoldConfig)
  Keep:   src/graph.rs (RelationshipGraph)
  Keep:   src/lockfile.rs (harmless, reusable)
  Keep:   src/lib.rs (trimmed to only re-export the above)
  Stub:   src/main.rs (prints "TODO: new picker" and writes an empty active_pairs.json)

Step 2 — delete the math
  Delete: src/pipeline.rs
  Delete: src/scorer.rs
  Delete: src/regime.rs
  Delete: src/thompson.rs
  Delete: src/stats/ (adf, halflife, ols, beta_stability)
  Delete: tests/integration_pipeline.rs
  Delete: benches/stats_bench.rs

Step 3 — rewrite runner's integration layer
  Rewrite: runner/src/pair_picker_service.rs
    - generate_pairs_with_config() becomes a thin shim that either
      (a) reads a hand-curated active_pairs.json for Phase 2, OR
      (b) calls the new picker (once it exists)
    - to_pair_configs() and write_active_pairs() stay, they're just conversions

Step 4 — temporarily point runner at a curated list
  Update: runner/src/main.rs
    - Remove PipelineConfig::metals/force/default switch (or leave as
      no-op enum) — runner reads directly from trading/active_pairs.json
      for now, no "generate" step at all

Step 5 — workspace should compile and all existing tests pass
  cargo build --workspace
  cargo test --workspace
  cargo run -p openquant-runner -- replay --engine snp500 ...
```

**Critical invariant:** after Step 5, the engine/runner still work using a
pre-existing `trading/active_pairs.json` (from whatever source) and the
backtest infrastructure is intact. Then we can build the new picker at
leisure, run it in the autoresearch loop, and turn the results back into
`active_pairs.json` — without the workspace ever being broken.

## What the new picker must eventually provide

Based on how `pair_picker_service.rs` is used by the runner today, the new
picker's public API probably looks like:

```rust
pub fn generate_active_pairs(
    candidates: &[PairCandidate],
    oracle: &OracleData,          // new — the ~/quant-data/oracle/ verdicts
    config: &PickerConfig,        // new — per-approach knobs
) -> Vec<ActivePair>;
```

One function, deterministic, testable. The six approach implementations
(`bertram.rs`, `distance.rs`, etc.) each expose a scoring function with a
common trait:

```rust
pub trait PickerApproach {
    fn name(&self) -> &str;
    fn score_pair(&self, pair: &PairData) -> Score;
}
```

And `generate_active_pairs` picks which trait impl to use based on config.

No combining. No pipeline. One approach at a time, selected by config.
