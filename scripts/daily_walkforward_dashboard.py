#!/usr/bin/env python3
"""
Daily walk-forward pairs trading simulation + HTML dashboard.

Picks 2-3 best pairs each day (using a 90-day formation window),
trades with frozen entry-time stats, and generates a rich HTML dashboard.
"""

import json
import logging
import math
import sys
from dataclasses import dataclass, field
from datetime import datetime
from pathlib import Path

# ── Rust bridge (capital metrics math) ───────────────────────────────────────
# All math (RoEC, RoCC, utilization) lives in Rust. Python is orchestration only.
_root = Path(__file__).resolve().parent.parent
_venv_site = _root / "engine" / ".venv" / "lib"
_site_pkgs = next(_venv_site.glob("python*/site-packages"), None)
if _site_pkgs and str(_site_pkgs) not in sys.path:
    sys.path.insert(0, str(_site_pkgs))

try:
    from openquant import openquant as _oq
    _compute_capital_metrics = _oq.compute_capital_metrics
except ImportError:
    # Graceful degradation: capital metrics unavailable (bridge not built)
    _compute_capital_metrics = None

# ── Persistent logger ─────────────────────────────────────────────────────────

LOG_DIR = Path(__file__).resolve().parent.parent / "data" / "journal"
LOG_FILE = LOG_DIR / "walkforward.log"

logger = logging.getLogger("walkforward.sim")
logger.setLevel(logging.DEBUG)
logger.propagate = False

_fh = logging.FileHandler(LOG_FILE, mode="a", encoding="utf-8")
_fh.setLevel(logging.DEBUG)
_fh.setFormatter(logging.Formatter("%(asctime)s %(message)s", datefmt="%Y-%m-%d %H:%M:%S"))
logger.addHandler(_fh)

_sh = logging.StreamHandler(sys.stderr)
_sh.setLevel(logging.INFO)
_sh.setFormatter(logging.Formatter("%(message)s"))
logger.addHandler(_sh)

# ── Config ────────────────────────────────────────────────────────────────────

FORMATION_DAYS = 90       # lookback for pair selection
ENTRY_Z = 2.0             # |z| > this to enter
EXIT_Z = 0.5              # |z| < this to exit (at entry; decays over hold period)
MAX_HOLD = 10             # 10d optimal — shorter hold cuts winners more than losers
MAX_PAIRS = 3             # max simultaneous pairs
CAPITAL_PER_LEG = 10_000  # $ per leg
MIN_R2 = 0.30             # minimum R² for OLS
COST_BPS = 5              # round-trip cost in bps (Alpaca $0 commission, ~3-5 bps bid-ask S&P 500)
MIN_R2_ENTRY = 0.85       # tighter R² for actual entry (scan can be looser)
MAX_HL_ENTRY = 4.0        # tighter HL for entry — faster reversion pairs only
MIN_ADF_ENTRY = -2.5      # tighter ADF for entry (more negative = stronger)
EARNINGS_BLACKOUT = 5     # skip entry if either leg has earnings within ±N trading days


# ── Per-pair portfolio config ─────────────────────────────────────────────────

def load_pair_portfolio():
    """Load per-pair trading config from pair_portfolio.json.
    Returns (defaults_dict, {(leg_a, leg_b): config_dict})."""
    portfolio_path = Path(__file__).resolve().parent.parent / "trading" / "pair_portfolio.json"
    if not portfolio_path.exists():
        return None, {}
    with open(portfolio_path) as f:
        raw = json.load(f)
    defaults = raw.get("defaults", {})
    pair_configs = {}
    for p in raw.get("pairs", []):
        key = (p["leg_a"], p["leg_b"])
        pair_configs[key] = p
    return defaults, pair_configs


def get_pair_config(leg_a, leg_b, pair_configs, defaults):
    """Get config for a specific pair, falling back to defaults."""
    cfg = pair_configs.get((leg_a, leg_b), pair_configs.get((leg_b, leg_a), {}))
    return {
        'capital_per_leg': cfg.get('capital_per_leg', defaults.get('capital_per_leg', CAPITAL_PER_LEG)),
        'max_hold': cfg.get('max_hold', defaults.get('max_hold', MAX_HOLD)),
        'entry_z': cfg.get('entry_z', defaults.get('entry_z', ENTRY_Z)),
        'exit_z': cfg.get('exit_z', defaults.get('exit_z', EXIT_Z)),
    }


# ── Earnings calendar ─────────────────────────────────────────────────────────

def load_earnings_calendar():
    """Load earnings dates from static JSON. Returns {symbol: [day_indices]}."""
    cal_path = Path(__file__).resolve().parent.parent / "data" / "earnings_calendar.json"
    if not cal_path.exists():
        logger.debug("[earnings] No earnings_calendar.json found — filter disabled")
        return {}
    with open(cal_path) as f:
        raw = json.load(f)
    # raw format: {"AAPL": ["2026-01-30", "2026-05-01"], ...}
    # We need to convert dates to day indices in our price series.
    # For now, return raw dates — conversion happens at scan time.
    return raw


