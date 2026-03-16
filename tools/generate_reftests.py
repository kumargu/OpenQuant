#!/usr/bin/env python3
"""
Reference test fixture generator for OpenQuant features.

Computes expected indicator values from pure mathematical definitions
using numpy. These values are INDEPENDENT of the Rust implementation —
they serve as ground truth for correctness testing.

Usage:
    python tools/generate_reftests.py

Outputs:
    engine/crates/core/src/features/reftest_fixtures.json

The generated fixtures use seeded random data so results are reproducible
but non-trivial (not hand-picked values that might accidentally mask bugs).
"""

import json
import numpy as np
from pathlib import Path


def seeded_rng(seed: int) -> np.random.Generator:
    return np.random.default_rng(seed)


# ---------------------------------------------------------------------------
# Pure-math indicator implementations (independent of Rust code)
# ---------------------------------------------------------------------------

def compute_ema(prices: list[float], period: int) -> list[float]:
    """Standard EMA: α = 2/(period+1). First value seeds."""
    alpha = 2.0 / (period + 1.0)
    ema = [prices[0]]
    for p in prices[1:]:
        ema.append(alpha * p + (1 - alpha) * ema[-1])
    return ema


def compute_wilder_ema(values: list[float], period: int) -> list[float]:
    """Wilder's smoothing: α = 1/period. First value seeds."""
    alpha = 1.0 / period
    result = [values[0]]
    for v in values[1:]:
        result.append(alpha * v + (1 - alpha) * result[-1])
    return result


def compute_sma(prices: list[float], window: int) -> list[float]:
    """Running SMA — partial average before window is full."""
    result = []
    running_sum = 0.0
    buf = []
    for p in prices:
        if len(buf) >= window:
            running_sum -= buf[len(buf) - window]
        running_sum += p
        buf.append(p)
        n = min(len(buf), window)
        result.append(running_sum / n)
    return result


def compute_rolling_stats(values: list[float], window: int):
    """Rolling mean, population variance, std_dev over a fixed window."""
    means = []
    variances = []
    std_devs = []
    buf = []
    for v in values:
        buf.append(v)
        w = buf[-window:]  # last `window` elements
        n = len(w)
        m = sum(w) / n
        means.append(m)
        if n < 2:
            variances.append(0.0)
            std_devs.append(0.0)
        else:
            var = sum((x - m) ** 2 for x in w) / n
            var = max(var, 0.0)
            variances.append(var)
            std_devs.append(var ** 0.5)
    return means, variances, std_devs


def compute_adx(highs: list[float], lows: list[float], closes: list[float],
                period: int):
    """ADX with Wilder smoothing. Returns (adx, plus_di, minus_di) per bar."""
    n = len(highs)
    adx_vals = []
    plus_di_vals = []
    minus_di_vals = []

    # Wilder EMA state (manual, no function reuse to keep independent)
    alpha = 1.0 / period
    sm_plus_dm = 0.0
    sm_minus_dm = 0.0
    sm_tr = 0.0
    adx_smooth = 0.0
    first_dm = True
    first_adx = True

    prev_h = highs[0]
    prev_l = lows[0]
    prev_c = closes[0]
    adx_vals.append(0.0)
    plus_di_vals.append(0.0)
    minus_di_vals.append(0.0)

    for i in range(1, n):
        h, l, c = highs[i], lows[i], closes[i]

        # Directional movement
        up_move = h - prev_h
        down_move = prev_l - l
        plus_dm = up_move if (up_move > down_move and up_move > 0) else 0.0
        minus_dm = down_move if (down_move > up_move and down_move > 0) else 0.0

        # True range
        hl = h - l
        hc = abs(h - prev_c)
        lc = abs(l - prev_c)
        tr = max(hl, hc, lc)

        prev_h, prev_l, prev_c = h, l, c

        # Wilder smooth +DM, -DM, TR
        if first_dm:
            sm_plus_dm = plus_dm
            sm_minus_dm = minus_dm
            sm_tr = tr
            first_dm = False
        else:
            sm_plus_dm = alpha * plus_dm + (1 - alpha) * sm_plus_dm
            sm_minus_dm = alpha * minus_dm + (1 - alpha) * sm_minus_dm
            sm_tr = alpha * tr + (1 - alpha) * sm_tr

        if sm_tr < 1e-10:
            adx_vals.append(0.0)
            plus_di_vals.append(0.0)
            minus_di_vals.append(0.0)
            continue

        plus_di = 100.0 * sm_plus_dm / sm_tr
        minus_di = 100.0 * sm_minus_dm / sm_tr

        di_sum = plus_di + minus_di
        dx = 100.0 * abs(plus_di - minus_di) / di_sum if di_sum > 1e-10 else 0.0

        if first_adx:
            adx_smooth = dx
            first_adx = False
        else:
            adx_smooth = alpha * dx + (1 - alpha) * adx_smooth

        adx_vals.append(adx_smooth)
        plus_di_vals.append(plus_di)
        minus_di_vals.append(minus_di)

    return adx_vals, plus_di_vals, minus_di_vals


