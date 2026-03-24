#!/usr/bin/env python3
"""
Pattern analysis for pairs trading — find exploitable regularities.

Analyzes spread behavior across days-of-week, holding periods, and
pair characteristics. Outputs an HTML dashboard with visualizations.

The goal: watch the patterns FIRST, apply math AFTER.
"""

import json
import logging
import math
import sys
from collections import defaultdict
from datetime import datetime, timedelta
from pathlib import Path

LOG_DIR = Path(__file__).resolve().parent.parent / "data" / "journal"
LOG_FILE = LOG_DIR / "patterns.log"

logger = logging.getLogger("patterns")
logger.setLevel(logging.DEBUG)
logger.propagate = False
_fh = logging.FileHandler(LOG_FILE, mode="a", encoding="utf-8")
_fh.setLevel(logging.DEBUG)
_fh.setFormatter(logging.Formatter("%(asctime)s %(message)s", datefmt="%Y-%m-%d %H:%M:%S"))
logger.addHandler(_fh)
_sh = logging.StreamHandler(sys.stdout)
_sh.setLevel(logging.INFO)
_sh.setFormatter(logging.Formatter("%(message)s"))
logger.addHandler(_sh)


def ols_simple(x, y):
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
    r2 = 1.0 - sum((yi - alpha - beta * xi) ** 2 for xi, yi in zip(x, y)) / syy
    return alpha, beta, r2


def compute_spread_series(prices_a, prices_b, lookback=90):
    """Compute spread series with rolling OLS beta, return (spreads, betas, r2s)."""
    n = min(len(prices_a), len(prices_b))
    spreads = []
    betas = []
    r2s = []

    for day in range(lookback, n):
        pa = prices_a[day - lookback:day]
        pb = prices_b[day - lookback:day]
        log_a = [math.log(p) for p in pa if p > 0]
        log_b = [math.log(p) for p in pb if p > 0]
        if len(log_a) != lookback or len(log_b) != lookback:
            spreads.append(None)
            betas.append(None)
            r2s.append(None)
            continue

        result = ols_simple(log_b, log_a)
        if result is None:
            spreads.append(None)
            betas.append(None)
            r2s.append(None)
            continue

        alpha, beta, r2 = result
        spread = math.log(prices_a[day]) - alpha - beta * math.log(prices_b[day])
        spreads.append(spread)
        betas.append(beta)
        r2s.append(r2)

    return spreads, betas, r2s


def day_index_to_weekday(day_idx, total_bars):
    """Approximate day index to weekday (0=Mon, 4=Fri)."""
    # End date is ~2026-03-24, total_bars=359
    end_date = datetime(2026, 3, 24)
    cal_days_back = int(total_bars * 365 / 252)
    start_date = end_date - timedelta(days=cal_days_back)
    approx_date = start_date + timedelta(days=int(day_idx * 365 / 252))
    return approx_date.weekday(), approx_date.strftime("%Y-%m-%d")


