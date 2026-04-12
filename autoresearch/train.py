"""
OpenQuant train.py — Karpathy autoresearch pattern.

The agent edits the EDITABLE CONSTANTS below, runs this script, reads
the canonical `---` metrics block from stdout. One experiment per run.

  python train.py > run.log 2>&1
  grep '^trades:' run.log

prepare.py is READ-ONLY — never edited by the agent.
program.md is HUMAN-EDITED — the research brief.
results.tsv is APPEND-ONLY — one row per experiment.
NOTEBOOK.md is APPEND-ONLY — narrative log.
"""

import os
import re
import subprocess
import sys
import time
from datetime import datetime
from pathlib import Path

ROOT = Path(__file__).parent.parent
ENGINE_DIR = ROOT / "engine"
RUNNER = ENGINE_DIR / "target" / "release" / "openquant-runner"
RESULTS_TSV = ROOT / "autoresearch" / "results.tsv"
NOTEBOOK = ROOT / "autoresearch" / "NOTEBOOK.md"

# ============================================================================
# EDITABLE CONSTANTS — the agent changes these, runs, reads metrics.
# ============================================================================
NAME          = "baseline_2026"
CANDIDATES    = "trading/year2026_candidates_top100.json"
REPLAY_START  = "2026-01-02"
REPLAY_END    = "2026-04-09"
BAR_CACHE     = "data/bar_cache_2026"

# Mock server URL — replay always uses parquet-backed mock (same data as lab).
# Start with: python scripts/mock_alpaca.py --port 8787
MOCK_URL      = "http://127.0.0.1:8787/v2/stocks/bars"

# One-sentence prediction — written BEFORE running.
HYPOTHESIS    = (
    "Baseline: lab top-100 candidates through Rust picker (lab pipeline, "
    "top_k=40) on Jan-Apr 2026 should reproduce +$3,399 / 73.7% win."
)
# ============================================================================


def build():
    """Build the runner. Returns (ok, seconds)."""
    t0 = time.time()
    r = subprocess.run(
        ["cargo", "build", "--release", "-p", "openquant-runner"],
        cwd=ENGINE_DIR, capture_output=True, text=True,
    )
    secs = time.time() - t0
    if r.returncode != 0:
        print("BUILD FAILED", file=sys.stderr)
        print(r.stderr[-2000:], file=sys.stderr)
    return r.returncode == 0, secs


def replay():
    """Run the replay. Returns (log_path, seconds, exit_code)."""
    log_path = ROOT / "autoresearch" / "replay.log"
    t0 = time.time()
    env = os.environ.copy()
    env["ALPACA_DATA_URL"] = MOCK_URL

    cmd = [
        str(RUNNER), "replay", "--engine", "snp500",
        "--start", REPLAY_START, "--end", REPLAY_END,
        "--candidates", CANDIDATES,
        "--bar-cache", BAR_CACHE,
    ]
    print(f"CMD: {' '.join(cmd)}", file=sys.stderr)
    with open(log_path, "w") as f:
        subprocess.run(cmd, cwd=ROOT, stdout=subprocess.DEVNULL, stderr=f, env=env)
    return log_path, time.time() - t0


def parse_trades(log_path):
    """Parse EXIT lines from engine log. Returns list of (pair, bps, reason)."""
    trades = []
    try:
        for line in open(log_path):
            if "pairs: EXIT" not in line:
                continue
            mp = re.search(r'pair="([^"]+)"', line)
            mb = re.search(r'net_bps="([^"]+)"', line)
            mr = re.search(r'exit="([^"]+)"', line)
            if mp and mb:
                trades.append((
                    mp.group(1),
                    float(mb.group(1)),
                    mr.group(1) if mr else "?",
                ))
    except FileNotFoundError:
        pass
    return trades


