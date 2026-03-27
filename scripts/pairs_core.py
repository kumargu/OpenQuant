"""
Shared pairs trading logic — single source of truth.

Both capital_sim.py (backtest) and live_pipeline.py (live) import from here.
If a threshold, gate, or formula exists in only one place, it's a bug.

Architecture:
    pairs_core.py (this file)    ← orchestration, config, earnings
    openquant (Rust via pybridge)← ALL math: OLS, ADF, HL, z-score, priority, rotation
    Python does ZERO math        ← Alpaca API, file I/O, logging only
"""

import json
import logging
import sys
from dataclasses import dataclass
from datetime import datetime
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
logger = logging.getLogger("pairs_core")

# ── Rust bridge — ALL math lives here ─────────────────────────────────────────

_venv_site = ROOT / "engine" / ".venv" / "lib"
_site_pkgs = next(_venv_site.glob("python*/site-packages"), None)
if _site_pkgs and str(_site_pkgs) not in sys.path:
    sys.path.insert(0, str(_site_pkgs))

try:
    from openquant import openquant as _oq
    _rust_scan_pair = _oq.scan_pair
    _rust_compute_z = _oq.compute_z
    _rust_decide_exit = _oq.decide_exit
    compute_priority_score = _oq.compute_priority_score
    expected_return_per_dollar_per_day = _oq.expected_return_per_dollar_per_day
    compute_max_hold_days = _oq.compute_max_hold_days
    compute_remaining_per_day = _oq.compute_remaining_per_day
    should_rotate = _oq.should_rotate
    compute_capital_metrics = _oq.compute_capital_metrics
except ImportError as _e:
    raise ImportError(
        "openquant pybridge not found. Run: cd engine && .venv/bin/maturin develop --release"
    ) from _e

# ── Config — single source of truth ──────────────────────────────────────────

TOTAL_CAPITAL = 10_000
MIN_TRADE_CAPITAL = 200
MAX_PER_TRADE_FRAC = 0.25

# Quality gates (Rust enforces R², ADF, HL, beta stability internally;
# these are for the Python-side entry gate that adds spread_std + earnings)
MIN_R2_ENTRY = 0.70
MIN_HL_ENTRY = 2.0
MAX_HL_ENTRY = 5.0
MIN_ADF_ENTRY = -2.5
MIN_BETA = 0.1
MIN_SPREAD_STD = 0.005
BETA_STABILITY_THRESHOLD = 0.30
COST_BPS = 5

# Hold / exit
HOLD_MULTIPLIER = 2.5
MAX_HOLD_CAP = 10
EXIT_Z_DEFAULT = 0.2
EXIT_DECAY_FLOOR = 0.3

# Stability / win rate
STABILITY_LOOKBACK = 10
MAX_REJECT_DAYS = 5
MIN_WIN_RATE = 0.40

# Rotation
ROTATION_COST_PER_DAY = 0.001
MAX_ROTATIONS_PER_DAY = 2

FORMATION_DAYS = 90  # kept for backtest loop start index


# ── ScanResult dataclass ─────────────────────────────────────────────────────

@dataclass
class ScanResult:
    """Result of scanning a pair — returned by scan_pair()."""
    alpha: float
    beta: float
    r2: float
    adf_stat: float
    half_life: float
    spread_mean: float
    spread_std: float
    score: float
    passed: bool


# ── Core functions (delegate to Rust) ─────────────────────────────────────────

