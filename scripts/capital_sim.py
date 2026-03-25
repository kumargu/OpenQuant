#!/usr/bin/env python3
"""
Capital-aware pairs simulation.

Instead of fixed slots, manages a capital pool:
- $10K total budget
- Each trade allocates capital_per_leg × 2 from the pool
- When a trade closes, capital returns to pool
- Next day, pool is re-allocated to best available signals
- Capital per leg must be >= max stock price (to buy at least 1 share)
- Same pair CAN re-enter immediately if it signals again

Rotation (Leung & Li 2015):
- Per-pair max_hold = ceil(hold_multiplier * half_life), capped at MAX_HOLD_CAP days.
- Each day, active trades are scored for remaining edge vs best queued signal.
- Profitable trades with low remaining edge are evicted to free capital for better signals.
- Max MAX_ROTATIONS_PER_DAY rotations per day to limit churn.

All rotation math (remaining_per_day, should_rotate, compute_max_hold_days) lives in
Rust and is called via the openquant pybridge.  This file is orchestration only.
"""

import argparse
import json
import logging
import sys
from dataclasses import dataclass
from datetime import datetime
from pathlib import Path

root = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(root / "scripts"))

# Rust bridge — all math lives here.
# Paths: engine/.venv is the canonical venv for the project.
_venv_site = root / "engine" / ".venv" / "lib"
_site_pkgs = next(_venv_site.glob("python*/site-packages"), None)
if _site_pkgs and str(_site_pkgs) not in sys.path:
    sys.path.insert(0, str(_site_pkgs))

try:
    from openquant import openquant as _oq
    _compute_priority_score = _oq.compute_priority_score
    _expected_return_per_dollar_per_day = _oq.expected_return_per_dollar_per_day
    _compute_max_hold_days = _oq.compute_max_hold_days
    _compute_remaining_per_day = _oq.compute_remaining_per_day
    _should_rotate = _oq.should_rotate
    _compute_capital_metrics = _oq.compute_capital_metrics
except ImportError as _e:
    raise ImportError(
        "openquant pybridge not found. Run: cd engine && maturin develop --release"
    ) from _e

from daily_walkforward_dashboard import (
    scan_pair, compute_z, ols_simple, PairParams,
    FORMATION_DAYS, MIN_R2, COST_BPS,
    load_earnings_calendar, is_near_earnings,
)

# ── Logger ────────────────────────────────────────────────────────────────────

LOG_FILE = root / "data" / "journal" / "capital_sim.log"
logger = logging.getLogger("capital_sim")
logger.setLevel(logging.DEBUG)
logger.propagate = False
_fh = logging.FileHandler(LOG_FILE, mode="w", encoding="utf-8")
_fh.setLevel(logging.DEBUG)
_fh.setFormatter(logging.Formatter("%(asctime)s %(message)s", datefmt="%Y-%m-%d %H:%M:%S"))
logger.addHandler(_fh)
_sh = logging.StreamHandler(sys.stdout)
_sh.setLevel(logging.INFO)
_sh.setFormatter(logging.Formatter("%(message)s"))
logger.addHandler(_sh)

# ── Config ────────────────────────────────────────────────────────────────────

TOTAL_CAPITAL = 10_000
MIN_TRADE_CAPITAL = 200     # minimum per leg to bother trading
MAX_PER_TRADE_FRAC = 0.25   # max 25% of total in one trade
MIN_R2_ENTRY = 0.70
MAX_HL_ENTRY = 5.0
MIN_ADF_ENTRY = -2.5

# Dynamic max_hold parameters — passed to Rust compute_max_hold_days.
# OU theory: after hold_multiplier half-lives, expected reversion is 1 - 2^{-multiplier}.
# 2.5x gives ~82% reversion; 2.0x gives ~75%.  Default 2.5x matches pair-picker default.
HOLD_MULTIPLIER = 2.5
MAX_HOLD_CAP = 10           # sweep shows 7-10d tie at $13/d; longer = more reversions

# Rotation parameters — passed to Rust should_rotate.
# cost_per_day: one-way 5 bps * 2 legs / expected_hold_days (≈ 2.5d) ≈ 0.001/day
ROTATION_COST_PER_DAY = 0.001
MAX_ROTATIONS_PER_DAY = 2   # cap churn to 2 rotations per day


@dataclass
class Trade:
    leg_a: str
    leg_b: str
    params: PairParams
    direction: int
    entry_day: int
    entry_pa: float
    entry_pb: float
    entry_z: float
    capital_per_leg: float
    max_hold: int
    exit_z: float
    priority_score: float = 0.0  # score at entry time, for observability
    trade_id: str = ""  # unique trace ID: "LEG_A/LEG_B:entry_day" — links ENTER→HOLD→EXIT logs

    def pair_id(self):
        return f"{self.leg_a}/{self.leg_b}"


@dataclass
class ClosedTrade:
    leg_a: str
    leg_b: str
    direction: int
    entry_day: int
    exit_day: int
    pnl: float
    capital_used: float  # per leg
    reason: str


def compute_pnl(trade, exit_pa, exit_pb):
    """Beta-neutral P&L consistent with the beta-weighted spread z-score.

    The z-score is computed on spread = ln(A) - alpha - beta * ln(B), so the
    mean-reversion signal captures divergence in beta-weighted terms.  The P&L
    must use the same weighting: A leg gets $capital, B leg gets $capital * beta.

    Sizing consequence: A leg = $capital_per_leg, B leg = $capital_per_leg * |beta|.
    For low-beta pairs (e.g. NVDA/AMD beta=0.19) the B leg is intentionally small
    — that is correct.  Using equal-dollar legs (dollar-neutral) over-hedges B
    relative to what the signal predicts, which adds noise and loses P&L.

    Cite: Avellaneda & Lee (2010), "Statistical Arbitrage in the U.S. Equities Market",
    equation for spread construction and corresponding position sizing.
    """
    c = trade.capital_per_leg
    beta = trade.params.beta
    ret_a = (exit_pa - trade.entry_pa) / trade.entry_pa
    ret_b = (exit_pb - trade.entry_pb) / trade.entry_pb
    if trade.direction == 1:  # long A, short B
        pnl = c * ret_a - c * beta * ret_b
    else:  # short A, long B
        pnl = -c * ret_a + c * beta * ret_b
    cost = 2 * c * COST_BPS / 10_000
    return pnl - cost


