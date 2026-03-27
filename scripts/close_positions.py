#!/usr/bin/env python3
"""DEPRECATED — use 'python3 scripts/live_pipeline.py exit' instead.

This script bypasses the live pipeline's quality gates and logging.
Kept only for emergency manual intervention.

Usage (EMERGENCY ONLY):
    python3 scripts/close_positions.py HD LOW     # Close HD/LOW pair
    python3 scripts/close_positions.py PNC USB    # Close PNC/USB pair
    python3 scripts/close_positions.py --all      # Close all positions
"""
import warnings
warnings.warn(
    "close_positions.py is DEPRECATED. Use 'python3 scripts/live_pipeline.py exit' instead.",
    DeprecationWarning,
    stacklevel=2,
)
import argparse
import json
import logging
import sys
from datetime import datetime
from zoneinfo import ZoneInfo

logging.getLogger("urllib3").setLevel(logging.WARNING)

with open(".env") as f:
    env = dict(line.strip().split("=", 1) for line in f if "=" in line and not line.startswith("#"))

from alpaca.trading.client import TradingClient
from alpaca.trading.requests import ClosePositionRequest

client = TradingClient(env["ALPACA_API_KEY"], env["ALPACA_SECRET_KEY"], paper=True)

def close_pair(sym_a: str, sym_b: str, live_data: dict) -> float:
    """Close both legs of a pair. Returns net P&L."""
    total_pnl = 0
    for sym in [sym_a, sym_b]:
        try:
            pos = client.get_open_position(sym)
            pnl = float(pos.unrealized_pl)
            total_pnl += pnl
            client.close_position(sym)
            print(f"  Closed {sym}: {pos.qty} shares @ ${float(pos.current_price):.2f}, P&L ${pnl:+.2f}")
        except Exception as e:
            print(f"  {sym}: {e}")

    # Remove from live_positions.json
    pair_key = f"{sym_a}/{sym_b}"
    live_data['positions'] = [p for p in live_data['positions'] if p['pair'] != pair_key]

    return total_pnl

def main():
    parser = argparse.ArgumentParser(description="Close pair positions")
    parser.add_argument("symbols", nargs="*", help="Symbol A and B to close")
    parser.add_argument("--all", action="store_true", help="Close all positions")
    args = parser.parse_args()

    live = json.load(open("trading/live_positions.json"))

    if args.all:
        pairs_to_close = [(p['leg_a']['symbol'], p['leg_b']['symbol']) for p in live['positions']]
    elif len(args.symbols) == 2:
        pairs_to_close = [(args.symbols[0].upper(), args.symbols[1].upper())]
    else:
        parser.print_help()
        sys.exit(1)

    total_pnl = 0
    for sym_a, sym_b in pairs_to_close:
        print(f"\nClosing {sym_a}/{sym_b}:")
        pnl = close_pair(sym_a, sym_b, live)
        total_pnl += pnl
        print(f"  Pair P&L: ${pnl:+.2f}")

    # Update live_positions.json
    deployed = sum(p['capital_per_leg'] * 2 for p in live['positions'])
    live['account']['total_deployed'] = deployed
    live['account']['cash_remaining'] = 10000 - deployed
    with open("trading/live_positions.json", "w") as f:
        json.dump(live, f, indent=2)
        f.write("\n")

    print(f"\nTotal closed P&L: ${total_pnl:+.2f}")
    print(f"Remaining positions: {len(live['positions'])}")

    # Log to journal
    with open("data/journal/engine.log", "a") as log:
        log.write(f"\n[{datetime.now(ZoneInfo('US/Eastern')).isoformat()}] CLOSE {[f'{a}/{b}' for a,b in pairs_to_close]} "
                  f"total_pnl=${total_pnl:+.2f}\n")

if __name__ == "__main__":
    main()
