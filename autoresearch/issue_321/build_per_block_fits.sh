#!/bin/bash
# Build a fit artifact per (variant, block) using the same as-of cutoff the
# replay used. These give us the exact reject-list and reasons per block.
# Run after the 15 walk-forward replays complete.
set -e
cd "$(dirname "$0")/../.."

mkdir -p autoresearch/issue_321/walkforward_fits

build_one() {
  local v=$1 u=$2 b=$3 as_of=$4
  local out_dir=autoresearch/issue_321/walkforward_fits/$v
  mkdir -p "$out_dir"
  ./engine/target/release/openquant-runner freeze-basket-fits \
    --universe "$u" \
    --as-of "$as_of" \
    --out "$out_dir/${b}_fit.json" 2>&1 | grep -E "wrote|failed|error" | head -1
}

for v_pair in \
  "baseline:config/basket_universe_v1_no_mining_baseline.toml" \
  "dom050:config/basket_universe_v1_no_mining_dom050.toml" \
  "dom060:config/basket_universe_v1_no_mining_statgate2_dom_only.toml" \
  "nomegacaps:config/basket_universe_v1_no_megacaps.toml" \
  "nomegacaps_dom050:config/basket_universe_v1_no_megacaps_dom050.toml"; do
  v="${v_pair%%:*}"
  u="${v_pair#*:}"
  build_one "$v" "$u" test1 2025-01-01
  build_one "$v" "$u" test2 2025-07-01
  build_one "$v" "$u" test3 2026-01-01
done
echo "all 15 per-block fits built"
