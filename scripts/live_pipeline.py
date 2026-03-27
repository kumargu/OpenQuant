#!/usr/bin/env python3
"""
OpenQuant Live Trading Pipeline — THE single entry point for live trading.

Usage:
    python3 scripts/live_pipeline.py run       # Full daily cycle (scan → enter → monitor → exit → eod)
    python3 scripts/live_pipeline.py run --dry  # Same cycle, no orders (signal watching)
    python3 scripts/live_pipeline.py monitor   # Quick position check (no orders)

The 'run' command does everything:
  1. Exit positions that reverted or hit max_hold
  2. Scan portfolio for new signals (quality gates, stability, win rate)
  3. Enter top signals (respecting capital limits)
  4. Monitor all positions with frozen z-scores
  5. Log EOD summary

Architecture:
    - Pair universe: ONLY from trading/pair_portfolio.json (backtested)
    - Quality gates: identical to capital_sim.py (R², HL, ADF, beta, stability)
    - Win rate gate: rejects pair+direction combos with <40% backtest win rate
    - Stability gate: rejects pairs failing scan_pair >5 of last 10 days
    - Orders: Alpaca paper account via API
    - State: trading/live_positions.json (single source of truth)
    - Logs: data/journal/engine.log (structured, grepable)
"""

import argparse
import json
import logging
import sys
from datetime import datetime
from pathlib import Path
from zoneinfo import ZoneInfo

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT / "scripts"))

# All times in Eastern. Server runs IST but we trade US markets.
ET = ZoneInfo("US/Eastern")

# Shared logic — single source of truth for all quality gates, thresholds, scoring
from pairs_core import (
    scan_pair, compute_z, load_earnings_calendar,
    validate_entry, compute_frozen_z, decide_exit, score_signal,
    check_beta_drift,
    TOTAL_CAPITAL, HOLD_MULTIPLIER, MAX_HOLD_CAP, EXIT_Z_DEFAULT,
)

# ── Logger ────────────────────────────────────────────────────────────────────

LOG_FILE = ROOT / "data" / "journal" / "engine.log"
LOG_FILE.parent.mkdir(parents=True, exist_ok=True)

logger = logging.getLogger("live_pipeline")
logger.setLevel(logging.DEBUG)
logger.propagate = False

_fh = logging.FileHandler(LOG_FILE, mode="a", encoding="utf-8")
_fh.setLevel(logging.DEBUG)
# Force ET timestamps in log file (server runs IST but we trade US markets)
_et_fmt = logging.Formatter("%(asctime)s %(message)s", datefmt="%Y-%m-%d %H:%M:%S")
_et_fmt.converter = lambda *args: datetime.now(ET).timetuple()
_fh.setFormatter(_et_fmt)
logger.addHandler(_fh)

_sh = logging.StreamHandler(sys.stdout)
_sh.setLevel(logging.INFO)
_sh.setFormatter(logging.Formatter("%(message)s"))
logger.addHandler(_sh)

# Silence noisy libraries
logging.getLogger("urllib3").setLevel(logging.WARNING)
logging.getLogger("alpaca").setLevel(logging.WARNING)


# Config is in pairs_core.py — single source of truth

# ── Data loading ──────────────────────────────────────────────────────────────

def load_env():
    env = {}
    env_path = ROOT / ".env"
    if env_path.exists():
        with open(env_path) as f:
            for line in f:
                line = line.strip()
                if "=" in line and not line.startswith("#"):
                    k, v = line.split("=", 1)
                    env[k] = v
    return env


def load_portfolio():
    """Load pair_portfolio.json — the ONLY source of tradeable pairs."""
    path = ROOT / "trading" / "pair_portfolio.json"
    if not path.exists():
        logger.error(f"Portfolio not found: {path}")
        sys.exit(1)
    with open(path) as f:
        raw = json.load(f)
    return raw["pairs"], raw.get("defaults", {})


def load_live_positions():
    path = ROOT / "trading" / "live_positions.json"
    if path.exists():
        with open(path) as f:
            return json.load(f)
    return {"positions": [], "account": {"broker": "alpaca_paper", "total_deployed": 0, "cash_remaining": TOTAL_CAPITAL}}


def save_live_positions(data):
    path = ROOT / "trading" / "live_positions.json"
    with open(path, "w") as f:
        json.dump(data, f, indent=2)
        f.write("\n")


def load_prices():
    path = ROOT / "data" / "pair_picker_prices.json"
    if not path.exists():
        logger.error(f"Price data not found: {path}")
        sys.exit(1)
    with open(path) as f:
        return json.load(f)


