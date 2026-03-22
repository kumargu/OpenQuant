"""
Claude API skill for weekly pair candidate generation.

Calls the Claude API with a structured prompt to propose pair trading
candidates with economic rationale. The LLM proposes → Rust validates.
LLM never makes final trading decisions.

Usage:
    python -m paper_trading.candidate_generator
    python -m paper_trading.candidate_generator --dry-run  # print prompt, don't call API
    python -m paper_trading.candidate_generator --universe custom_universe.json
"""

from __future__ import annotations

import argparse
import json
import logging
import sys
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

logger = logging.getLogger(__name__)

# ---------------------------------------------------------------------------
# Stock universe — the LLM considers pairs within this set
# ---------------------------------------------------------------------------

DEFAULT_UNIVERSE: list[str] = [
    # Tech
    "AAPL", "MSFT", "NVDA", "GOOGL", "META", "AMZN", "ORCL", "CRM", "ADBE", "NOW",
    # Semis
    "AMD", "AVGO", "INTC", "QCOM", "TXN", "MU", "MRVL", "LRCX", "AMAT", "KLAC", "TSM",
    # Financials
    "JPM", "BAC", "GS", "MS", "C", "WFC", "USB", "PNC", "SCHW", "BLK", "AXP",
    # Energy
    "XOM", "CVX", "COP", "SLB", "EOG", "PSX", "VLO", "MPC", "HAL",
    # Consumer
    "WMT", "COST", "HD", "LOW", "MCD", "SBUX", "NKE", "TGT",
    # Payments
    "V", "MA", "PYPL", "SQ",
    # Ride-sharing / Delivery
    "UBER", "LYFT",
    # Airlines
    "DAL", "UAL", "LUV", "AAL",
    # Telecom
    "T", "VZ", "TMUS",
    # Streaming / Media
    "DIS", "NFLX",
    # Logistics
    "FDX", "UPS",
    # Precious metals ETFs
    "GLD", "SLV",
    # Consumer staples
    "KO", "PEP", "PG", "CL",
    # Waste management
    "WM", "RSG",
    # REITs
    "O", "NNN",
    # Defense
    "LMT", "NOC",
    # Utilities
    "DUK", "SO",
    # Credit ratings
    "MCO", "SPGI",
    # Medical devices
    "ABT", "MDT",
    # Health insurance
    "ELV", "CI",
    # Sector ETFs (for reference, not as pair legs)
    "XLF", "XLE", "XLK", "SMH", "QQQ",
]

# ---------------------------------------------------------------------------
# Prompt construction
# ---------------------------------------------------------------------------

SYSTEM_PROMPT = """\
You are a quantitative financial analyst specializing in statistical \
arbitrage and pairs trading. Your task is to identify stock pairs with \
strong fundamental economic links that make them good candidates for \
cointegration-based pairs trading.

A good pair has:
1. A clear economic reason why the two stocks should move together \
(same industry, same customers, same regulatory regime, supply chain link)
2. Similar business models or revenue drivers
3. Shared macro factor exposure (interest rates, commodity prices, etc.)
4. Historical co-movement that is FUNDAMENTALLY driven, not just statistical coincidence

A bad pair is:
1. An ETF paired with one of its components (e.g., XLF/JPM) — this is \
mechanical correlation, not economic
2. Two stocks that happen to correlate statistically but have no economic link
3. Pairs where one stock is much larger/more diversified than the other \
(asymmetric sensitivity)

Output ONLY valid JSON. No markdown, no code fences, no commentary outside the JSON."""

ETF_SYMBOLS: set[str] = {
    "XLF", "XLE", "XLK", "XLV", "XLY", "XLP", "XLU", "XLI", "XLB", "XLRE",
    "GLD", "SLV", "SMH", "QQQ", "SPY", "IWM", "DIA", "EEM", "TLT", "HYG",
    "LQD", "XBI",
}

