#!/usr/bin/env bash
# Pre-push hook: runs benchmark comparison against baseline.
# Informational only — never blocks the push.
#
# Install:
#   ln -sf ../../scripts/pre-push-benchmark.sh .git/hooks/pre-push
#
# Or run manually:
#   ./scripts/pre-push-benchmark.sh

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# If run from .git/hooks, adjust project root
if [[ "$SCRIPT_DIR" == *".git/hooks"* ]]; then
    PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
fi

cd "$PROJECT_ROOT"

echo "============================================"
echo "  OpenQuant Pre-Push Benchmark"
echo "============================================"
echo ""

# Check if baseline exists
BASELINE="data/baseline/benchmark.json"
if [ ! -f "$BASELINE" ]; then
    echo "No baseline found at $BASELINE"
    echo "Run: python -m paper_trading.benchmark --save-baseline"
    echo "Skipping benchmark comparison."
    exit 0
fi

# Check if engine needs rebuild
echo "Building engine..."
cd engine && maturin develop --release 2>/dev/null && cd ..
if [ $? -ne 0 ]; then
    echo "WARNING: Engine build failed. Skipping benchmark."
    exit 0
fi

echo ""
echo "Running benchmark comparison..."
echo ""

# Run benchmark with comparison
python -m paper_trading.benchmark --compare --days 30 2>&1 || true

echo ""
echo "============================================"
echo "  Push will proceed regardless of results"
echo "  Include this data in your PR description"
echo "============================================"

# Always exit 0 — informational only
exit 0