# Quality gates, stability, win rate, validate_entry → all in pairs_core.py


def _compute_position_frozen_z(pos, a_now, b_now, prices, total_bars):
    """Compute frozen z for an open position. Handles legacy positions without alpha."""
    sig = pos["signal"]
    alpha = sig.get("alpha")
    beta = sig.get("beta", 0)
    spread_mean = sig.get("spread_mean")
    spread_std = sig.get("spread_std")

    if alpha is not None and spread_mean is not None and spread_std:
        return compute_frozen_z(a_now, b_now, alpha, beta, spread_mean, spread_std)

    # Legacy position — fall back to today's scan (log warning)
    a, b = pos["leg_a"]["symbol"], pos["leg_b"]["symbol"]
    if a_now > 0 and b_now > 0 and a in prices and b in prices:
        params = scan_pair(a, b, prices[a], prices[b], total_bars - 1)
        if params:
            logger.warning(f"  {pos['pair']}: using today's alpha (legacy — missing entry-time alpha)")
            return compute_frozen_z(a_now, b_now, params.alpha, params.beta,
                                    params.spread_mean, params.spread_std)
        logger.warning(f"  {pos['pair']}: scan_pair FAILED — cannot compute z")
    return None


# ── Commands ──────────────────────────────────────────────────────────────────

_last_betas = {}  # persist beta across daily scans for stability check

def cmd_scan(args):
    """Pre-market scan: find actionable signals from the portfolio."""
    pairs, defaults = load_portfolio()
    prices = load_prices()
    total_bars = min(len(v) for v in prices.values() if len(v) >= 200)
    live = load_live_positions()
    held_pairs = {p["pair"] for p in live["positions"]}
    earnings_cal = load_earnings_calendar()

    logger.info(f"{'='*60}")
    logger.info(f"SCAN — {datetime.now(ET).strftime('%Y-%m-%d %H:%M ET')}")
    logger.info(f"Portfolio: {len(pairs)} pairs | Held: {len(held_pairs)} | Data: {total_bars} bars")
    logger.info(f"{'='*60}")

    signals = []
    for pcfg in pairs:
        leg_a, leg_b = pcfg["leg_a"], pcfg["leg_b"]
        pair_id = f"{leg_a}/{leg_b}"

        if pair_id in held_pairs:
            continue
        if leg_a not in prices or leg_b not in prices:
            continue

        params = scan_pair(leg_a, leg_b, prices[leg_a], prices[leg_b], total_bars - 1)
        if params is None:
            logger.debug(f"  {pair_id}: scan_pair REJECTED")
            continue

        # Beta stability check (shared with capital_sim via pairs_core)
        prev_beta = _last_betas.get(pair_id)
        stable, change = check_beta_drift(params.beta, prev_beta)
        _last_betas[pair_id] = params.beta
        if not stable:
            logger.info(f"  {pair_id}: REJECTED — beta_unstable {params.beta:.3f} (prev={prev_beta:.3f}, change={change:.0%})")
            continue

        pa = prices[leg_a][total_bars - 1]
        pb = prices[leg_b][total_bars - 1]
        z = compute_z(params, pa, pb)

        entry_z = pcfg.get("entry_z", 1.0)
        if abs(z) <= entry_z:
            continue

        # z too extreme — structural break
        if abs(z) >= entry_z + 1.5:
            logger.info(f"  {pair_id}: SKIP z={z:+.2f} (>{entry_z+1.5:.1f} = possible structural break)")
            continue

        # Run all quality gates
        ok, reason = validate_entry(leg_a, leg_b, params, z, prices, total_bars, earnings_cal, entry_z=entry_z)
        if not ok:
            logger.info(f"  {pair_id}: REJECTED — {reason}")
            continue

        direction = "LONG" if z < 0 else "SHORT"
        prio, erpdd, max_hold, kappa = score_signal(z, params.half_life, params.spread_std)

        signals.append({
            "pair": pair_id,
            "leg_a": leg_a, "leg_b": leg_b,
            "direction": direction,
            "z": z,
            "r2": params.r2,
            "half_life": params.half_life,
            "beta": params.beta,
            "adf_stat": params.adf_stat,
            "spread_std": params.spread_std,
            "spread_mean": params.spread_mean,
            "alpha": params.alpha,
            "priority": prio,
            "max_hold": max_hold,
            "capital_per_leg": pcfg.get("capital_per_leg", 500),
            "exit_z": pcfg.get("exit_z", 0.2),
        })

    signals.sort(key=lambda x: x["priority"], reverse=True)

    if not signals:
        logger.info("\nNo actionable signals today.")
    else:
        logger.info(f"\n{'='*60}")
        logger.info(f"ACTIONABLE SIGNALS ({len(signals)}):")
        logger.info(f"{'='*60}")
        for i, s in enumerate(signals, 1):
            logger.info(
                f"  {i}. {s['pair']} {s['direction']} "
                f"z={s['z']:+.2f} prio={s['priority']:.1f} "
                f"R²={s['r2']:.3f} HL={s['half_life']:.1f} "
                f"beta={s['beta']:.3f} max_hold={s['max_hold']}d "
                f"${s['capital_per_leg']}/leg"
            )

    # Save signals for enter command
    sig_path = ROOT / "trading" / "pending_signals.json"
    with open(sig_path, "w") as f:
        json.dump({"scan_time": datetime.now(ET).isoformat(), "signals": signals}, f, indent=2)
    logger.info(f"\nSignals saved to {sig_path}")
    return signals


