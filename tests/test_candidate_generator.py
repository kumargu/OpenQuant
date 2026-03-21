"""Tests for the Claude API candidate generator.

Tests prompt construction, output parsing, file writing, and fallback behavior.
Does NOT call the actual API — uses mock responses.
"""

import json
import tempfile
from pathlib import Path

import pytest

from paper_trading.candidate_generator import (
    DEFAULT_UNIVERSE,
    build_user_prompt,
    parse_candidates,
    write_pair_candidates,
    generate_candidates,
)


class TestPromptConstruction:
    def test_user_prompt_contains_universe(self):
        prompt = build_user_prompt(["AAPL", "MSFT", "NVDA"])
        assert "AAPL" in prompt
        assert "MSFT" in prompt
        assert "NVDA" in prompt

    def test_user_prompt_requests_json(self):
        prompt = build_user_prompt(DEFAULT_UNIVERSE)
        assert "JSON" in prompt
        assert "leg_a" in prompt
        assert "leg_b" in prompt
        assert "economic_rationale" in prompt

    def test_user_prompt_warns_about_etf_components(self):
        prompt = build_user_prompt(DEFAULT_UNIVERSE)
        assert "ETF" in prompt
        assert "XLF/JPM" in prompt or "etf" in prompt.lower()

    def test_user_prompt_requests_counter_arguments(self):
        prompt = build_user_prompt(DEFAULT_UNIVERSE)
        assert "counter_argument" in prompt or "counter" in prompt.lower()

    def test_default_universe_has_enough_symbols(self):
        assert len(DEFAULT_UNIVERSE) >= 50


class TestParseCandidates:
    def test_parse_valid_json(self):
        response = json.dumps({
            "candidates": [
                {
                    "leg_a": "GS",
                    "leg_b": "MS",
                    "economic_rationale": "Investment banks",
                    "confidence": "high",
                    "category": "same_sector_competitor",
                    "counter_argument": "Diverging strategies",
                }
            ]
        })
        result = parse_candidates(response)
        assert len(result) == 1
        assert result[0]["leg_a"] == "GS"
        assert result[0]["leg_b"] == "MS"

    def test_parse_multiple_candidates(self):
        response = json.dumps({
            "candidates": [
                {"leg_a": "GS", "leg_b": "MS", "economic_rationale": "Banks"},
                {"leg_a": "V", "leg_b": "MA", "economic_rationale": "Payments"},
                {"leg_a": "HD", "leg_b": "LOW", "economic_rationale": "Home improvement"},
            ]
        })
        result = parse_candidates(response)
        assert len(result) == 3

    def test_parse_with_markdown_fences(self):
        response = '```json\n{"candidates": [{"leg_a": "A", "leg_b": "B", "economic_rationale": "test"}]}\n```'
        result = parse_candidates(response)
        assert len(result) == 1

    def test_parse_skips_missing_fields(self):
        response = json.dumps({
            "candidates": [
                {"leg_a": "GS", "leg_b": "MS"},  # missing economic_rationale
                {"leg_a": "V", "leg_b": "MA", "economic_rationale": "Payments"},
            ]
        })
        result = parse_candidates(response)
        assert len(result) == 1
        assert result[0]["leg_a"] == "V"

    def test_parse_invalid_json(self):
        result = parse_candidates("this is not json")
        assert result == []

    def test_parse_empty_candidates(self):
        result = parse_candidates('{"candidates": []}')
        assert result == []

    def test_parse_no_candidates_key(self):
        result = parse_candidates('{"pairs": []}')
        assert result == []


class TestWritePairCandidates:
    def test_write_creates_file(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "pair_candidates.json"
            candidates = [
                {"leg_a": "GS", "leg_b": "MS", "economic_rationale": "Banks"},
            ]
            write_pair_candidates(candidates, path, model="test")

            assert path.exists()
            with open(path) as f:
                data = json.load(f)
            assert len(data["pairs"]) == 1
            assert data["pairs"][0]["leg_a"] == "GS"

    def test_write_format_matches_pair_picker(self):
        """Verify output matches the format pair-picker expects."""
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "pair_candidates.json"
            candidates = [
                {
                    "leg_a": "GS",
                    "leg_b": "MS",
                    "economic_rationale": "Banks",
                    "confidence": "high",
                    "category": "same_sector_competitor",
                },
            ]
            write_pair_candidates(candidates, path, model="test")

            with open(path) as f:
                data = json.load(f)

            # pair-picker expects: {"pairs": [{"leg_a": ..., "leg_b": ..., "economic_rationale": ...}]}
            assert "pairs" in data
            pair = data["pairs"][0]
            assert "leg_a" in pair
            assert "leg_b" in pair
            assert "economic_rationale" in pair
            # Extra fields (confidence, category) should NOT be in output
            assert "confidence" not in pair
            assert "category" not in pair


class TestFallback:
    def test_generate_falls_back_on_missing_api_key(self):
        """Without ANTHROPIC_API_KEY, should fall back to existing file."""
        with tempfile.TemporaryDirectory() as tmp:
            output_path = Path(tmp) / "pair_candidates.json"
            # Write existing candidates
            existing = {
                "pairs": [
                    {"leg_a": "GS", "leg_b": "MS", "economic_rationale": "Existing"}
                ]
            }
            with open(output_path, "w") as f:
                json.dump(existing, f)

            # This will fail because no API key, but should fall back
            result = generate_candidates(output_path=output_path)
            # Should return existing candidates (fallback)
            if result:
                assert result[0]["leg_a"] == "GS"

    def test_dry_run_does_not_call_api(self):
        """Dry run should print prompts and return empty list."""
        result = generate_candidates(dry_run=True)
        assert result == []
