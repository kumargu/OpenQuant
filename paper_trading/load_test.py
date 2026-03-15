"""
End-to-end load test: measures latency across the full Python → Rust engine
→ journal → metrics pipeline under sustained load.

Scenarios:
  1. single-symbol   — one symbol, sustained bar rate
  2. multi-symbol    — 10 symbols concurrently
  3. burst           — sudden 10x spike in bar rate
  4. journal-stress  — journal + metrics enabled under load

Usage:
  python -m paper_trading.load_test                          # quick (60s default)
  python -m paper_trading.load_test --duration 300           # 5-min post-merge run
  python -m paper_trading.load_test --scenario single-symbol # one scenario only
  python -m paper_trading.load_test --duration 120 --scenario all
"""

import argparse
import math
import os
import random
import statistics
import sys
import tempfile
import time

from openquant import Engine


# ---------------------------------------------------------------------------
# Synthetic bar generation
# ---------------------------------------------------------------------------

def generate_bars(symbol: str, n: int, seed: int = 42) -> list[tuple]:
    """Generate n synthetic bars as (symbol, ts, o, h, l, c, v) tuples."""
    rng = random.Random(seed)
    price = 100.0
    bars = []
    base_ts = 1700000000000  # fixed epoch

    for i in range(n):
        ret = rng.uniform(-0.02, 0.02) + (100.0 - price) * 0.001
        price *= 1.0 + ret
        price = max(price, 10.0)

        rng_range = price * rng.uniform(0.001, 0.01)
        open_ = price + rng.uniform(-rng_range, rng_range) * 0.5
        high = max(open_, price) + rng_range * rng.uniform(0.0, 1.0)
        low = min(open_, price) - rng_range * rng.uniform(0.0, 1.0)
        volume = 1000.0 + rng.uniform(0.0, 2000.0)

        bars.append((symbol, base_ts + i * 60_000, open_, high, low, price, volume))

    return bars


def generate_multi_symbol_bars(symbols: list[str], n_per_symbol: int, seed: int = 42):
    """Generate interleaved bars for multiple symbols."""
    all_bars = []
    for idx, sym in enumerate(symbols):
        all_bars.extend(generate_bars(sym, n_per_symbol, seed=seed + idx))
    # Sort by timestamp to interleave
    all_bars.sort(key=lambda b: b[1])
    return all_bars


# ---------------------------------------------------------------------------
# Latency measurement
# ---------------------------------------------------------------------------