def cmd_enter(args):
    """Place orders for pending signals. Requires Alpaca credentials."""
    sig_path = ROOT / "trading" / "pending_signals.json"
    if not sig_path.exists():
        logger.error("No pending signals. Run 'scan' first.")
        sys.exit(1)

    with open(sig_path) as f:
        pending = json.load(f)

    signals = pending["signals"]
    if not signals:
        logger.info("No signals to enter.")
        return

    env = load_env()
    api_key = env.get("ALPACA_API_KEY")
    api_secret = env.get("ALPACA_SECRET_KEY")
    if not api_key or not api_secret:
        logger.error("Missing ALPACA_API_KEY or ALPACA_SECRET_KEY in .env")
        sys.exit(1)

    from alpaca.trading.client import TradingClient
    from alpaca.trading.requests import MarketOrderRequest
    from alpaca.trading.enums import OrderSide, TimeInForce

    client = TradingClient(api_key, api_secret, paper=True)
    live = load_live_positions()
    held_pairs = {p["pair"] for p in live["positions"]}

    account = client.get_account()
    cash = float(account.cash)
    logger.info(f"Account cash: ${cash:,.0f}")

    # Load prices once for all signals (consistent snapshot)
    prices_data = load_prices()
    total_bars = min(len(v) for v in prices_data.values() if len(v) >= 200)

    entered = 0
    for sig in signals:
        if sig["pair"] in held_pairs:
            logger.info(f"  SKIP {sig['pair']}: already held")
            continue

        # Cap capital per leg (same as capital_sim: MAX_PER_TRADE_FRAC * TOTAL_CAPITAL)
        from pairs_core import MAX_PER_TRADE_FRAC
        max_per_leg = TOTAL_CAPITAL * MAX_PER_TRADE_FRAC
        capital_per_leg = min(sig["capital_per_leg"], max_per_leg)

        capital_needed = capital_per_leg * 2
        if capital_needed > cash:
            logger.info(f"  SKIP {sig['pair']}: need ${capital_needed} but cash=${cash:.0f}")
            continue

        # Calculate quantities
        pa = prices_data[sig["leg_a"]][total_bars - 1]
        pb = prices_data[sig["leg_b"]][total_bars - 1]

        qty_a = max(1, int(capital_per_leg / pa))
        qty_b = max(1, int(capital_per_leg * abs(sig["beta"]) / pb))

        is_long = sig["direction"] == "LONG"
        side_a = OrderSide.BUY if is_long else OrderSide.SELL
        side_b = OrderSide.SELL if is_long else OrderSide.BUY

        logger.info(f"\n  ENTERING {sig['pair']} {sig['direction']}:")
        logger.info(f"    {side_a.name} {qty_a} {sig['leg_a']} @ ~${pa:.2f}")
        logger.info(f"    {side_b.name} {qty_b} {sig['leg_b']} @ ~${pb:.2f}")

        try:
            order_a = client.submit_order(MarketOrderRequest(
                symbol=sig["leg_a"], qty=qty_a, side=side_a, time_in_force=TimeInForce.DAY))
            logger.info(f"    {sig['leg_a']} {side_a.name}: {order_a.status}")

            try:
                order_b = client.submit_order(MarketOrderRequest(
                    symbol=sig["leg_b"], qty=qty_b, side=side_b, time_in_force=TimeInForce.DAY))
                logger.info(f"    {sig['leg_b']} {side_b.name}: {order_b.status}")
            except Exception as e_b:
                # Leg B failed — close leg A to avoid naked directional exposure
                logger.error(f"    LEG B FAILED: {e_b}")
                logger.error(f"    CLOSING LEG A ({sig['leg_a']}) to prevent naked position")
                try:
                    client.close_position(sig["leg_a"])
                except Exception as e_close:
                    logger.critical(f"    CRITICAL: Could not close leg A: {e_close} — MANUAL INTERVENTION NEEDED")
                continue

            # Record position
            et_now = datetime.now(ET)
            position = {
                "pair": sig["pair"],
                "direction": sig["direction"],
                "entry_date": et_now.strftime("%Y-%m-%d"),
                "entry_time_et": et_now.strftime("%H:%M"),
                "leg_a": {
                    "symbol": sig["leg_a"],
                    "side": "long" if is_long else "short",
                    "qty": qty_a,
                    "entry_price": pa,
                },
                "leg_b": {
                    "symbol": sig["leg_b"],
                    "side": "short" if is_long else "long",
                    "qty": qty_b,
                    "entry_price": pb,
                },
                "signal": {
                    "z_entry": round(sig["z"], 3),
                    "r2": round(sig["r2"], 3),
                    "half_life_days": round(sig["half_life"], 1),
                    "beta": round(sig["beta"], 3),
                    "alpha": sig["alpha"],
                    "spread_mean": sig["spread_mean"],
                    "spread_std": sig["spread_std"],
                },
                "capital_per_leg": capital_per_leg,
                "max_hold_days": sig["max_hold"],
                "exit_z_threshold": sig["exit_z"],
            }
            live["positions"].append(position)
            held_pairs.add(sig["pair"])
            cash -= capital_needed
            entered += 1

        except Exception as e:
            logger.error(f"    ORDER FAILED: {e}")

    # Update account totals
    deployed = sum(p["capital_per_leg"] * 2 for p in live["positions"])
    live["account"]["total_deployed"] = deployed
    live["account"]["cash_remaining"] = TOTAL_CAPITAL - deployed
    save_live_positions(live)
    logger.info(f"\nEntered {entered} trades. Positions: {len(live['positions'])}")


