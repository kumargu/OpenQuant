#!/usr/bin/env python3
"""
OpenQuant Live Crypto Pairs Trading — BTC/ETH with rich logging.

Hardcoded pair: BTC/USD vs ETH/USD, $5K per leg, aggressive.
Crypto trades 24/7 — no market hours check.

Usage:
    python3 run_live_crypto.py              # live paper trading
    python3 run_live_crypto.py --dry-run    # watch signals, no orders
    python3 run_live_crypto.py --interval 60
"""

import argparse
import json
import logging
import math
import os
import signal
import sys
import time
from datetime import datetime, timedelta, timezone
from pathlib import Path

from dotenv import load_dotenv
load_dotenv()

# ── Logging: rich, structured, to file + stderr ──────────────────────────────

LOG_FILE = Path("data/journal/engine.log")
LOG_FILE.parent.mkdir(parents=True, exist_ok=True)

logging.basicConfig(
    level=logging.DEBUG,
    format="%(asctime)s %(levelname)-5s %(message)s",
    datefmt="%Y-%m-%d %H:%M:%S",
    handlers=[
        logging.StreamHandler(sys.stderr),
        logging.FileHandler(LOG_FILE, mode="a"),
    ],
)
log = logging.getLogger("live_crypto")

# ── Config ────────────────────────────────────────────────────────────────────

PAIR_A = "BTC/USD"
PAIR_B = "ETH/USD"
CAPITAL_PER_LEG = 5000
ENTRY_Z = 0.8       # aggressive for crypto — higher vol, faster moves
EXIT_Z = 0.2        # tighter exit — take profits quicker
MAX_HOLD_MINUTES = 60 * 4  # 4 hours max hold (crypto moves fast)
LOOKBACK_BARS = 60   # 1 hour of minute bars for z-score
OLS_WINDOW = 60 * 6  # 6 hours of minute bars for OLS (crypto regime changes fast)
COST_BPS = 10  # crypto spread is wider

# ── State ─────────────────────────────────────────────────────────────────────

class LiveState:
    def __init__(self):
        self.bars_a = []  # (timestamp, close)
        self.bars_b = []
        self.position = None  # None or dict with entry info
        self.bar_count = 0
        self.trade_count = 0
        self.total_pnl = 0.0
        self.session_start = datetime.now(timezone.utc)

    def add_bar(self, symbol, timestamp, close):
        if symbol == PAIR_A:
            self.bars_a.append((timestamp, close))
            # Keep last OLS_WINDOW + 100 bars
            if len(self.bars_a) > OLS_WINDOW + 200:
                self.bars_a = self.bars_a[-(OLS_WINDOW + 100):]
        elif symbol == PAIR_B:
            self.bars_b.append((timestamp, close))
            if len(self.bars_b) > OLS_WINDOW + 200:
                self.bars_b = self.bars_b[-(OLS_WINDOW + 100):]
        self.bar_count += 1


def ols_simple(x, y):
    n = min(len(x), len(y))
    if n < 30:
        return None
    x, y = x[-n:], y[-n:]
    mx = sum(x) / n
    my = sum(y) / n
    sxx = sum((xi - mx) ** 2 for xi in x)
    sxy = sum((xi - mx) * (yi - my) for xi, yi in zip(x, y))
    syy = sum((yi - my) ** 2 for yi in y)
    if sxx < 1e-15 or syy < 1e-15:
        return None
    beta = sxy / sxx
    alpha = my - beta * mx
    r2 = 1.0 - sum((yi - alpha - beta * xi) ** 2 for xi, yi in zip(x, y)) / syy
    return alpha, beta, r2