def run_capital_sim(
    prices,
    pair_configs,
    total_capital=TOTAL_CAPITAL,
    scoring_mode="priority",
    rotation_enabled=False,
):
    """Run simulation with capital pool management.

    Args:
        scoring_mode: "priority"  — sort by |z|×sqrt(kappa)/sigma (Avellaneda-Lee)
                      "z_abs"     — sort by |z| only (legacy baseline)
        rotation_enabled: if True, evict stale profitable trades when a better
                          signal is available (Leung & Li 2015 opportunity-cost rotation).
    """
    total_bars = min(len(v) for v in prices.values() if len(v) >= 200)
    earnings_cal = load_earnings_calendar()

    available_capital = total_capital
    open_trades: list[Trade] = []
    closed_trades: list[ClosedTrade] = []
    daily_log = []
    last_beta = {}

    # Build candidate list from portfolio config
    candidates = [(p['leg_a'], p['leg_b'], p) for p in pair_configs]

    logger.info("=" * 70)
    logger.info(f"CAPITAL SIMULATION — {datetime.now().strftime('%Y-%m-%d %H:%M:%S')}")
    logger.info(f"Total capital: ${total_capital:,}")
    logger.info(f"Pairs: {len(candidates)}")
    logger.info(f"Rotation: {'ON' if rotation_enabled else 'OFF'} "
                f"(max {MAX_ROTATIONS_PER_DAY}/day, cost={ROTATION_COST_PER_DAY:.4f}/day)")
    logger.info(f"Min trade: ${MIN_TRADE_CAPITAL}/leg, Max per trade: {MAX_PER_TRADE_FRAC*100:.0f}%")
    logger.info("=" * 70)

    for day in range(FORMATION_DAYS, total_bars):
        day_pnl = 0.0
        day_entries = 0
        day_exits = 0
        day_rotations = 0

        # ── STEP 1: Check natural exits + log per-bar holding state ──
        to_close = []
        for i, trade in enumerate(open_trades):
            pa = prices[trade.leg_a][day]
            pb = prices[trade.leg_b][day]
            z = compute_z(trade.params, pa, pb)
            bars_held = day - trade.entry_day
            pnl_unrealized = compute_pnl(trade, pa, pb)
            ret_unrealized = pnl_unrealized / (trade.capital_per_leg * 2) * 100 if trade.capital_per_leg > 0 else 0

            # Time-decay exit threshold
            decay = min(bars_held / trade.max_hold, 1.0)
            floor = min(0.3, trade.exit_z)
            eff_exit = trade.exit_z - (trade.exit_z - floor) * decay

            # Per-bar holding log — the key diagnostic for understanding trade evolution
            logger.debug(f"[Day {day:>3}] HOLD {trade.pair_id():<12} "
                         f"[{trade.trade_id}] "
                         f"{'L' if trade.direction==1 else 'S'} "
                         f"held={bars_held}/{trade.max_hold}d "
                         f"z_now={z:+.3f} z_entry={trade.entry_z:+.3f} "
                         f"exit_thresh={eff_exit:.3f} "
                         f"unreal=${pnl_unrealized:+.2f} ({ret_unrealized:+.2f}%) "
                         f"prio={trade.priority_score:.1f} "
                         f"${trade.capital_per_leg:.0f}/leg")

            reason = None
            if bars_held >= trade.max_hold:
                reason = "max_hold"
            elif trade.direction == 1 and z > -eff_exit:
                reason = "reversion"
            elif trade.direction == -1 and z < eff_exit:
                reason = "reversion"

            if reason:
                pnl = compute_pnl(trade, pa, pb)
                ret_pct = pnl / (trade.capital_per_leg * 2) * 100 if trade.capital_per_leg > 0 else 0
                available_capital += trade.capital_per_leg * 2
                day_pnl += pnl
                day_exits += 1

                logger.info(f"[Day {day:>3}] EXIT:{reason:<8} {trade.pair_id()} "
                            f"[{trade.trade_id}] "
                            f"{'L' if trade.direction==1 else 'S'} {bars_held}d "
                            f"${pnl:>+7.2f} ({ret_pct:+.2f}%) "
                            f"z_now={z:+.3f} z_entry={trade.entry_z:+.3f} | "
                            f"freed ${trade.capital_per_leg*2:.0f} | "
                            f"pool=${available_capital:.0f}")

                closed_trades.append(ClosedTrade(
                    leg_a=trade.leg_a, leg_b=trade.leg_b,
                    direction=trade.direction,
                    entry_day=trade.entry_day, exit_day=day,
                    pnl=pnl, capital_used=trade.capital_per_leg, reason=reason,
                ))
                to_close.append(i)

        for i in sorted(to_close, reverse=True):
            open_trades.pop(i)

        # ── STEP 2: Scan for queued signals ──
        # Always scan, even if capital is unavailable — we need best_queued_per_day
        # for the rotation decision in Step 2.5.
        held_pairs = {(t.leg_a, t.leg_b) for t in open_trades}
        signals = []

        for leg_a, leg_b, pcfg in candidates:
            if leg_a not in prices or leg_b not in prices:
                continue
            if (leg_a, leg_b) in held_pairs:
                continue

            # Earnings check
            if earnings_cal and (
                is_near_earnings(leg_a, day, total_bars, earnings_cal) or
                is_near_earnings(leg_b, day, total_bars, earnings_cal)
            ):
                continue

            params = scan_pair(leg_a, leg_b, prices[leg_a], prices[leg_b], day)
            if params is None or params.beta < 0.1:
                continue

            # Beta stability
            pair_key = (leg_a, leg_b)
            if pair_key in last_beta:
                prev = last_beta[pair_key]
                if prev > 0 and abs(params.beta - prev) / prev > 0.30:
                    logger.debug(f"[Day {day:>3}] REJECT {leg_a}/{leg_b} beta_unstable "
                                 f"beta={params.beta:.3f} prev={prev:.3f} "
                                 f"change={abs(params.beta-prev)/prev*100:.0f}%")
                    continue
            last_beta[pair_key] = params.beta

            # Quality gate
            if params.r2 < MIN_R2_ENTRY or params.half_life > MAX_HL_ENTRY or params.adf_stat > MIN_ADF_ENTRY:
                reject_reasons = []
                if params.r2 < MIN_R2_ENTRY:
                    reject_reasons.append(f"r2={params.r2:.3f}")
                if params.half_life > MAX_HL_ENTRY:
                    reject_reasons.append(f"hl={params.half_life:.1f}")
                if params.adf_stat > MIN_ADF_ENTRY:
                    reject_reasons.append(f"adf={params.adf_stat:.2f}")
                logger.debug(f"[Day {day:>3}] REJECT {leg_a}/{leg_b} quality_gate "
                             f"{' '.join(reject_reasons)}")
                continue

            pa = prices[leg_a][day]
            pb = prices[leg_b][day]
            z = compute_z(params, pa, pb)

            entry_z = pcfg.get('entry_z', 1.0)
            if abs(z) >= entry_z + 1.5:
                logger.debug(f"[Day {day:>3}] REJECT {leg_a}/{leg_b} z_cap "
                             f"z={z:+.3f} cap={entry_z+1.5:.1f} "
                             f"(z too extreme — possible structural break, not reversion)")
            elif abs(z) > entry_z:
                # Capital needed: must afford at least 1 share of each
                min_capital_leg = max(pa, pb * abs(params.beta), MIN_TRADE_CAPITAL)
                # Desired capital from config, but cap at pool fraction
                desired_capital = pcfg.get('capital_per_leg', 500)
                max_capital = min(
                    total_capital * MAX_PER_TRADE_FRAC,
                    available_capital / 2,  # per leg
                )
                actual_capital = max(min(desired_capital, max_capital), min_capital_leg)

                if actual_capital * 2 > available_capital:
                    logger.debug(f"[Day {day:>3}] REJECT {leg_a}/{leg_b} no_capital "
                                 f"z={z:+.3f} need=${actual_capital*2:.0f} pool=${available_capital:.0f}")
                else:
                    # Priority score: |z| × sqrt(kappa) / sigma (Rust-computed)
                    kappa = 0.693147 / params.half_life  # ln(2) / HL
                    prio = _compute_priority_score(z, kappa, params.spread_std)
                    # Expected return/$/day for this queued signal (Rust-computed)
                    # Dynamic max_hold from Rust: ceil(HOLD_MULTIPLIER * HL), capped at MAX_HOLD_CAP
                    dyn_max_hold = _compute_max_hold_days(
                        params.half_life,
                        hold_multiplier=HOLD_MULTIPLIER,
                        max_hold_cap=MAX_HOLD_CAP,
                    )
                    erpdd = _expected_return_per_dollar_per_day(
                        z, params.spread_std, kappa, float(dyn_max_hold)
                    )
                    signals.append((leg_a, leg_b, params, z, pa, pb, pcfg,
                                    actual_capital, prio, erpdd, dyn_max_hold))

        # Sort by priority score or |z|
        if scoring_mode == "priority":
            signals.sort(key=lambda x: x[8], reverse=True)
        else:
            signals.sort(key=lambda x: abs(x[3]), reverse=True)

        # Daily scan funnel summary — diagnose why signals are thin
        if signals:
            top = signals[0]
            logger.debug(f"[Day {day:>3}] SCAN_FUNNEL candidates={len(candidates)} "
                         f"valid_signals={len(signals)} open={len(open_trades)} "
                         f"pool=${available_capital:.0f} "
                         f"top={top[0]}/{top[1]} z={top[3]:+.3f} prio={top[8]:.4f}")
        else:
            logger.debug(f"[Day {day:>3}] SCAN_FUNNEL candidates={len(candidates)} "
                         f"valid_signals=0 open={len(open_trades)} pool=${available_capital:.0f} "
                         f"(no actionable signals today)")

        # ── STEP 2.5: Opportunity-cost rotation (Leung & Li 2015) ──
        # Only if rotation is enabled and there is a better signal waiting.
        if rotation_enabled and signals and open_trades:
            best_queued_erpdd = signals[0][9]  # expected_return_per_dollar_per_day of top signal

            # Score each active trade for remaining edge
            rotation_candidates = []
            for i, trade in enumerate(open_trades):
                pa = prices[trade.leg_a][day]
                pb = prices[trade.leg_b][day]
                pnl = compute_pnl(trade, pa, pb)
                unrealized_return = pnl / (trade.capital_per_leg * 2) if trade.capital_per_leg > 0 else 0.0
                bars_held = day - trade.entry_day

                # Rust-computed remaining expected return/$/day
                remaining = _compute_remaining_per_day(
                    abs(trade.entry_z),
                    trade.params.spread_std,
                    unrealized_return,
                    bars_held,
                    trade.max_hold,
                )

                # Rust-computed rotation decision
                rotate = _should_rotate(
                    unrealized_return,
                    remaining,
                    best_queued_erpdd,
                    ROTATION_COST_PER_DAY,
                )

                # Log the rotation reasoning for every active trade
                best_sig = signals[0]
                logger.debug(f"[Day {day:>3}] ROT_CHECK {trade.pair_id():<12} "
                             f"unreal_ret={unrealized_return:+.4f} "
                             f"remaining/d={remaining:.6f} "
                             f"best_queued={best_sig[0]}/{best_sig[1]} erpdd={best_queued_erpdd:.6f} "
                             f"→ {'ROTATE' if rotate else 'KEEP'}"
                             f"{' (losing)' if unrealized_return <= 0 else ''}"
                             f"{' (edge>opp)' if remaining >= best_queued_erpdd else ''}")

                if rotate:
                    rotation_candidates.append((i, trade, pnl, unrealized_return, remaining))

            # Sort by most stale first (lowest remaining_per_day)
            rotation_candidates.sort(key=lambda x: x[4])

            for i, trade, pnl, unrealized_ret, remaining in rotation_candidates:
                if day_rotations >= MAX_ROTATIONS_PER_DAY:
                    break
                # Evict this trade
                available_capital += trade.capital_per_leg * 2
                day_pnl += pnl
                day_exits += 1
                day_rotations += 1

                kappa = 0.693147 / trade.params.half_life
                active_erpdd = _expected_return_per_dollar_per_day(
                    abs(trade.entry_z), trade.params.spread_std, kappa, float(trade.max_hold)
                )

                logger.info(
                    f"[Day {day:>3}] ROTATE       {trade.pair_id()} "
                    f"[{trade.trade_id}] "
                    f"{'L' if trade.direction==1 else 'S'} "
                    f"held={day - trade.entry_day}d/${trade.max_hold}d "
                    f"${pnl:>+7.2f} unrealized={unrealized_ret:+.4f} "
                    f"remaining/day={remaining:.6f} "
                    f"active_erpdd={active_erpdd:.6f} "
                    f"best_queued_erpdd={best_queued_erpdd:.6f} | "
                    f"freed ${trade.capital_per_leg*2:.0f} | pool=${available_capital:.0f}"
                )

                closed_trades.append(ClosedTrade(
                    leg_a=trade.leg_a, leg_b=trade.leg_b,
                    direction=trade.direction,
                    entry_day=trade.entry_day, exit_day=day,
                    pnl=pnl, capital_used=trade.capital_per_leg, reason="rotation",
                ))

            # Remove rotated trades (indices may have shifted due to step 1, collect fresh set)
            rotated_pair_ids = {(rc[1].leg_a, rc[1].leg_b) for rc in rotation_candidates[:MAX_ROTATIONS_PER_DAY]}
            open_trades = [t for t in open_trades if (t.leg_a, t.leg_b) not in rotated_pair_ids]
            # Refresh held_pairs after rotation
            held_pairs = {(t.leg_a, t.leg_b) for t in open_trades}
            # Re-filter signals to exclude pairs we still hold
            signals = [s for s in signals if (s[0], s[1]) not in held_pairs]
            # Re-cap capital per leg for remaining signals
            signals = [
                (leg_a, leg_b, params, z, pa, pb, pcfg,
                 max(min(pcfg.get('capital_per_leg', 500),
                         min(total_capital * MAX_PER_TRADE_FRAC, available_capital / 2)),
                     max(pa, pb * abs(params.beta), MIN_TRADE_CAPITAL)),
                 prio, erpdd, dyn_max_hold)
                for leg_a, leg_b, params, z, pa, pb, pcfg, _, prio, erpdd, dyn_max_hold in signals
                if max(min(pcfg.get('capital_per_leg', 500),
                           min(total_capital * MAX_PER_TRADE_FRAC, available_capital / 2)),
                       max(pa, pb * abs(params.beta), MIN_TRADE_CAPITAL)) * 2 <= available_capital
            ]

        # ── STEP 3: Enter new trades with available capital ──
        if available_capital >= MIN_TRADE_CAPITAL * 2:
            for leg_a, leg_b, params, z, pa, pb, pcfg, capital, prio, erpdd, dyn_max_hold in signals:
                if available_capital < capital * 2:
                    logger.debug(f"[Day {day:>3}] SKIP_CAPITAL  {leg_a}/{leg_b} "
                                 f"need=${capital*2:.0f} pool=${available_capital:.0f} "
                                 f"(capital exhausted — valid signal left on table)")
                    break
                direction = 1 if z < -pcfg.get('entry_z', 1.0) else -1
                tid = f"{leg_a}/{leg_b}:{day}"
                trade = Trade(
                    leg_a=leg_a, leg_b=leg_b, params=params,
                    direction=direction, entry_day=day,
                    entry_pa=pa, entry_pb=pb, entry_z=z,
                    capital_per_leg=capital,
                    max_hold=dyn_max_hold,  # dynamic: ceil(HOLD_MULTIPLIER * HL), cap=MAX_HOLD_CAP
                    exit_z=pcfg.get('exit_z', 0.3),
                    priority_score=prio,
                    trade_id=tid,
                )
                open_trades.append(trade)
                available_capital -= capital * 2
                day_entries += 1

                logger.info(f"[Day {day:>3}] ENTER        {trade.pair_id()} "
                            f"[{tid}] "
                            f"{'L' if direction==1 else 'S'} z={z:+.2f} "
                            f"prio={prio:.4f} erpdd={erpdd:.6f} "
                            f"max_hold={dyn_max_hold}d "
                            f"${capital:.0f}/leg | alloc ${capital*2:.0f} | "
                            f"pool=${available_capital:.0f}")

        # ── Daily summary with position snapshot ──
        deployed = sum(t.capital_per_leg * 2 for t in open_trades)
        util = deployed / total_capital * 100

        # Snapshot of each open position
        positions = []
        for t in open_trades:
            pa = prices[t.leg_a][day]
            pb = prices[t.leg_b][day]
            unrealized = compute_pnl(t, pa, pb)
            positions.append({
                'pair': t.pair_id(),
                'dir': 'L' if t.direction == 1 else 'S',
                'capital': t.capital_per_leg,
                'held': day - t.entry_day,
                'max_hold': t.max_hold,
                'unrealized': round(unrealized, 2),
                'entry_z': round(t.entry_z, 2),
            })

        daily_log.append({
            'day': day, 'pnl': round(day_pnl, 2),
            'open': len(open_trades), 'entries': day_entries, 'exits': day_exits,
            'rotations': day_rotations,
            'deployed': round(deployed, 0), 'available': round(available_capital, 0),
            'util': round(util, 1),
            'positions': positions,
        })

        # Daily snapshot — always log (not just on entry/exit days)
        pos_summary = ", ".join(
            f"{t.pair_id()}({'L' if t.direction==1 else 'S'} {day-t.entry_day}d ${compute_pnl(t, prices[t.leg_a][day], prices[t.leg_b][day]):+.0f})"
            for t in open_trades
        ) if open_trades else "IDLE"
        logger.debug(f"[Day {day:>3}] DAILY: {len(open_trades)} pos, "
                     f"deployed=${deployed:.0f}/{total_capital} ({util:.0f}%), "
                     f"pool=${available_capital:.0f}, "
                     f"entries={day_entries} exits={day_exits} rot={day_rotations} "
                     f"pnl=${day_pnl:+.2f} | {pos_summary}")

    # Force close remaining
    for trade in open_trades:
        pa = prices[trade.leg_a][total_bars - 1]
        pb = prices[trade.leg_b][total_bars - 1]
        pnl = compute_pnl(trade, pa, pb)
        closed_trades.append(ClosedTrade(
            leg_a=trade.leg_a, leg_b=trade.leg_b,
            direction=trade.direction,
            entry_day=trade.entry_day, exit_day=total_bars - 1,
            pnl=pnl, capital_used=trade.capital_per_leg, reason="eod_force",
        ))

    return closed_trades, daily_log