def cmd_monitor(args):
    """Monitor open positions with current P&L and frozen z-scores."""
    live = load_live_positions()
    if not live["positions"]:
        logger.info("No open positions.")
        return

    env = load_env()
    from alpaca.trading.client import TradingClient
    client = TradingClient(env["ALPACA_API_KEY"], env["ALPACA_SECRET_KEY"], paper=True)

    prices = load_prices()
    total_bars = min(len(v) for v in prices.values() if len(v) >= 200)
    alpaca_pos = {p.symbol: p for p in client.get_all_positions()}

    logger.info(f"\n{'='*60}")
    logger.info(f"POSITIONS — {datetime.now(ET).strftime('%H:%M ET')}")
    logger.info(f"{'='*60}")

    total_pnl = 0
    for pos in live["positions"]:
        a, b = pos["leg_a"]["symbol"], pos["leg_b"]["symbol"]
        a_pnl = float(alpaca_pos[a].unrealized_pl) if a in alpaca_pos else 0
        b_pnl = float(alpaca_pos[b].unrealized_pl) if b in alpaca_pos else 0
        net = a_pnl + b_pnl
        total_pnl += net

        a_now = float(alpaca_pos[a].current_price) if a in alpaca_pos else 0
        b_now = float(alpaca_pos[b].current_price) if b in alpaca_pos else 0

        # Frozen z — shared function from pairs_core
        frozen_z = _compute_position_frozen_z(pos, a_now, b_now, prices, total_bars)

        z_entry = pos["signal"]["z_entry"]
        is_long = pos["direction"] == "LONG"

        # Days held (ET date, not local IST — server is in India)
        entry_date = datetime.fromisoformat(pos["entry_date"]).date()
        days_held = (datetime.now(ET).date() - entry_date).days

        if frozen_z is not None:
            z_moved = frozen_z - z_entry
            reverting = (is_long and z_moved > 0) or (not is_long and z_moved < 0)
            status = "REVERTING" if reverting else "DIVERGING"
            z_str = f"z: {z_entry:+.2f} -> {frozen_z:+.2f} ({status})"
        else:
            z_str = "z: NO DATA (scan_pair failed)"

        logger.info(f"\n  {pos['pair']} {pos['direction']} (day {days_held}/{pos['max_hold_days']}):")
        logger.info(f"    {a}: ${a_now:.2f} P&L ${a_pnl:+.2f}")
        logger.info(f"    {b}: ${b_now:.2f} P&L ${b_pnl:+.2f}")
        logger.info(f"    NET: ${net:+.2f} | {z_str}")

    logger.info(f"\n  TOTAL P&L: ${total_pnl:+.2f}")