def compute_metrics(trades):
    """Compute metrics from trade list."""
    if not trades:
        return dict(trades=0, wins=0, win_rate=0, total_bps=0, avg_bps=0,
                    sharpe=0, max_dd_bps=0, status="no_trades")
    n = len(trades)
    bps_list = [t[1] for t in trades]
    wins = sum(1 for b in bps_list if b > 0)
    total = sum(bps_list)
    avg = total / n
    if n > 1:
        var = sum((b - avg) ** 2 for b in bps_list) / (n - 1)
        sharpe = avg / (var ** 0.5) if var > 0 else 0
    else:
        sharpe = 0
    cum = peak = dd = 0
    for b in bps_list:
        cum += b
        peak = max(peak, cum)
        dd = max(dd, peak - cum)
    reasons = {}
    for _, _, r in trades:
        reasons[r] = reasons.get(r, 0) + 1
    return dict(trades=n, wins=wins, win_rate=100 * wins / n, total_bps=total,
                avg_bps=avg, sharpe=sharpe, max_dd_bps=-dd, status="ok",
                reasons=reasons)


def append_results_tsv(m, build_secs, replay_secs):
    header = "name\tstart\tend\ttrades\twin_rate\ttotal_bps\tsharpe\thypothesis\n"
    if not RESULTS_TSV.exists():
        RESULTS_TSV.write_text(header)
    row = (f"{NAME}\t{REPLAY_START}\t{REPLAY_END}\t{m['trades']}\t"
           f"{m['win_rate']:.1f}\t{m['total_bps']:.0f}\t{m['sharpe']:.2f}\t"
           f"{HYPOTHESIS}\n")
    with open(RESULTS_TSV, "a") as f:
        f.write(row)


def append_notebook(m, build_secs, replay_secs):
    if not NOTEBOOK.exists():
        NOTEBOOK.write_text("# OpenQuant Autoresearch NOTEBOOK\n\nAppend-only experiment log.\n")
    entry = f"""
---

## {NAME} ({datetime.now().isoformat(timespec='seconds')})

- **candidates**: `{CANDIDATES}`
- **window**: {REPLAY_START} → {REPLAY_END}
- **hypothesis**: {HYPOTHESIS}
- **trades**: {m['trades']} (W/L: {m['wins']}/{m['trades']-m['wins']}, {m['win_rate']:.1f}%)
- **total P&L**: {m['total_bps']:+.0f} bps
- **sharpe**: {m['sharpe']:.2f}
- **max drawdown**: {m['max_dd_bps']:.0f} bps
- **build**: {build_secs:.1f}s, replay: {replay_secs:.1f}s
"""
    with open(NOTEBOOK, "a") as f:
        f.write(entry)


def main():
    print(f"=== {NAME} ===", file=sys.stderr)
    print(f"hypothesis: {HYPOTHESIS}", file=sys.stderr)

    ok, build_secs = build()
    if not ok:
        print("---")
        print("status:       build_failed")
        return

    log_path, replay_secs = replay()
    trades = parse_trades(log_path)
    m = compute_metrics(trades)

    # Append to results.tsv and NOTEBOOK.md
    append_results_tsv(m, build_secs, replay_secs)
    append_notebook(m, build_secs, replay_secs)

    # Exit reasons
    if "reasons" in m:
        for r, c in sorted(m["reasons"].items()):
            print(f"  {r}: {c}", file=sys.stderr)

    # Canonical metrics footer — agents read this from run.log
    print()
    print("---")
    print(f"trades:       {m['trades']}")
    print(f"wins:         {m['wins']}")
    print(f"win_rate:     {m['win_rate']:.1f}")
    print(f"total_bps:    {m['total_bps']:.0f}")
    print(f"avg_bps:      {m['avg_bps']:.1f}")
    print(f"sharpe:       {m['sharpe']:.2f}")
    print(f"max_dd_bps:   {m['max_dd_bps']:.0f}")
    print(f"build_secs:   {build_secs:.1f}")
    print(f"replay_secs:  {replay_secs:.1f}")
    print(f"status:       {m['status']}")


if __name__ == "__main__":
    main()