def compute_spread_and_z(state):
    """Compute OLS spread and z-score from current bars."""
    n = min(len(state.bars_a), len(state.bars_b))
    if n < LOOKBACK_BARS:
        return None

    # Align by taking last n bars
    prices_a = [b[1] for b in state.bars_a[-n:]]
    prices_b = [b[1] for b in state.bars_b[-n:]]

    # Guard non-positive
    if any(p <= 0 for p in prices_a) or any(p <= 0 for p in prices_b):
        return None

    log_a = [math.log(p) for p in prices_a]
    log_b = [math.log(p) for p in prices_b]

    # OLS on full window
    ols_n = min(n, OLS_WINDOW)
    result = ols_simple(log_b[-ols_n:], log_a[-ols_n:])
    if result is None:
        return None
    alpha, beta, r2 = result

    # For crypto, beta can be very small (BTC=$70K vs ETH=$2K → log-beta ~0.05)
    # This is fine — it means BTC is less volatile relative to ETH in log terms
    if beta < 0.01 or beta > 100:
        log.debug(f"REJECT beta={beta:.4f} out of range [0.01, 100]")
        return None

    # Spread
    spread = [log_a[i] - alpha - beta * log_b[i] for i in range(n)]

    # Z-score on last LOOKBACK_BARS
    window = spread[-LOOKBACK_BARS:]
    mean = sum(window) / len(window)
    std = math.sqrt(sum((s - mean) ** 2 for s in window) / (len(window) - 1))
    if std < 1e-10:
        return None

    current_spread = spread[-1]
    z = (current_spread - mean) / std

    # Half-life estimate
    ds = [spread[i] - spread[i-1] for i in range(-min(200, len(spread)-1), 0)]
    s_lag = spread[-len(ds)-1:-1]
    if len(ds) >= 30:
        ols_hl = ols_simple(s_lag, ds)
        if ols_hl and ols_hl[1] < 0:
            hl = -math.log(2) / ols_hl[1]
        else:
            hl = 999
    else:
        hl = 999

    return {
        'alpha': alpha, 'beta': beta, 'r2': r2,
        'spread_mean': mean, 'spread_std': std,
        'z': z, 'current_spread': current_spread,
        'half_life': hl,
        'price_a': prices_a[-1], 'price_b': prices_b[-1],
    }


def check_exit(state, params):
    """Check if current position should exit."""
    if state.position is None:
        return None

    pos = state.position
    z = params['z']
    minutes_held = (datetime.now(timezone.utc) - pos['entry_time']).total_seconds() / 60

    # Time-decay exit threshold
    decay = min(minutes_held / MAX_HOLD_MINUTES, 1.0)
    eff_exit = EXIT_Z - (EXIT_Z - 0.2) * decay

    reason = None
    if minutes_held >= MAX_HOLD_MINUTES:
        reason = "max_hold"
    elif pos['direction'] == 1 and z > -eff_exit:
        reason = "reversion"
    elif pos['direction'] == -1 and z < eff_exit:
        reason = "reversion"

    if reason:
        # Compute P&L
        ret_a = (params['price_a'] - pos['entry_price_a']) / pos['entry_price_a']
        ret_b = (params['price_b'] - pos['entry_price_b']) / pos['entry_price_b']
        beta = pos['beta']
        if pos['direction'] == 1:
            pnl = CAPITAL_PER_LEG * ret_a - CAPITAL_PER_LEG * beta * ret_b
        else:
            pnl = -CAPITAL_PER_LEG * ret_a + CAPITAL_PER_LEG * beta * ret_b
        cost = 2 * CAPITAL_PER_LEG * COST_BPS / 10000
        pnl -= cost
        return {'reason': reason, 'pnl': pnl, 'minutes_held': minutes_held,
                'ret_a': ret_a * 100, 'ret_b': ret_b * 100, 'z_exit': z, 'eff_exit': eff_exit}
    return None


def check_entry(state, params):
    """Check if we should enter a trade."""
    if state.position is not None:
        return None

    z = params['z']
    if abs(z) <= ENTRY_Z or abs(z) > ENTRY_Z + 2.0:
        return None

    if params['r2'] < 0.5:
        log.debug(f"REJECT entry: r2={params['r2']:.3f} < 0.5")
        return None

    direction = 1 if z < -ENTRY_Z else -1
    return {'direction': direction, 'z': z}


def execute_order(client, symbol, side, notional, dry_run=True):
    """Execute an order via Alpaca."""
    if dry_run:
        log.info(f"DRY_RUN ORDER: {side} ${notional:.2f} of {symbol}")
        return {"status": "dry_run", "id": "dry_run"}

    from paper_trading.alpaca_client import buy, sell
    try:
        if side == "buy":
            result = buy(symbol, notional, order_type="market", notional=True)
        else:
            result = sell(symbol, notional, order_type="market", notional=True)
        log.info(f"ORDER FILLED: {side} ${notional:.2f} of {symbol} → {result['status']}")
        return result
    except Exception as e:
        log.error(f"ORDER FAILED: {side} ${notional:.2f} of {symbol} → {e}")
        return None


