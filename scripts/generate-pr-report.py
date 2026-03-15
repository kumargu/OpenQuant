#!/usr/bin/env python3
"""
Generate a PR description with benchmark comparison data.

Usage:
  python scripts/generate-pr-report.py                    # print to stdout
  python scripts/generate-pr-report.py --hypothesis "..."  # include hypothesis
  python scripts/generate-pr-report.py --output pr.md      # write to file

Designed to work with: gh pr create --body "$(python scripts/generate-pr-report.py)"
"""

import argparse
import json
import subprocess
import sys
from pathlib import Path

# Add project root to path
PROJECT_ROOT = Path(__file__).parent.parent
sys.path.insert(0, str(PROJECT_ROOT))


def get_git_info() -> dict:
    """Get current branch, SHA, and diff stats."""
    info = {}
    try:
        info["branch"] = subprocess.check_output(
            ["git", "rev-parse", "--abbrev-ref", "HEAD"],
            stderr=subprocess.DEVNULL,
        ).decode().strip()
    except Exception:
        info["branch"] = "unknown"

    try:
        info["sha"] = subprocess.check_output(
            ["git", "rev-parse", "--short", "HEAD"],
            stderr=subprocess.DEVNULL,
        ).decode().strip()
    except Exception:
        info["sha"] = "unknown"

    try:
        info["diff_stat"] = subprocess.check_output(
            ["git", "diff", "--stat", "main...HEAD"],
            stderr=subprocess.DEVNULL,
        ).decode().strip()
    except Exception:
        info["diff_stat"] = ""

    try:
        # Get commit messages since divergence from main
        info["commits"] = subprocess.check_output(
            ["git", "log", "--oneline", "main..HEAD"],
            stderr=subprocess.DEVNULL,
        ).decode().strip()
    except Exception:
        info["commits"] = ""

    return info


def run_benchmark_comparison() -> str | None:
    """Run benchmark and return comparison markdown."""
    try:
        from paper_trading.benchmark import (
            run_by_category,
            load_baseline,
            compare_reports,
        )

        baseline = load_baseline()
        if baseline is None:
            return None

        print("Running benchmark suite...", file=sys.stderr)
        candidate = run_by_category(days=30)
        return compare_reports(baseline, candidate)
    except ImportError:
        print("WARNING: benchmark module not available", file=sys.stderr)
        return None
    except Exception as e:
        print(f"WARNING: benchmark failed: {e}", file=sys.stderr)
        return None


def generate_pr_body(hypothesis: str | None = None, skip_benchmark: bool = False) -> str:
    """Generate the full PR body."""
    git = get_git_info()
    lines = []

    lines.append("## Summary")
    lines.append("")

    if hypothesis:
        lines.append(f"**Hypothesis:** {hypothesis}")
        lines.append("")

    if git.get("commits"):
        lines.append("**Commits:**")
        lines.append("```")
        lines.append(git["commits"])
        lines.append("```")
        lines.append("")

    # Benchmark comparison
    if not skip_benchmark:
        comparison = run_benchmark_comparison()
        if comparison:
            lines.append(comparison)
        else:
            lines.append("*No baseline available for benchmark comparison.*")
            lines.append("*Run `python -m paper_trading.benchmark --save-baseline` on main first.*")
        lines.append("")

    # Test plan
    lines.append("## Test plan")
    lines.append("")
    lines.append("- [ ] Rust tests pass (`cd engine && cargo test`)")
    lines.append("- [ ] Benchmark comparison shows no regressions")
    lines.append("- [ ] Manual review of changed signal/risk logic")
    lines.append("")
    lines.append("---")
    lines.append("Generated with [Claude Code](https://claude.com/claude-code)")

    return "\n".join(lines)


def main():
    parser = argparse.ArgumentParser(description="Generate PR description with benchmark data")
    parser.add_argument("--hypothesis", "-H", help="The hypothesis this PR tests")
    parser.add_argument("--output", "-o", help="Write to file instead of stdout")
    parser.add_argument("--skip-benchmark", action="store_true", help="Skip benchmark comparison")
    args = parser.parse_args()

    body = generate_pr_body(
        hypothesis=args.hypothesis,
        skip_benchmark=args.skip_benchmark,
    )

    if args.output:
        Path(args.output).write_text(body)
        print(f"PR body written to {args.output}", file=sys.stderr)
    else:
        print(body)


if __name__ == "__main__":
    main()
