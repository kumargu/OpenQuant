#!/usr/bin/env python3
"""
Deep-dive logging for a single pair's walk-forward simulation.
Logs every decision point: scan results, entry signals, daily z-score evolution,
exit decisions, P&L attribution.

Writes structured logs to data/journal/walkforward.log (persists across runs)
and also prints to stdout.
"""

import json
import logging
import math
import sys
from datetime import datetime
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from scripts.daily_walkforward_dashboard import (
    FORMATION_DAYS, ENTRY_Z, EXIT_Z, MAX_HOLD, CAPITAL_PER_LEG, COST_BPS, MIN_R2,
    ols_simple, estimate_half_life, adf_simple, scan_pair, compute_z, compute_trade_pnl,
    PairParams, OpenTrade, ClosedTrade,
)

# ── Persistent logger ─────────────────────────────────────────────────────────

LOG_DIR = Path(__file__).resolve().parent.parent / "data" / "journal"
LOG_FILE = LOG_DIR / "walkforward.log"

_logger = logging.getLogger("walkforward")
_logger.setLevel(logging.DEBUG)
_logger.propagate = False

# File handler — append mode, persists across runs
_fh = logging.FileHandler(LOG_FILE, mode="a", encoding="utf-8")
_fh.setLevel(logging.DEBUG)
_fh.setFormatter(logging.Formatter("%(asctime)s %(message)s", datefmt="%Y-%m-%d %H:%M:%S"))
_logger.addHandler(_fh)

# Stdout handler — same format
_sh = logging.StreamHandler(sys.stdout)
_sh.setLevel(logging.DEBUG)
_sh.setFormatter(logging.Formatter("%(message)s"))
_logger.addHandler(_sh)


def log(day, tag, msg, **kwargs):
    extra = " | ".join(f"{k}={v}" for k, v in kwargs.items())
    prefix = f"[Day {day:>3}]" if day is not None else "[     ]"
    _logger.info(f"{prefix} [{tag:<12}] {msg}  {extra}")


def log_scan_detail(day, leg_a, leg_b, prices_a, prices_b):
    """Full scan with detailed logging of every filter stage."""
    start = max(0, day - FORMATION_DAYS)
    pa = prices_a[start:day]
    pb = prices_b[start:day]
    n = min(len(pa), len(pb))

    log(day, "SCAN", f"--- Scanning {leg_a}/{leg_b} ---", window=f"[{start}:{day}]", n_bars=n)

    if n < FORMATION_DAYS - 5:
        log(day, "SCAN:REJECT", "Insufficient bars", need=FORMATION_DAYS - 5, have=n)
        return None

    pa, pb = pa[:n], pb[:n]
    log_a = [math.log(p) for p in pa]
    log_b = [math.log(p) for p in pb]

    # OLS
    result = ols_simple(log_b, log_a)
    if result is None:
        log(day, "SCAN:REJECT", "OLS failed")
        return None
    alpha, beta, r2 = result
    log(day, "OLS", f"alpha={alpha:.6f} beta={beta:.4f} R²={r2:.4f}")
    if r2 < MIN_R2:
        log(day, "SCAN:REJECT", f"R² too low", r2=f"{r2:.4f}", min=MIN_R2)
        return None

    # Spread
    spread = [log_a[i] - alpha - beta * log_b[i] for i in range(n)]
    log(day, "SPREAD", f"last_5_spread=[{', '.join(f'{s:.6f}' for s in spread[-5:])}]")

    # Half-life
    hl = estimate_half_life(spread)
    if hl is None:
        log(day, "SCAN:REJECT", "Half-life estimation failed (theta >= 0)")
        return None
    log(day, "HALF-LIFE", f"HL={hl:.2f} days", valid="2.0-5.0")
    if hl < 2.0 or hl > 5.0:
        log(day, "SCAN:REJECT", f"HL out of range", hl=f"{hl:.2f}")
        return None

    # ADF
    adf = adf_simple(spread)
    log(day, "ADF", f"ADF stat={adf:.4f}", threshold=-2.0)
    if adf > -2.0:
        log(day, "SCAN:REJECT", f"ADF too high (not stationary)", adf=f"{adf:.4f}")
        return None

    # Z-score
    window = spread[-30:]
    mean = sum(window) / len(window)
    std = math.sqrt(sum((s - mean) ** 2 for s in window) / (len(window) - 1))
    current_spread = spread[-1]
    z = (current_spread - mean) / std

    log(day, "Z-SCORE", f"z={z:.4f}", spread_mean=f"{mean:.6f}", spread_std=f"{std:.6f}",
        current_spread=f"{current_spread:.6f}")

    params = PairParams(
        leg_a=leg_a, leg_b=leg_b, beta=beta, alpha=alpha,
        spread_mean=mean, spread_std=std, half_life=hl, r2=r2, adf_stat=adf,
    )

    if abs(z) > ENTRY_Z:
        direction = "LONG_SPREAD" if z < -ENTRY_Z else "SHORT_SPREAD"
        log(day, "SIGNAL", f"|z|={abs(z):.4f} > {ENTRY_Z} → {direction}")
    else:
        log(day, "NO_SIGNAL", f"|z|={abs(z):.4f} < {ENTRY_Z}")

    return params, z


