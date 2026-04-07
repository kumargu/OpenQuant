"""
OpenQuant autoresearch experiment runner.
Builds the engine (if Rust changed), runs a replay, extracts metrics.

Usage: python train.py > run.log 2>&1

This is the equivalent of train.py in karpathy/autoresearch.
The LLM modifies strategy code (config/pairs.toml, engine/crates/...),
then runs this script to measure the result.

The LLM CAN also modify this file — same as in autoresearch where
the agent edits train.py. But prepare.py is always read-only.
"""

import os
import re
import subprocess
import sys
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
ENGINE = os.path.join(ROOT, "engine")
RUNNER = os.path.join(ENGINE, "target", "release", "openquant-runner")

# Replay period — default is 1 year of history for robust results.
# Override with env vars for faster iteration during development.
REPLAY_START = os.environ.get("REPLAY_START", "2025-04-07")
REPLAY_END = os.environ.get("REPLAY_END", "2026-03-28")

# Bar cache — dramatically speeds up repeated replays by caching Alpaca bars
BAR_CACHE = os.path.join(ROOT, "data", "bar_cache")

# Metals mode — set METALS=1 to use metals config, candidates, and pipeline.
# Overrides config, candidates, and pipeline flags for the replay command.
METALS = os.environ.get("METALS", "")
if METALS:
    CONFIG = os.environ.get("CONFIG", os.path.join(ROOT, "config", "metals.toml"))
    CANDIDATES = os.environ.get("CANDIDATES", os.path.join(ROOT, "trading", "pair_candidates_metals_filtered.json"))
    PIPELINE = os.environ.get("PIPELINE", "metals")
    BAR_CACHE = os.path.join(ROOT, "data", "bar_cache_metals")
    if REPLAY_START == "2025-04-07":  # only override if not explicitly set
        REPLAY_START = "2025-07-01"
else:
    CONFIG = os.environ.get("CONFIG", os.path.join(ROOT, "config", "pairs.toml"))
    CANDIDATES = os.environ.get("CANDIDATES", "")
    PIPELINE = os.environ.get("PIPELINE", "")

# ---------------------------------------------------------------------------
# Step 1: Build (if needed)
# ---------------------------------------------------------------------------

print("=== BUILD ===")
t_build_start = time.time()

result = subprocess.run(
    ["cargo", "build", "--release", "-p", "openquant-runner"],
    cwd=ENGINE,
    capture_output=True,
    text=True,
)

build_time = time.time() - t_build_start

if result.returncode != 0:
    print("BUILD FAILED")
    print(result.stderr[-3000:] if len(result.stderr) > 3000 else result.stderr)
    print()
    print("---")
    print("avg_bps:      0.0")
    print("trades:       0")
    print("sharpe:       0.0")
    print("win_rate:     0.0")
    print("net_pnl_bps:  0.0")
    print("max_dd_bps:   0.0")
    print("build_secs:   %.1f" % build_time)
    print("replay_secs:  0.0")
    print("status:       build_failed")
    sys.exit(1)

print(f"Build OK ({build_time:.1f}s)")

# ---------------------------------------------------------------------------
# Step 2: Run replay
# ---------------------------------------------------------------------------

print(f"=== REPLAY ({REPLAY_START} to {REPLAY_END}) ===")
t_replay_start = time.time()

replay_log_path = os.path.join(ROOT, "autoresearch", "replay.log")

with open(replay_log_path, "w") as log_file:
    cmd = [RUNNER, "replay", "--config", CONFIG,
           "--start", REPLAY_START, "--end", REPLAY_END,
           "--bar-cache", BAR_CACHE]
    if CANDIDATES:
        cmd += ["--candidates", CANDIDATES]
    if PIPELINE:
        cmd += ["--pipeline", PIPELINE]
    print(f"CMD: {' '.join(cmd)}")
    proc = subprocess.run(
        cmd,
        cwd=ROOT,
        stdout=subprocess.DEVNULL,
        stderr=log_file,
    )

replay_time = time.time() - t_replay_start
print(f"Replay finished ({replay_time:.1f}s, exit code {proc.returncode})")

# ---------------------------------------------------------------------------
# Step 3: Parse EXIT lines for metrics
# ---------------------------------------------------------------------------

print("=== METRICS ===")

exits = []
try:
    with open(replay_log_path) as f:
        for line in f:
            if "pairs: EXIT" in line and "net_bps=" in line:
                m = re.search(r'net_bps="([^"]+)"', line)
                if m:
                    exits.append(float(m.group(1)))
except FileNotFoundError:
    pass

if not exits:
    print("No trades found in replay log.")
    print()
    print("---")
    print("avg_bps:      0.0")
    print("trades:       0")
    print("sharpe:       0.0")
    print("win_rate:     0.0")
    print("net_pnl_bps:  0.0")
    print("max_dd_bps:   0.0")
    print("build_secs:   %.1f" % build_time)
    print("replay_secs:  %.1f" % replay_time)
    print("status:       no_trades")
    sys.exit(0)

# Compute stats
n = len(exits)
wins = sum(1 for x in exits if x > 0)
total_bps = sum(exits)
avg_bps = total_bps / n
win_rate = wins / n * 100

# Sharpe — two-pass for numerical stability
mean = avg_bps
if n > 1:
    variance = sum((x - mean) ** 2 for x in exits) / (n - 1)
    std = variance ** 0.5
    sharpe = mean / std if std > 0 else 0.0
else:
    sharpe = 0.0

# Max drawdown — cumulative P&L peak-to-trough
cumulative = 0.0
peak = 0.0
max_dd = 0.0
for bps in exits:
    cumulative += bps
    if cumulative > peak:
        peak = cumulative
    dd = peak - cumulative
    if dd > max_dd:
        max_dd = dd

# Count exit reasons
exit_reasons = {}
try:
    with open(replay_log_path) as f:
        for line in f:
            if "pairs: EXIT" in line:
                m = re.search(r'exit="([^"]+)"', line)
                if m:
                    reason = m.group(1)
                    exit_reasons[reason] = exit_reasons.get(reason, 0) + 1
except FileNotFoundError:
    pass

# ---------------------------------------------------------------------------
# Print results (same format as autoresearch's val_bpb output)
# ---------------------------------------------------------------------------

print()
for reason, count in sorted(exit_reasons.items()):
    print(f"  {reason}: {count}")
print()

print("---")
print(f"avg_bps:      {avg_bps:.1f}")
print(f"trades:       {n}")
print(f"sharpe:       {sharpe:.2f}")
print(f"win_rate:     {win_rate:.1f}")
print(f"net_pnl_bps:  {total_bps:.1f}")
print(f"max_dd_bps:   {-max_dd:.1f}")
print(f"build_secs:   {build_time:.1f}")
print(f"replay_secs:  {replay_time:.1f}")