def is_near_earnings(symbol, day_idx, total_bars, earnings_cal, blackout=EARNINGS_BLACKOUT):
    """Check if a day index is within ±blackout days of an earnings date."""
    dates = earnings_cal.get(symbol, [])
    if not dates:
        return False
    # Map day_idx to approximate date: assume bars are consecutive trading days
    # starting from ~14 months ago. Day 0 ≈ 2025-01-02, Day 358 ≈ 2026-03-24
    # 359 bars over ~14.5 months. Approximate: day_idx / 252 years from start.
    from datetime import datetime, timedelta
    # End date is approximately today (2026-03-24), bar 0 is ~359 trading days before
    end_date = datetime(2026, 3, 24)
    # Trading days ≈ calendar days * 252/365
    cal_days_back = int(total_bars * 365 / 252)
    start_date = end_date - timedelta(days=cal_days_back)
    # Approximate date for this day_idx
    approx_date = start_date + timedelta(days=int(day_idx * 365 / 252))
    approx_str = approx_date.strftime("%Y-%m-%d")

    for earn_date_str in dates:
        try:
            earn_date = datetime.strptime(earn_date_str, "%Y-%m-%d")
            diff = abs((approx_date - earn_date).days)
            # Convert calendar days to approximate trading days
            trading_diff = int(diff * 252 / 365)
            if trading_diff <= blackout:
                return True
        except ValueError:
            continue
    return False


# ── Data types ────────────────────────────────────────────────────────────────

@dataclass
class PairParams:
    leg_a: str
    leg_b: str
    beta: float
    alpha: float
    spread_mean: float
    spread_std: float
    half_life: float
    r2: float
    adf_stat: float

@dataclass
class OpenTrade:
    pair: PairParams
    direction: int  # +1 long spread, -1 short spread
    entry_day: int
    entry_price_a: float
    entry_price_b: float
    entry_spread: float
    entry_z: float
    # Per-pair config (from pair_portfolio.json or globals)
    trade_capital: float = 0.0   # capital per leg for this trade
    trade_max_hold: int = 10     # max hold days for this trade
    trade_exit_z: float = 0.5    # exit z threshold for this trade

@dataclass
class ClosedTrade:
    leg_a: str
    leg_b: str
    direction: int
    entry_day: int
    exit_day: int
    entry_price_a: float
    exit_price_a: float
    entry_price_b: float
    exit_price_b: float
    pnl_usd: float
    exit_reason: str
    capital_per_leg: float = 0.0  # capital deployed per leg for this trade

@dataclass
class DayRecord:
    day_idx: int
    n_open: int
    n_closed_today: int
    daily_pnl: float
    cumulative_pnl: float
    pairs_scanned: int
    pairs_selected: int
    deployed_capital: float = 0.0  # total capital deployed in open positions that day
    trades_today: list = field(default_factory=list)


# ── Math helpers ──────────────────────────────────────────────────────────────

def ols_simple(x, y):
    """Simple OLS: y = alpha + beta * x. Returns (alpha, beta, r2)."""
    n = len(x)
    if n < 10:
        return None
    mx = sum(x) / n
    my = sum(y) / n
    sxx = sum((xi - mx) ** 2 for xi in x)
    sxy = sum((xi - mx) * (yi - my) for xi, yi in zip(x, y))
    syy = sum((yi - my) ** 2 for yi in y)
    if sxx < 1e-15 or syy < 1e-15:
        return None
    beta = sxy / sxx
    alpha = my - beta * mx
    ss_res = sum((yi - alpha - beta * xi) ** 2 for xi, yi in zip(x, y))
    r2 = 1.0 - ss_res / syy
    return alpha, beta, r2


def estimate_half_life(spread):
    """OU half-life: regress Δs on s_{t-1}."""
    n = len(spread)
    if n < 20:
        return None
    ds = [spread[i] - spread[i - 1] for i in range(1, n)]
    s_lag = spread[:-1]
    result = ols_simple(s_lag, ds)
    if result is None:
        return None
    _, theta, _ = result
    if theta >= 0:
        return None
    hl = -math.log(2) / theta
    if not math.isfinite(hl) or hl <= 0:
        return None
    return hl


def adf_simple(spread):
    """Simplified ADF: just return the t-stat of the lag coefficient."""
    n = len(spread)
    if n < 20:
        return 0.0
    ds = [spread[i] - spread[i - 1] for i in range(1, n)]
    s_lag = spread[:-1]
    result = ols_simple(s_lag, ds)
    if result is None:
        return 0.0
    _, theta, _ = result
    # Approximate se: use residual std / sqrt(sum(s_lag^2))
    alpha, beta, _ = result
    residuals = [ds[i] - alpha - beta * s_lag[i] for i in range(len(ds))]
    rss = sum(r ** 2 for r in residuals)
    se_sq = rss / (len(ds) - 2)
    ss_x = sum((s - sum(s_lag) / len(s_lag)) ** 2 for s in s_lag)
    if ss_x < 1e-15 or se_sq < 1e-15:
        return 0.0
    se_beta = math.sqrt(se_sq / ss_x)
    return theta / se_beta if se_beta > 1e-15 else 0.0


