#!/usr/bin/env python3
"""
Capital-aware pairs simulation.

Instead of fixed slots, manages a capital pool:
- $10K total budget
- Each trade allocates capital_per_leg × 2 from the pool
- When a trade closes, capital returns to pool
- Next day, pool is re-allocated to best available signals
- Capital per leg must be ≥ max stock price (to buy at least 1 share)
- Same pair CAN re-enter immediately if it signals again
"""

import json
import logging
import math
import sys
from dataclasses import dataclass, field
from datetime import datetime
from pathlib import Path

root = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(root / "scripts"))
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
    c = trade.capital_per_leg
    ret_a = (exit_pa - trade.entry_pa) / trade.entry_pa
    ret_b = (exit_pb - trade.entry_pb) / trade.entry_pb
    if trade.direction == 1:
        pnl = c * ret_a - c * trade.params.beta * ret_b
    else:
        pnl = -c * ret_a + c * trade.params.beta * ret_b
    cost = 2 * c * COST_BPS / 10_000
    return pnl - cost


def run_capital_sim(prices, pair_configs, total_capital=TOTAL_CAPITAL):
    """Run simulation with capital pool management."""
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
    logger.info(f"Min trade: ${MIN_TRADE_CAPITAL}/leg, Max per trade: {MAX_PER_TRADE_FRAC*100:.0f}%")
    logger.info("=" * 70)

    for day in range(FORMATION_DAYS, total_bars):
        day_pnl = 0.0
        day_entries = 0
        day_exits = 0

        # ── STEP 1: Check exits ──
        to_close = []
        for i, trade in enumerate(open_trades):
            pa = prices[trade.leg_a][day]
            pb = prices[trade.leg_b][day]
            z = compute_z(trade.params, pa, pb)
            bars_held = day - trade.entry_day

            # Time-decay exit threshold
            decay = min(bars_held / trade.max_hold, 1.0)
            floor = min(0.3, trade.exit_z)
            eff_exit = trade.exit_z - (trade.exit_z - floor) * decay

            reason = None
            if bars_held >= trade.max_hold:
                reason = "max_hold"
            elif trade.direction == 1 and z > -eff_exit:
                reason = "reversion"
            elif trade.direction == -1 and z < eff_exit:
                reason = "reversion"

            if reason:
                pnl = compute_pnl(trade, pa, pb)
                # Return capital to pool
                available_capital += trade.capital_per_leg * 2
                day_pnl += pnl
                day_exits += 1

                logger.info(f"[Day {day:>3}] EXIT:{reason:<8} {trade.pair_id()} "
                           f"{'L' if trade.direction==1 else 'S'} {bars_held}d "
                           f"${pnl:>+7.2f} | freed ${trade.capital_per_leg*2:.0f} | "
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

        # ── STEP 2: Scan for new entries with available capital ──
        if available_capital >= MIN_TRADE_CAPITAL * 2:
            signals = []
            held_pairs = {(t.leg_a, t.leg_b) for t in open_trades}

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
                        continue
                last_beta[pair_key] = params.beta

                # Quality gate
                if params.r2 < MIN_R2_ENTRY or params.half_life > MAX_HL_ENTRY or params.adf_stat > MIN_ADF_ENTRY:
                    continue

                pa = prices[leg_a][day]
                pb = prices[leg_b][day]
                z = compute_z(params, pa, pb)

                entry_z = pcfg.get('entry_z', 1.0)
                if abs(z) > entry_z and abs(z) < entry_z + 1.5:
                    # Capital needed: must afford at least 1 share of each
                    min_capital_leg = max(pa, pb * abs(params.beta), MIN_TRADE_CAPITAL)
                    # Desired capital from config, but cap at pool fraction
                    desired_capital = pcfg.get('capital_per_leg', 500)
                    max_capital = min(
                        total_capital * MAX_PER_TRADE_FRAC,
                        available_capital / 2,  # per leg
                    )
                    actual_capital = max(min(desired_capital, max_capital), min_capital_leg)

                    if actual_capital * 2 <= available_capital:
                        signals.append((leg_a, leg_b, params, z, pa, pb, pcfg, actual_capital))

            # Sort by |z| descending, allocate greedily
            signals.sort(key=lambda x: abs(x[3]), reverse=True)

            for leg_a, leg_b, params, z, pa, pb, pcfg, capital in signals:
                if available_capital < capital * 2:
                    break

                direction = 1 if z < -pcfg.get('entry_z', 1.0) else -1
                trade = Trade(
                    leg_a=leg_a, leg_b=leg_b, params=params,
                    direction=direction, entry_day=day,
                    entry_pa=pa, entry_pb=pb, entry_z=z,
                    capital_per_leg=capital,
                    max_hold=pcfg.get('max_hold', 10),
                    exit_z=pcfg.get('exit_z', 0.3),
                )
                open_trades.append(trade)
                available_capital -= capital * 2
                day_entries += 1

                logger.info(f"[Day {day:>3}] ENTER        {trade.pair_id()} "
                           f"{'L' if direction==1 else 'S'} z={z:+.2f} "
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
            'deployed': round(deployed, 0), 'available': round(available_capital, 0),
            'util': round(util, 1),
            'positions': positions,
        })

        if day_entries > 0 or day_exits > 0:
            logger.debug(f"[Day {day:>3}] DAILY: {len(open_trades)} open, "
                        f"deployed=${deployed:.0f} ({util:.0f}%), "
                        f"pool=${available_capital:.0f}, pnl=${day_pnl:+.2f}")

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