def analyze_pair(leg_a, leg_b, prices, total_bars):
    """Full pattern analysis for one pair."""
    prices_a = prices[leg_a]
    prices_b = prices[leg_b]
    spreads, betas, r2s = compute_spread_series(prices_a, prices_b)

    if len(spreads) < 50:
        return None

    # Daily spread changes
    spread_changes = []
    for i in range(1, len(spreads)):
        if spreads[i] is not None and spreads[i - 1] is not None:
            spread_changes.append({
                'day_idx': 90 + i,
                'delta': spreads[i] - spreads[i - 1],
                'spread': spreads[i],
                'prev_spread': spreads[i - 1],
            })

    if len(spread_changes) < 30:
        return None

    # Day-of-week analysis
    dow_changes = defaultdict(list)  # weekday -> list of spread deltas
    dow_names = ['Mon', 'Tue', 'Wed', 'Thu', 'Fri']
    for sc in spread_changes:
        wd, _ = day_index_to_weekday(sc['day_idx'], total_bars)
        if 0 <= wd <= 4:
            dow_changes[wd].append(sc['delta'])

    dow_stats = {}
    for wd in range(5):
        deltas = dow_changes[wd]
        if len(deltas) < 5:
            continue
        mean = sum(deltas) / len(deltas)
        std = math.sqrt(sum((d - mean) ** 2 for d in deltas) / (len(deltas) - 1)) if len(deltas) > 1 else 0
        abs_mean = sum(abs(d) for d in deltas) / len(deltas)
        pos_frac = sum(1 for d in deltas if d > 0) / len(deltas)
        # Percentiles
        sorted_d = sorted(deltas)
        p5 = sorted_d[max(0, int(len(sorted_d) * 0.05))]
        p95 = sorted_d[min(len(sorted_d) - 1, int(len(sorted_d) * 0.95))]
        p50 = sorted_d[len(sorted_d) // 2]
        dow_stats[wd] = {
            'name': dow_names[wd], 'n': len(deltas),
            'mean': mean, 'std': std, 'abs_mean': abs_mean,
            'pos_frac': pos_frac, 'p5': p5, 'p50': p50, 'p95': p95,
        }

    # Autocorrelation: does yesterday's spread change predict today's?
    # Negative autocorrelation = mean-reverting!
    deltas = [sc['delta'] for sc in spread_changes]
    if len(deltas) > 10:
        mean_d = sum(deltas) / len(deltas)
        var_d = sum((d - mean_d) ** 2 for d in deltas)
        if var_d > 1e-15:
            # Lag-1 autocorrelation
            cov_1 = sum((deltas[i] - mean_d) * (deltas[i - 1] - mean_d) for i in range(1, len(deltas)))
            autocorr_1 = cov_1 / var_d
            # Lag-2
            cov_2 = sum((deltas[i] - mean_d) * (deltas[i - 2] - mean_d) for i in range(2, len(deltas)))
            autocorr_2 = cov_2 / var_d
        else:
            autocorr_1 = autocorr_2 = 0
    else:
        autocorr_1 = autocorr_2 = 0

    # Holding period P&L curve: if you entered at each spread extreme,
    # what would your P&L be after N days?
    # Use z-score > 2 entries (matching our strategy)
    valid_spreads = [(i, s) for i, s in enumerate(spreads) if s is not None]
    if len(valid_spreads) < 60:
        return None

    vals = [s for _, s in valid_spreads]
    window = 30
    holding_curve = defaultdict(list)  # hold_days -> list of returns

    for idx in range(window, len(valid_spreads)):
        # Rolling z-score
        recent = vals[idx - window:idx]
        mean_s = sum(recent) / len(recent)
        std_s = math.sqrt(sum((s - mean_s) ** 2 for s in recent) / (len(recent) - 1))
        if std_s < 1e-10:
            continue
        z = (vals[idx] - mean_s) / std_s

        if abs(z) > 2.0:
            # Simulate holding for 1-10 days
            direction = 1 if z < -2.0 else -1  # long spread if z<-2, short if z>2
            entry_spread = vals[idx]
            for hold in range(1, 11):
                if idx + hold < len(valid_spreads):
                    exit_spread = vals[idx + hold]
                    # P&L proportional to spread change in favorable direction
                    pnl = direction * (exit_spread - entry_spread)
                    holding_curve[hold].append(pnl)

    hold_stats = {}
    for hold in range(1, 11):
        pnls = holding_curve.get(hold, [])
        if len(pnls) < 5:
            continue
        mean_pnl = sum(pnls) / len(pnls)
        win_rate = sum(1 for p in pnls if p > 0) / len(pnls)
        hold_stats[hold] = {'mean_pnl': mean_pnl, 'win_rate': win_rate, 'n': len(pnls)}

    # Spread distribution stats
    all_deltas = [sc['delta'] for sc in spread_changes]
    sorted_all = sorted(all_deltas)
    skewness = 0
    kurtosis = 0
    if len(all_deltas) > 10:
        m = sum(all_deltas) / len(all_deltas)
        s = math.sqrt(sum((d - m) ** 2 for d in all_deltas) / (len(all_deltas) - 1))
        if s > 1e-15:
            skewness = sum((d - m) ** 3 for d in all_deltas) / (len(all_deltas) * s ** 3)
            kurtosis = sum((d - m) ** 4 for d in all_deltas) / (len(all_deltas) * s ** 4) - 3

    # Beta stability
    valid_betas = [b for b in betas if b is not None]
    beta_cv = 0
    if valid_betas and sum(valid_betas) / len(valid_betas) != 0:
        beta_mean = sum(valid_betas) / len(valid_betas)
        beta_std = math.sqrt(sum((b - beta_mean) ** 2 for b in valid_betas) / max(len(valid_betas) - 1, 1))
        beta_cv = beta_std / abs(beta_mean) if beta_mean != 0 else 0

    valid_r2 = [r for r in r2s if r is not None]
    avg_r2 = sum(valid_r2) / len(valid_r2) if valid_r2 else 0

    return {
        'pair': f"{leg_a}/{leg_b}",
        'n_days': len(spread_changes),
        'dow_stats': dow_stats,
        'autocorr_1': autocorr_1,
        'autocorr_2': autocorr_2,
        'hold_stats': hold_stats,
        'spread_mean_delta': sum(all_deltas) / len(all_deltas),
        'spread_std_delta': math.sqrt(sum((d - sum(all_deltas) / len(all_deltas)) ** 2 for d in all_deltas) / (len(all_deltas) - 1)),
        'skewness': skewness,
        'kurtosis': kurtosis,
        'beta_cv': beta_cv,
        'avg_r2': avg_r2,
        'spread_changes': [sc['delta'] * 10000 for sc in spread_changes],  # in bps for charting
    }


def generate_pattern_dashboard(results, output_path):
    """Generate HTML dashboard with pattern visualizations."""

    # Sort by autocorrelation (most negative = most mean-reverting)
    results.sort(key=lambda r: r['autocorr_1'])

    # Build pair cards
    pair_cards = ""
    for r in results[:20]:  # top 20 most mean-reverting
        # Day of week heatmap data
        dow_data = []
        for wd in range(5):
            s = r['dow_stats'].get(wd, {})
            dow_data.append({
                'name': ['Mon', 'Tue', 'Wed', 'Thu', 'Fri'][wd],
                'abs_move': round(s.get('abs_mean', 0) * 10000, 1),
                'bias': round(s.get('mean', 0) * 10000, 2),
                'n': s.get('n', 0),
                'pos_frac': round(s.get('pos_frac', 0.5) * 100, 0),
            })

        # Holding curve data
        hold_data = []
        for h in range(1, 11):
            hs = r['hold_stats'].get(h, {})
            hold_data.append({
                'days': h,
                'win_rate': round(hs.get('win_rate', 0.5) * 100, 1),
                'mean_pnl_bps': round(hs.get('mean_pnl', 0) * 10000, 1),
                'n': hs.get('n', 0),
            })

        ac1_color = "#22c55e" if r['autocorr_1'] < -0.1 else "#ef4444" if r['autocorr_1'] > 0.1 else "#94a3b8"
        r2_color = "#22c55e" if r['avg_r2'] > 0.85 else "#f59e0b" if r['avg_r2'] > 0.7 else "#ef4444"

        # Best hold day
        best_hold = max(r['hold_stats'].items(), key=lambda x: x[1]['win_rate'])[0] if r['hold_stats'] else "?"

        pair_cards += f"""
    <div class="pair-card">
      <div class="pair-header">
        <h3>{r['pair']}</h3>
        <div class="pair-badges">
          <span class="badge" style="color:{ac1_color}">AC1: {r['autocorr_1']:.3f}</span>
          <span class="badge" style="color:{r2_color}">R²: {r['avg_r2']:.2f}</span>
          <span class="badge">βCV: {r['beta_cv']:.2f}</span>
          <span class="badge">skew: {r['skewness']:.2f}</span>
          <span class="badge">kurt: {r['kurtosis']:.1f}</span>
          <span class="badge">best hold: {best_hold}d</span>
        </div>
      </div>

      <div class="pair-grid">
        <div class="mini-section">
          <h4>Day-of-Week Pattern</h4>
          <table class="mini-table">
            <tr><th>Day</th><th>|Move| bps</th><th>Bias bps</th><th>Up%</th><th>N</th></tr>
            {"".join(f"<tr><td>{d['name']}</td><td>{d['abs_move']}</td><td style='color:{'#22c55e' if d['bias']>0 else '#ef4444'}'>{d['bias']:+.1f}</td><td>{d['pos_frac']:.0f}%</td><td>{d['n']}</td></tr>" for d in dow_data)}
          </table>
        </div>

        <div class="mini-section">
          <h4>Holding Period (after |z|>2 entry)</h4>
          <table class="mini-table">
            <tr><th>Hold</th><th>Win%</th><th>Avg bps</th><th>N</th></tr>
            {"".join(f"<tr style='background:{'rgba(34,197,94,0.1)' if d['win_rate']>55 else 'rgba(239,68,68,0.1)' if d['win_rate']<45 else 'transparent'}'><td>{d['days']}d</td><td>{d['win_rate']:.0f}%</td><td style='color:{'#22c55e' if d['mean_pnl_bps']>0 else '#ef4444'}'>{d['mean_pnl_bps']:+.0f}</td><td>{d['n']}</td></tr>" for d in hold_data)}
          </table>
        </div>
      </div>

      <div class="mini-section">
        <h4>Daily Spread Change Distribution (bps)</h4>
        <canvas id="dist_{r['pair'].replace('/', '_')}" height="60"></canvas>
      </div>
    </div>
"""

    # Summary table
    summary_rows = ""
    for r in results:
        ac_color = "#22c55e" if r['autocorr_1'] < -0.1 else "#ef4444" if r['autocorr_1'] > 0.1 else "#94a3b8"
        best_wr = max((r['hold_stats'].get(h, {}).get('win_rate', 0) for h in range(1, 8)), default=0)
        summary_rows += f"""
        <tr>
          <td style="font-weight:600">{r['pair']}</td>
          <td style="color:{ac_color}">{r['autocorr_1']:.3f}</td>
          <td>{r['autocorr_2']:.3f}</td>
          <td>{r['avg_r2']:.3f}</td>
          <td>{r['beta_cv']:.2f}</td>
          <td>{r['skewness']:+.2f}</td>
          <td>{r['kurtosis']:.1f}</td>
          <td>{r['spread_std_delta']*10000:.1f}</td>
          <td>{best_wr*100:.0f}%</td>
        </tr>"""

    # Chart data for distributions
    chart_scripts = ""
    for r in results[:20]:
        chart_id = r['pair'].replace('/', '_')
        data = json.dumps(r['spread_changes'][-200:])  # last 200 days
        chart_scripts += f"""
    new Chart(document.getElementById('dist_{chart_id}'), {{
      type: 'bar',
      data: {{
        labels: {data}.map((_, i) => i),
        datasets: [{{
          data: {data},
          backgroundColor: {data}.map(v => v >= 0 ? 'rgba(34,197,94,0.6)' : 'rgba(239,68,68,0.6)'),
          borderWidth: 0, barPercentage: 1.0, categoryPercentage: 1.0
        }}]
      }},
      options: {{
        responsive: true,
        plugins: {{ legend: {{ display: false }} }},
        scales: {{ x: {{ display: false }}, y: {{ display: true, ticks: {{ font: {{ size: 9 }} }} }} }}
      }}
    }});
"""

    html = f"""<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<title>Pairs Pattern Analysis</title>
<script src="https://cdn.jsdelivr.net/npm/chart.js@4.4.0/dist/chart.umd.min.js"></script>
<style>
  * {{ margin: 0; padding: 0; box-sizing: border-box; }}
  body {{ font-family: -apple-system, BlinkMacSystemFont, sans-serif;
         background: #0f172a; color: #e2e8f0; padding: 24px; }}
  h1 {{ font-size: 28px; font-weight: 700; margin-bottom: 8px; }}
  h2 {{ font-size: 20px; margin: 32px 0 16px; color: #94a3b8; }}
  h3 {{ font-size: 18px; color: #f1f5f9; }}
  h4 {{ font-size: 13px; color: #64748b; margin-bottom: 8px; text-transform: uppercase; letter-spacing: 0.05em; }}
  .subtitle {{ color: #64748b; font-size: 14px; margin-bottom: 24px; }}
  .pair-card {{ background: #1e293b; border-radius: 12px; padding: 20px; margin-bottom: 20px; border: 1px solid #334155; }}
  .pair-header {{ display: flex; justify-content: space-between; align-items: center; margin-bottom: 16px; flex-wrap: wrap; gap: 8px; }}
  .pair-badges {{ display: flex; gap: 8px; flex-wrap: wrap; }}
  .badge {{ background: #0f172a; padding: 4px 10px; border-radius: 6px; font-size: 12px; font-family: monospace; }}
  .pair-grid {{ display: grid; grid-template-columns: 1fr 1fr; gap: 16px; margin-bottom: 16px; }}
  .mini-section {{ }}
  .mini-table {{ width: 100%; font-size: 12px; border-collapse: collapse; }}
  .mini-table th {{ text-align: left; padding: 4px 8px; color: #64748b; border-bottom: 1px solid #334155; font-size: 10px; text-transform: uppercase; }}
  .mini-table td {{ padding: 4px 8px; border-bottom: 1px solid #1e293b; }}
  .summary-table {{ width: 100%; border-collapse: collapse; font-size: 13px; }}
  .summary-table th {{ text-align: left; padding: 8px; color: #64748b; border-bottom: 2px solid #334155; font-size: 11px; text-transform: uppercase; }}
  .summary-table td {{ padding: 6px 8px; border-bottom: 1px solid #1e293b; }}
  .summary-table tr:hover {{ background: #1e293b; }}
  .table-wrap {{ background: #1e293b; border-radius: 12px; padding: 4px; border: 1px solid #334155; overflow-x: auto; }}
  @media (max-width: 700px) {{ .pair-grid {{ grid-template-columns: 1fr; }} }}
</style>
</head>
<body>

<h1>Pairs Pattern Analysis</h1>
<p class="subtitle">Analyzing {len(results)} pairs · 359 bars · Sorted by autocorrelation (most mean-reverting first)</p>

<h2>Summary — All Pairs</h2>
<div class="table-wrap">
<table class="summary-table">
<thead><tr>
  <th>Pair</th><th>AC(1)</th><th>AC(2)</th><th>Avg R²</th><th>Beta CV</th><th>Skew</th><th>Kurt</th><th>σ(Δspread) bps</th><th>Best Win%</th>
</tr></thead>
<tbody>{summary_rows}</tbody>
</table>
</div>

<h2>Detailed Pair Patterns (Top 20 Most Mean-Reverting)</h2>
{pair_cards}

<script>
Chart.defaults.color = '#94a3b8';
Chart.defaults.borderColor = '#334155';
{chart_scripts}
</script>
</body>
</html>"""

    Path(output_path).write_text(html)


def main():
    root = Path(__file__).resolve().parent.parent
    prices = json.load(open(root / "data" / "pair_picker_prices.json"))
    expanded = json.load(open(root / "trading" / "pair_candidates_expanded.json"))
    candidates = [(p['leg_a'], p['leg_b']) for p in expanded['pairs']]

    total_bars = min(len(v) for v in prices.values())

    logger.info("=" * 60)
    logger.info(f"PATTERN ANALYSIS — {datetime.now().strftime('%Y-%m-%d %H:%M:%S')}")
    logger.info(f"Analyzing {len(candidates)} pairs, {total_bars} bars")
    logger.info("=" * 60)

    results = []
    for leg_a, leg_b in candidates:
        if leg_a not in prices or leg_b not in prices:
            continue
        r = analyze_pair(leg_a, leg_b, prices, total_bars)
        if r:
            results.append(r)
            logger.info(f"  {r['pair']:<15} AC1={r['autocorr_1']:+.3f} R²={r['avg_r2']:.3f} "
                        f"βCV={r['beta_cv']:.2f} skew={r['skewness']:+.2f}")

    logger.info(f"\nAnalyzed {len(results)} pairs with sufficient data")

    # Key findings
    strong_mr = [r for r in results if r['autocorr_1'] < -0.10]
    weak_mr = [r for r in results if r['autocorr_1'] > 0.05]
    logger.info(f"Strong mean-reversion (AC1 < -0.10): {len(strong_mr)} pairs")
    logger.info(f"Trending (AC1 > 0.05): {len(weak_mr)} pairs")

    for r in sorted(results, key=lambda x: x['autocorr_1'])[:10]:
        best_hold = max(r['hold_stats'].items(), key=lambda x: x[1]['win_rate']) if r['hold_stats'] else (0, {'win_rate': 0})
        logger.info(f"  TOP: {r['pair']:<15} AC1={r['autocorr_1']:+.3f} "
                    f"best_hold={best_hold[0]}d win={best_hold[1]['win_rate']*100:.0f}%")

    output = root / "data" / "patterns_dashboard.html"
    generate_pattern_dashboard(results, output)
    logger.info(f"\nDashboard: file://{output}")
    logger.info(f"Log: {LOG_FILE}")
    print(f"\nOpen: file://{output}")


if __name__ == "__main__":
    main()
