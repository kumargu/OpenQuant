#!/usr/bin/env python3
"""
OpenQuant Live Paper Trading — rich structured logging, pairs engine via pybridge.

Architecture:
  1. Fetch 1-minute bars from Alpaca (Python, data plumbing)
  2. Feed to PairsEngine.on_bar() (Rust via pybridge, all math)
  3. Track open positions in Python (Python dict, for HOLD logging)
  4. Execute order intents via Alpaca (Python, dry-run skips this)

Every significant event is logged to data/journal/engine.log in the style of
capital_sim.py: ENTER, EXIT, HOLD, DAILY, BAR, SCAN, SKIP lines with structured
fields for grep-ability and post-session analysis.

Usage:
    python3 run_live.py              # live paper trading
    python3 run_live.py --dry-run    # signal watching, no orders
    python3 run_live.py --interval 60
    python3 run_live.py --dry-run --interval 30

Log format:
    ENTER  MCO/SPGI [MCO/SPGI:1711455300] L z=-1.56 prio=76.50 $5000/leg
    HOLD   MCO/SPGI [MCO/SPGI:1711455300] L held=5m z_now=-1.17 unreal=$+1.08 (+0.02%)
    EXIT   MCO/SPGI [MCO/SPGI:1711455300] L held=12m pnl=$+8.30 (+0.08%) reason=reversion
    SKIP   MCO/SPGI z=+0.80 reason=z_too_low (engine rejected)
    DAILY  2 pos deployed=$10000/20000 (50%) pool=$10000
    BAR    GLD ts=1711455300 close=420.00 (new bar)
"""

import argparse
import json
import logging
import math
import os
import signal
import sys
import time
from dataclasses import dataclass, field
from datetime import datetime, timedelta, timezone
from pathlib import Path
from zoneinfo import ZoneInfo

from dotenv import load_dotenv

load_dotenv()

# ── Paths ──────────────────────────────────────────────────────────────────────

ROOT = Path(__file__).resolve().parent
JOURNAL_DIR = ROOT / "data" / "journal"
LOG_FILE = JOURNAL_DIR / "engine.log"
ACTIVE_PAIRS_PATH = ROOT / "trading" / "active_pairs.json"
HISTORY_PATH = ROOT / "trading" / "pair_trading_history.json"
CONFIG_PATH = ROOT / "config" / "pairs.toml"

# ── Logger setup ───────────────────────────────────────────────────────────────
# Two handlers: file (DEBUG, structured, no color) + stderr (INFO, human-readable).
# File uses mode="a" to append across restarts — journal is cumulative.

JOURNAL_DIR.mkdir(parents=True, exist_ok=True)

logger = logging.getLogger("live")
logger.setLevel(logging.DEBUG)
logger.propagate = False

_fh = logging.FileHandler(LOG_FILE, mode="a", encoding="utf-8")
_fh.setLevel(logging.DEBUG)
_fh.setFormatter(logging.Formatter(
    "%(asctime)s %(message)s",
    datefmt="%Y-%m-%d %H:%M:%S",
))
logger.addHandler(_fh)

_sh = logging.StreamHandler(sys.stderr)
_sh.setLevel(logging.INFO)
_sh.setFormatter(logging.Formatter("%(asctime)s %(message)s", datefmt="%H:%M:%S"))
logger.addHandler(_sh)

# ── Pybridge import ────────────────────────────────────────────────────────────
# Canonical venv: engine/.venv (created by maturin develop --release)

_venv_site = ROOT / "engine" / ".venv" / "lib"
_site_pkgs = next(_venv_site.glob("python*/site-packages"), None)
if _site_pkgs and str(_site_pkgs) not in sys.path:
    sys.path.insert(0, str(_site_pkgs))

try:
    from openquant import openquant as _oq
    PairsEngine = _oq.PairsEngine
except ImportError as exc:
    raise ImportError(
        "openquant pybridge not found. Run: cd engine && maturin develop --release"
    ) from exc