def main():
    parser = argparse.ArgumentParser(description="BTC/ETH Live Pairs Trading")
    parser.add_argument("--dry-run", action="store_true", help="Watch signals, no orders")
    parser.add_argument("--interval", type=int, default=60, help="Poll interval in seconds")
    args = parser.parse_args()

    log.info("=" * 70)
    log.info("OPENQUANT LIVE CRYPTO — BTC/ETH PAIRS")
    log.info(f"  Pair: {PAIR_A} / {PAIR_B}")
    log.info(f"  Capital: ${CAPITAL_PER_LEG}/leg")
    log.info(f"  Entry z: {ENTRY_Z}, Exit z: {EXIT_Z}")
    log.info(f"  Max hold: {MAX_HOLD_MINUTES/60:.0f} hours")
    log.info(f"  Dry run: {args.dry_run}")
    log.info(f"  Interval: {args.interval}s")
    log.info(f"  Log: {LOG_FILE}")
    log.info("=" * 70)

    # Alpaca crypto data client (no auth needed for market data)
    from alpaca.data.historical import CryptoHistoricalDataClient
    from alpaca.data.requests import CryptoBarsRequest
    from alpaca.data.timeframe import TimeFrame

    data_client = CryptoHistoricalDataClient()
    state = LiveState()

    # Backfill: fetch last 24 hours of minute bars
    log.info("Backfilling 24h of minute bars...")
    end = datetime.now(timezone.utc)
    start = end - timedelta(hours=24)
    try:
        request = CryptoBarsRequest(
            symbol_or_symbols=[PAIR_A, PAIR_B],
            timeframe=TimeFrame.Minute,
            start=start, end=end,
        )
        bars = data_client.get_crypto_bars(request)
        for sym in [PAIR_A, PAIR_B]:
            sym_bars = bars.data.get(sym, [])
            for bar in sym_bars:
                state.add_bar(sym, int(bar.timestamp.timestamp() * 1000), float(bar.close))
            log.info(f"  Backfilled {len(sym_bars)} bars for {sym}, last=${float(sym_bars[-1].close):,.2f}" if sym_bars else f"  No bars for {sym}")
    except Exception as e:
        log.error(f"Backfill failed: {e}")

    # Graceful shutdown
    running = True
    def shutdown(sig, frame):
        nonlocal running
        log.info("Shutting down...")
        running = False
    signal.signal(signal.SIGINT, shutdown)
    signal.signal(signal.SIGTERM, shutdown)

    log.info(f"Starting live loop (Ctrl+C to stop)...")

    tick = 0
    while running:
        try:
            # Fetch latest bars
            end = datetime.now(timezone.utc)
            start = end - timedelta(seconds=args.interval + 30)
            request = CryptoBarsRequest(
                symbol_or_symbols=[PAIR_A, PAIR_B],
                timeframe=TimeFrame.Minute,
                start=start, end=end,
            )
            bars = data_client.get_crypto_bars(request)

            new_bars = 0
            for sym in [PAIR_A, PAIR_B]:
                sym_bars = bars.data.get(sym, [])
                for bar in sym_bars:
                    ts = int(bar.timestamp.timestamp() * 1000)
                    # Dedup: only add if newer than last bar
                    existing = state.bars_a if sym == PAIR_A else state.bars_b
                    if not existing or ts > existing[-1][0]:
                        state.add_bar(sym, ts, float(bar.close))
                        new_bars += 1

            # Compute spread and z-score
            params = compute_spread_and_z(state)

            if params is None:
                if tick % 10 == 0:  # Log every 10 ticks when no signal
                    log.debug(f"TICK {tick}: {len(state.bars_a)} A bars, {len(state.bars_b)} B bars, "
                             f"need {LOOKBACK_BARS} for z-score")
                tick += 1
                time.sleep(args.interval)
                continue

            # ── RICH LOGGING: every tick with params ──
            pos_str = "FLAT"
            unrealized = 0
            if state.position:
                pos = state.position
                ret_a = (params['price_a'] - pos['entry_price_a']) / pos['entry_price_a'] * 100
                ret_b = (params['price_b'] - pos['entry_price_b']) / pos['entry_price_b'] * 100
                beta = pos['beta']
                if pos['direction'] == 1:
                    unrealized = CAPITAL_PER_LEG * ret_a/100 - CAPITAL_PER_LEG * beta * ret_b/100
                else:
                    unrealized = -CAPITAL_PER_LEG * ret_a/100 + CAPITAL_PER_LEG * beta * ret_b/100
                mins_held = (datetime.now(timezone.utc) - pos['entry_time']).total_seconds() / 60
                pos_str = (f"{'LONG' if pos['direction']==1 else 'SHORT'} "
                          f"held={mins_held:.0f}m unreal=${unrealized:+.2f} "
                          f"z_entry={pos['entry_z']:+.2f}")

            log.info(f"TICK {tick}: BTC=${params['price_a']:,.2f} ETH=${params['price_b']:,.2f} "
                     f"z={params['z']:+.3f} r2={params['r2']:.3f} beta={params['beta']:.3f} "
                     f"hl={params['half_life']:.1f} std={params['spread_std']:.6f} | "
                     f"{pos_str} | bars={state.bar_count} trades={state.trade_count} "
                     f"session_pnl=${state.total_pnl:+.2f}")

            # ── Check EXIT ──
            if state.position:
                exit_info = check_exit(state, params)
                if exit_info:
                    pos = state.position
                    tid = pos['trace_id']
                    log.info(f"EXIT:{exit_info['reason']:<8} [{tid}] "
                             f"{'LONG' if pos['direction']==1 else 'SHORT'} "
                             f"{exit_info['minutes_held']:.0f}m "
                             f"${exit_info['pnl']:+.2f} "
                             f"z_entry={pos['entry_z']:+.2f} z_exit={exit_info['z_exit']:+.2f} "
                             f"ret_a={exit_info['ret_a']:+.2f}% ret_b={exit_info['ret_b']:+.2f}%")

                    # Execute close orders
                    if pos['direction'] == 1:
                        execute_order(None, PAIR_A, "sell", CAPITAL_PER_LEG, args.dry_run)
                        execute_order(None, PAIR_B, "buy", CAPITAL_PER_LEG * pos['beta'], args.dry_run)
                    else:
                        execute_order(None, PAIR_A, "buy", CAPITAL_PER_LEG, args.dry_run)
                        execute_order(None, PAIR_B, "sell", CAPITAL_PER_LEG * pos['beta'], args.dry_run)

                    state.total_pnl += exit_info['pnl']
                    state.trade_count += 1
                    state.position = None

            # ── Check ENTRY ──
            if state.position is None:
                entry = check_entry(state, params)
                if entry:
                    tid = f"BTC/ETH:{int(datetime.now(timezone.utc).timestamp())}"
                    dir_str = "LONG" if entry['direction'] == 1 else "SHORT"
                    log.info(f"ENTER [{tid}] {dir_str} "
                             f"z={entry['z']:+.3f} r2={params['r2']:.3f} "
                             f"beta={params['beta']:.3f} hl={params['half_life']:.1f} "
                             f"BTC=${params['price_a']:,.2f} ETH=${params['price_b']:,.2f} "
                             f"${CAPITAL_PER_LEG}/leg")

                    # Execute entry orders
                    if entry['direction'] == 1:  # LONG spread: buy BTC, sell ETH
                        execute_order(None, PAIR_A, "buy", CAPITAL_PER_LEG, args.dry_run)
                        execute_order(None, PAIR_B, "sell", CAPITAL_PER_LEG * params['beta'], args.dry_run)
                    else:  # SHORT spread: sell BTC, buy ETH
                        execute_order(None, PAIR_A, "sell", CAPITAL_PER_LEG, args.dry_run)
                        execute_order(None, PAIR_B, "buy", CAPITAL_PER_LEG * params['beta'], args.dry_run)

                    state.position = {
                        'direction': entry['direction'],
                        'entry_price_a': params['price_a'],
                        'entry_price_b': params['price_b'],
                        'entry_z': entry['z'],
                        'beta': params['beta'],
                        'entry_time': datetime.now(timezone.utc),
                        'trace_id': tid,
                    }
                else:
                    if tick % 5 == 0:
                        log.debug(f"NO_SIGNAL: |z|={abs(params['z']):.3f} < {ENTRY_Z} "
                                 f"(need |z|>{ENTRY_Z} for entry)")

            tick += 1
            time.sleep(args.interval)

        except KeyboardInterrupt:
            break
        except Exception as e:
            log.error(f"Loop error: {e}", exc_info=True)
            time.sleep(5)

    # Final state
    log.info("=" * 70)
    log.info(f"SESSION END: {state.trade_count} trades, ${state.total_pnl:+.2f} P&L")
    log.info(f"  Bars processed: {state.bar_count}")
    log.info(f"  Duration: {(datetime.now(timezone.utc) - state.session_start).total_seconds()/60:.0f} minutes")
    log.info("=" * 70)


if __name__ == "__main__":
    main()
