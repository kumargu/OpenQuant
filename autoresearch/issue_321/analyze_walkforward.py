#!/usr/bin/env python3
"""Walk-forward analysis: 5 variants x 3 OOS blocks.

Outputs per https://github.com/kumargu/OpenQuant/issues/321#issuecomment-4366548230:
- flat table (variant x block) with cum_return, sharpe, max_dd,
  n_valid_baskets, avg_active_baskets, order_count, rejected_count
- rejected basket list per (variant, block)
- reject-list stability (Jaccard similarity across blocks) for dom050 and dom060
- additivity check for dom050 + nomegacaps

Run AFTER all 15 replays complete and per-block fit artifacts have been generated
via build_per_block_fits.sh (which calls `freeze-basket-fits --as-of <block_start>`).
"""
import json
import re
from pathlib import Path
from statistics import mean

ROOT = Path("autoresearch/issue_321/walkforward")
FIT_ROOT = Path("autoresearch/issue_321/walkforward_fits")  # per-block fits go here

VARIANTS = ["baseline", "dom050", "dom060", "nomegacaps", "nomegacaps_dom050"]
BLOCKS = ["test1", "test2", "test3"]
BLOCK_RANGES = {
    "test1": ("2025-01-01", "2025-06-30"),
    "test2": ("2025-07-01", "2025-12-31"),
    "test3": ("2026-01-01", "2026-03-31"),
}


def parse_report(p: Path) -> dict:
    if not p.exists():
        return {}
    head = p.read_text().splitlines()[0]
    out = {}
    for f in ["cum_return", "sharpe", "max_dd", "n_days"]:
        m = re.search(rf"{f}=([\-\d\.]+)", head)
        out[f] = float(m.group(1)) if m else None
    return out


def parse_engine_log(p: Path) -> dict:
    """Extract n_valid, avg_active, n_orders from engine.log."""
    if not p.exists():
        return {}
    n_valid = None
    actives = []
    n_orders = 0
    with p.open() as f:
        for line in f:
            if n_valid is None and "loaded basket fits" in line:
                m = re.search(r"valid=(\d+)", line)
                if m:
                    n_valid = int(m.group(1))
            if "admitted=" in line and "openquant_runner::basket_live" in line:
                m = re.search(r"admitted=(\d+)", line)
                if m:
                    actives.append(int(m.group(1)))
            if "BASKET_INTENT" in line:
                n_orders += 1
    return {
        "n_valid": n_valid,
        "avg_active": mean(actives) if actives else None,
        "n_decisions": len(actives),
        "n_orders": n_orders,
    }


def parse_fit_artifact(p: Path) -> dict:
    """Read fit artifact, extract per-basket valid/reject info."""
    if not p.exists():
        return {"present": False}
    with p.open() as f:
        a = json.load(f)
    fits = a.get("fits", [])
    valid_ids, rejected = [], []
    for fit in fits:
        c = fit["candidate"]
        bid = f"{c['sector']}:{c['target']}"
        if fit.get("valid"):
            valid_ids.append(bid)
        else:
            rejected.append((bid, fit.get("reject_reason", "?")))
    return {
        "present": True,
        "total": len(fits),
        "n_valid": len(valid_ids),
        "valid_ids": set(valid_ids),
        "rejected_ids": [r[0] for r in rejected],
        "rejected_with_reason": rejected,
    }


def jaccard(a: set, b: set) -> float:
    if not a and not b:
        return 1.0
    return len(a & b) / max(1, len(a | b))