def scan_pair(leg_a, leg_b, prices_a, prices_b, day_idx=None):
    """Validate a pair using the Rust pair-picker pipeline.

    Args:
        leg_a, leg_b: symbol names
        prices_a, prices_b: full price arrays (daily closes)
        day_idx: if given, use prices up to this index (for backtest)

    Returns ScanResult or None if pair fails Rust validation.
    """
    pa = prices_a[:day_idx + 1] if day_idx is not None else prices_a
    pb = prices_b[:day_idx + 1] if day_idx is not None else prices_b

    result = _rust_scan_pair(leg_a, leg_b, pa, pb)
    if result is None:
        return None

    if not result.get("passed", False):
        reasons = result.get("rejection_reasons", [])
        if reasons:
            logger.debug(f"scan_pair {leg_a}/{leg_b} REJECTED by Rust: {'; '.join(reasons)}")
        return None

    # Rust returns a dict — unpack into ScanResult
    alpha = result.get("alpha")
    beta = result.get("beta")
    spread_mean = result.get("spread_mean")
    spread_std = result.get("spread_std")

    half_life = result.get("half_life")

    if alpha is None or beta is None or spread_mean is None or spread_std is None or half_life is None:
        return None

    return ScanResult(
        alpha=alpha,
        beta=beta,
        r2=result.get("r2", 0.0),
        adf_stat=result.get("adf_stat", 0.0),
        half_life=half_life,
        spread_mean=spread_mean,
        spread_std=spread_std,
        score=result.get("score", 0.0),
        passed=result.get("passed", False),
    )


def compute_z(params, price_a, price_b):
    """Compute z-score using Rust. Returns 0.0 if Rust returns None."""
    result = _rust_compute_z(price_a, price_b, params.alpha, params.beta,
                              params.spread_mean, params.spread_std)
    if result is not None:
        return result
    return 0.0


def compute_frozen_z(price_a, price_b, alpha, beta, spread_mean, spread_std):
    """Compute z-score using entry-time parameters (frozen stats). Returns float or None."""
    result = _rust_compute_z(price_a, price_b, alpha, beta, spread_mean, spread_std)
    return result


# ── Earnings calendar ─────────────────────────────────────────────────────────

def load_earnings_calendar():
    """Load earnings calendar from JSON file."""
    path = ROOT / "data" / "earnings_calendar.json"
    if not path.exists():
        return None
    with open(path) as f:
        return json.load(f)


def is_near_earnings(symbol, day_idx, total_bars, earnings_cal, blackout=5):
    """Check if a day index is within ±blackout days of an earnings date."""
    dates = earnings_cal.get(symbol, [])
    if not dates:
        return False
    from zoneinfo import ZoneInfo
    end_date = datetime.now(ZoneInfo("US/Eastern")).replace(tzinfo=None)
    cal_days_back = int(total_bars * 365 / 252)
    from datetime import timedelta
    start_date = end_date - timedelta(days=cal_days_back)
    approx_date = start_date + timedelta(days=int(day_idx * 365 / 252))
    for earn_date_str in dates:
        try:
            earn_date = datetime.strptime(earn_date_str, "%Y-%m-%d")
            diff = abs((approx_date - earn_date).days)
            trading_diff = int(diff * 252 / 365)
            if trading_diff <= blackout:
                return True
        except ValueError:
            continue
    return False


# ── Quality gates (Python-side, on top of Rust validation) ────────────────────

def check_quality_gate(params):
    """Check thresholds that Rust may not enforce at our desired level.
    Returns (ok, [reasons])."""
    reasons = []
    if params.r2 < MIN_R2_ENTRY:
        reasons.append(f"r2={params.r2:.3f}")
    if params.half_life < MIN_HL_ENTRY:
        reasons.append(f"hl={params.half_life:.1f}<{MIN_HL_ENTRY}")
    if params.half_life > MAX_HL_ENTRY:
        reasons.append(f"hl={params.half_life:.1f}")
    if params.adf_stat > MIN_ADF_ENTRY:
        reasons.append(f"adf={params.adf_stat:.2f}")
    if params.beta < MIN_BETA:
        reasons.append(f"beta={params.beta:.3f}")
    if params.spread_std < MIN_SPREAD_STD:
        reasons.append(f"spread_std={params.spread_std:.4f}")
    return len(reasons) == 0, reasons


def check_beta_drift(beta_now, beta_prev):
    """Check if beta drifted too much between consecutive daily scans. Returns (ok, change_pct).
    Note: this is day-over-day drift, NOT the rolling-CV beta stability from Rust's beta_stability.rs."""
    if beta_prev is None or beta_prev <= 0:
        return True, 0.0
    change = abs(beta_now - beta_prev) / beta_prev
    return change <= BETA_STABILITY_THRESHOLD, change