def measure_latencies(engine: Engine, bars: list[tuple]) -> dict:
    """Feed bars and measure per-bar latency. Returns stats dict."""
    latencies_ns = []

    for bar in bars:
        sym, ts, o, h, l, c, v = bar
        t0 = time.perf_counter_ns()
        engine.on_bar(sym, ts, o, h, l, c, v)
        elapsed = time.perf_counter_ns() - t0
        latencies_ns.append(elapsed)

    if not latencies_ns:
        return {"error": "no bars processed"}

    latencies_ns.sort()
    n = len(latencies_ns)

    return {
        "bars": n,
        "p50_us": latencies_ns[n // 2] / 1000,
        "p95_us": latencies_ns[int(n * 0.95)] / 1000,
        "p99_us": latencies_ns[int(n * 0.99)] / 1000,
        "max_us": latencies_ns[-1] / 1000,
        "mean_us": statistics.mean(latencies_ns) / 1000,
        "stdev_us": statistics.stdev(latencies_ns) / 1000 if n > 1 else 0,
        "total_ms": sum(latencies_ns) / 1_000_000,
        "throughput_bars_per_sec": n / (sum(latencies_ns) / 1_000_000_000) if sum(latencies_ns) > 0 else 0,
    }


# ---------------------------------------------------------------------------
# Scenarios
# ---------------------------------------------------------------------------

def scenario_single_symbol(duration_s: int) -> dict:
    """Sustained load on one symbol."""
    # Generate enough bars for the duration at ~50k bars/sec
    n_bars = max(10_000, duration_s * 50_000)
    bars = generate_bars("BTCUSD", n_bars)

    engine = Engine()
    stats = measure_latencies(engine, bars)
    stats["scenario"] = "single-symbol"
    stats["description"] = f"1 symbol, {n_bars:,} bars"
    return stats


def scenario_multi_symbol(duration_s: int) -> dict:
    """10 symbols interleaved."""
    symbols = [f"SYM{i}" for i in range(10)]
    n_per = max(1_000, duration_s * 5_000)
    bars = generate_multi_symbol_bars(symbols, n_per)

    engine = Engine()
    stats = measure_latencies(engine, bars)
    stats["scenario"] = "multi-symbol"
    stats["description"] = f"10 symbols, {len(bars):,} bars"
    return stats


def scenario_burst(duration_s: int) -> dict:
    """Warm up at normal rate, then burst."""
    warmup_bars = generate_bars("BTCUSD", 5_000, seed=1)
    burst_bars = generate_bars("BTCUSD", max(50_000, duration_s * 100_000), seed=2)
    # Shift burst timestamps to continue from warmup
    last_ts = warmup_bars[-1][1]
    burst_bars = [
        (b[0], last_ts + (i + 1) * 60_000, b[2], b[3], b[4], b[5], b[6])
        for i, b in enumerate(burst_bars)
    ]

    engine = Engine()

    # Warmup phase (not measured)
    for bar in warmup_bars:
        engine.on_bar(*bar)

    # Burst phase (measured)
    stats = measure_latencies(engine, burst_bars)
    stats["scenario"] = "burst"
    stats["description"] = f"5k warmup + {len(burst_bars):,} burst bars"
    return stats


def scenario_journal_stress(duration_s: int) -> dict:
    """Journal + metrics enabled under sustained load."""
    n_bars = max(10_000, duration_s * 30_000)
    bars = generate_bars("BTCUSD", n_bars)

    with tempfile.TemporaryDirectory() as tmp:
        journal_path = os.path.join(tmp, "load_test.db")
        engine = Engine(journal_path=journal_path)

        stats = measure_latencies(engine, bars)

        # Capture journal drops before shutdown
        dropped = engine.journal_dropped()
        stats["journal_drops"] = dropped

        # Flush and measure how many bars the writer actually persisted
        engine.shutdown_journal()

        # Count persisted rows to measure writer throughput
        import sqlite3
        conn = sqlite3.connect(journal_path)
        persisted = conn.execute("SELECT COUNT(*) FROM bars").fetchone()[0]
        conn.close()

    written = n_bars - dropped
    drop_pct = (dropped / n_bars * 100) if n_bars > 0 else 0
    stats["scenario"] = "journal-stress"
    stats["journal_persisted"] = persisted
    stats["journal_drop_pct"] = round(drop_pct, 1)
    stats["description"] = (
        f"journal enabled, {n_bars:,} bars, "
        f"{persisted:,} persisted, {dropped:,} drops ({drop_pct:.1f}%)"
    )
    return stats


def scenario_journal_realistic(duration_s: int) -> dict:
    """Journal at realistic rate (~1k bars/sec) — should have zero drops."""
    # 1k bars/sec is 60x faster than production (1 bar/min) but realistic
    # for multi-symbol scenarios (e.g. 10 symbols × 1 bar/sec each)
    n_bars = min(5_000, duration_s * 100)  # keep it short
    bars = generate_bars("BTCUSD", n_bars)

    with tempfile.TemporaryDirectory() as tmp:
        journal_path = os.path.join(tmp, "load_test_realistic.db")
        engine = Engine(journal_path=journal_path)

        latencies_ns = []
        for bar in bars:
            sym, ts, o, h, l, c, v = bar
            t0 = time.perf_counter_ns()
            engine.on_bar(sym, ts, o, h, l, c, v)
            elapsed = time.perf_counter_ns() - t0
            latencies_ns.append(elapsed)
            # Throttle to ~1k bars/sec (1ms between bars)
            time.sleep(0.001)

        dropped = engine.journal_dropped()
        engine.shutdown_journal()

        import sqlite3
        conn = sqlite3.connect(journal_path)
        persisted = conn.execute("SELECT COUNT(*) FROM bars").fetchone()[0]
        conn.close()

    latencies_ns.sort()
    n = len(latencies_ns)

    stats = {
        "bars": n,
        "p50_us": latencies_ns[n // 2] / 1000,
        "p95_us": latencies_ns[int(n * 0.95)] / 1000,
        "p99_us": latencies_ns[int(n * 0.99)] / 1000,
        "max_us": latencies_ns[-1] / 1000,
        "mean_us": statistics.mean(latencies_ns) / 1000,
        "stdev_us": statistics.stdev(latencies_ns) / 1000 if n > 1 else 0,
        "total_ms": sum(latencies_ns) / 1_000_000,
        "throughput_bars_per_sec": 1000,  # throttled rate
        "scenario": "journal-realistic",
        "journal_drops": dropped,
        "journal_persisted": persisted,
        "journal_drop_pct": round((dropped / n * 100) if n > 0 else 0, 1),
        "description": (
            f"journal at ~1k bars/s, {n:,} bars, "
            f"{persisted:,} persisted, {dropped} drops"
        ),
    }
    return stats


SCENARIOS = {
    "single-symbol": scenario_single_symbol,
    "multi-symbol": scenario_multi_symbol,
    "burst": scenario_burst,
    "journal-stress": scenario_journal_stress,
    "journal-realistic": scenario_journal_realistic,
}


# ---------------------------------------------------------------------------
# Report
# ---------------------------------------------------------------------------

def print_report(results: list[dict]):
    """Print markdown latency report."""
    print("\n## Load Test Results\n")
    print(f"| Scenario | Bars | p50 (µs) | p95 (µs) | p99 (µs) | Max (µs) | Throughput | Drops |")
    print(f"|----------|------|----------|----------|----------|----------|------------|-------|")

    for r in results:
        if "error" in r:
            print(f"| {r.get('scenario', '?')} | ERROR | - | - | - | - | - | - |")
            continue

        drops = r.get("journal_drops", "-")
        tp = r["throughput_bars_per_sec"]
        if tp >= 1_000_000:
            tp_str = f"{tp/1_000_000:.1f}M/s"
        elif tp >= 1_000:
            tp_str = f"{tp/1_000:.0f}k/s"
        else:
            tp_str = f"{tp:.0f}/s"

        print(
            f"| {r['scenario']} | {r['bars']:,} | "
            f"{r['p50_us']:.1f} | {r['p95_us']:.1f} | "
            f"{r['p99_us']:.1f} | {r['max_us']:.1f} | "
            f"{tp_str} | {drops} |"
        )

    print()

    # Latency distribution detail
    for r in results:
        if "error" in r:
            continue
        print(f"### {r['scenario']}")
        print(f"  {r['description']}")
        print(f"  Mean: {r['mean_us']:.1f}µs | Stdev: {r['stdev_us']:.1f}µs | Total: {r['total_ms']:.0f}ms")
        if "journal_drops" in r:
            print(f"  Journal drops: {r['journal_drops']}")
        print()

    # Overall verdict
    all_p99 = [r["p99_us"] for r in results if "error" not in r]
    all_drops = [r.get("journal_drops", 0) for r in results if isinstance(r.get("journal_drops"), int)]

    if all_p99:
        worst_p99 = max(all_p99)
        print(f"**Worst p99: {worst_p99:.1f}µs**")
        if worst_p99 > 1000:
            print("WARNING: p99 > 1ms — investigate tail latency")
        elif worst_p99 > 100:
            print("NOTE: p99 > 100µs — acceptable for 1-min bars, monitor for drift")
        else:
            print("PASS: all p99 latencies under 100µs")

    if all_drops and sum(all_drops) > 0:
        print(f"\nWARNING: {sum(all_drops)} journal drops detected under load")
    elif all_drops:
        print("PASS: zero journal drops")


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------

def main():
    parser = argparse.ArgumentParser(description="OpenQuant End-to-End Load Test")
    parser.add_argument("--duration", "-d", type=int, default=60,
                        help="Test duration in seconds (default: 60)")
    parser.add_argument("--scenario", "-s", default="all",
                        choices=["all"] + list(SCENARIOS.keys()),
                        help="Scenario to run (default: all)")
    args = parser.parse_args()

    print(f"OpenQuant Load Test — duration={args.duration}s, scenario={args.scenario}")
    print("=" * 60)

    scenarios = SCENARIOS if args.scenario == "all" else {args.scenario: SCENARIOS[args.scenario]}

    results = []
    for name, fn in scenarios.items():
        print(f"\nRunning: {name}...", end=" ", flush=True)
        t0 = time.time()
        result = fn(args.duration)
        elapsed = time.time() - t0
        print(f"done ({elapsed:.1f}s)")
        results.append(result)

    print_report(results)


if __name__ == "__main__":
    main()
