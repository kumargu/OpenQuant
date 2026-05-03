#!/usr/bin/env python3
"""Read the 11 verification reports and print two tables.

(1) random9 subset distribution vs dom_only Sharpe 1.568
(2) dominance threshold sweep d=0.40..0.80 vs no_mining baseline 0.47
"""
import re
from pathlib import Path
from statistics import mean, stdev

ROOT = Path("autoresearch/issue_321")
DOM_ONLY_REF = 1.568   # autoresearch/issue_321/q3_q1_statgate2/dom_only
NO_MINING_REF = 0.47   # autoresearch/issue_321/q3_q1_full_universe baseline

FIELDS = ["cum_return", "ann_return", "sharpe", "max_dd", "n_days"]


def parse_summary(report_path: Path) -> dict:
    if not report_path.exists():
        return {f: None for f in FIELDS} | {"present": False}
    head = report_path.read_text().splitlines()[0]
    out = {"present": True}
    for f in FIELDS:
        m = re.search(rf"{f}=([\-\d\.]+)", head)
        out[f] = float(m.group(1)) if m else None
    return out


def fmt(v, prec=3):
    if v is None:
        return "  ---"
    return f"{v:+.{prec}f}" if isinstance(v, float) else str(v)


def main():
    print("=" * 78)
    print(" RANDOM 9-BASKET SUBSET DISTRIBUTION (5 seeds)")
    print(f"  Compares against dom_only Sharpe = {DOM_ONLY_REF}")
    print("=" * 78)
    print(f"{'seed':<6} {'cum_ret':>10} {'sharpe':>10} {'max_dd':>10} {'ann':>10}")
    print("-" * 78)
    sharpes_r9 = []
    cums_r9 = []
    for seed in range(5):
        r = parse_summary(ROOT / f"q3_q1_random9/seed{seed}/report.tsv")
        if r["present"]:
            sharpes_r9.append(r["sharpe"])
            cums_r9.append(r["cum_return"])
        print(f"seed{seed:<2} {fmt(r['cum_return'])} {fmt(r['sharpe'])} {fmt(r['max_dd'])} {fmt(r['ann_return'])}")
    if len(sharpes_r9) >= 2:
        print("-" * 78)
        sm, ss = mean(sharpes_r9), stdev(sharpes_r9)
        cm, cs = mean(cums_r9), stdev(cums_r9)
        print(f"{'mean':<6} {fmt(cm)} {fmt(sm)}")
        print(f"{'std':<6} {fmt(cs)} {fmt(ss)}")
        z = (DOM_ONLY_REF - sm) / ss if ss > 0 else float("nan")
        print(f"\ndom_only Sharpe vs random9 distribution: z = {z:+.2f}")
        if z >= 1.5:
            print("  → dom_only is at upper tail (>1.5σ above random) → real signal")
        elif z >= 0.5:
            print("  → dom_only above mean but within noise → weak / inconclusive")
        else:
            print("  → dom_only is at/below random subset mean → narrowing effect, no signal")

    print()
    print("=" * 78)
    print(" DOMINANCE THRESHOLD SWEEP")
    print(f"  Compares against no-mining baseline Sharpe = {NO_MINING_REF}")
    print(f"  Reference: dom=0.60 (statgate2/dom_only) Sharpe = {DOM_ONLY_REF}")
    print("=" * 78)
    print(f"{'thresh':<8} {'cum_ret':>10} {'sharpe':>10} {'max_dd':>10} {'ann':>10}")
    print("-" * 78)
    sweep = []
    for k_label, k_val in [("0.40", "040"), ("0.50", "050"), ("0.55", "055"),
                           ("0.60", "060_REF"), ("0.65", "065"), ("0.70", "070"),
                           ("0.80", "080")]:
        if k_val == "060_REF":
            print(f"{k_label:<8} (use dom_only result above)  sharpe={DOM_ONLY_REF}")
            sweep.append((float(k_label), DOM_ONLY_REF))
            continue
        r = parse_summary(ROOT / f"q3_q1_dom_sweep/d{k_val}/report.tsv")
        if r["present"]:
            sweep.append((float(k_label), r["sharpe"]))
        print(f"{k_label:<8} {fmt(r['cum_return'])} {fmt(r['sharpe'])} {fmt(r['max_dd'])} {fmt(r['ann_return'])}")

    valid = [s for _, s in sweep if s is not None]
    if len(valid) >= 3:
        print("-" * 78)
        print(f"sharpe range across thresholds: {min(valid):+.3f} .. {max(valid):+.3f}")
        spread = max(valid) - min(valid)
        if spread < 0.30:
            print(f"  → spread {spread:.3f} < 0.30 → robust across thresholds → real")
        elif spread < 0.70:
            print(f"  → spread {spread:.3f} → moderate sensitivity → mostly robust")
        else:
            print(f"  → spread {spread:.3f} > 0.70 → sharply peaked → likely overfit to 0.60")


if __name__ == "__main__":
    main()