def check_stability(leg_a, leg_b, prices, total_bars):
    """Check if pair passed scan_pair on enough recent days."""
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
    """Compute historical win rate for pair+direction.
    direction: 1=LONG (z<0), -1=SHORT (z>0)
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

        if direction == 1 and z >= -entry_z:
            continue
        if direction == -1 and z <= entry_z:
            continue

        reverted = False
        max_hold = compute_max_hold_days(params.half_life, hold_multiplier=HOLD_MULTIPLIER, max_hold_cap=MAX_HOLD_CAP)
        for fwd in range(1, min(max_hold + 1, total_bars - d)):
            fz = compute_frozen_z(prices[leg_a][d + fwd], prices[leg_b][d + fwd],
                                   params.alpha, params.beta,
                                   params.spread_mean, params.spread_std)
            if fz is not None and abs(fz) < exit_z:
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
    """Run all quality gates. Returns (ok, reject_reason)."""
    ok, reasons = check_quality_gate(params)
    if not ok:
        return False, f"quality_gate: {' '.join(reasons)}"

    if earnings_cal:
        if is_near_earnings(leg_a, total_bars - 1, total_bars, earnings_cal):
            return False, f"{leg_a} near earnings"
        if is_near_earnings(leg_b, total_bars - 1, total_bars, earnings_cal):
            return False, f"{leg_b} near earnings"

    stable, pass_count, reject_count = check_stability(leg_a, leg_b, prices, total_bars)
    if not stable:
        # If the pair passes Rust validation TODAY but has no history (cold start after
        # threshold change), allow it through. Rust's own beta stability + structural
        # break + ADF is rigorous enough. See research issue #202.
        if pass_count == 0:
            logger.debug(f"  {leg_a}/{leg_b}: stability cold-start (0 history) — allowing (Rust-validated)")
        else:
            return False, f"unstable: scan_pair rejected {reject_count}/{STABILITY_LOOKBACK} days"

    direction = 1 if z < 0 else -1
    dir_label = "LONG" if direction == 1 else "SHORT"
    wr, wins, losses = compute_win_rate(leg_a, leg_b, direction, prices, total_bars, entry_z=entry_z, exit_z=exit_z)
    if wr is None:
        # Insufficient historical data — Rust validation is strict enough to allow entry.
        # Log it but don't block. See research issue #202.
        logger.debug(f"  {leg_a}/{leg_b}: no historical {dir_label} entries — allowing (Rust-validated)")
    elif wr < MIN_WIN_RATE:
        return False, f"{dir_label} win_rate={wr:.0%} ({wins}W/{losses}L) < {MIN_WIN_RATE:.0%}"

    return True, None


# ── Exit / scoring ────────────────────────────────────────────────────────────

def decide_exit(frozen_z, days_held, max_hold, exit_z=EXIT_Z_DEFAULT, use_decay=True):
    """Decide whether to exit. Delegates to Rust. Returns reason string or None."""
    return _rust_decide_exit(frozen_z, days_held, max_hold,
                              exit_z=exit_z, decay_floor=EXIT_DECAY_FLOOR, use_decay=use_decay)


LN2 = 0.6931471805599453  # math.log(2), exact to float64 precision

def score_signal(z, half_life, spread_std):
    """Compute priority score and expected return per dollar per day.
    Returns (priority, erpdd, max_hold, kappa).
    Note: kappa = ln(2)/half_life is a unit conversion, not statistical math.
    TODO: Rust should accept half_life directly."""
    kappa = LN2 / half_life
    priority = compute_priority_score(z, kappa, spread_std)
    max_hold = compute_max_hold_days(half_life, hold_multiplier=HOLD_MULTIPLIER, max_hold_cap=MAX_HOLD_CAP)
    erpdd = expected_return_per_dollar_per_day(z, spread_std, kappa, float(max_hold))
    return priority, erpdd, max_hold, kappa
