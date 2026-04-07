"""Analyze replay.log — break down P&L by 2-week periods."""

import re
import sys
from datetime import datetime, timedelta
from collections import defaultdict

LOG = "autoresearch/replay.log"

# Parse EXIT lines with timestamps and net_bps
trades = []
with open(LOG) as f:
    for line in f:
        if "pairs: EXIT" not in line or "net_bps=" not in line:
            continue
        # Extract timestamp from log line (ISO format at start)
        ts_match = re.match(r'(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2})', line)
        # Extract pair, net_bps, exit reason, and the trade timestamp (ts= field)
        pair_match = re.search(r'pair="([^"]+)"', line)
        bps_match = re.search(r'net_bps="([^"]+)"', line)
        exit_match = re.search(r'exit="([^"]+)"', line)
        trade_ts_match = re.search(r'ts=(\d+)', line)

        if bps_match and trade_ts_match:
            trade_ts = int(trade_ts_match.group(1))
            # Convert ms timestamp to date
            dt = datetime.utcfromtimestamp(trade_ts / 1000)
            trades.append({
                'date': dt,
                'pair': pair_match.group(1) if pair_match else '?',
                'bps': float(bps_match.group(1)),
                'exit': exit_match.group(1) if exit_match else '?',
            })

if not trades:
    print("No trades found in replay.log")
    sys.exit(1)

trades.sort(key=lambda t: t['date'])

# Group by 2-week periods
start = trades[0]['date'].replace(day=1)  # Start of first month
periods = defaultdict(lambda: {'trades': [], 'bps': []})

for t in trades:
    # 2-week bucket: days 1-14 = "first half", 15-end = "second half"
    month_str = t['date'].strftime('%Y-%m')
    half = 'a' if t['date'].day <= 14 else 'b'
    key = f"{month_str}-{half}"
    periods[key]['trades'].append(t)
    periods[key]['bps'].append(t['bps'])

# Print table
print(f"{'Period':<12} {'Trades':>6} {'Wins':>5} {'WR%':>6} {'Stops':>5} {'RevOK':>5} {'Net bps':>9} {'Avg bps':>9} {'Cum bps':>10}")
print("-" * 80)

cumulative = 0
for key in sorted(periods.keys()):
    p = periods[key]
    n = len(p['bps'])
    total = sum(p['bps'])
    avg = total / n if n else 0
    wins = sum(1 for b in p['bps'] if b > 0)
    wr = wins / n * 100 if n else 0
    stops = sum(1 for t in p['trades'] if t['exit'] == 'stop_loss')
    revs = sum(1 for t in p['trades'] if t['exit'] == 'reversion')
    cumulative += total

    # Format period label nicely
    year, month, half = key.split('-')
    month_name = datetime(int(year), int(month), 1).strftime('%b')
    label = f"{month_name} {year} {'1H' if half == 'a' else '2H'}"

    # Color coding via symbols
    indicator = "+" if total > 0 else "-"

    print(f"{label:<12} {n:>6} {wins:>5} {wr:>5.1f}% {stops:>5} {revs:>5} {total:>+9.1f} {avg:>+9.1f} {cumulative:>+10.1f}")

print("-" * 80)
n_total = len(trades)
wins_total = sum(1 for t in trades if t['bps'] > 0)
stops_total = sum(1 for t in trades if t['exit'] == 'stop_loss')
revs_total = sum(1 for t in trades if t['exit'] == 'reversion')
print(f"{'TOTAL':<12} {n_total:>6} {wins_total:>5} {wins_total/n_total*100:>5.1f}% {stops_total:>5} {revs_total:>5} {cumulative:>+9.1f} {cumulative/n_total:>+9.1f}")

# Worst and best trades
print("\nTop 5 winning trades:")
sorted_trades = sorted(trades, key=lambda t: t['bps'], reverse=True)
for t in sorted_trades[:5]:
    print(f"  {t['date'].strftime('%Y-%m-%d')} {t['pair']:<12} {t['bps']:>+8.1f} bps  ({t['exit']})")

print("\nTop 5 losing trades:")
for t in sorted_trades[-5:]:
    print(f"  {t['date'].strftime('%Y-%m-%d')} {t['pair']:<12} {t['bps']:>+8.1f} bps  ({t['exit']})")

# Monthly summary
print("\nMonthly P&L:")
monthly = defaultdict(float)
monthly_count = defaultdict(int)
for t in trades:
    key = t['date'].strftime('%Y-%m')
    monthly[key] += t['bps']
    monthly_count[key] += 1
for key in sorted(monthly.keys()):
    month_name = datetime.strptime(key, '%Y-%m').strftime('%b %Y')
    print(f"  {month_name:<10} {monthly[key]:>+9.1f} bps  ({monthly_count[key]} trades)")