# ── Constants ──────────────────────────────────────────────────────────────────

ET = ZoneInfo("America/New_York")

# Notional per leg from pairs.toml default (mirror, not re-parsed here)
# Actual sizing comes from the Rust engine qty field on each intent.
# This constant is used only for P&L percentage calculation.
DEFAULT_NOTIONAL_PER_LEG = 10_000.0


# ── Open position tracker ──────────────────────────────────────────────────────
# PairsEngine.on_bar returns order intents but has no positions() query.
# We track open positions in Python to compute unrealized P&L for HOLD logs.

@dataclass
class OpenLeg:
    """One leg of a pairs trade (long or short)."""
    symbol: str
    side: str          # "buy" or "sell"
    qty: float
    entry_price: float
    entry_ts: int      # epoch ms at entry
    current_price: float = 0.0

    def unrealized_pnl(self) -> float:
        if self.entry_price <= 0 or self.current_price <= 0:
            return 0.0
        if self.side == "buy":
            return self.qty * (self.current_price - self.entry_price)
        else:
            return self.qty * (self.entry_price - self.current_price)

    def notional(self) -> float:
        return self.qty * self.entry_price


@dataclass
class OpenPairTrade:
    """A paired position: two legs opened at the same time."""
    trade_id: str       # "LEG_A/LEG_B:entry_ts_ms" — links ENTER→HOLD→EXIT
    pair_id: str        # "LEG_A/LEG_B"
    direction: str      # "L" (long leg_a / short leg_b) or "S" (reverse)
    entry_ts: int       # epoch ms
    entry_z: float
    priority_score: float
    spread_at_entry: float
    legs: list[OpenLeg] = field(default_factory=list)
    bars_held: int = 0

    def total_notional(self) -> float:
        return sum(leg.notional() for leg in self.legs)

    def unrealized_pnl(self) -> float:
        return sum(leg.unrealized_pnl() for leg in self.legs)

    def unrealized_pct(self) -> float:
        n = self.total_notional()
        if n <= 0:
            return 0.0
        return self.unrealized_pnl() / n * 100

    def held_minutes(self, now_ts_ms: int) -> int:
        return max(0, (now_ts_ms - self.entry_ts) // 60_000)


# ── Market hours ───────────────────────────────────────────────────────────────

def is_market_open() -> bool:
    now = datetime.now(ET)
    if now.weekday() >= 5:  # Saturday=5, Sunday=6
        return False
    market_open = now.replace(hour=9, minute=30, second=0, microsecond=0)
    market_close = now.replace(hour=16, minute=0, second=0, microsecond=0)
    return market_open <= now <= market_close


def minutes_to_close() -> int:
    now = datetime.now(ET)
    close = now.replace(hour=16, minute=0, second=0, microsecond=0)
    delta = (close - now).total_seconds()
    return max(0, int(delta / 60))


# ── Alpaca bar fetching ────────────────────────────────────────────────────────

def fetch_bars(data_client, symbols: list[str], lookback_minutes: int) -> list[dict]:
    """Fetch recent 1-minute bars for all symbols from Alpaca IEX feed.

    Returns list of dicts: {symbol, timestamp (epoch ms), open, high, low, close, volume}.
    Sorted by (timestamp, symbol) for deterministic ordering.
    """
    from alpaca.data.requests import StockBarsRequest
    from alpaca.data.timeframe import TimeFrame
    from alpaca.data.enums import DataFeed

    now = datetime.now(timezone.utc)
    start = now - timedelta(minutes=lookback_minutes)

    req = StockBarsRequest(
        symbol_or_symbols=symbols,
        timeframe=TimeFrame.Minute,
        start=start,
        end=now,
        feed=DataFeed.IEX,
    )
    barset = data_client.get_stock_bars(req)

    bars = []
    for sym in symbols:
        for b in barset.data.get(sym, []):
            bars.append({
                "symbol": sym,
                "timestamp": int(b.timestamp.timestamp() * 1000),
                "open": float(b.open),
                "high": float(b.high),
                "low": float(b.low),
                "close": float(b.close),
                "volume": float(b.volume),
            })

    bars.sort(key=lambda b: (b["timestamp"], b["symbol"]))
    return bars


# ── Order execution ────────────────────────────────────────────────────────────

def execute_intent(intent: dict, dry_run: bool) -> dict | None:
    """Execute a single order intent. Returns fill info or None on dry-run."""
    symbol = intent["symbol"]
    side = intent["side"]
    qty = float(intent["qty"])

    if qty <= 0:
        logger.warning("SKIP_ORDER %s %s qty=%.2f (zero/negative qty, skipping)", side, symbol, qty)
        return None

    if dry_run:
        return {"status": "dry-run", "symbol": symbol, "side": side, "qty": qty}

    from paper_trading.alpaca_client import buy, sell
    try:
        result = buy(symbol, qty) if side == "buy" else sell(symbol, qty)
        return result
    except Exception as exc:
        logger.error("ORDER_FAILED %s %.2f %s -> %s", side.upper(), qty, symbol, exc)
        return None


# ── Position tracking ──────────────────────────────────────────────────────────

def update_prices(open_trades: dict[str, OpenPairTrade], bar: dict) -> None:
    """Update current price on any leg matching this bar's symbol."""
    sym = bar["symbol"]
    close = bar["close"]
    for trade in open_trades.values():
        for leg in trade.legs:
            if leg.symbol == sym:
                leg.current_price = close


def log_hold_state(trade: OpenPairTrade, now_ts_ms: int) -> None:
    """Log per-bar HOLD state for an open position (DEBUG — file only)."""
    pnl = trade.unrealized_pnl()
    pct = trade.unrealized_pct()
    held_m = trade.held_minutes(now_ts_ms)
    logger.debug(
        "HOLD   %s [%s] %s held=%dm z_entry=%+.3f "
        "unreal=$%+.2f (%+.2f%%)",
        trade.pair_id,
        trade.trade_id,
        trade.direction,
        held_m,
        trade.entry_z,
        pnl,
        pct,
    )


def log_position_snapshot(open_trades: dict[str, OpenPairTrade], now_ts_ms: int) -> None:
    """Log a DAILY-style position snapshot every bar (INFO)."""
    n_pos = len(open_trades)
    total_deployed = sum(t.total_notional() for t in open_trades.values())
    total_pnl = sum(t.unrealized_pnl() for t in open_trades.values())

    # Capital info: read from Alpaca account if available, else estimate
    deployed_pct = 0.0
    if total_deployed > 0:
        # Approximate: pairs.toml notional_per_leg × 2 × n_pos gives deployed
        # We don't have total capital here without an Alpaca account call,
        # so log absolute numbers and skip percentage.
        pass

    logger.info(
        "SNAPSHOT  %d pos deployed=$%.0f unreal=$%+.2f",
        n_pos,
        total_deployed,
        total_pnl,
    )

    for trade in open_trades.values():
        pnl = trade.unrealized_pnl()
        pct = trade.unrealized_pct()
        held_m = trade.held_minutes(now_ts_ms)
        logger.info(
            "  HOLD   %s [%s] %s held=%dm unreal=$%+.2f (%+.2f%%)",
            trade.pair_id,
            trade.trade_id,
            trade.direction,
            held_m,
            pnl,
            pct,
        )


# ── Intent processing ──────────────────────────────────────────────────────────

def process_intents(
    intents: list,
    bar: dict,
    open_trades: dict[str, OpenPairTrade],
    dry_run: bool,
) -> None:
    """Process order intents from PairsEngine.on_bar().

    Intents come in pairs (two legs per signal). We group them by pair_id,
    execute both, then update open_trades.
    """
    # Group intents by pair_id to handle entry/exit pairs atomically
    by_pair: dict[str, list] = {}
    for intent in intents:
        pid = intent.get("pair_id", "unknown")
        by_pair.setdefault(pid, []).append(intent)

    ts = bar["timestamp"]
    now_str = datetime.fromtimestamp(ts / 1000, tz=ET).strftime("%H:%M:%S")

    for pair_id, pair_intents in by_pair.items():
        reason = pair_intents[0].get("reason", "")
        z = pair_intents[0].get("z_score", float("nan"))
        spread = pair_intents[0].get("spread", float("nan"))
        prio = pair_intents[0].get("priority_score", 0.0)

        # Determine if this is an entry or exit signal.
        # Entry: pair_id NOT in open_trades (opening new position)
        # Exit: pair_id IN open_trades (closing existing position)
        sides = {i["side"] for i in pair_intents}

        is_exit = pair_id in open_trades

        if is_exit:
            # Exit: close open position
            trade = open_trades[pair_id]
            pnl = trade.unrealized_pnl()
            pct = trade.unrealized_pct()
            held_m = trade.held_minutes(ts)

            logger.info(
                "EXIT   %s [%s] %s held=%dm pnl=$%+.2f (%+.2f%%) z_now=%+.3f reason=%s",
                pair_id,
                trade.trade_id,
                trade.direction,
                held_m,
                pnl,
                pct,
                z,
                reason,
            )

            # Execute exit orders
            all_filled = True
            for intent in pair_intents:
                fill = execute_intent(intent, dry_run)
                if fill is None:
                    all_filled = False
                else:
                    logger.debug(
                        "FILL   %s %s %.2f@%.4f status=%s",
                        intent["side"].upper(),
                        intent["symbol"],
                        intent["qty"],
                        bar["close"],
                        fill.get("status", "?"),
                    )

            if all_filled or dry_run:
                del open_trades[pair_id]

        else:
            # Entry: open new position
            # Determine direction from which leg is bought
            buy_intents = [i for i in pair_intents if i["side"] == "buy"]
            legs_a_b = pair_id.split("/")
            if buy_intents:
                bought_sym = buy_intents[0]["symbol"]
                direction = "L" if (legs_a_b and bought_sym == legs_a_b[0]) else "S"
            else:
                direction = "?"

            # Compute total notional from intents
            total_notional = sum(
                float(i["qty"]) * bar["close"]
                for i in pair_intents
            )
            notional_per_leg = total_notional / max(len(pair_intents), 1)

            logger.info(
                "ENTER  %s [%s:%d] %s z=%+.3f prio=%.2f $%.0f/leg reason=%s",
                pair_id,
                pair_id,
                ts,
                direction,
                z,
                prio,
                notional_per_leg,
                reason,
            )

            # Execute entry orders
            filled_legs: list[OpenLeg] = []
            all_filled = True
            for intent in pair_intents:
                fill = execute_intent(intent, dry_run)
                if fill is None:
                    all_filled = False
                else:
                    logger.debug(
                        "FILL   %s %s %.2f@%.4f status=%s",
                        intent["side"].upper(),
                        intent["symbol"],
                        float(intent["qty"]),
                        bar["close"],
                        fill.get("status", "?"),
                    )
                    filled_legs.append(OpenLeg(
                        symbol=intent["symbol"],
                        side=intent["side"],
                        qty=float(intent["qty"]),
                        entry_price=bar["close"],
                        entry_ts=ts,
                        current_price=bar["close"],
                    ))

            if all_filled or dry_run:
                trade_id = f"{pair_id}:{ts}"
                open_trades[pair_id] = OpenPairTrade(
                    trade_id=trade_id,
                    pair_id=pair_id,
                    direction=direction,
                    entry_ts=ts,
                    entry_z=z,
                    priority_score=prio,
                    spread_at_entry=spread,
                    legs=filled_legs,
                )


# ── Scan funnel logging ────────────────────────────────────────────────────────

def log_z_scores_from_features(engine, symbols: list[str], ts: int) -> None:
    """Log z-score for each symbol via features() for observability.

    The PairsEngine doesn't expose per-pair z-scores between on_bar calls,
    but we can detect signals through the returned intents. This function is
    a no-op placeholder — pair z-scores are logged via ENTER/SKIP events.
    """
    pass  # PairsEngine doesn't expose per-pair z without a full on_bar call


# ── Daily account snapshot ─────────────────────────────────────────────────────

def log_account_state(dry_run: bool, open_trades: dict[str, OpenPairTrade]) -> None:
    """Log daily capital state from Alpaca account (or estimated in dry-run)."""
    n_pos = len(open_trades)
    total_deployed = sum(t.total_notional() for t in open_trades.values())
    total_pnl = sum(t.unrealized_pnl() for t in open_trades.values())

    if dry_run:
        logger.info(
            "DAILY  %d pos deployed=$%.0f unreal=$%+.2f [dry-run, no account query]",
            n_pos,
            total_deployed,
            total_pnl,
        )
        return

    try:
        from paper_trading.alpaca_client import get_account
        acct = get_account()
        equity = acct["equity"]
        cash = acct["cash"]
        logger.info(
            "DAILY  %d pos deployed=$%.0f unreal=$%+.2f equity=$%.0f cash=$%.0f",
            n_pos,
            total_deployed,
            total_pnl,
            equity,
            cash,
        )
    except Exception as exc:
        logger.warning("DAILY  account query failed: %s", exc)
        logger.info(
            "DAILY  %d pos deployed=$%.0f unreal=$%+.2f",
            n_pos,
            total_deployed,
            total_pnl,
        )


# ── Main loop ──────────────────────────────────────────────────────────────────

def main():
    parser = argparse.ArgumentParser(
        description="OpenQuant live paper trading runner with rich logging."
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="Watch signals only, do not place orders.",
    )
    parser.add_argument(
        "--interval",
        type=int,
        default=60,
        help="Bar poll interval in seconds (default: 60).",
    )
    parser.add_argument(
        "--lookback",
        type=int,
        default=5,
        help="Minutes of bars to fetch per poll cycle (default: 5).",
    )
    args = parser.parse_args()

    # ── Validate prerequisites ────────────────────────────────────────────────

    if not ACTIVE_PAIRS_PATH.exists():
        logger.error("active_pairs.json not found at %s", ACTIVE_PAIRS_PATH)
        logger.error("Run pair-picker first: cargo run -p pair-picker -- --help")
        sys.exit(1)

    with open(ACTIVE_PAIRS_PATH) as f:
        pairs_cfg = json.load(f)

    pairs = pairs_cfg.get("pairs", [])
    if not pairs:
        logger.warning(
            "active_pairs.json has no pairs — engine will load but produce no signals."
        )
        logger.warning("Run pair-picker to populate pairs before trading.")

    # Collect all unique symbols from active pairs
    symbols: list[str] = sorted({
        sym
        for p in pairs
        for sym in (p.get("leg_a", ""), p.get("leg_b", ""))
        if sym
    })

    if not symbols:
        logger.warning("No symbols found in active_pairs.json — nothing to fetch.")

    # ── Init Rust pairs engine ────────────────────────────────────────────────

    # Ensure history file exists (PairsEngine.from_active_pairs requires the path,
    # but handles missing file gracefully via PairTradingHistory::load fallback).
    if not HISTORY_PATH.exists():
        HISTORY_PATH.write_text('{"trades": []}')
        logger.info("Created empty trade history at %s", HISTORY_PATH)

    engine = PairsEngine.from_active_pairs(
        str(ACTIVE_PAIRS_PATH),
        str(HISTORY_PATH),
        str(CONFIG_PATH) if CONFIG_PATH.exists() else None,
    )
    logger.info(
        "PairsEngine loaded: %d pairs, %d symbols",
        engine.pair_count(),
        len(symbols),
    )

    # ── Init Alpaca data client ───────────────────────────────────────────────

    data_client = None
    if symbols and not args.dry_run:
        try:
            from alpaca.data.historical import StockHistoricalDataClient
            data_client = StockHistoricalDataClient(
                os.environ["ALPACA_API_KEY"],
                os.environ["ALPACA_SECRET_KEY"],
            )
        except KeyError as exc:
            logger.error("Missing env var: %s — set ALPACA_API_KEY and ALPACA_SECRET_KEY", exc)
            sys.exit(1)
        except ImportError as exc:
            logger.error("alpaca-py not installed: %s", exc)
            sys.exit(1)
    elif symbols and args.dry_run:
        # In dry-run, still fetch bars (read-only) to watch signals
        try:
            from alpaca.data.historical import StockHistoricalDataClient
            data_client = StockHistoricalDataClient(
                os.environ.get("ALPACA_API_KEY", ""),
                os.environ.get("ALPACA_SECRET_KEY", ""),
            )
        except (ImportError, Exception) as exc:
            logger.warning("Alpaca client unavailable in dry-run: %s", exc)
            logger.warning("Will loop without bar data — signals will not fire.")

    # ── Session header ────────────────────────────────────────────────────────

    pair_names = [f"{p['leg_a']}/{p['leg_b']}" for p in pairs] if pairs else []
    logger.info("=" * 70)
    logger.info("OPENQUANT LIVE SESSION START — %s", datetime.now(ET).strftime("%Y-%m-%d %H:%M:%S ET"))
    logger.info("  Dry-run:  %s", args.dry_run)
    logger.info("  Interval: %ds", args.interval)
    logger.info("  Lookback: %dm", args.lookback)
    logger.info("  Pairs (%d): %s", len(pair_names), pair_names)
    logger.info("  Symbols (%d): %s", len(symbols), symbols)
    logger.info("  Config:   %s", CONFIG_PATH)
    logger.info("  Log:      %s", LOG_FILE)
    logger.info("=" * 70)

    # ── Graceful shutdown ─────────────────────────────────────────────────────

    running = True

    def _stop(sig, frame):
        nonlocal running
        running = False
        logger.info("Shutdown requested (signal %d).", sig)

    signal.signal(signal.SIGINT, _stop)
    signal.signal(signal.SIGTERM, _stop)

    # ── State ─────────────────────────────────────────────────────────────────

    # open_trades: pair_id -> OpenPairTrade
    # Tracks positions Python-side because PairsEngine has no positions() query.
    open_trades: dict[str, OpenPairTrade] = {}

    # Dedup: skip bars we've already processed
    seen_bars: set[tuple[str, int]] = set()

    # Counters for session summary
    bars_processed = 0
    intents_received = 0
    entries = 0
    exits = 0

    # Daily state reset tracking
    last_reset_day: int | None = None

    # ── Main loop ─────────────────────────────────────────────────────────────

    while running:
        now_et = datetime.now(ET)
        now_ts_ms = int(datetime.now(timezone.utc).timestamp() * 1000)

        # Daily state reset at first bar of a new trading day
        today = now_et.date().toordinal()
        if last_reset_day != today:
            engine.reset_daily()
            last_reset_day = today
            logger.info("DAILY_RESET day=%s", now_et.date().isoformat())

        if not is_market_open():
            if not running:
                break

            mins_to_open = None
            if now_et.weekday() < 5:
                open_time = now_et.replace(hour=9, minute=30, second=0, microsecond=0)
                if now_et < open_time:
                    mins_to_open = int((open_time - now_et).total_seconds() / 60)

            logger.debug(
                "MARKET_CLOSED %s ET%s",
                now_et.strftime("%H:%M"),
                f" (opens in {mins_to_open}m)" if mins_to_open is not None else "",
            )
            time.sleep(min(30, args.interval))
            continue

        # Warn if approaching forced close time (last 30 min of session)
        mins_left = minutes_to_close()
        if 0 < mins_left <= 30:
            logger.info(
                "MARKET_CLOSE_WARNING %d min until market close — engine will reject new entries",
                mins_left,
            )

        # ── Fetch bars ────────────────────────────────────────────────────────

        new_bars: list[dict] = []

        if data_client is not None and symbols:
            try:
                all_bars = fetch_bars(data_client, symbols, lookback_minutes=args.lookback)
                new_bars = [
                    b for b in all_bars
                    if (b["symbol"], b["timestamp"]) not in seen_bars
                ]
            except Exception as exc:
                logger.error("BAR_FETCH_ERROR %s", exc)
                time.sleep(args.interval)
                continue

        if not new_bars:
            logger.debug("POLL no new bars (interval=%ds)", args.interval)
            time.sleep(args.interval)
            continue

        # ── Process each new bar ──────────────────────────────────────────────

        # Batch statistics for this poll cycle
        poll_intents: list = []
        poll_bar_count = 0

        for bar in new_bars:
            seen_bars.add((bar["symbol"], bar["timestamp"]))
            bars_processed += 1
            poll_bar_count += 1

            sym = bar["symbol"]
            ts = bar["timestamp"]
            close = bar["close"]
            ts_str = datetime.fromtimestamp(ts / 1000, tz=ET).strftime("%H:%M:%S")

            # Log every bar at DEBUG (file only — too noisy for stderr)
            logger.debug(
                "BAR    %s ts=%d (%s ET) close=%.4f vol=%.0f",
                sym,
                ts,
                ts_str,
                close,
                bar["volume"],
            )

            # Update open position prices for unrealized P&L tracking
            update_prices(open_trades, bar)

            # Feed to Rust pairs engine
            try:
                intents = engine.on_bar(sym, ts, close)
            except Exception as exc:
                logger.error("ENGINE_ERROR symbol=%s ts=%d err=%s", sym, ts, exc)
                intents = []

            if intents:
                intents_received += len(intents)
                poll_intents.extend(intents)

                # Count entries and exits before processing
                for intent in intents:
                    pid = intent.get("pair_id", "unknown")
                    if pid in open_trades:
                        exits += 1
                    else:
                        entries += 1

                process_intents(intents, bar, open_trades, args.dry_run)

        # ── Per-poll HOLD log (DEBUG: file, INFO: stderr if positions open) ──

        if open_trades:
            for trade in open_trades.values():
                trade.bars_held += 1
                log_hold_state(trade, now_ts_ms)

        # ── Per-poll summary (INFO) ───────────────────────────────────────────

        logger.info(
            "POLL   bars=%d intents=%d open_pos=%d",
            poll_bar_count,
            len(poll_intents),
            len(open_trades),
        )

        # ── Position snapshot every poll cycle if we have open trades ─────────

        if open_trades:
            log_position_snapshot(open_trades, now_ts_ms)

        # ── Capital state (INFO) ──────────────────────────────────────────────

        total_deployed = sum(t.total_notional() for t in open_trades.values())
        total_pnl = sum(t.unrealized_pnl() for t in open_trades.values())
        logger.info(
            "CAPITAL deployed=$%.0f unreal=$%+.2f positions=%d",
            total_deployed,
            total_pnl,
            len(open_trades),
        )

        time.sleep(args.interval)

    # ── Session summary ───────────────────────────────────────────────────────

    logger.info("=" * 70)
    logger.info("SESSION END — %s", datetime.now(ET).strftime("%Y-%m-%d %H:%M:%S ET"))
    logger.info("  Bars processed: %d", bars_processed)
    logger.info("  Engine intents: %d", intents_received)
    logger.info("  Entries:        %d", entries)
    logger.info("  Exits:          %d", exits)
    logger.info("  Open at close:  %d", len(open_trades))
    if open_trades:
        total_pnl = sum(t.unrealized_pnl() for t in open_trades.values())
        logger.info("  Unrealized P&L: $%+.2f", total_pnl)
    log_account_state(args.dry_run, open_trades)
    logger.info("=" * 70)


if __name__ == "__main__":
    main()