def build_user_prompt(universe: list[str]) -> str:
    """Build the user prompt with the current stock universe (ETFs excluded)."""
    # Exclude ETFs from the tradeable universe — they should not be pair legs
    tradeable = [s for s in universe if s not in ETF_SYMBOLS]
    universe_str = ", ".join(sorted(tradeable))
    return f"""\
Analyze the following stock universe and propose 20-30 pair trading candidates.

Stock universe: {universe_str}

For each pair, provide:
- leg_a, leg_b: the two ticker symbols
- economic_rationale: 2-3 sentences explaining WHY these stocks should be \
cointegrated (shared revenue drivers, regulatory overlap, factor exposure)
- confidence: "high", "medium", or "low"
- category: one of "same_sector_competitor", "supply_chain", \
"same_regulator", "same_factor", "duopoly", "etf_components_peer"
- counter_argument: one sentence on why this pair might NOT work

IMPORTANT:
- Do NOT use ETFs as pair legs. The following are ETFs — NEVER propose them \
as leg_a or leg_b: XLF, XLE, XLK, SMH, QQQ, GLD, SLV, SPY, IWM, DIA
- Do NOT pair an ETF with one of its components (e.g., XLF/JPM is INVALID)
- Focus on pairs with genuine economic links, not just statistical correlation
- Include pairs across different sectors if there's a fundamental link \
(e.g., AMZN/UPS for ecommerce/logistics)
- Prefer pairs where both stocks are similar in market cap and business scope

Return a JSON object with this exact schema:
{{
  "candidates": [
    {{
      "leg_a": "TICKER1",
      "leg_b": "TICKER2",
      "economic_rationale": "...",
      "confidence": "high|medium|low",
      "category": "...",
      "counter_argument": "..."
    }}
  ]
}}"""

# ---------------------------------------------------------------------------
# API call
# ---------------------------------------------------------------------------

def call_claude_api(
    system_prompt: str,
    user_prompt: str,
    model: str = "claude-sonnet-4-6-20250514",
    max_tokens: int = 8192,
) -> dict[str, Any]:
    """Call the Claude API and return the parsed response.

    Requires ANTHROPIC_API_KEY environment variable.
    """
    import anthropic

    client = anthropic.Anthropic()

    logger.info("Calling Claude API (model=%s, max_tokens=%d)", model, max_tokens)

    message = client.messages.create(
        model=model,
        max_tokens=max_tokens,
        temperature=0,  # reproducibility
        system=system_prompt,
        messages=[{"role": "user", "content": user_prompt}],
    )

    # Extract text content
    text = ""
    for block in message.content:
        if block.type == "text":
            text += block.text

    # Log usage
    usage = message.usage
    logger.info(
        "API response: input_tokens=%d, output_tokens=%d, stop_reason=%s",
        usage.input_tokens,
        usage.output_tokens,
        message.stop_reason,
    )

    return {
        "text": text,
        "model": message.model,
        "input_tokens": usage.input_tokens,
        "output_tokens": usage.output_tokens,
        "stop_reason": message.stop_reason,
    }


# ---------------------------------------------------------------------------
# Output parsing and writing
# ---------------------------------------------------------------------------

def parse_candidates(
    response_text: str,
    universe: list[str] | None = None,
) -> list[dict[str, str]]:
    """Parse the LLM response into a list of candidate pairs.

    Handles: markdown fences, missing fields, duplicate pairs,
    hallucinated tickers not in the universe.
    """
    text = response_text.strip()

    # Strip markdown code fences if present
    if text.startswith("```"):
        lines = text.split("\n")
        lines = [l for l in lines if not l.strip().startswith("```")]
        text = "\n".join(lines)

    try:
        data = json.loads(text)
    except json.JSONDecodeError as e:
        logger.error("Failed to parse LLM response as JSON: %s", e)
        logger.error("Response text: %s", text[:500])
        return []

    candidates = data.get("candidates", [])
    if not isinstance(candidates, list):
        logger.error("'candidates' is not a list: %s", type(candidates))
        return []

    universe_set = set(universe) if universe else None
    seen: set[tuple[str, str]] = set()
    valid = []
    required_fields = {"leg_a", "leg_b", "economic_rationale"}

    for i, c in enumerate(candidates):
        if not isinstance(c, dict):
            logger.warning("Candidate %d is not a dict, skipping", i)
            continue
        missing = required_fields - set(c.keys())
        if missing:
            logger.warning("Candidate %d missing fields %s, skipping", i, missing)
            continue

        # Validate tickers are in the universe (catch LLM hallucinations)
        if universe_set is not None:
            if c["leg_a"] not in universe_set or c["leg_b"] not in universe_set:
                logger.warning(
                    "Candidate %d has unknown symbol (%s/%s), skipping",
                    i, c["leg_a"], c["leg_b"],
                )
                continue

        # Deduplicate by canonical ordering (GS/MS == MS/GS)
        key = tuple(sorted([c["leg_a"], c["leg_b"]]))
        if key in seen:
            logger.info("Dedup: %s/%s already seen, skipping", c["leg_a"], c["leg_b"])
            continue
        seen.add(key)

        valid.append(c)

    logger.info("Parsed %d valid candidates from %d total", len(valid), len(candidates))
    return valid


