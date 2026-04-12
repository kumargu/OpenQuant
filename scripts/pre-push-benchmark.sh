#!/usr/bin/env bash
# Pre-push hook: runs replay baseline and compares against known-good metrics.
# Informational only — never blocks the push.
#
# Requires mock_alpaca.py running on port 8787:
#   python3 scripts/mock_alpaca.py --port 8787 &
#
# Install:
#   ln -sf ../../scripts/pre-push-benchmark.sh .git/hooks/pre-push
#
# Or run manually:
#   ./scripts/pre-push-benchmark.sh

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

if [[ "$SCRIPT_DIR" == *".git/hooks"* ]]; then
    PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
fi

cd "$PROJECT_ROOT"

echo "============================================"
echo "  OpenQuant Pre-Push Benchmark (Replay)"
echo "============================================"
echo ""

# Known baseline: Jan-Apr 2026 replay with lab top-100 candidates
BASELINE_TRADES=38
BASELINE_WIN_PCT=73.7
BASELINE_PNL=3399
CANDIDATES="pairs/year2026_candidates_top100.json"

if [ ! -f "$CANDIDATES" ]; then
    echo "No candidates file at $CANDIDATES — skipping."
    exit 0
fi

# Check mock server
if ! curl -s http://127.0.0.1:8787/v2/stocks/bars?symbols=AAPL\&timeframe=1Day\&start=2026-01-02\&end=2026-01-03 > /dev/null 2>&1; then
    echo "Mock server not running on port 8787."
    echo "Start with: python3 scripts/mock_alpaca.py --port 8787 &"
    echo "Skipping benchmark."
    exit 0
fi

# Build
echo "Building engine..."
cd engine && cargo build --release -p openquant-runner 2>/dev/null
cd "$PROJECT_ROOT"

# Run replay
echo "Running Jan-Apr 2026 baseline replay..."
ALPACA_DATA_URL=http://127.0.0.1:8787/v2/stocks/bars \
./engine/target/release/openquant-runner replay \
    --engine snp500 \
    --start 2026-01-02 --end 2026-04-09 \
    --candidates "$CANDIDATES" \
    --bar-cache data/bar_cache_2026 \
    > /tmp/pre_push_replay.log 2>&1

# Parse results
RESULT=$(grep "pairs: EXIT" /tmp/pre_push_replay.log | python3 -c "
import re, sys
trades = [float(re.search(r'net_bps=\"([^\"]+)\"', l).group(1)) for l in sys.stdin if 'EXIT' in l]
if not trades: print('0 0.0 0'); sys.exit()
w = sum(1 for t in trades if t > 0)
print(f'{len(trades)} {100*w/len(trades):.1f} {sum(trades):.0f}')
")

TRADES=$(echo "$RESULT" | cut -d' ' -f1)
WIN_PCT=$(echo "$RESULT" | cut -d' ' -f2)
PNL=$(echo "$RESULT" | cut -d' ' -f3)

echo ""
echo "  Metric       Baseline    Current"
echo "  ------       --------    -------"
printf "  Trades       %-11s %s\n" "$BASELINE_TRADES" "$TRADES"
printf "  Win %%        %-11s %s\n" "${BASELINE_WIN_PCT}%" "${WIN_PCT}%"
printf "  P&L (bps)    %-11s %s\n" "+$BASELINE_PNL" "+$PNL"
echo ""

if [ "$TRADES" = "$BASELINE_TRADES" ] && [ "$PNL" = "$BASELINE_PNL" ]; then
    echo "  ✓ Baseline MATCHED"
else
    echo "  ⚠ Baseline CHANGED — include this in your PR description"
fi

echo ""
echo "============================================"
echo "  Push will proceed regardless of results"
echo "============================================"

exit 0