def main():

    prices = json.load(open(root / "data" / "pair_picker_prices.json"))
    portfolio = json.load(open(root / "trading" / "pair_portfolio.json"))
    pair_configs = portfolio['pairs']

    closed, daily = run_capital_sim(prices, pair_configs)

    total_bars = min(len(v) for v in prices.values() if len(v) >= 200)
    last14 = total_bars - 10
    last30 = total_bars - 22

    # ── Results ──
    all_pnl = sum(t.pnl for t in closed)
    recent_30 = [t for t in closed if t.exit_day >= last30]
    recent_14 = [t for t in closed if t.exit_day >= last14]

    # Utilization stats
    last30_daily = [d for d in daily if d['day'] >= last30]
    avg_util = sum(d['util'] for d in last30_daily) / len(last30_daily) if last30_daily else 0
    avg_deployed = sum(d['deployed'] for d in last30_daily) / len(last30_daily) if last30_daily else 0

    logger.info("")
    logger.info("=" * 70)
    logger.info(f"RESULTS — ${TOTAL_CAPITAL:,} capital, {len(pair_configs)} pairs")
    logger.info("=" * 70)
    logger.info(f"ALL TIME: {len(closed)}t ${all_pnl:+,.2f}")
    if recent_30:
        r30_pnl = sum(t.pnl for t in recent_30)
        r30_wins = sum(1 for t in recent_30 if t.pnl > 0)
        logger.info(f"LAST 30d: {len(recent_30)}t {r30_wins}w ({r30_wins/len(recent_30)*100:.0f}%) "
                    f"${r30_pnl:+,.2f} (${r30_pnl/22:+.2f}/day)")
    if recent_14:
        r14_pnl = sum(t.pnl for t in recent_14)
        r14_wins = sum(1 for t in recent_14 if t.pnl > 0)
        logger.info(f"LAST 2wk: {len(recent_14)}t {r14_wins}w ${r14_pnl:+,.2f} (${r14_pnl/10:+.2f}/day)")

    logger.info(f"\nCAPITAL UTILIZATION (last 30d):")
    logger.info(f"  Avg deployed: ${avg_deployed:,.0f} / ${TOTAL_CAPITAL:,} = {avg_util:.0f}%")

    # Daily breakdown last 2 weeks
    logger.info(f"\nDAILY (last 2 weeks):")
    cum = 0
    for d in daily:
        if d['day'] >= last14:
            cum += d['pnl']
            logger.info(f"  Day {d['day']}: {d['open']} open, "
                       f"${d['deployed']:>6,.0f} deployed ({d['util']:>2.0f}%), "
                       f"pnl=${d['pnl']:>+7.2f} cum=${cum:>+7.2f}")

    # Per pair
    pair_pnl = {}
    for t in closed:
        k = f"{t.leg_a}/{t.leg_b}"
        if k not in pair_pnl:
            pair_pnl[k] = {'trades': 0, 'pnl': 0.0, 'wins': 0, 'capital': 0}
        pair_pnl[k]['trades'] += 1
        pair_pnl[k]['pnl'] += t.pnl
        pair_pnl[k]['capital'] = t.capital_used
        if t.pnl > 0:
            pair_pnl[k]['wins'] += 1

    logger.info(f"\nPER PAIR:")
    for pair in sorted(pair_pnl, key=lambda p: pair_pnl[p]['pnl'], reverse=True):
        s = pair_pnl[pair]
        logger.info(f"  {pair:<15} {s['trades']:>2}t {s['wins']:>2}w "
                    f"${s['pnl']:>+7.2f} (${s['capital']:.0f}/leg)")

    # Generate dashboard
    dashboard_path = root / "dashboards" / "capital_dashboard.html"
    dashboard_path.parent.mkdir(exist_ok=True)
    generate_capital_dashboard(closed, daily, dashboard_path)
    print(f"Dashboard: file://{dashboard_path}")
    print(f"Log: {LOG_FILE}")


if __name__ == "__main__":
    main()