def generate_capital_dashboard(closed_trades, daily_log, output_path, total_capital=TOTAL_CAPITAL):
    """Generate HTML dashboard with capital allocation view."""

    # Prepare chart data
    days = [d['day'] for d in daily_log]
    deployed = [d['deployed'] for d in daily_log]
    available = [d['available'] for d in daily_log]
    util = [d['util'] for d in daily_log]
    cum_pnl = []
    running = 0
    for d in daily_log:
        running += d['pnl']
        cum_pnl.append(round(running, 2))
    daily_pnl = [d['pnl'] for d in daily_log]

    # Position data for the table (last 60 days)
    position_days = []
    for d in daily_log[-60:]:
        if d['positions']:
            position_days.append(d)

    # Per-pair P&L
    pair_stats = {}
    for t in closed_trades:
        k = f"{t.leg_a}/{t.leg_b}"
        if k not in pair_stats:
            pair_stats[k] = {'trades': 0, 'pnl': 0.0, 'wins': 0, 'capital': 0}
        pair_stats[k]['trades'] += 1
        pair_stats[k]['pnl'] += t.pnl
        pair_stats[k]['capital'] = t.capital_used
        if t.pnl > 0:
            pair_stats[k]['wins'] += 1

    all_pnl = sum(t.pnl for t in closed_trades)
    n_trades = len(closed_trades)
    n_wins = sum(1 for t in closed_trades if t.pnl > 0)

    # Build position detail rows as JSON for the interactive table
    pos_json_data = json.dumps([{
        'day': d['day'],
        'deployed': d['deployed'],
        'available': d['available'],
        'util': d['util'],
        'pnl': d['pnl'],
        'positions': d['positions'],
    } for d in daily_log[-90:]])

    pair_rows = ""
    for pair in sorted(pair_stats, key=lambda p: pair_stats[p]['pnl'], reverse=True):
        s = pair_stats[pair]
        wr = s['wins'] / s['trades'] * 100 if s['trades'] else 0
        color = "#22c55e" if s['pnl'] > 0 else "#ef4444"
        pair_rows += f"""<tr>
          <td style="font-weight:600">{pair}</td>
          <td>{s['trades']}</td><td>{wr:.0f}%</td>
          <td style="color:{color};font-weight:700">${s['pnl']:+,.0f}</td>
          <td>${s['capital']:,.0f}</td></tr>"""

    html = f"""<!DOCTYPE html>
<html lang="en"><head>
<meta charset="UTF-8"><title>Capital Allocation Dashboard</title>
<script src="https://cdn.jsdelivr.net/npm/chart.js@4.4.0/dist/chart.umd.min.js"></script>
<style>
  * {{ margin:0; padding:0; box-sizing:border-box; }}
  body {{ font-family:-apple-system,sans-serif; background:#0f172a; color:#e2e8f0; padding:24px; }}
  h1 {{ font-size:24px; margin-bottom:4px; }}
  h2 {{ font-size:18px; margin:24px 0 12px; color:#94a3b8; }}
  .sub {{ color:#64748b; font-size:13px; margin-bottom:20px; }}
  .grid {{ display:grid; grid-template-columns:repeat(auto-fit,minmax(180px,1fr)); gap:12px; margin-bottom:24px; }}
  .card {{ background:#1e293b; border-radius:10px; padding:16px; border:1px solid #334155; }}
  .card .lbl {{ font-size:11px; text-transform:uppercase; color:#64748b; margin-bottom:2px; }}
  .card .val {{ font-size:24px; font-weight:700; }}
  .green {{ color:#22c55e; }} .red {{ color:#ef4444; }} .blue {{ color:#3b82f6; }} .amber {{ color:#f59e0b; }}
  .chart-box {{ background:#1e293b; border-radius:10px; padding:20px; border:1px solid #334155; margin-bottom:16px; }}
  table {{ width:100%; border-collapse:collapse; font-size:12px; }}
  th {{ text-align:left; padding:6px 8px; border-bottom:2px solid #334155; color:#64748b; font-size:10px; text-transform:uppercase; }}
  td {{ padding:5px 8px; border-bottom:1px solid #1e293b; }}
  tr:hover {{ background:#1e293b; }}
  .tbl-wrap {{ background:#1e293b; border-radius:10px; padding:4px; border:1px solid #334155; overflow-x:auto; max-height:500px; overflow-y:auto; }}
  .two {{ display:grid; grid-template-columns:1fr 1fr; gap:16px; }}
  .pos-row {{ background:#0f172a; padding:8px 12px; border-radius:6px; margin:4px 0; font-size:12px; display:flex; justify-content:space-between; }}
  .day-filter {{ margin-bottom:16px; }}
  .day-filter input {{ background:#1e293b; border:1px solid #334155; color:#e2e8f0; padding:6px 12px; border-radius:6px; font-size:13px; width:120px; }}
  .day-filter label {{ color:#64748b; font-size:12px; margin-right:8px; }}
  @media(max-width:800px){{ .two{{grid-template-columns:1fr;}} }}
</style>
</head><body>
<h1>Capital Allocation Dashboard</h1>
<p class="sub">${total_capital:,} budget · {n_trades} trades · {len(pair_stats)} pairs</p>

<div class="grid">
  <div class="card"><div class="lbl">Total P&L</div><div class="val {'green' if all_pnl>0 else 'red'}">${all_pnl:+,.0f}</div></div>
  <div class="card"><div class="lbl">Win Rate</div><div class="val {'green' if n_wins/max(n_trades,1)>0.55 else 'amber'}">{n_wins/max(n_trades,1)*100:.0f}%</div></div>
  <div class="card"><div class="lbl">Trades</div><div class="val blue">{n_trades}</div></div>
  <div class="card"><div class="lbl">Avg Utilization</div><div class="val amber">{sum(d['util'] for d in daily_log[-60:])/max(len(daily_log[-60:]),1):.0f}%</div></div>
</div>

<div class="two">
  <div class="chart-box"><h2 style="margin-top:0">Capital Deployed vs Available</h2><canvas id="capChart" height="120"></canvas></div>
  <div class="chart-box"><h2 style="margin-top:0">Cumulative P&L</h2><canvas id="pnlChart" height="120"></canvas></div>
</div>

<div class="chart-box"><h2 style="margin-top:0">Utilization %</h2><canvas id="utilChart" height="60"></canvas></div>

<h2>Daily Positions</h2>
<div class="day-filter">
  <label>From day:</label><input type="number" id="dayFrom" value="{max(0, len(daily_log)-30)}" min="0" max="{len(daily_log)}">
  <label>To day:</label><input type="number" id="dayTo" value="{len(daily_log)}" min="0" max="{len(daily_log)}">
  <button onclick="filterDays()" style="background:#3b82f6;color:white;border:none;padding:6px 16px;border-radius:6px;cursor:pointer;margin-left:8px">Filter</button>
</div>
<div id="posTable" class="tbl-wrap"></div>

<h2>Per Pair Summary</h2>
<div class="tbl-wrap">
<table><thead><tr><th>Pair</th><th>Trades</th><th>Win%</th><th>P&L</th><th>Capital/Leg</th></tr></thead>
<tbody>{pair_rows}</tbody></table>
</div>

<script>
Chart.defaults.color='#94a3b8';Chart.defaults.borderColor='#334155';
const days={json.dumps(days[-90:])};
const deployed={json.dumps(deployed[-90:])};
const available={json.dumps(available[-90:])};
const utilData={json.dumps(util[-90:])};
const cumPnl={json.dumps(cum_pnl[-90:])};
const dailyPnl={json.dumps(daily_pnl[-90:])};
const posData={pos_json_data};

new Chart(document.getElementById('capChart'),{{
  type:'line',data:{{labels:days,datasets:[
    {{label:'Deployed',data:deployed,borderColor:'#3b82f6',backgroundColor:'rgba(59,130,246,0.1)',fill:true,tension:0.3,pointRadius:0,borderWidth:2}},
    {{label:'Available',data:available,borderColor:'#22c55e',backgroundColor:'rgba(34,197,94,0.05)',fill:true,tension:0.3,pointRadius:0,borderWidth:1.5}}
  ]}},options:{{responsive:true,plugins:{{legend:{{position:'top',labels:{{boxWidth:12,font:{{size:11}}}}}}}},scales:{{x:{{ticks:{{maxTicksLimit:15,font:{{size:9}}}}}},y:{{min:0,max:{total_capital},grid:{{color:'#1e293b'}}}}}}}}
}});

new Chart(document.getElementById('pnlChart'),{{
  type:'line',data:{{labels:days,datasets:[{{label:'Cumulative P&L',data:cumPnl,borderColor:'#f59e0b',tension:0.3,pointRadius:0,borderWidth:2.5}}]}},
  options:{{responsive:true,plugins:{{legend:{{display:false}}}},scales:{{x:{{ticks:{{maxTicksLimit:15,font:{{size:9}}}}}},y:{{grid:{{color:'#1e293b'}}}}}}}}
}});

new Chart(document.getElementById('utilChart'),{{
  type:'bar',data:{{labels:days,datasets:[{{data:utilData,backgroundColor:utilData.map(v=>v>60?'rgba(34,197,94,0.6)':v>30?'rgba(245,158,11,0.6)':'rgba(239,68,68,0.4)'),borderWidth:0}}]}},
  options:{{responsive:true,plugins:{{legend:{{display:false}}}},scales:{{x:{{display:false}},y:{{min:0,max:100,grid:{{color:'#1e293b'}}}}}}}}
}});

function filterDays(){{
  const from=parseInt(document.getElementById('dayFrom').value);
  const to=parseInt(document.getElementById('dayTo').value);
  const filtered=posData.filter(d=>d.day>=from&&d.day<=to);
  let html='<table><thead><tr><th>Day</th><th>Deployed</th><th>Util%</th><th>P&L</th><th>Positions</th></tr></thead><tbody>';
  filtered.forEach(d=>{{
    const posStr=d.positions.map(p=>
      `<span style="display:inline-block;background:#0f172a;padding:2px 8px;border-radius:4px;margin:2px;font-size:11px">`+
      `${{p.pair}} ${{p.dir}} ${{p.held}}/${{p.max_hold}}d $${{p.capital}}/leg <span style="color:${{p.unrealized>=0?'#22c55e':'#ef4444'}}">${{p.unrealized>=0?'+':''}}${{p.unrealized}}</span></span>`
    ).join('');
    const pnlColor=d.pnl>0?'#22c55e':d.pnl<0?'#ef4444':'#64748b';
    html+=`<tr><td>${{d.day}}</td><td>$${{d.deployed.toLocaleString()}}</td><td>${{d.util}}%</td><td style="color:${{pnlColor}}">${{d.pnl>=0?'+':''}}$${{d.pnl.toFixed(2)}}</td><td>${{posStr||'<span style="color:#64748b">idle</span>'}}</td></tr>`;
  }});
  html+='</tbody></table>';
  document.getElementById('posTable').innerHTML=html;
}}
filterDays();
</script>
</body></html>"""

    Path(output_path).write_text(html)
    logger.info(f"Dashboard: file://{output_path}")