def main():
    print("=" * 96)
    print(" WALK-FORWARD: 5 variants x 3 OOS blocks (frozen rules)")
    print("=" * 96)
    rows = {}
    fits = {}
    for v in VARIANTS:
        rows[v] = {}
        fits[v] = {}
        for b in BLOCKS:
            d = ROOT / v / b
            r = parse_report(d / "report.tsv")
            e = parse_engine_log(d / "journal" / "engine.log")
            fa = parse_fit_artifact(FIT_ROOT / v / f"{b}_fit.json")
            rows[v][b] = {**r, **e}
            fits[v][b] = fa

    # Per-variant flat table
    for v in VARIANTS:
        print(f"\n{v}")
        hdr = f"{'block':<8} {'cum':>8} {'sharpe':>8} {'mdd':>8} {'n_valid':>8} {'avg_act':>8} {'orders':>8}"
        print(hdr)
        print("-" * len(hdr))
        for b in BLOCKS:
            r = rows[v][b]
            valid = fits[v][b].get("n_valid", r.get("n_valid"))
            cum = r.get("cum_return"); sh = r.get("sharpe"); mdd = r.get("max_dd")
            avg_a = r.get("avg_active"); orders = r.get("n_orders")
            print(f"{b:<8}"
                  f" {cum:>+8.3f}" if cum is not None else f"{b:<8} {'  ---':>8}",
                  end="")
            print(f" {sh:>+8.3f}" if sh is not None else f" {'  ---':>8}", end="")
            print(f" {mdd:>+8.3f}" if mdd is not None else f" {'  ---':>8}", end="")
            print(f" {valid:>8}" if valid is not None else f" {'  ---':>8}", end="")
            print(f" {avg_a:>8.2f}" if avg_a is not None else f" {'  ---':>8}", end="")
            print(f" {orders:>8}" if orders is not None else f" {'  ---':>8}")

    # Reject lists per (variant, block)
    print("\n" + "=" * 96)
    print(" REJECTED BASKET LISTS (gate-rejected; sorted)")
    print("=" * 96)
    for v in ["dom050", "dom060", "nomegacaps", "nomegacaps_dom050"]:
        for b in BLOCKS:
            fa = fits[v][b]
            if fa.get("present"):
                reject_ids = sorted(fa["rejected_ids"])
                print(f"\n{v} / {b}: {len(reject_ids)} rejected of {fa['total']}")
                if reject_ids:
                    print("  ", ", ".join(reject_ids))

    # Reject-list stability (Jaccard)
    print("\n" + "=" * 96)
    print(" REJECT-LIST STABILITY (Jaccard across blocks)")
    print("=" * 96)
    for v in ["dom050", "dom060"]:
        sets = {b: set(fits[v][b].get("rejected_ids", [])) for b in BLOCKS}
        if all(fits[v][b].get("present") for b in BLOCKS):
            j12 = jaccard(sets["test1"], sets["test2"])
            j23 = jaccard(sets["test2"], sets["test3"])
            j13 = jaccard(sets["test1"], sets["test3"])
            j_all = sets["test1"] & sets["test2"] & sets["test3"]
            print(f"\n{v}:")
            print(f"  Jaccard(test1, test2) = {j12:.2f}")
            print(f"  Jaccard(test2, test3) = {j23:.2f}")
            print(f"  Jaccard(test1, test3) = {j13:.2f}")
            print(f"  Always rejected: {sorted(j_all) if j_all else '(none)'}")

    # Additivity check for dom050 + nomegacaps
    print("\n" + "=" * 96)
    print(" ADDITIVITY: dom050 + nomegacaps")
    print("=" * 96)
    print(f"\n{'block':<8} {'baseline':>10} {'dom050':>10} {'nomega':>10} {'stack':>10}"
          f" {'dom_lift':>10} {'nm_lift':>10} {'stack_lift':>10}")
    for b in BLOCKS:
        baseline = rows["baseline"][b].get("sharpe")
        dom = rows["dom050"][b].get("sharpe")
        nm = rows["nomegacaps"][b].get("sharpe")
        st = rows["nomegacaps_dom050"][b].get("sharpe")
        if all(x is not None for x in [baseline, dom, nm, st]):
            print(f"{b:<8} {baseline:>+10.3f} {dom:>+10.3f} {nm:>+10.3f} {st:>+10.3f}"
                  f" {dom - baseline:>+10.3f} {nm - baseline:>+10.3f} {st - baseline:>+10.3f}")

    # Final verdict
    print("\n" + "=" * 96)
    print(" KEY DECISION QUESTION")
    print(" 'If dom_only_0p50 or dom_only_0p60 stays materially better than baseline OOS'")
    print("=" * 96)
    for v in ["dom050", "dom060"]:
        diffs = []
        for b in BLOCKS:
            base = rows["baseline"][b].get("sharpe")
            cur = rows[v][b].get("sharpe")
            if base is not None and cur is not None:
                diffs.append((b, cur - base, cur, base))
        if diffs:
            print(f"\n{v}:")
            for b, d, cur, base in diffs:
                print(f"  {b}: Sharpe {cur:+.3f} vs baseline {base:+.3f}  "
                      f"-> {'+' if d >= 0 else ''}{d:.3f}")
            avg = mean(x[1] for x in diffs)
            n_pos = sum(1 for x in diffs if x[1] > 0.2)
            print(f"  mean lift = {avg:+.3f}, blocks with lift>0.2: {n_pos}/3")


if __name__ == "__main__":
    main()