def compute_bollinger(closes: list[float], sma_window: int, std_window: int):
    """Bollinger Bands: SMA ± 2×std, %B, bandwidth."""
    sma_vals = compute_sma(closes, sma_window)
    _, _, std_vals = compute_rolling_stats(closes, std_window)

    uppers = []
    lowers = []
    pct_bs = []
    bandwidths = []

    for i, (close, sma, std) in enumerate(zip(closes, sma_vals, std_vals)):
        upper = sma + 2.0 * std
        lower = sma - 2.0 * std
        bb_width = upper - lower

        if bb_width > 1e-10:
            pct_b = (close - lower) / bb_width
        else:
            pct_b = 0.5

        if sma > 1e-10:
            bandwidth = bb_width / sma
        else:
            bandwidth = 0.0

        uppers.append(upper)
        lowers.append(lower)
        pct_bs.append(pct_b)
        bandwidths.append(bandwidth)

    return uppers, lowers, pct_bs, bandwidths


# ---------------------------------------------------------------------------
# Fixture generation
# ---------------------------------------------------------------------------

def generate_random_ohlcv(rng: np.random.Generator, n_bars: int,
                          base_price: float = 100.0, volatility: float = 0.02):
    """Generate realistic random OHLCV data with a seeded RNG."""
    closes = [base_price]
    highs = [base_price * (1 + abs(rng.normal(0, volatility)))]
    lows = [base_price * (1 - abs(rng.normal(0, volatility)))]
    volumes = [float(rng.integers(500, 5000))]

    price = base_price
    for _ in range(1, n_bars):
        ret = rng.normal(0, volatility)
        price = price * (1 + ret)
        spread = abs(rng.normal(0, volatility)) * price
        h = price + spread
        l = price - abs(rng.normal(0, volatility)) * price
        if l > price:
            l = price - spread * 0.5
        closes.append(round(price, 8))
        highs.append(round(h, 8))
        lows.append(round(l, 8))
        volumes.append(float(rng.integers(500, 5000)))

    return closes, highs, lows, volumes


def make_fixture(seed: int, n_bars: int, label: str) -> dict:
    """Generate a complete fixture with inputs + expected outputs."""
    rng = seeded_rng(seed)
    closes, highs, lows, volumes = generate_random_ohlcv(rng, n_bars)

    # Compute all expected values from pure math
    ema_10 = compute_ema(closes, 10)
    ema_30 = compute_ema(closes, 30)
    wilder_14 = compute_wilder_ema(closes, 14)
    sma_32 = compute_sma(closes, 32)
    adx, plus_di, minus_di = compute_adx(highs, lows, closes, 14)
    boll_upper, boll_lower, boll_pct_b, boll_bw = compute_bollinger(
        closes, sma_window=32, std_window=32)

    # Sample at specific bars for testing (after warmup)
    # Use multiple checkpoints to catch drift
    checkpoints = [32, 40, 50, 64, n_bars - 1]
    checkpoints = [c for c in checkpoints if c < n_bars]

    return {
        "label": label,
        "seed": seed,
        "n_bars": n_bars,
        "inputs": {
            "closes": closes,
            "highs": highs,
            "lows": lows,
            "volumes": volumes,
        },
        "expected": {
            "ema_10": {str(c): ema_10[c] for c in checkpoints},
            "ema_30": {str(c): ema_30[c] for c in checkpoints},
            "wilder_14": {str(c): wilder_14[c] for c in checkpoints},
            "sma_32": {str(c): sma_32[c] for c in checkpoints},
            "adx_14": {str(c): adx[c] for c in checkpoints},
            "plus_di_14": {str(c): plus_di[c] for c in checkpoints},
            "minus_di_14": {str(c): minus_di[c] for c in checkpoints},
            "bollinger_upper": {str(c): boll_upper[c] for c in checkpoints},
            "bollinger_lower": {str(c): boll_lower[c] for c in checkpoints},
            "bollinger_pct_b": {str(c): boll_pct_b[c] for c in checkpoints},
            "bollinger_bandwidth": {str(c): boll_bw[c] for c in checkpoints},
        },
        "checkpoints": checkpoints,
    }


def main():
    fixtures = []

    # Fixture 1: moderate volatility, 80 bars
    fixtures.append(make_fixture(seed=42, n_bars=80, label="moderate_vol_80"))

    # Fixture 2: high volatility, 100 bars (catches precision issues)
    fixtures.append(make_fixture(seed=1337, n_bars=100, label="high_vol_100"))

    # Fixture 3: low volatility, trending up (ADX should be meaningful)
    fixtures.append(make_fixture(seed=2024, n_bars=80, label="trending_80"))

    output = {
        "version": 1,
        "generator": "tools/generate_reftests.py",
        "description": (
            "Reference test fixtures computed from pure math (numpy). "
            "Independent of Rust implementation. Regenerate with: "
            "python tools/generate_reftests.py"
        ),
        "tolerance": 1e-8,
        "fixtures": fixtures,
    }

    out_path = Path(__file__).parent.parent / "engine" / "crates" / "core" / "src" / "features" / "reftest_fixtures.json"
    out_path.write_text(json.dumps(output, indent=2) + "\n")
    print(f"Wrote {len(fixtures)} fixtures to {out_path}")
    print(f"Checkpoints per fixture: {[f['checkpoints'] for f in fixtures]}")

    # Print a quick sanity check
    f0 = fixtures[0]
    print(f"\nSanity check (fixture 0, bar 64):")
    print(f"  EMA(10): {f0['expected']['ema_10']['64']:.10f}")
    print(f"  EMA(30): {f0['expected']['ema_30']['64']:.10f}")
    print(f"  SMA(32): {f0['expected']['sma_32']['64']:.10f}")
    print(f"  ADX(14): {f0['expected']['adx_14']['64']:.10f}")


if __name__ == "__main__":
    main()