def summarize(closed, daily, label="", total_capital=TOTAL_CAPITAL):
    """Return a dict of summary stats for a simulation run.

    All capital utilization math (RoEC, Utilization, RoCC) is computed in
    Rust via compute_capital_metrics (Gatev et al. 2006).
    """
    total_bars = max((d['day'] for d in daily), default=0) + 1
    last30 = total_bars - 22
    n_trades = len(closed)
    n_wins = sum(1 for t in closed if t.pnl > 0)
    all_pnl = sum(t.pnl for t in closed)
    recent_30 = [t for t in closed if t.exit_day >= last30]
    avg_hold = sum(t.exit_day - t.entry_day for t in closed) / n_trades if n_trades else 0
    max_hold_exits = sum(1 for t in closed if t.reason == "max_hold")
    rotation_exits = sum(1 for t in closed if t.reason == "rotation")
    r30_pnl = sum(t.pnl for t in recent_30)

    # Capital metrics (Rust) — Gatev et al. 2006 decomposition over last 30 days.
    # Rust guards NaN/invalid inputs; we only forward valid entries.
    last30_daily = [d for d in daily if d['day'] >= last30]
    last30_closed = [t for t in closed if t.exit_day >= last30]
    trade_inputs = [
        (t.pnl, t.capital_used, float(t.exit_day - t.entry_day))
        for t in last30_closed
        if t.exit_day > t.entry_day  # guard zero hold-time
    ]
    daily_inputs = [
        (float(total_capital), float(d['deployed']))
        for d in last30_daily
        if d.get('deployed') is not None
    ]
    n_last30_days = max(len(last30_daily), 1)
    cm = _compute_capital_metrics(
        trade_inputs, daily_inputs, float(total_capital), n_last30_days
    )

    return {
        "label": label,
        "n_trades": n_trades,
        "win_rate": n_wins / n_trades * 100 if n_trades else 0,
        "all_pnl": all_pnl,
        "r30_pnl": r30_pnl,
        "r30_per_day": r30_pnl / 22 if recent_30 else 0,
        # Gatev et al. (2006) decomposition — computed in Rust
        "roec": cm["roec"],
        "avg_util": cm["avg_utilization"] * 100,  # percent for display
        "rocc": cm["rocc"],
        "avg_return_per_trade": cm["avg_return_per_trade"],
        "opportunity_cost": cm["opportunity_cost"],
        "avg_hold": avg_hold,
        "max_hold_exits": max_hold_exits,
        "max_hold_exit_pct": max_hold_exits / n_trades * 100 if n_trades else 0,
        "rotation_exits": rotation_exits,
        "rotation_exit_pct": rotation_exits / n_trades * 100 if n_trades else 0,
        "ret_per_trade": all_pnl / (n_trades * total_capital / 10) if n_trades else 0,
    }


