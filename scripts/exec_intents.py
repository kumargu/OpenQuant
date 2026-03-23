#!/usr/bin/env python3
"""Read order intents from stdin (Rust engine output) and execute via Alpaca.

Minimal script — only places orders. All trading logic is in Rust.

Reads one JSON line per intent:
  {"symbol":"GLD","side":"buy","qty":23,"pair_id":"GLD/SLV","z_score":-2.1}
"""

import json
import os
import sys
from pathlib import Path

from dotenv import load_dotenv

load_dotenv(Path(__file__).resolve().parent.parent / ".env")

DRY_RUN = "--dry-run" in sys.argv

sys.stderr.write(f"Order executor ready (dry_run={DRY_RUN})\n")

for line in sys.stdin:
    line = line.strip()
    if not line:
        continue

    try:
        intent = json.loads(line)
    except json.JSONDecodeError:
        sys.stderr.write(f"Bad JSON: {line[:100]}\n")
        continue

    symbol = intent.get("symbol", "")
    side = intent.get("side", "")
    qty = intent.get("qty", 0)
    pair_id = intent.get("pair_id", "")
    z = intent.get("z_score", 0)

    action = f"{side.upper()} {qty:.0f} {symbol} (pair={pair_id}, z={z:.2f})"

    if DRY_RUN:
        sys.stderr.write(f"DRY RUN: {action}\n")
        continue

    try:
        from paper_trading.alpaca_client import buy, sell
        result = buy(symbol, qty) if side == "buy" else sell(symbol, qty)
        sys.stderr.write(f"EXECUTED: {action} → {result['status']}\n")
    except Exception as e:
        sys.stderr.write(f"FAILED: {action} → {e}\n")