# ── Pair scanning ─────────────────────────────────────────────────────────────

def scan_pair(leg_a, leg_b, prices_a, prices_b, formation_end):
    """Scan one pair using [formation_end-90, formation_end) window."""
    start = max(0, formation_end - FORMATION_DAYS)
    pa = prices_a[start:formation_end]
    pb = prices_b[start:formation_end]
    n = min(len(pa), len(pb))
    if n < FORMATION_DAYS - 5:
        return None

    pa, pb = pa[:n], pb[:n]
    if any(p <= 0 or not math.isfinite(p) for p in pa):
        return None
    if any(p <= 0 or not math.isfinite(p) for p in pb):
        return None

    log_a = [math.log(p) for p in pa]
    log_b = [math.log(p) for p in pb]

    result = ols_simple(log_b, log_a)
    if result is None:
        return None
    alpha, beta, r2 = result
    if r2 < MIN_R2:
        logger.debug(f"[scan] {leg_a}/{leg_b} REJECT r2={r2:.4f} < {MIN_R2}")
        return None

    # FIX #1: Guard negative/near-zero beta — nonsensical relationship
    if beta < 0.1:
        logger.debug(f"[scan] {leg_a}/{leg_b} REJECT beta={beta:.4f} < 0.1 (non-economic)")
        return None

    spread = [log_a[i] - alpha - beta * log_b[i] for i in range(n)]

    hl = estimate_half_life(spread)
    if hl is None or hl < 2.0 or hl > 5.0:
        return None

    adf = adf_simple(spread)
    if adf > -2.0:
        logger.debug(f"[scan] {leg_a}/{leg_b} REJECT adf={adf:.4f} > -2.0")
        return None

    # Z-score on last 30 days of formation window
    window = spread[-30:]
    mean = sum(window) / len(window)
    std = math.sqrt(sum((s - mean) ** 2 for s in window) / (len(window) - 1))
    if std < 1e-10:
        return None

    # FIX #3: Guard against tiny spread_std — if spread volatility is too small
    # relative to the current deviation, the z-score will be extreme and never
    # cross the exit threshold within max_hold days. Require spread_std to be
    # at least 50 bps (0.005 in log space).
    if std < 0.005:
        logger.debug(f"[scan] {leg_a}/{leg_b} REJECT spread_std={std:.6f} < 0.005 "
                     f"(too narrow — z will never revert in max_hold)")
        return None

    return PairParams(
        leg_a=leg_a, leg_b=leg_b,
        beta=beta, alpha=alpha,
        spread_mean=mean, spread_std=std,
        half_life=hl, r2=r2, adf_stat=adf,
    )


def compute_z(params, price_a, price_b):
    """Z-score using frozen formation-window stats."""
    if price_a <= 0 or price_b <= 0:
        return 0.0
    spread = math.log(price_a) - params.alpha - params.beta * math.log(price_b)
    return (spread - params.spread_mean) / params.spread_std


def compute_trade_pnl(trade, exit_price_a, exit_price_b):
    """P&L for a round-trip trade including costs. Uses per-trade capital."""
    capital = trade.trade_capital if trade.trade_capital > 0 else CAPITAL_PER_LEG
    ret_a = (exit_price_a - trade.entry_price_a) / trade.entry_price_a
    ret_b = (exit_price_b - trade.entry_price_b) / trade.entry_price_b
    if trade.direction == 1:  # long spread: long A, short B
        pnl = capital * ret_a - capital * trade.pair.beta * ret_b
    else:  # short spread: short A, long B
        pnl = -capital * ret_a + capital * trade.pair.beta * ret_b
    # Subtract round-trip cost
    cost = 2 * capital * COST_BPS / 10_000
    return pnl - cost


# ── Main simulation ──────────────────────────────────────────────────────────