def print_comparison_table(a, b):
    """Print markdown comparison table for A/B test results.

    RoCC (Return on Committed Capital) is the headline metric per Gatev et al. (2006):
      RoCC = RoEC × Utilization
    It enables apples-to-apples comparison across different utilization regimes.
    """
    rows = [
        ("Trades", f"{a['n_trades']}", f"{b['n_trades']}"),
        ("Win rate", f"{a['win_rate']:.1f}%", f"{b['win_rate']:.1f}%"),
        ("All-time P&L", f"${a['all_pnl']:+,.0f}", f"${b['all_pnl']:+,.0f}"),
        ("Last 30d P&L", f"${a['r30_pnl']:+,.0f}", f"${b['r30_pnl']:+,.0f}"),
        ("RoCC (30d)", f"{a['rocc']*100:+.4f}%/day", f"{b['rocc']*100:+.4f}%/day"),
        ("  RoEC (30d)", f"{a['roec']*100:+.4f}%/day", f"{b['roec']*100:+.4f}%/day"),
        ("  Utilization (30d)", f"{a['avg_util']:.1f}%", f"{b['avg_util']:.1f}%"),
        ("Ret/trade (30d)", f"{a['avg_return_per_trade']*100:+.3f}%", f"{b['avg_return_per_trade']*100:+.3f}%"),
        ("Opp. cost (30d)", f"${a['opportunity_cost']:+,.0f}", f"${b['opportunity_cost']:+,.0f}"),
        ("Avg hold (days)", f"{a['avg_hold']:.1f}", f"{b['avg_hold']:.1f}"),
        ("Max-hold exits", f"{a['max_hold_exits']} ({a['max_hold_exit_pct']:.1f}%)",
                           f"{b['max_hold_exits']} ({b['max_hold_exit_pct']:.1f}%)"),
        ("Rotation exits", f"{a['rotation_exits']} ({a['rotation_exit_pct']:.1f}%)",
                           f"{b['rotation_exits']} ({b['rotation_exit_pct']:.1f}%)"),
    ]
    print(f"\n{'Metric':<25} {'Before ('+a['label']+')':<28} {'After ('+b['label']+')':<28} Delta")
    print("-" * 90)
    for metric, va, vb in rows:
        print(f"{metric:<25} {va:<28} {vb:<28}")


