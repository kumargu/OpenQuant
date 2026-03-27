"""
Shared pairs trading logic — single source of truth.

Both capital_sim.py (backtest) and live_pipeline.py (live) import from here.
If a threshold, gate, or formula exists in only one place, it's a bug.

Architecture:
    pairs_core.py (this file)    ← shared logic, config, quality gates
    daily_walkforward_dashboard  ← scan_pair, compute_z, PairParams, OLS
    openquant (Rust bridge)      ← priority scoring, max_hold, rotation math
"""

import logging
import math
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent

# ── Rust bridge ───────────────────────────────────────────────────────────────

_venv_site = ROOT / "engine" / ".venv" / "lib"
_site_pkgs = next(_venv_site.glob("python*/site-packages"), None)
if _site_pkgs and str(_site_pkgs) not in sys.path:
    sys.path.insert(0, str(_site_pkgs))

try:
    from openquant import openquant as _oq
    compute_priority_score = _oq.compute_priority_score
    expected_return_per_dollar_per_day = _oq.expected_return_per_dollar_per_day
    compute_max_hold_days = _oq.compute_max_hold_days
    compute_remaining_per_day = _oq.compute_remaining_per_day
    should_rotate = _oq.should_rotate
    compute_capital_metrics = _oq.compute_capital_metrics
except ImportError as _e:
    raise ImportError(
        "openquant pybridge not found. Run: cd engine && maturin develop --release"
    ) from _e

# ── Re-exports from dashboard ─────────────────────────────────────────────────

sys.path.insert(0, str(ROOT / "scripts"))

from daily_walkforward_dashboard import (  # noqa: E402
    scan_pair,
    compute_z,
    ols_simple,
    PairParams,
    FORMATION_DAYS,
    COST_BPS,
    load_earnings_calendar,
    is_near_earnings,
)

# ── Config — single source of truth ──────────────────────────────────────────
# Both capital_sim.py and live_pipeline.py use these. Change here, changes everywhere.

TOTAL_CAPITAL = 10_000
MIN_TRADE_CAPITAL = 200         # minimum per leg to bother trading
MAX_PER_TRADE_FRAC = 0.25       # max 25% of total in one trade

# Quality gates
MIN_R2_ENTRY = 0.70
MAX_HL_ENTRY = 5.0
MIN_ADF_ENTRY = -2.5
MIN_BETA = 0.1
MIN_SPREAD_STD = 0.005

# Hold / exit
HOLD_MULTIPLIER = 2.5           # max_hold = ceil(HOLD_MULTIPLIER * HL)
MAX_HOLD_CAP = 10               # absolute cap on max_hold days
EXIT_Z_DEFAULT = 0.2            # default exit z threshold
EXIT_DECAY_FLOOR = 0.3          # time-decay exit floor

# Stability / win rate
STABILITY_LOOKBACK = 10         # days to check for scan_pair stability
MAX_REJECT_DAYS = 5             # max allowed rejections in lookback
MIN_WIN_RATE = 0.40             # pair+direction must have >= 40% win rate

# Rotation (Leung & Li 2015)
ROTATION_COST_PER_DAY = 0.001
MAX_ROTATIONS_PER_DAY = 2

logger = logging.getLogger("pairs_core")


# ── Quality gates ─────────────────────────────────────────────────────────────

def check_quality_gate(params):
    """Check R², HL, ADF, beta, spread_std. Returns (ok, [reasons])."""
    reasons = []
    if params.r2 < MIN_R2_ENTRY:
        reasons.append(f"r2={params.r2:.3f}")
    if params.half_life > MAX_HL_ENTRY:
        reasons.append(f"hl={params.half_life:.1f}")
    if params.adf_stat > MIN_ADF_ENTRY:
        reasons.append(f"adf={params.adf_stat:.2f}")
    if params.beta < MIN_BETA:
        reasons.append(f"beta={params.beta:.3f}")
    if params.spread_std < MIN_SPREAD_STD:
        reasons.append(f"spread_std={params.spread_std:.4f}")
    return len(reasons) == 0, reasons


def check_stability(leg_a, leg_b, prices, total_bars):
    """Check if pair passed scan_pair on enough recent days.

    Returns (ok, pass_count, reject_count).
    """
    pass_count = 0
    check_days = min(STABILITY_LOOKBACK, total_bars)
    for offset in range(check_days):
        day = total_bars - 1 - offset
        if day < 0:
            break
        params = scan_pair(leg_a, leg_b, prices[leg_a], prices[leg_b], day)
        if params is not None:
            pass_count += 1
    reject_count = check_days - pass_count
    return reject_count <= MAX_REJECT_DAYS, pass_count, reject_count


