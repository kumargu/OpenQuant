#!/bin/bash
# Regenerate the trading dashboard after an experiment run.
#
# Usage:
#   ./refresh_dashboard.sh                  # use existing trade_results.json
#   ./refresh_dashboard.sh --run            # re-run backtest first
#   ./refresh_dashboard.sh --pairs GLD/SLV  # single pair only

set -e
ROOT="$(cd "$(dirname "$0")" && pwd)"
DATA="$ROOT/data"
ENGINE="$ROOT/engine"

GREEN='\033[0;32m'
NC='\033[0m'

# Parse args
RUN=false
PAIRS=""
while [[ $# -gt 0 ]]; do
  case $1 in
    --run) RUN=true; shift ;;
    --pairs) PAIRS="$2"; shift 2 ;;
    *) echo "Unknown arg: $1"; exit 1 ;;
  esac
done

if $RUN; then
  echo -e "${GREEN}Running backtest...${NC}"
  rm -f "$DATA/trade_results.json" "$DATA/journal/engine.log"
  mkdir -p "$DATA/journal"
  RUST_LOG=warn "$ENGINE/target/release/openquant-runner" backtest \
    --config "$ROOT/config/pairs.toml" \
    --data-dir "$DATA" --output-dir "$DATA" --warmup-bars 0 2>/dev/null
fi

echo -e "${GREEN}Generating dashboard data...${NC}"
python3 << 'PYEOF'
import json, os, sys
from datetime import datetime, timezone
from collections import defaultdict

data_dir = os.environ.get('DATA_DIR', 'data')

# If specific pairs requested via env, run per-pair; otherwise use existing trade_results
pairs_arg = os.environ.get('PAIRS', '')

if not os.path.exists(f'{data_dir}/trade_results.json'):
    print("No trade_results.json found. Run with --run first.")
    sys.exit(1)

with open(f'{data_dir}/trade_results.json') as f:
    trades = json.load(f)

# Group by pair and date
by_pair = defaultdict(lambda: defaultdict(lambda: {'pnl_bps': 0.0, 'trades': 0, 'wins': 0}))
for t in trades:
    dt = datetime.fromtimestamp(t['exit_ts'] / 1000, tz=timezone.utc)
    day = dt.strftime('%Y-%m-%d')
    pair = t.get('id', 'unknown')
    by_pair[pair][day]['pnl_bps'] += t['return_bps']
    by_pair[pair][day]['trades'] += 1
    if t['return_bps'] > 0:
        by_pair[pair][day]['wins'] += 1

all_pair_data = {}
for pair_name, daily in by_pair.items():
    pair_days = []
    for day in sorted(daily.keys()):
        d = daily[day]
        dollar = d['pnl_bps'] * 2
        wr = d['wins'] / d['trades'] * 100 if d['trades'] else 0
        pair_days.append({
            'date': day, 'pnl_bps': round(d['pnl_bps'], 1),
            'dollar_pnl': round(dollar, 2), 'trades': d['trades'],
            'win_rate': round(wr, 1),
        })
    all_pair_data[pair_name] = pair_days
    total = sum(d['dollar_pnl'] for d in pair_days)
    print(f"  {pair_name}: {len(pair_days)} days, ${total:.0f}")

# Combined ALL view
all_days = defaultdict(lambda: {'pnl_bps': 0.0, 'trades': 0, 'wins': 0, 'dollar_pnl': 0.0})
for pair_name, days in all_pair_data.items():
    for d in days:
        all_days[d['date']]['dollar_pnl'] += d['dollar_pnl']
        all_days[d['date']]['pnl_bps'] += d['pnl_bps']
        all_days[d['date']]['trades'] += d['trades']
        all_days[d['date']]['wins'] += round(d['win_rate'] * d['trades'] / 100)
combined = []
for day in sorted(all_days.keys()):
    d = all_days[day]
    wr = d['wins'] / d['trades'] * 100 if d['trades'] else 0
    combined.append({'date': day, 'pnl_bps': round(d['pnl_bps'], 1),
                     'dollar_pnl': round(d['dollar_pnl'], 2),
                     'trades': d['trades'], 'win_rate': round(wr, 1)})
all_pair_data['ALL'] = combined

dashboard = {
    'generated_at': datetime.utcnow().strftime('%Y-%m-%dT%H:%M:%SZ'),
    'config': 'config/pairs.toml',
    'pairs': all_pair_data,
}
with open(f'{data_dir}/dashboard_data.json', 'w') as f:
    json.dump(dashboard, f)

# Inject into HTML template
template_path = f'{data_dir}/dashboard.html'
with open(template_path) as f:
    html = f.read()

# Replace data (handle both fresh template and previously injected)
import re
html = re.sub(r'const dashboard = .+?;', f'const dashboard = {json.dumps(dashboard)};', html, count=1)
with open(template_path, 'w') as f:
    f.write(html)

print(f"Dashboard updated: {len(all_pair_data)} views")
PYEOF

echo -e "${GREEN}Done. Open data/dashboard.html${NC}"