def cmd_exit(args):
    """Close positions that have reverted or hit max_hold."""
    live = load_live_positions()
    if not live["positions"]:
        logger.info("No open positions.")
        return

    env = load_env()
    from alpaca.trading.client import TradingClient
    client = TradingClient(env["ALPACA_API_KEY"], env["ALPACA_SECRET_KEY"], paper=True)

    prices = load_prices()
    total_bars = min(len(v) for v in prices.values() if len(v) >= 200)
    alpaca_pos = {p.symbol: p for p in client.get_all_positions()}

    to_close = []
    for i, pos in enumerate(live["positions"]):
        a, b = pos["leg_a"]["symbol"], pos["leg_b"]["symbol"]
        a_now = float(alpaca_pos[a].current_price) if a in alpaca_pos else 0
        b_now = float(alpaca_pos[b].current_price) if b in alpaca_pos else 0

        # Frozen z — shared function from pairs_core
        frozen_z = _compute_position_frozen_z(pos, a_now, b_now, prices, total_bars)

        # Exit decision — shared logic from pairs_core (with time-decay)
        entry_date = datetime.fromisoformat(pos["entry_date"]).date()
        days_held = (datetime.now(ET).date() - entry_date).days
        exit_thresh = pos.get("exit_z_threshold", EXIT_Z_DEFAULT)
        max_hold = pos.get("max_hold_days", 5)
        reason = decide_exit(frozen_z, days_held, max_hold, exit_z=exit_thresh, use_decay=True)

        if reason:
            # Close both legs
            net_pnl = 0
            for sym in [a, b]:
                if sym in alpaca_pos:
                    pnl = float(alpaca_pos[sym].unrealized_pl)
                    net_pnl += pnl
                    try:
                        client.close_position(sym)
                        logger.info(f"  CLOSED {sym}: P&L ${pnl:+.2f}")
                    except Exception as e:
                        logger.error(f"  CLOSE FAILED {sym}: {e}")
                else:
                    logger.critical(f"  CRITICAL: {sym} not found on Alpaca — state divergence! Check manually.")

            logger.info(f"  EXIT {pos['pair']} reason={reason} days={days_held} net=${net_pnl:+.2f}")
            to_close.append(i)

    # Remove closed positions
    for i in sorted(to_close, reverse=True):
        live["positions"].pop(i)

    deployed = sum(p["capital_per_leg"] * 2 for p in live["positions"])
    live["account"]["total_deployed"] = deployed
    live["account"]["cash_remaining"] = TOTAL_CAPITAL - deployed
    save_live_positions(live)

    if to_close:
        logger.info(f"\nClosed {len(to_close)} positions. Remaining: {len(live['positions'])}")
    else:
        logger.info("\nNo positions ready to close.")


def cmd_eod(args):
    """End-of-day summary: P&L, positions, capital."""
    live = load_live_positions()

    env = load_env()
    from alpaca.trading.client import TradingClient
    client = TradingClient(env["ALPACA_API_KEY"], env["ALPACA_SECRET_KEY"], paper=True)

    account = client.get_account()
    alpaca_pos = {p.symbol: p for p in client.get_all_positions()}

    logger.info(f"\n{'='*60}")
    logger.info(f"END OF DAY — {datetime.now(ET).strftime('%Y-%m-%d')}")
    logger.info(f"{'='*60}")
    logger.info(f"  Account equity: ${float(account.equity):,.2f}")
    logger.info(f"  Cash: ${float(account.cash):,.2f}")
    logger.info(f"  Open positions: {len(live['positions'])}")

    total_unrealized = 0
    for pos in live["positions"]:
        a = pos["leg_a"]["symbol"]
        b = pos["leg_b"]["symbol"]
        a_pnl = float(alpaca_pos[a].unrealized_pl) if a in alpaca_pos else 0
        b_pnl = float(alpaca_pos[b].unrealized_pl) if b in alpaca_pos else 0
        net = a_pnl + b_pnl
        total_unrealized += net
        logger.info(f"    {pos['pair']} {pos['direction']}: ${net:+.2f}")

    logger.info(f"  Total unrealized: ${total_unrealized:+.2f}")
    logger.info(f"{'='*60}")