def main():
    parser = argparse.ArgumentParser(description="Capital simulation for pairs trading")
    parser.add_argument("--ab-compare", action="store_true",
                        help="Run A/B comparison: priority scoring vs |z|-only baseline")
    parser.add_argument("--rotation-ab", action="store_true",
                        help="Run A/B comparison: rotation OFF vs rotation ON")
    parser.add_argument("--scoring-mode", default="priority",
                        choices=["priority", "z_abs"],
                        help="Signal allocation scoring mode (default: priority)")
    parser.add_argument("--rotation", action="store_true", default=False,
                        help="Enable opportunity-cost rotation (Leung & Li 2015)")
    args = parser.parse_args()

    prices = json.load(open(root / "data" / "pair_picker_prices.json"))
    portfolio = json.load(open(root / "trading" / "pair_portfolio.json"))
    pair_configs = portfolio['pairs']

    if args.ab_compare:
        print("Running A/B comparison: |z| baseline vs priority scoring...")
        closed_baseline, daily_baseline = run_capital_sim(
            prices, pair_configs, scoring_mode="z_abs", rotation_enabled=False)
        closed_priority, daily_priority = run_capital_sim(
            prices, pair_configs, scoring_mode="priority", rotation_enabled=False)
        stats_a = summarize(closed_baseline, daily_baseline, label="z_abs")
        stats_b = summarize(closed_priority, daily_priority, label="priority")
        print_comparison_table(stats_a, stats_b)
        closed, daily = closed_priority, daily_priority

    elif args.rotation_ab:
        print("Running rotation A/B: rotation=OFF vs rotation=ON (priority scoring)...")
        closed_off, daily_off = run_capital_sim(
            prices, pair_configs, scoring_mode="priority", rotation_enabled=False)
        closed_on, daily_on = run_capital_sim(
            prices, pair_configs, scoring_mode="priority", rotation_enabled=True)
        stats_off = summarize(closed_off, daily_off, label="rotation_off")
        stats_on = summarize(closed_on, daily_on, label="rotation_on")
        print_comparison_table(stats_off, stats_on)
        closed, daily = closed_on, daily_on

    else:
        closed, daily = run_capital_sim(
            prices, pair_configs,
            scoring_mode=args.scoring_mode,
            rotation_enabled=args.rotation,
        )

    total_bars = min(len(v) for v in prices.values() if len(v) >= 200)
    last14 = total_bars - 10
    last30 = total_bars - 22

    # ── Results ──
    all_pnl = sum(t.pnl for t in closed)
    recent_30 = [t for t in closed if t.exit_day >= last30]
    recent_14 = [t for t in closed if t.exit_day >= last14]

    # Capital metrics (Rust) — Gatev et al. 2006 decomposition over last 30 days.
    # RoCC = RoEC × Utilization is the headline metric.
    last30_daily = [d for d in daily if d['day'] >= last30]
    last30_closed = [t for t in closed if t.exit_day >= last30]
    trade_inputs_30 = [
        (t.pnl, t.capital_used, float(t.exit_day - t.entry_day))
        for t in last30_closed
        if t.exit_day > t.entry_day
    ]
    daily_inputs_30 = [
        (float(TOTAL_CAPITAL), float(d['deployed']))
        for d in last30_daily
        if d.get('deployed') is not None
    ]
    n_last30_days = max(len(last30_daily), 1)
    cm30 = _compute_capital_metrics(
        trade_inputs_30, daily_inputs_30, float(TOTAL_CAPITAL), n_last30_days
    )

    # Capital metrics over full history
    all_trade_inputs = [
        (t.pnl, t.capital_used, float(t.exit_day - t.entry_day))
        for t in closed
        if t.exit_day > t.entry_day
    ]
    all_daily_inputs = [
        (float(TOTAL_CAPITAL), float(d['deployed']))
        for d in daily
        if d.get('deployed') is not None
    ]
    cm_all = _compute_capital_metrics(
        all_trade_inputs, all_daily_inputs, float(TOTAL_CAPITAL), max(len(daily), 1)
    )

    logger.info("")
    logger.info("=" * 70)
    logger.info(f"RESULTS — ${TOTAL_CAPITAL:,} capital, {len(pair_configs)} pairs")
    logger.info("=" * 70)
    logger.info(f"ALL TIME: {len(closed)}t ${all_pnl:+,.2f} "
                f"RoCC={cm_all['rocc']*100:+.4f}%/day "
                f"RoEC={cm_all['roec']*100:+.4f}%/day "
                f"Util={cm_all['avg_utilization']*100:.1f}%")
    if recent_30:
        r30_pnl = sum(t.pnl for t in recent_30)
        r30_wins = sum(1 for t in recent_30 if t.pnl > 0)
        logger.info(f"LAST 30d: {len(recent_30)}t {r30_wins}w ({r30_wins/len(recent_30)*100:.0f}%) "
                    f"${r30_pnl:+,.2f}")
        logger.info(f"  RoEC (return on employed):  {cm30['roec']*100:+.4f}%/day")
        logger.info(f"  Utilization:                {cm30['avg_utilization']*100:.1f}%")
        logger.info(f"  RoCC (return on committed): {cm30['rocc']*100:+.4f}%/day  (= RoEC x Util)")
        logger.info(f"  Return per trade:           {cm30['avg_return_per_trade']*100:+.3f}%")
        logger.info(f"  Opportunity cost (idle $):  ${cm30['opportunity_cost']:+,.2f}")
    if recent_14:
        r14_pnl = sum(t.pnl for t in recent_14)
        r14_wins = sum(1 for t in recent_14 if t.pnl > 0)
        logger.info(f"LAST 2wk: {len(recent_14)}t {r14_wins}w ${r14_pnl:+,.2f} (${r14_pnl/10:+.2f}/day)")

    # Exit reason breakdown
    max_hold_exits = sum(1 for t in closed if t.reason == "max_hold")
    rotation_exits = sum(1 for t in closed if t.reason == "rotation")
    logger.info(f"\nEXIT REASONS: max_hold={max_hold_exits} "
                f"({max_hold_exits/max(len(closed),1)*100:.0f}%), "
                f"rotation={rotation_exits} "
                f"({rotation_exits/max(len(closed),1)*100:.0f}%)")

    # Daily breakdown last 2 weeks
    logger.info(f"\nDAILY (last 2 weeks):")
    cum = 0
    for d in daily:
        if d['day'] >= last14:
            cum += d['pnl']
            logger.info(f"  Day {d['day']}: {d['open']} open, "
                        f"${d['deployed']:>6,.0f} deployed ({d['util']:>2.0f}%), "
                        f"pnl=${d['pnl']:>+7.2f} cum=${cum:>+7.2f}"
                        + (f" rotations={d['rotations']}" if d['rotations'] else ""))

    # Per pair — compute per-pair return on employed capital
    pair_stats: dict = {}
    for t in closed:
        k = f"{t.leg_a}/{t.leg_b}"
        if k not in pair_stats:
            pair_stats[k] = {
                'trades': 0, 'pnl': 0.0, 'wins': 0, 'capital': 0,
                'hold_days': [], 'trade_inputs': [],
            }
        pair_stats[k]['trades'] += 1
        pair_stats[k]['pnl'] += t.pnl
        pair_stats[k]['capital'] = t.capital_used
        pair_stats[k]['hold_days'].append(t.exit_day - t.entry_day)
        if t.pnl > 0:
            pair_stats[k]['wins'] += 1
        if t.exit_day > t.entry_day:
            pair_stats[k]['trade_inputs'].append(
                (t.pnl, t.capital_used, float(t.exit_day - t.entry_day))
            )

    logger.info(f"\nPER PAIR:")
    for pair in sorted(pair_stats, key=lambda p: pair_stats[p]['pnl'], reverse=True):
        s = pair_stats[pair]
        avg_hold = sum(s['hold_days']) / len(s['hold_days']) if s['hold_days'] else 0.0
        # RoEC per pair (no daily snapshots — trade-weighted estimate)
        pm = _compute_capital_metrics(
            s['trade_inputs'], [], float(s['capital'] * 2), max(len(s['hold_days']), 1)
        ) if s['trade_inputs'] else {"roec": 0.0, "avg_return_per_trade": 0.0}
        logger.info(
            f"  {pair:<15} {s['trades']:>2}t {s['wins']:>2}w "
            f"${s['pnl']:>+7.2f} "
            f"RoEC={pm['roec']*100:+.3f}%/day "
            f"ret/trade={pm['avg_return_per_trade']*100:+.2f}% "
            f"avg_hold={avg_hold:.1f}d "
            f"(${s['capital']:.0f}/leg)"
        )

    # Generate dashboard
    dashboard_path = root / "dashboards" / "capital_dashboard.html"
    dashboard_path.parent.mkdir(exist_ok=True)
    generate_capital_dashboard(closed, daily, dashboard_path)
    print(f"Dashboard: file://{dashboard_path}")
    print(f"Log: {LOG_FILE}")


if __name__ == "__main__":
    main()
