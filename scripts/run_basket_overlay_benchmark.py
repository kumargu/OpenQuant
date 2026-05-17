#!/usr/bin/env python3
from __future__ import annotations

import argparse
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path


REPO_ROOT = Path(__file__).resolve().parents[1]
RUN_REPLAY = REPO_ROOT / "scripts" / "run_basket_replay_quiet.sh"
REPLAY_ROOT = REPO_ROOT / "data" / "replay"
DEFAULT_LEADERSHIP_ARGS = [
    "--leadership-overlay-sectors",
    "faang,chips",
    "--leadership-ret5d-threshold",
    "0.02",
    "--leadership-breadth5d-threshold",
    "0.56",
    "--leadership-long-only-leverage",
    "1.0",
]


@dataclass(frozen=True)
class Window:
    name: str
    start: str
    end: str


@dataclass(frozen=True)
class Variant:
    name: str
    args: tuple[str, ...]


@dataclass(frozen=True)
class Metrics:
    cum_return: float
    sharpe: float
    max_dd: float
    n_days: int


WINDOWS = [
    Window("wide_q3", "2025-07-01", "2025-09-30"),
    Window("wide_q4", "2025-10-01", "2025-12-31"),
    Window("wide_2026ytd", "2026-01-01", "2026-05-15"),
    Window("strong_2025_q1", "2025-01-01", "2025-03-31"),
]

VARIANTS = [
    Variant("baseline", ()),
    Variant(
        "fixed_suppress",
        (
            *DEFAULT_LEADERSHIP_ARGS,
            "--leadership-picker",
            "fixed",
            "--leadership-mode",
            "suppress-shorts",
        ),
    ),
    Variant(
        "fixed_sleeve",
        (
            *DEFAULT_LEADERSHIP_ARGS,
            "--leadership-picker",
            "fixed",
            "--leadership-mode",
            "add-capped-long-sleeve",
        ),
    ),
    Variant(
        "rule_v1",
        (*DEFAULT_LEADERSHIP_ARGS, "--leadership-picker", "rule-v1"),
    ),
]


def parse_metrics(report_path: Path) -> Metrics:
    first = report_path.read_text().splitlines()[0]
    fields: dict[str, str] = {}
    for token in first.removeprefix("# ").split("\t"):
        key, value = token.split("=", 1)
        fields[key] = value
    return Metrics(
        cum_return=float(fields["cum_return"]),
        sharpe=float(fields["sharpe"]),
        max_dd=float(fields["max_dd"]),
        n_days=int(fields["n_days"]),
    )


def run_replay(name: str, window: Window, args: tuple[str, ...], reuse_existing: bool) -> Metrics:
    report_path = REPLAY_ROOT / name / "report.tsv"
    if reuse_existing and report_path.exists():
        return parse_metrics(report_path)
    cmd = [str(RUN_REPLAY), name, window.start, window.end, *args]
    print(f"running {name} ...", flush=True)
    subprocess.run(cmd, cwd=REPO_ROOT, check=True)
    return parse_metrics(report_path)


def fmt_pct(value: float) -> str:
    return f"{value:+.2%}"


def fmt_dd(value: float) -> str:
    return f"{value:.2%}"


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Run the basket overlay picker replay matrix against fixed mechanism benchmarks."
    )
    parser.add_argument(
        "--prefix",
        default="overlay_bench",
        help="Replay directory prefix under data/replay.",
    )
    parser.add_argument(
        "--reuse-existing",
        action="store_true",
        help="Read existing report.tsv files instead of rerunning completed cases.",
    )
    args = parser.parse_args()

    results: dict[tuple[str, str], Metrics] = {}
    for window in WINDOWS:
        for variant in VARIANTS:
            run_name = f"{args.prefix}_{window.name}_{variant.name}"
            results[(window.name, variant.name)] = run_replay(
                run_name, window, variant.args, args.reuse_existing
            )

    print(
        "\nwindow\tbaseline\tfixed_suppress\tfixed_sleeve\t"
        "rule_v1\tbest_fixed\trule_vs_best_fixed"
    )
    for window in WINDOWS:
        baseline = results[(window.name, "baseline")]
        suppress = results[(window.name, "fixed_suppress")]
        sleeve = results[(window.name, "fixed_sleeve")]
        rule = results[(window.name, "rule_v1")]
        fixed = {
            "baseline": baseline,
            "fixed_suppress": suppress,
            "fixed_sleeve": sleeve,
        }
        best_fixed_name, best_fixed = max(fixed.items(), key=lambda item: item[1].cum_return)
        print(
            "\t".join(
                [
                    window.name,
                    f"{fmt_pct(baseline.cum_return)} / DD {fmt_dd(baseline.max_dd)}",
                    f"{fmt_pct(suppress.cum_return)} / DD {fmt_dd(suppress.max_dd)}",
                    f"{fmt_pct(sleeve.cum_return)} / DD {fmt_dd(sleeve.max_dd)}",
                    f"{fmt_pct(rule.cum_return)} / DD {fmt_dd(rule.max_dd)}",
                    best_fixed_name,
                    f"{fmt_pct(rule.cum_return - best_fixed.cum_return)} / "
                    f"DD diff {fmt_dd(rule.max_dd - best_fixed.max_dd)}",
                ]
            )
        )

    print(
        "\nDesign guardrail: treat rule_v1 as a conservative governor with dwell/hysteresis, "
        "not a daily overlay optimizer; prefer stable mode holds unless switching clearly "
        "earns its complexity."
    )

    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except subprocess.CalledProcessError as exc:
        print(
            f"benchmark command failed with exit code {exc.returncode}: "
            f"{' '.join(exc.cmd)}",
            file=sys.stderr,
        )
        raise