def write_pair_candidates(
    candidates: list[dict[str, str]],
    output_path: Path,
    model: str,
) -> None:
    """Write pair_candidates.json in the format expected by pair-picker."""
    # Convert to pair-picker format (leg_a, leg_b, economic_rationale)
    pairs = []
    for c in candidates:
        pairs.append({
            "leg_a": c["leg_a"],
            "leg_b": c["leg_b"],
            "economic_rationale": c["economic_rationale"],
        })

    output = {"pairs": pairs}

    output_path.parent.mkdir(parents=True, exist_ok=True)
    with open(output_path, "w") as f:
        json.dump(output, f, indent=2)

    logger.info("Wrote %d candidates to %s", len(pairs), output_path)


def write_audit_log(
    candidates: list[dict[str, str]],
    api_response: dict[str, Any] | None,
    output_dir: Path,
    model: str,
) -> None:
    """Write full audit log with API response, candidates, and metadata."""
    timestamp = datetime.now(timezone.utc).strftime("%Y%m%d_%H%M%S")
    audit_path = output_dir / "candidate_audit" / f"candidates_{timestamp}.json"
    audit_path.parent.mkdir(parents=True, exist_ok=True)

    audit = {
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "model": model,
        "api_response": api_response,
        "candidates": candidates,
        "candidate_count": len(candidates),
    }

    with open(audit_path, "w") as f:
        json.dump(audit, f, indent=2)

    logger.info("Audit log written to %s", audit_path)


# ---------------------------------------------------------------------------
# Main entry point
# ---------------------------------------------------------------------------

def generate_candidates(
    universe: list[str] | None = None,
    output_path: Path | None = None,
    dry_run: bool = False,
    model: str = "claude-sonnet-4-6-20250514",
) -> list[dict[str, str]]:
    """Generate pair candidates using Claude API.

    Returns the list of candidates. Writes pair_candidates.json and audit log.
    Falls back to existing candidates if API call fails.
    """
    if universe is None:
        universe = DEFAULT_UNIVERSE

    data_dir = Path(__file__).resolve().parent.parent / "data"
    if output_path is None:
        output_path = data_dir / "pair_candidates.json"

    system_prompt = SYSTEM_PROMPT
    user_prompt = build_user_prompt(universe)

    if dry_run:
        print("=== SYSTEM PROMPT ===")
        print(system_prompt)
        print("\n=== USER PROMPT ===")
        print(user_prompt)
        print(f"\nPrompt length: ~{len(system_prompt) + len(user_prompt)} chars")
        return []

    # Call API
    try:
        response = call_claude_api(
            system_prompt=system_prompt,
            user_prompt=user_prompt,
            model=model,
        )
    except Exception as e:
        logger.error("Claude API call failed: %s", e)
        logger.info("Falling back to existing pair_candidates.json")
        if output_path.exists():
            with open(output_path) as f:
                existing = json.load(f)
            return existing.get("pairs", [])
        return []

    # Parse response (validate tickers against universe, deduplicate)
    candidates = parse_candidates(response["text"], universe=universe)

    if not candidates:
        logger.warning("No valid candidates from API — keeping existing file")
        return []

    # Write outputs
    write_pair_candidates(candidates, output_path, model=response.get("model", model))
    write_audit_log(candidates, response, data_dir, model=response.get("model", model))

    return candidates


def main() -> None:
    """CLI entry point."""
    parser = argparse.ArgumentParser(
        description="Generate pair trading candidates using Claude API"
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="Print prompts without calling API",
    )
    parser.add_argument(
        "--model",
        default="claude-sonnet-4-6-20250514",
        help="Claude model to use (default: claude-sonnet-4-6-20250514)",
    )
    parser.add_argument(
        "--output",
        type=Path,
        default=None,
        help="Output path for pair_candidates.json",
    )
    parser.add_argument(
        "--universe",
        type=Path,
        default=None,
        help="JSON file with custom stock universe (list of symbols)",
    )
    args = parser.parse_args()

    logging.basicConfig(
        level=logging.INFO,
        format="%(asctime)s %(levelname)s %(name)s: %(message)s",
    )

    universe = None
    if args.universe:
        with open(args.universe) as f:
            universe = json.load(f)
        logger.info("Loaded %d symbols from %s", len(universe), args.universe)

    candidates = generate_candidates(
        universe=universe,
        output_path=args.output,
        dry_run=args.dry_run,
        model=args.model,
    )

    if candidates:
        print(f"\nGenerated {len(candidates)} candidates:")
        for c in candidates:
            conf = c.get("confidence", "?")
            print(f"  {c['leg_a']}/{c['leg_b']} [{conf}] — {c['economic_rationale'][:80]}...")
    elif not args.dry_run:
        print("No candidates generated (check logs for errors)")


if __name__ == "__main__":
    main()
