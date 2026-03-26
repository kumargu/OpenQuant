#!/usr/bin/env python3
"""Quick position + z-score check. Run anytime to see current state."""
import json, math, sys, os, logging
logging.getLogger("urllib3").setLevel(logging.WARNING)

sys.path.insert(0, 'scripts')
from daily_walkforward_dashboard import scan_pair, compute_z

with open(".env") as f:
    env = dict(line.strip().split("=", 1) for line in f if "=" in line and not line.startswith("#"))

from alpaca.trading.client import TradingClient
client = TradingClient(env["ALPACA_API_KEY"], env["ALPACA_SECRET_KEY"], paper=True)

# Load live positions for entry details
live = json.load(open("trading/live_positions.json"))
prices = json.load(open("data/pair_picker_prices.json"))
total_bars = min(len(v) for v in prices.values() if len(v) >= 200)

# Current Alpaca positions
alpaca_pos = {p.symbol: p for p in client.get_all_positions()}

print(f"=== POSITION CHECK ===\n")

total_pnl = 0
for pos in live['positions']:
    a, b = pos['leg_a']['symbol'], pos['leg_b']['symbol']
    pair = pos['pair']
    direction = pos['direction']
    
    # Alpaca P&L
    a_pnl = float(alpaca_pos[a].unrealized_pl) if a in alpaca_pos else 0
    b_pnl = float(alpaca_pos[b].unrealized_pl) if b in alpaca_pos else 0
    net = a_pnl + b_pnl
    total_pnl += net
    
    a_now = float(alpaca_pos[a].current_price) if a in alpaca_pos else 0
    b_now = float(alpaca_pos[b].current_price) if b in alpaca_pos else 0
    
    # Current z-score (from daily data — may be stale intraday)
    params = scan_pair(a, b, prices[a], prices[b], total_bars - 1)
    if params:
        z_now = compute_z(params, a_now if a_now > 0 else prices[a][-1], 
                          b_now if b_now > 0 else prices[b][-1])
        z_entry = pos['signal']['z_entry']
        # Frozen z using entry stats
        if a_now > 0 and b_now > 0:
            spread_now = math.log(a_now) - params.alpha - params.beta * math.log(b_now)
            frozen_z = (spread_now - pos['signal'].get('spread_mean', params.spread_mean)) / \
                       pos['signal'].get('spread_std', params.spread_std)
        else:
            frozen_z = z_now
    else:
        z_now = z_entry = frozen_z = 0
    
    z_moved = frozen_z - z_entry
    reverting = (direction == "LONG" and z_moved > 0) or (direction == "SHORT" and z_moved < 0)
    
    print(f"{pair} {direction}:")
    print(f"  {a}: ${a_now:.2f} P&L ${a_pnl:+.2f}")
    print(f"  {b}: ${b_now:.2f} P&L ${b_pnl:+.2f}")
    print(f"  NET: ${net:+.2f}")
    print(f"  z: entry={z_entry:+.2f} → frozen={frozen_z:+.2f} (Δ={z_moved:+.2f}) "
          f"{'REVERTING ✓' if reverting else 'DIVERGING ✗'}")
    print(f"  Exit at frozen_z {'>' if direction=='LONG' else '<'} "
          f"{'+' if direction=='LONG' else ''}{-pos['exit_z_threshold'] if direction=='LONG' else pos['exit_z_threshold']:.1f}")
    print()

print(f"TOTAL P&L: ${total_pnl:+.2f}")
print(f"\nRun: python3 scripts/check_positions.py")