def run_pair_deep_dive(leg_a, leg_b, prices):
    """Run simulation for a single pair with exhaustive logging."""
    prices_a = prices[leg_a]
    prices_b = prices[leg_b]
    total_bars = min(len(prices_a), len(prices_b))

    log(None, "CONFIG", f"Pair: {leg_a}/{leg_b}",
        formation=FORMATION_DAYS, entry_z=ENTRY_Z, exit_z=EXIT_Z,
        max_hold=MAX_HOLD, capital=CAPITAL_PER_LEG, cost_bps=COST_BPS)
    log(None, "DATA", f"Total bars: {total_bars}",
        price_a_range=f"[{prices_a[0]:.2f}..{prices_a[-1]:.2f}]",
        price_b_range=f"[{prices_b[0]:.2f}..{prices_b[-1]:.2f}]")

    open_trade = None
    closed_trades = []
    scan_results_log = []

    for day in range(FORMATION_DAYS, total_bars):
        pa = prices_a[day]
        pb = prices_b[day]

        # ── If we have an open trade, log daily z evolution ──
        if open_trade is not None:
            z = compute_z(open_trade.pair, pa, pb)
            bars_held = day - open_trade.entry_day
            current_spread = math.log(pa) - open_trade.pair.alpha - open_trade.pair.beta * math.log(pb)

            # Also compute what the ROLLING z would be (for comparison)
            # This shows the drift effect
            start = max(0, day - 30)
            recent_spreads = []
            for i in range(start, day + 1):
                if i < len(prices_a) and i < len(prices_b):
                    s = math.log(prices_a[i]) - open_trade.pair.alpha - open_trade.pair.beta * math.log(prices_b[i])
                    recent_spreads.append(s)
            if len(recent_spreads) > 1:
                rolling_mean = sum(recent_spreads) / len(recent_spreads)
                rolling_std = math.sqrt(sum((s - rolling_mean)**2 for s in recent_spreads) / (len(recent_spreads) - 1))
                rolling_z = (current_spread - rolling_mean) / rolling_std if rolling_std > 1e-10 else 0
            else:
                rolling_mean = rolling_z = 0

            dir_str = "LONG" if open_trade.direction == 1 else "SHORT"
            unrealized_pnl = compute_trade_pnl(open_trade, pa, pb)

            log(day, "HOLDING", f"{dir_str} {leg_a}/{leg_b}",
                bars_held=bars_held,
                fixed_z=f"{z:.4f}",
                rolling_z=f"{rolling_z:.4f}",
                z_drift=f"{abs(z - rolling_z):.4f}",
                spread=f"{current_spread:.6f}",
                frozen_mean=f"{open_trade.pair.spread_mean:.6f}",
                rolling_mean=f"{rolling_mean:.6f}",
                unrealized_pnl=f"${unrealized_pnl:+.2f}",
                price_a=f"{pa:.2f}",
                price_b=f"{pb:.2f}")

            # Check exit conditions
            reason = None
            if bars_held >= MAX_HOLD:
                reason = "max_hold"
                log(day, "EXIT:MAXHOLD", f"Held {bars_held} bars >= {MAX_HOLD}", fixed_z=f"{z:.4f}")
            elif open_trade.direction == 1 and z > -EXIT_Z:
                reason = "reversion"
                log(day, "EXIT:REVERT", f"Long spread, z={z:.4f} > {-EXIT_Z}", bars_held=bars_held)
            elif open_trade.direction == -1 and z < EXIT_Z:
                reason = "reversion"
                log(day, "EXIT:REVERT", f"Short spread, z={z:.4f} < {EXIT_Z}", bars_held=bars_held)

            if reason:
                pnl = compute_trade_pnl(open_trade, pa, pb)
                cost = 2 * CAPITAL_PER_LEG * COST_BPS / 10_000
                raw_pnl = pnl + cost  # P&L before costs

                ct = ClosedTrade(
                    leg_a=leg_a, leg_b=leg_b, direction=open_trade.direction,
                    entry_day=open_trade.entry_day, exit_day=day,
                    entry_price_a=open_trade.entry_price_a, exit_price_a=pa,
                    entry_price_b=open_trade.entry_price_b, exit_price_b=pb,
                    pnl_usd=pnl, exit_reason=reason,
                )
                closed_trades.append(ct)

                ret_a = (pa - open_trade.entry_price_a) / open_trade.entry_price_a * 100
                ret_b = (pb - open_trade.entry_price_b) / open_trade.entry_price_b * 100

                log(day, "CLOSED", f"{'WIN' if pnl > 0 else 'LOSS'} — {reason}",
                    pnl=f"${pnl:+.2f}",
                    raw_pnl=f"${raw_pnl:+.2f}",
                    cost=f"${cost:.2f}",
                    ret_a=f"{ret_a:+.2f}%",
                    ret_b=f"{ret_b:+.2f}%",
                    price_a_entry=f"{open_trade.entry_price_a:.2f}",
                    price_a_exit=f"{pa:.2f}",
                    price_b_entry=f"{open_trade.entry_price_b:.2f}",
                    price_b_exit=f"{pb:.2f}")

                open_trade = None

        # ── If flat, scan for entry ──
        if open_trade is None:
            result = log_scan_detail(day, leg_a, leg_b, prices_a, prices_b)
            if result is not None:
                params, z = result
                scan_results_log.append((day, z))

                if abs(z) > ENTRY_Z:
                    direction = 1 if z < -ENTRY_Z else -1
                    open_trade = OpenTrade(
                        pair=params, direction=direction,
                        entry_day=day, entry_price_a=pa, entry_price_b=pb,
                        entry_spread=math.log(pa) - params.alpha - params.beta * math.log(pb),
                        entry_z=z,
                    )
                    dir_str = "LONG_SPREAD" if direction == 1 else "SHORT_SPREAD"
                    log(day, "ENTRY", f"Opening {dir_str}",
                        z=f"{z:.4f}",
                        price_a=f"{pa:.2f}",
                        price_b=f"{pb:.2f}",
                        beta=f"{params.beta:.4f}",
                        spread_mean=f"{params.spread_mean:.6f}",
                        spread_std=f"{params.spread_std:.6f}",
                        half_life=f"{params.half_life:.2f}d")

    # Force close at end
    if open_trade is not None:
        pa = prices_a[total_bars - 1]
        pb = prices_b[total_bars - 1]
        pnl = compute_trade_pnl(open_trade, pa, pb)
        log(total_bars - 1, "FORCE_CLOSE", f"End of data", pnl=f"${pnl:+.2f}")
        closed_trades.append(ClosedTrade(
            leg_a=leg_a, leg_b=leg_b, direction=open_trade.direction,
            entry_day=open_trade.entry_day, exit_day=total_bars - 1,
            entry_price_a=open_trade.entry_price_a, exit_price_a=pa,
            entry_price_b=open_trade.entry_price_b, exit_price_b=pb,
            pnl_usd=pnl, exit_reason="eod_force",
        ))

    # ── Summary ──
    log(None, "SUMMARY", "=" * 60)
    log(None, "SUMMARY", f"Pair: {leg_a}/{leg_b}")
    total = sum(t.pnl_usd for t in closed_trades)
    wins = sum(1 for t in closed_trades if t.pnl_usd > 0)
    log(None, "SUMMARY", f"Trades: {len(closed_trades)}")
    if closed_trades:
        log(None, "SUMMARY", f"Winners: {wins} ({wins/len(closed_trades)*100:.0f}%)")
    log(None, "SUMMARY", f"Total P&L: ${total:+.2f}")
    for t in closed_trades:
        hold = t.exit_day - t.entry_day
        log(None, "SUMMARY", f"  Day {t.entry_day}->{t.exit_day} ({hold}d) "
            f"{'LONG' if t.direction==1 else 'SHORT'} ${t.pnl_usd:+.2f} [{t.exit_reason}]")
    log(None, "SUMMARY", f"Total scan results logged: {len(scan_results_log)}")
    log(None, "SUMMARY", "=" * 60)


if __name__ == "__main__":
    # Session header in log file
    _logger.info("")
    _logger.info("=" * 80)
    _logger.info(f"WALK-FORWARD DEEP DIVE — {datetime.now().strftime('%Y-%m-%d %H:%M:%S')}")
    _logger.info(f"Log file: {LOG_FILE}")
    _logger.info("=" * 80)

    prices = json.load(open("data/pair_picker_prices.json"))
    pair_arg = sys.argv[1] if len(sys.argv) > 1 else "FDX/UPS"
    leg_a, leg_b = pair_arg.split("/")
    run_pair_deep_dive(leg_a, leg_b, prices)

    _logger.info(f"Log persisted to {LOG_FILE}")