def run_simulation(prices, candidates):
    """Run daily walk-forward simulation from day 90 onwards.

    If trading/pair_portfolio.json exists, uses per-pair config for
    capital, max_hold, entry_z, exit_z. Otherwise uses global defaults.
    """
    total_bars = min(len(v) for v in prices.values())
    start_day = FORMATION_DAYS

    # Load per-pair portfolio config
    portfolio_defaults, pair_configs = load_pair_portfolio()
    if pair_configs:
        logger.info(f"Portfolio config loaded: {len(pair_configs)} pairs with custom config")

    open_trades: list[OpenTrade] = []
    closed_trades: list[ClosedTrade] = []
    day_records: list[DayRecord] = []
    # FIX #2: Track last known beta per pair to detect instability
    last_beta: dict[tuple[str, str], float] = {}
    cumulative_pnl = 0.0
    earnings_cal = load_earnings_calendar()
    if earnings_cal:
        logger.info(f"Earnings calendar loaded: {len(earnings_cal)} symbols")

    for day in range(start_day, total_bars):
        daily_pnl = 0.0
        trades_today = []

        # ── Check exits on open trades ──
        to_close = []
        for i, trade in enumerate(open_trades):
            pa = prices[trade.pair.leg_a][day]
            pb = prices[trade.pair.leg_b][day]
            z = compute_z(trade.pair, pa, pb)
            bars_held = day - trade.entry_day
            pair_id = f"{trade.pair.leg_a}/{trade.pair.leg_b}"
            dir_str = "LONG" if trade.direction == 1 else "SHORT"
            unrealized = compute_trade_pnl(trade, pa, pb)

            # Per-trade config (from pair_portfolio.json)
            t_max_hold = trade.trade_max_hold
            t_exit_z = trade.trade_exit_z

            # Time-decay exit using per-trade thresholds
            EXIT_Z_FLOOR = min(0.3, t_exit_z)
            decay_frac = min(bars_held / t_max_hold, 1.0)
            effective_exit_z = t_exit_z - (t_exit_z - EXIT_Z_FLOOR) * decay_frac

            logger.debug(f"[Day {day:>3}] [HOLDING     ] {dir_str} {pair_id} | "
                         f"bars_held={bars_held}/{t_max_hold} | z={z:.4f} | "
                         f"exit_thresh={effective_exit_z:.3f} | "
                         f"unrealized=${unrealized:+.2f} | capital=${trade.trade_capital:.0f}/leg")

            reason = None
            if bars_held >= t_max_hold:
                reason = "max_hold"
            elif trade.direction == 1 and z > -effective_exit_z:
                reason = "reversion"
            elif trade.direction == -1 and z < effective_exit_z:
                reason = "reversion"

            if reason:
                pnl = compute_trade_pnl(trade, pa, pb)
                cost = 2 * trade.trade_capital * COST_BPS / 10_000
                ret_a = (pa - trade.entry_price_a) / trade.entry_price_a * 100
                ret_b = (pb - trade.entry_price_b) / trade.entry_price_b * 100

                logger.info(f"[Day {day:>3}] [EXIT:{reason.upper():<7}] {pair_id} {dir_str} | "
                            f"pnl=${pnl:+.2f} | raw_pnl=${pnl + cost:+.2f} | cost=${cost:.2f} | "
                            f"fixed_z={z:.4f} | exit_thresh={effective_exit_z:.3f} | bars_held={bars_held} | "
                            f"ret_a={ret_a:+.2f}% | ret_b={ret_b:+.2f}% | "
                            f"beta={trade.pair.beta:.4f}")

                ct = ClosedTrade(
                    leg_a=trade.pair.leg_a, leg_b=trade.pair.leg_b,
                    direction=trade.direction,
                    entry_day=trade.entry_day, exit_day=day,
                    entry_price_a=trade.entry_price_a, exit_price_a=pa,
                    entry_price_b=trade.entry_price_b, exit_price_b=pb,
                    pnl_usd=pnl, exit_reason=reason,
                    capital_per_leg=trade.trade_capital,
                )
                closed_trades.append(ct)
                trades_today.append(ct)
                daily_pnl += pnl
                to_close.append(i)

        for i in sorted(to_close, reverse=True):
            open_trades.pop(i)

        # ── Scan for new entries (only if we have capacity) ──
        n_selected = 0
        if len(open_trades) < MAX_PAIRS:
            scanned = []
            for leg_a, leg_b in candidates:
                if leg_a not in prices or leg_b not in prices:
                    continue
                # Earnings blackout: skip if either leg near earnings
                if earnings_cal and (
                    is_near_earnings(leg_a, day, total_bars, earnings_cal) or
                    is_near_earnings(leg_b, day, total_bars, earnings_cal)
                ):
                    logger.debug(f"[Day {day:>3}] [EARNINGS    ] {leg_a}/{leg_b} "
                                 f"near earnings window — skipping")
                    continue

                params = scan_pair(leg_a, leg_b, prices[leg_a], prices[leg_b], day)
                if params is None:
                    continue

                # FIX #2: Beta stability — reject if beta changed > 30% from last known
                pair_key = (leg_a, leg_b)
                if pair_key in last_beta:
                    prev = last_beta[pair_key]
                    if prev > 0 and abs(params.beta - prev) / prev > 0.30:
                        logger.debug(f"[Day {day:>3}] [BETA_UNSTABLE] {leg_a}/{leg_b} "
                                     f"beta={params.beta:.4f} prev={prev:.4f} "
                                     f"change={abs(params.beta - prev) / prev * 100:.1f}%")
                        continue
                last_beta[pair_key] = params.beta

                # Quality gate: tighter filters at entry time
                # Log analysis showed winning trades have R²>0.85, HL<4d, ADF<-2.5
                if params.r2 < MIN_R2_ENTRY:
                    logger.debug(f"[Day {day:>3}] [QUALITY_GATE] {leg_a}/{leg_b} "
                                 f"r2={params.r2:.4f} < {MIN_R2_ENTRY} (too weak for entry)")
                    continue
                if params.half_life > MAX_HL_ENTRY:
                    logger.debug(f"[Day {day:>3}] [QUALITY_GATE] {leg_a}/{leg_b} "
                                 f"hl={params.half_life:.2f} > {MAX_HL_ENTRY} (too slow for entry)")
                    continue
                if params.adf_stat > MIN_ADF_ENTRY:
                    logger.debug(f"[Day {day:>3}] [QUALITY_GATE] {leg_a}/{leg_b} "
                                 f"adf={params.adf_stat:.4f} > {MIN_ADF_ENTRY} (weak stationarity)")
                    continue

                # Get per-pair config
                pcfg = get_pair_config(leg_a, leg_b, pair_configs, portfolio_defaults or {})
                p_entry_z = pcfg['entry_z']

                pa = prices[leg_a][day]
                pb = prices[leg_b][day]
                z = compute_z(params, pa, pb)
                if abs(z) > p_entry_z:
                    if abs(z) > p_entry_z + 1.5:  # z-cap relative to entry
                        continue
                    scanned.append((params, z, pa, pb, pcfg))

            n_selected = len(scanned)
            scanned.sort(key=lambda x: abs(x[1]), reverse=True)
            capacity = MAX_PAIRS - len(open_trades)

            if scanned:
                logger.debug(f"[Day {day:>3}] [SCAN        ] {n_selected} signals found, capacity={capacity}")

            held_pairs = {(t.pair.leg_a, t.pair.leg_b) for t in open_trades}
            for params, z, pa, pb, pcfg in scanned:
                if capacity <= 0:
                    break
                if (params.leg_a, params.leg_b) in held_pairs:
                    continue
                direction = 1 if z < -pcfg['entry_z'] else -1
                dir_str = "LONG_SPREAD" if direction == 1 else "SHORT_SPREAD"
                trade = OpenTrade(
                    pair=params, direction=direction,
                    entry_day=day, entry_price_a=pa, entry_price_b=pb,
                    entry_spread=math.log(pa) - params.alpha - params.beta * math.log(pb),
                    entry_z=z,
                    trade_capital=pcfg['capital_per_leg'],
                    trade_max_hold=pcfg['max_hold'],
                    trade_exit_z=pcfg['exit_z'],
                )
                open_trades.append(trade)
                held_pairs.add((params.leg_a, params.leg_b))
                capacity -= 1

                logger.info(f"[Day {day:>3}] [ENTRY       ] {params.leg_a}/{params.leg_b} {dir_str} | "
                            f"z={z:.4f} | beta={params.beta:.4f} | r2={params.r2:.4f} | "
                            f"hl={params.half_life:.2f}d | adf={params.adf_stat:.4f} | "
                            f"capital=${pcfg['capital_per_leg']}/leg | max_hold={pcfg['max_hold']}d | "
                            f"price_a={pa:.2f} | price_b={pb:.2f}")

        cumulative_pnl += daily_pnl
        deployed_today = sum(t.trade_capital * 2 for t in open_trades)
        day_records.append(DayRecord(
            day_idx=day, n_open=len(open_trades),
            n_closed_today=len(trades_today), daily_pnl=daily_pnl,
            cumulative_pnl=cumulative_pnl,
            pairs_scanned=len(candidates), pairs_selected=n_selected,
            deployed_capital=deployed_today,
            trades_today=trades_today,
        ))

    # Force-close remaining
    for trade in open_trades:
        pa = prices[trade.pair.leg_a][total_bars - 1]
        pb = prices[trade.pair.leg_b][total_bars - 1]
        pnl = compute_trade_pnl(trade, pa, pb)
        closed_trades.append(ClosedTrade(
            leg_a=trade.pair.leg_a, leg_b=trade.pair.leg_b,
            direction=trade.direction,
            entry_day=trade.entry_day, exit_day=total_bars - 1,
            entry_price_a=trade.entry_price_a, exit_price_a=pa,
            entry_price_b=trade.entry_price_b, exit_price_b=pb,
            pnl_usd=pnl, exit_reason="eod_force",
            capital_per_leg=trade.trade_capital,
        ))

    return day_records, closed_trades