def compute_win_rate(leg_a, leg_b, direction, prices, total_bars, entry_z=1.0, exit_z=None, lookback=250):
    """Compute historical win rate for pair+direction from backtest data.

    direction: 1=LONG (z<0), -1=SHORT (z>0)
    entry_z: per-pair entry threshold (must match live config)
    exit_z: per-pair exit threshold (defaults to EXIT_Z_DEFAULT)
    A 'win' = frozen z crosses exit threshold within max_hold days.
    """
    if exit_z is None:
        exit_z = EXIT_Z_DEFAULT
    wins = losses = 0
    start = max(0, total_bars - lookback)

    for d in range(start, total_bars - 10):
        params = scan_pair(leg_a, leg_b, prices[leg_a], prices[leg_b], d)
        if params is None:
            continue
        z = compute_z(params, prices[leg_a][d], prices[leg_b][d])

        # Check if this day would have been an entry (use per-pair threshold)
        if direction == 1 and z >= -entry_z:
            continue
        if direction == -1 and z <= entry_z:
            continue

        # Track forward outcome with frozen stats
        reverted = False
        max_hold = min(int(math.ceil(HOLD_MULTIPLIER * params.half_life)), MAX_HOLD_CAP)
        for fwd in range(1, min(max_hold + 1, total_bars - d)):
            spread_fwd = (math.log(prices[leg_a][d + fwd])
                          - params.alpha
                          - params.beta * math.log(prices[leg_b][d + fwd]))
            frozen_z = (spread_fwd - params.spread_mean) / params.spread_std
            if abs(frozen_z) < exit_z:
                reverted = True
                break

        if reverted:
            wins += 1
        else:
            losses += 1

    total = wins + losses
    if total == 0:
        return None, 0, 0
    return wins / total, wins, losses


def validate_entry(leg_a, leg_b, params, z, prices, total_bars, earnings_cal, entry_z=1.0, exit_z=None):
    """Run all quality gates. Returns (ok, reject_reason).

    Used by both capital_sim (backtest) and live_pipeline (live).
    """
    # 1. Quality gate
    ok, reasons = check_quality_gate(params)
    if not ok:
        return False, f"quality_gate: {' '.join(reasons)}"

    # 2. Earnings blackout
    if earnings_cal:
        if is_near_earnings(leg_a, total_bars - 1, total_bars, earnings_cal):
            return False, f"{leg_a} near earnings"
        if is_near_earnings(leg_b, total_bars - 1, total_bars, earnings_cal):
            return False, f"{leg_b} near earnings"

    # 3. Stability gate
    stable, pass_count, reject_count = check_stability(leg_a, leg_b, prices, total_bars)
    if not stable:
        return False, f"unstable: scan_pair rejected {reject_count}/{STABILITY_LOOKBACK} days"

    # 4. Win rate gate
    direction = 1 if z < 0 else -1
    dir_label = "LONG" if direction == 1 else "SHORT"
    wr, wins, losses = compute_win_rate(leg_a, leg_b, direction, prices, total_bars, entry_z=entry_z, exit_z=exit_z)
    if wr is None:
        return False, f"no historical {dir_label} entries to compute win rate"
    if wr < MIN_WIN_RATE:
        return False, f"{dir_label} win_rate={wr:.0%} ({wins}W/{losses}L) < {MIN_WIN_RATE:.0%}"

    return True, None


# ── Frozen z-score ────────────────────────────────────────────────────────────

def compute_frozen_z(price_a, price_b, alpha, beta, spread_mean, spread_std):
    """Compute z-score using entry-time parameters (frozen stats).

    All params must be from the SAME regression fit at entry time.
    Returns frozen_z or None if inputs are invalid.
    """
    if price_a <= 0 or price_b <= 0 or not spread_std or spread_std <= 0:
        return None
    spread_now = math.log(price_a) - alpha - beta * math.log(price_b)
    return (spread_now - spread_mean) / spread_std


# ── Exit decision ─────────────────────────────────────────────────────────────

def decide_exit(frozen_z, days_held, max_hold, exit_z=EXIT_Z_DEFAULT, use_decay=True):
    """Decide whether to exit a position.

    Args:
        frozen_z: current frozen z-score (None = skip z-based check)
        days_held: trading days since entry
        max_hold: maximum hold period
        exit_z: base exit z threshold
        use_decay: if True, tighten exit threshold over time (backtest style)

    Returns: reason string ("reversion", "max_hold") or None
    """
    if days_held >= max_hold:
        return "max_hold"

    if frozen_z is None:
        return None

    if use_decay:
        # Time-decay: threshold tightens toward floor as hold progresses
        decay = min(days_held / max_hold, 1.0)
        floor = min(EXIT_DECAY_FLOOR, exit_z)
        eff_exit = exit_z - (exit_z - floor) * decay
    else:
        eff_exit = exit_z

    if abs(frozen_z) < eff_exit:
        return "reversion"

    return None


# ── Signal scoring ────────────────────────────────────────────────────────────

def score_signal(z, half_life, spread_std):
    """Compute priority score and expected return per dollar per day.

    Returns (priority, erpdd, max_hold, kappa).
    """
    kappa = 0.693147 / half_life  # ln(2) / HL
    priority = compute_priority_score(z, kappa, spread_std)
    max_hold = compute_max_hold_days(
        half_life, hold_multiplier=HOLD_MULTIPLIER, max_hold_cap=MAX_HOLD_CAP
    )
    erpdd = expected_return_per_dollar_per_day(z, spread_std, kappa, float(max_hold))
    return priority, erpdd, max_hold, kappa