# ── Unified run command ───────────────────────────────────────────────────────

def cmd_run(args):
    """Full daily cycle: exit → scan → enter → monitor → eod.

    This is the ONE command to run each day. It handles everything in order:
    1. Close positions that have reverted or hit max_hold
    2. Scan the portfolio for new entry signals
    3. Place orders for validated signals (skip if --dry)
    4. Monitor all positions
    5. Log end-of-day summary
    """
    env = load_env()
    et_now = datetime.now(ET)
    logger.info(f"\n{'='*60}")
    logger.info(f"DAILY RUN — {et_now.strftime('%Y-%m-%d %H:%M ET')}")
    logger.info(f"Mode: {'DRY RUN (no orders)' if args.dry else 'LIVE'}")
    logger.info(f"{'='*60}")

    # Check market hours
    is_market_hours = (
        et_now.weekday() < 5
        and ((et_now.hour == 9 and et_now.minute >= 30) or et_now.hour >= 10)
        and et_now.hour < 16
    )
    if not is_market_hours:
        logger.info(f"  Market closed ({et_now.strftime('%H:%M ET')}). "
                     f"Monitoring only — no orders will be placed.")

    # Step 1: Exit positions
    logger.info(f"\n--- STEP 1: CHECK EXITS ---")
    if is_market_hours and not args.dry:
        cmd_exit(args)
    else:
        live = load_live_positions()
        if live["positions"]:
            logger.info(f"  {len(live['positions'])} open positions (exit check skipped — {'dry run' if args.dry else 'market closed'})")
        else:
            logger.info("  No open positions.")

    # Step 2: Scan for signals
    logger.info(f"\n--- STEP 2: SCAN SIGNALS ---")
    signals = cmd_scan(args)

    # Step 3: Enter trades
    logger.info(f"\n--- STEP 3: ENTER TRADES ---")
    if signals and is_market_hours and not args.dry:
        cmd_enter(args)
    elif signals and args.dry:
        logger.info(f"  {len(signals)} signals found (dry run — no orders)")
    elif signals and not is_market_hours:
        logger.info(f"  {len(signals)} signals found (market closed — no orders)")
    else:
        logger.info("  No signals to enter.")

    # Step 4: Monitor
    logger.info(f"\n--- STEP 4: MONITOR ---")
    live = load_live_positions()
    if live["positions"]:
        if env.get("ALPACA_API_KEY"):
            cmd_monitor(args)
        else:
            logger.info(f"  {len(live['positions'])} positions (no Alpaca key for live P&L)")
    else:
        logger.info("  No open positions.")

    # Step 5: EOD summary
    logger.info(f"\n--- STEP 5: EOD SUMMARY ---")
    if live["positions"] and env.get("ALPACA_API_KEY"):
        cmd_eod(args)
    else:
        logger.info(f"  Positions: {len(live['positions'])}")
        deployed = sum(p['capital_per_leg'] * 2 for p in live['positions'])
        logger.info(f"  Deployed: ${deployed:,.0f} / ${TOTAL_CAPITAL:,.0f}")

    logger.info(f"\n{'='*60}")
    logger.info(f"DAILY RUN COMPLETE — {datetime.now(ET).strftime('%H:%M ET')}")
    logger.info(f"{'='*60}")


# ── Main ──────────────────────────────────────────────────────────────────────

def main():
    parser = argparse.ArgumentParser(
        description="OpenQuant Live Trading Pipeline — single entry point",
        epilog="All trades must pass quality gates identical to the backtest. No manual overrides."
    )
    sub = parser.add_subparsers(dest="command")

    run_parser = sub.add_parser("run", help="Full daily cycle: exit → scan → enter → monitor → eod")
    run_parser.add_argument("--dry", action="store_true", help="Signal watching, no orders")

    sub.add_parser("monitor", help="Quick position check (no orders)")
    sub.add_parser("scan", help="Scan only (no orders)")

    exit_parser = sub.add_parser("exit", help="Close reverted/max-hold positions only")
    sub.add_parser("eod", help="End of day summary only")

    args = parser.parse_args()

    if args.command is None:
        # Default to 'run --dry' if no command given
        args.command = "run"
        args.dry = True
        logger.info("No command specified — defaulting to 'run --dry'")

    commands = {
        "run": cmd_run,
        "scan": cmd_scan,
        "monitor": cmd_monitor,
        "exit": cmd_exit,
        "eod": cmd_eod,
    }
    commands[args.command](args)


if __name__ == "__main__":
    main()