# ── Dashboard HTML ────────────────────────────────────────────────────────────

def generate_dashboard(day_records, closed_trades, output_path):
    """Generate a self-contained HTML dashboard."""

    # Aggregate stats
    total_pnl = sum(t.pnl_usd for t in closed_trades)
    n_trades = len(closed_trades)
    n_winners = sum(1 for t in closed_trades if t.pnl_usd > 0)
    win_rate = n_winners / n_trades * 100 if n_trades > 0 else 0
    avg_win = sum(t.pnl_usd for t in closed_trades if t.pnl_usd > 0) / max(n_winners, 1)
    n_losers = n_trades - n_winners
    avg_loss = sum(t.pnl_usd for t in closed_trades if t.pnl_usd <= 0) / max(n_losers, 1)
    profit_factor = abs(avg_win * n_winners / (avg_loss * n_losers)) if n_losers > 0 and avg_loss != 0 else float('inf')

    # Daily returns for Sharpe
    daily_pnls = [d.daily_pnl for d in day_records if d.daily_pnl != 0]
    if len(daily_pnls) > 1:
        mean_pnl = sum(daily_pnls) / len(daily_pnls)
        std_pnl = math.sqrt(sum((p - mean_pnl) ** 2 for p in daily_pnls) / (len(daily_pnls) - 1))
        sharpe = mean_pnl / std_pnl * math.sqrt(252) if std_pnl > 0 else 0
    else:
        sharpe = 0

    # Max drawdown
    peak = 0
    max_dd = 0
    for d in day_records:
        if d.cumulative_pnl > peak:
            peak = d.cumulative_pnl
        dd = peak - d.cumulative_pnl
        if dd > max_dd:
            max_dd = dd

    # Per-pair stats
    pair_stats = {}
    for t in closed_trades:
        key = f"{t.leg_a}/{t.leg_b}"
        if key not in pair_stats:
            pair_stats[key] = {"trades": 0, "winners": 0, "pnl": 0.0, "hold_days": []}
        pair_stats[key]["trades"] += 1
        if t.pnl_usd > 0:
            pair_stats[key]["winners"] += 1
        pair_stats[key]["pnl"] += t.pnl_usd
        pair_stats[key]["hold_days"].append(t.exit_day - t.entry_day)

    # Chart data
    cum_pnl_data = json.dumps([round(d.cumulative_pnl, 2) for d in day_records])
    daily_pnl_data = json.dumps([round(d.daily_pnl, 2) for d in day_records])
    n_open_data = json.dumps([d.n_open for d in day_records])
    day_labels = json.dumps([f"Day {d.day_idx}" for d in day_records])

    # Trade table rows
    trade_rows = ""
    for t in closed_trades:
        color = "#22c55e" if t.pnl_usd > 0 else "#ef4444"
        dir_str = "LONG" if t.direction == 1 else "SHORT"
        hold = t.exit_day - t.entry_day
        trade_rows += f"""
        <tr>
          <td>{t.leg_a}/{t.leg_b}</td>
          <td><span style="color:{color};font-weight:600">{dir_str}</span></td>
          <td>Day {t.entry_day}</td>
          <td>Day {t.exit_day}</td>
          <td>{hold}d</td>
          <td style="color:{color};font-weight:700">${t.pnl_usd:+.2f}</td>
          <td>{t.exit_reason}</td>
        </tr>"""

    # Pair breakdown rows
    pair_rows = ""
    for pair, s in sorted(pair_stats.items(), key=lambda x: x[1]["pnl"], reverse=True):
        wr = s["winners"] / s["trades"] * 100 if s["trades"] > 0 else 0
        avg_hold = sum(s["hold_days"]) / len(s["hold_days"]) if s["hold_days"] else 0
        color = "#22c55e" if s["pnl"] > 0 else "#ef4444"
        pair_rows += f"""
        <tr>
          <td style="font-weight:600">{pair}</td>
          <td>{s['trades']}</td>
          <td>{wr:.0f}%</td>
          <td style="color:{color};font-weight:700">${s['pnl']:+.2f}</td>
          <td>{avg_hold:.1f}d</td>
        </tr>"""

    pnl_color = "#22c55e" if total_pnl > 0 else "#ef4444"

    html = f"""<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>Pairs Trading — Walk-Forward Dashboard</title>
<script src="https://cdn.jsdelivr.net/npm/chart.js@4.4.0/dist/chart.umd.min.js"></script>
<style>
  * {{ margin: 0; padding: 0; box-sizing: border-box; }}
  body {{ font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif;
         background: #0f172a; color: #e2e8f0; padding: 24px; }}
  h1 {{ font-size: 28px; font-weight: 700; margin-bottom: 8px; color: #f1f5f9; }}
  h2 {{ font-size: 20px; font-weight: 600; margin: 32px 0 16px; color: #94a3b8; }}
  .subtitle {{ color: #64748b; font-size: 14px; margin-bottom: 32px; }}
  .grid {{ display: grid; grid-template-columns: repeat(auto-fit, minmax(200px, 1fr)); gap: 16px; margin-bottom: 32px; }}
  .card {{ background: #1e293b; border-radius: 12px; padding: 20px; border: 1px solid #334155; }}
  .card .label {{ font-size: 12px; text-transform: uppercase; letter-spacing: 0.05em; color: #64748b; margin-bottom: 4px; }}
  .card .value {{ font-size: 28px; font-weight: 700; }}
  .card .value.green {{ color: #22c55e; }}
  .card .value.red {{ color: #ef4444; }}
  .card .value.blue {{ color: #3b82f6; }}
  .card .value.amber {{ color: #f59e0b; }}
  .chart-container {{ background: #1e293b; border-radius: 12px; padding: 24px; border: 1px solid #334155; margin-bottom: 24px; }}
  canvas {{ width: 100% !important; }}
  table {{ width: 100%; border-collapse: collapse; font-size: 13px; }}
  th {{ text-align: left; padding: 10px 12px; border-bottom: 2px solid #334155; color: #94a3b8;
       font-weight: 600; text-transform: uppercase; letter-spacing: 0.05em; font-size: 11px; }}
  td {{ padding: 8px 12px; border-bottom: 1px solid #1e293b; }}
  tr:hover {{ background: #1e293b; }}
  .table-wrap {{ background: #1e293b; border-radius: 12px; padding: 4px; border: 1px solid #334155; overflow-x: auto; }}
  .two-col {{ display: grid; grid-template-columns: 1fr 1fr; gap: 24px; }}
  @media (max-width: 900px) {{ .two-col {{ grid-template-columns: 1fr; }} }}
</style>
</head>
<body>

<h1>Pairs Trading — Walk-Forward Dashboard</h1>
<p class="subtitle">Daily simulation · {n_trades} trades · Formation: {FORMATION_DAYS}d · Entry |z|>{ENTRY_Z} · Exit |z|<{EXIT_Z} · Max hold {MAX_HOLD}d · {MAX_PAIRS} pairs max · ${CAPITAL_PER_LEG:,.0f}/leg · {COST_BPS}bps cost</p>

<div class="grid">
  <div class="card">
    <div class="label">Total P&L</div>
    <div class="value" style="color:{pnl_color}">${total_pnl:+,.2f}</div>
  </div>
  <div class="card">
    <div class="label">Sharpe Ratio</div>
    <div class="value blue">{sharpe:.2f}</div>
  </div>
  <div class="card">
    <div class="label">Win Rate</div>
    <div class="value {'green' if win_rate > 50 else 'amber'}">{win_rate:.1f}%</div>
  </div>
  <div class="card">
    <div class="label">Total Trades</div>
    <div class="value blue">{n_trades}</div>
  </div>
  <div class="card">
    <div class="label">Profit Factor</div>
    <div class="value {'green' if profit_factor > 1 else 'red'}">{profit_factor:.2f}</div>
  </div>
  <div class="card">
    <div class="label">Max Drawdown</div>
    <div class="value red">${max_dd:,.2f}</div>
  </div>
  <div class="card">
    <div class="label">Avg Win</div>
    <div class="value green">${avg_win:+,.2f}</div>
  </div>
  <div class="card">
    <div class="label">Avg Loss</div>
    <div class="value red">${avg_loss:+,.2f}</div>
  </div>
</div>

<div class="chart-container">
  <h2 style="margin-top:0">Cumulative P&L</h2>
  <canvas id="cumPnlChart" height="100"></canvas>
</div>

<div class="two-col">
  <div class="chart-container">
    <h2 style="margin-top:0">Daily P&L</h2>
    <canvas id="dailyPnlChart" height="120"></canvas>
  </div>
  <div class="chart-container">
    <h2 style="margin-top:0">Open Positions</h2>
    <canvas id="posChart" height="120"></canvas>
  </div>
</div>

<h2>Pair Breakdown</h2>
<div class="table-wrap">
<table>
  <thead><tr><th>Pair</th><th>Trades</th><th>Win Rate</th><th>P&L</th><th>Avg Hold</th></tr></thead>
  <tbody>{pair_rows}</tbody>
</table>
</div>

<h2>All Trades</h2>
<div class="table-wrap">
<table>
  <thead><tr><th>Pair</th><th>Dir</th><th>Entry</th><th>Exit</th><th>Hold</th><th>P&L</th><th>Reason</th></tr></thead>
  <tbody>{trade_rows}</tbody>
</table>
</div>

<script>
const labels = {day_labels};
const cumPnl = {cum_pnl_data};
const dailyPnl = {daily_pnl_data};
const nOpen = {n_open_data};

Chart.defaults.color = '#94a3b8';
Chart.defaults.borderColor = '#334155';

new Chart(document.getElementById('cumPnlChart'), {{
  type: 'line',
  data: {{
    labels: labels,
    datasets: [{{
      label: 'Cumulative P&L ($)',
      data: cumPnl,
      borderColor: '#3b82f6',
      backgroundColor: 'rgba(59,130,246,0.1)',
      fill: true,
      tension: 0.3,
      pointRadius: 0,
      borderWidth: 2.5,
    }}]
  }},
  options: {{
    responsive: true,
    plugins: {{ legend: {{ display: false }} }},
    scales: {{
      x: {{ display: true, ticks: {{ maxTicksLimit: 15, font: {{ size: 10 }} }} }},
      y: {{ grid: {{ color: '#1e293b' }} }}
    }}
  }}
}});

new Chart(document.getElementById('dailyPnlChart'), {{
  type: 'bar',
  data: {{
    labels: labels,
    datasets: [{{
      label: 'Daily P&L ($)',
      data: dailyPnl,
      backgroundColor: dailyPnl.map(v => v >= 0 ? 'rgba(34,197,94,0.7)' : 'rgba(239,68,68,0.7)'),
      borderWidth: 0,
    }}]
  }},
  options: {{
    responsive: true,
    plugins: {{ legend: {{ display: false }} }},
    scales: {{
      x: {{ display: false }},
      y: {{ grid: {{ color: '#1e293b' }} }}
    }}
  }}
}});

new Chart(document.getElementById('posChart'), {{
  type: 'line',
  data: {{
    labels: labels,
    datasets: [{{
      label: 'Open Positions',
      data: nOpen,
      borderColor: '#f59e0b',
      backgroundColor: 'rgba(245,158,11,0.1)',
      fill: true,
      stepped: true,
      pointRadius: 0,
      borderWidth: 2,
    }}]
  }},
  options: {{
    responsive: true,
    plugins: {{ legend: {{ display: false }} }},
    scales: {{
      x: {{ display: false }},
      y: {{ min: 0, max: {MAX_PAIRS + 1}, grid: {{ color: '#1e293b' }} }}
    }}
  }}
}});
</script>

</body>
</html>"""

    Path(output_path).write_text(html)
    print(f"Dashboard written to {output_path}")


