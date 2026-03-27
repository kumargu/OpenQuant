#!/usr/bin/env python3
"""
Validate a pair trade against latest market news using Claude API + web search.

Before entering any trade, this checks:
1. Upcoming earnings for both symbols
2. M&A / analyst upgrades / downgrades
3. Major sector news that could cause gaps
4. Whether the pair relationship is likely to hold

Usage:
    python3 scripts/validate_pair_news.py COIN PYPL
    python3 scripts/validate_pair_news.py PNC USB --direction SHORT
"""

import argparse
import json
import os
import sys
from pathlib import Path

def validate_pair(symbol_a: str, symbol_b: str, direction: str = "LONG") -> dict:
    """Use Claude API with web search to validate a pair trade."""

    with open(Path(__file__).resolve().parent.parent / ".env") as f:
        env = dict(line.strip().split("=", 1) for line in f if "=" in line and not line.startswith("#"))

    api_key = env.get("ANTHROPIC_API_KEY", "")
    if not api_key:
        print("ERROR: ANTHROPIC_API_KEY not found in .env")
        print("Add it to .env: ANTHROPIC_API_KEY=sk-ant-...")
        return {"valid": False, "reason": "No API key"}

    import anthropic
    client = anthropic.Anthropic(api_key=api_key)

    prompt = f"""I'm about to enter a pairs trade: {direction} spread on {symbol_a}/{symbol_b}.
This means I will {"BUY" if direction == "LONG" else "SELL"} {symbol_a} and {"SELL" if direction == "LONG" else "BUY"} {symbol_b}.

Please search for the latest news and check:

1. **Earnings dates**: When are the next earnings for {symbol_a} and {symbol_b}? Are either reporting within the next 5 trading days?
2. **Major news**: Any M&A rumors, analyst upgrades/downgrades, SEC filings, or major product announcements for either company in the last 48 hours?
3. **Sector events**: Any sector-wide events (Fed decisions, regulation changes) that could cause one stock to gap relative to the other?
4. **Trade risk assessment**: Given the current news environment, rate this trade as LOW RISK, MEDIUM RISK, or HIGH RISK.

Be specific about dates and sources. If either stock has earnings within 5 days, flag it as HIGH RISK."""

    try:
        response = client.messages.create(
            model="claude-haiku-4-5",  # fast + cheap for news checks
            max_tokens=2000,
            tools=[
                {"type": "web_search_20250305", "name": "web_search"},
            ],
            messages=[{"role": "user", "content": prompt}],
        )

        # Extract the final text response
        result_text = ""
        for block in response.content:
            if hasattr(block, 'text'):
                result_text += block.text

        # Parse risk level
        risk = "UNKNOWN"
        for level in ["HIGH RISK", "MEDIUM RISK", "LOW RISK"]:
            if level in result_text.upper():
                risk = level
                break

        print(f"\n{'='*60}")
        print(f"PAIR VALIDATION: {symbol_a}/{symbol_b} {direction}")
        print(f"{'='*60}")
        print(result_text)
        print(f"\n{'='*60}")
        print(f"RISK: {risk}")
        print(f"{'='*60}")

        return {
            "valid": risk != "HIGH RISK",
            "risk": risk,
            "details": result_text,
            "pair": f"{symbol_a}/{symbol_b}",
            "direction": direction,
        }

    except Exception as e:
        print(f"Validation error: {e}")
        return {"valid": False, "reason": str(e)}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description="Validate pair trade against news")
    parser.add_argument("symbol_a", help="First symbol (e.g., COIN)")
    parser.add_argument("symbol_b", help="Second symbol (e.g., PYPL)")
    parser.add_argument("--direction", default="LONG", choices=["LONG", "SHORT"])
    args = parser.parse_args()

    result = validate_pair(args.symbol_a, args.symbol_b, args.direction)
    if not result["valid"]:
        print("\n⚠️  TRADE NOT RECOMMENDED — HIGH RISK detected")
        sys.exit(1)
    else:
        print(f"\n✓ Trade appears safe ({result['risk']})")