# ── Entry point ───────────────────────────────────────────────────────────────

def main():
    root = Path(__file__).resolve().parent.parent
    prices_path = root / "data" / "pair_picker_prices.json"
    candidates_path = root / "trading" / "pair_candidates.json"

    # Session header
    logger.info("")
    logger.info("=" * 80)
    logger.info(f"WALK-FORWARD SIMULATION — {datetime.now().strftime('%Y-%m-%d %H:%M:%S')}")
    logger.info(f"Config: formation={FORMATION_DAYS}d entry_z={ENTRY_Z} exit_z={EXIT_Z} "
                f"max_hold={MAX_HOLD} max_pairs={MAX_PAIRS} capital=${CAPITAL_PER_LEG} cost={COST_BPS}bps")
    logger.info(f"Log file: {LOG_FILE}")
    logger.info("=" * 80)

    with open(prices_path) as f:
        prices = json.load(f)

    with open(candidates_path) as f:
        cands_file = json.load(f)
    candidates = [(p["leg_a"], p["leg_b"]) for p in cands_file["pairs"]]

    logger.info(f"Loaded {len(prices)} symbols, {len(candidates)} candidate pairs, "
                f"{min(len(v) for v in prices.values())} bars/symbol")

    day_records, closed_trades = run_simulation(prices, candidates)

    # Summary
    total_pnl = sum(t.pnl_usd for t in closed_trades)
    n_trades = len(closed_trades)
    n_winners = sum(1 for t in closed_trades if t.pnl_usd > 0)

    logger.info("=" * 60)
    logger.info(f"RESULTS: {n_trades} trades | {n_winners} winners "
                f"({n_winners/n_trades*100:.1f}%) | P&L: ${total_pnl:+,.2f}" if n_trades else "No trades")

    # Capital metrics (Rust) — Gatev et al. 2006 decomposition.
    # RoCC = RoEC × Utilization is the headline metric.
    if _compute_capital_metrics is not None and n_trades > 0:
        trade_inputs = [
            (t.pnl_usd, t.capital_per_leg, float(t.exit_day - t.entry_day))
            for t in closed_trades
            if t.exit_day > t.entry_day and t.capital_per_leg > 0
        ]
        # Use deployed_capital from DayRecord for utilization
        total_cap = float(CAPITAL_PER_LEG * MAX_PAIRS)
        daily_inputs = [
            (total_cap, float(d.deployed_capital))
            for d in day_records
        ]
        n_sim_days = max(len(day_records), 1)
        cm = _compute_capital_metrics(trade_inputs, daily_inputs, total_cap, n_sim_days)
        logger.info(
            f"  RoEC (return on employed):  {cm['roec']*100:+.4f}%/day"
        )
        logger.info(
            f"  Utilization:                {cm['avg_utilization']*100:.1f}%"
        )
        logger.info(
            f"  RoCC (return on committed): {cm['rocc']*100:+.4f}%/day  (= RoEC x Util)"
        )
        logger.info(
            f"  Return per trade:           {cm['avg_return_per_trade']*100:+.3f}%"
        )
        logger.info(
            f"  Opportunity cost (idle $):  ${cm['opportunity_cost']:+,.2f}"
        )

    # Log exit reason breakdown
    reasons = {}
    for t in closed_trades:
        reasons[t.exit_reason] = reasons.get(t.exit_reason, 0) + 1
    logger.info(f"Exit reasons: {reasons}")
    # Log per-pair summary
    pair_pnl = {}
    for t in closed_trades:
        k = f"{t.leg_a}/{t.leg_b}"
        pair_pnl[k] = pair_pnl.get(k, 0) + t.pnl_usd
    for pair in sorted(pair_pnl, key=lambda p: pair_pnl[p]):
        logger.info(f"  {pair:<15} ${pair_pnl[pair]:+.2f}")
    logger.info("=" * 60)

    output = root / "dashboards" / "walkforward_dashboard.html"
    generate_dashboard(day_records, closed_trades, output)
    logger.info(f"Dashboard: file://{output}")
    logger.info(f"Full log:  {LOG_FILE}")
    print(f"\nOpen: file://{output}")
    print(f"Log:  {LOG_FILE}")


if __name__ == "__main__":
    main()
