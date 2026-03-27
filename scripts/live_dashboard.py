#!/usr/bin/env python3
"""
Live Pairs Trading Dashboard — Three-Panel Convergence View.

Panel 1: Price Ratio with Bollinger Bands
Panel 2: Rolling Z-Score colored by R²
Panel 3: P&L + status

Generates dashboards/live_dashboard.html (auto-refreshes).
All math via Rust pybridge. Python does only Alpaca API + HTML.

Usage:
    python3 scripts/live_dashboard.py              # One-shot
    python3 scripts/live_dashboard.py --watch 60   # Refresh every 60s
"""

import argparse
import json
import sys
import time
from datetime import datetime
from pathlib import Path
from zoneinfo import ZoneInfo

ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT / "scripts"))
ET = ZoneInfo("US/Eastern")

from pairs_core import scan_pair, compute_z, compute_frozen_z


def load_env():
    with open(ROOT / ".env") as f:
        return dict(line.strip().split("=", 1) for line in f if "=" in line and not line.startswith("#"))


def generate():
    import logging
    logging.getLogger("urllib3").setLevel(logging.WARNING)

    prices = json.load(open(ROOT / "data" / "pair_picker_prices.json"))
    live = json.load(open(ROOT / "trading" / "live_positions.json"))

    env = load_env()
    from alpaca.trading.client import TradingClient
    client = TradingClient(env["ALPACA_API_KEY"], env["ALPACA_SECRET_KEY"], paper=True)
    alpaca_pos = {p.symbol: {"price": float(p.current_price), "pnl": float(p.unrealized_pl)}
                  for p in client.get_all_positions()}
    equity = float(client.get_account().equity)
    now = datetime.now(ET).strftime("%Y-%m-%d %H:%M:%S ET")

    pairs_json = []
    for pos in live.get("positions", []):
        a, b = pos["leg_a"]["symbol"], pos["leg_b"]["symbol"]
        sig = pos["signal"]
        pa, pb = prices.get(a, []), prices.get(b, [])
        n = min(len(pa), len(pb))
        if n < 90:
            continue

        start = max(90, n - 60)
        z_series, r2_series, ratio_series = [], [], []
        for day in range(start, n):
            result = scan_pair(a, b, pa, pb, day)
            if result:
                z_series.append(round(compute_z(result, pa[day], pb[day]), 3))
                r2_series.append(round(result.r2, 3))
            else:
                z_series.append(None)
                r2_series.append(None)
            ratio_series.append(round(pa[day] / pb[day], 4) if pb[day] > 0 else None)

        a_now = alpaca_pos.get(a, {}).get("price", pa[-1])
        b_now = alpaca_pos.get(b, {}).get("price", pb[-1])
        fz = None
        if sig.get("alpha") is not None and sig.get("spread_std"):
            fz = compute_frozen_z(a_now, b_now, sig["alpha"], sig["beta"],
                                   sig["spread_mean"], sig["spread_std"])

        net_pnl = alpaca_pos.get(a, {}).get("pnl", 0) + alpaca_pos.get(b, {}).get("pnl", 0)

        pairs_json.append({
            "pair": pos["pair"], "direction": pos["direction"],
            "a": a, "b": b,
            "entry_z": sig["z_entry"], "frozen_z": round(fz, 3) if fz else None,
            "r2": sig["r2"], "hl": sig["half_life_days"], "pnl": round(net_pnl, 2),
            "z_series": z_series, "r2_series": r2_series, "ratio_series": ratio_series,
        })

    data_json = json.dumps({"updated": now, "equity": equity, "pairs": pairs_json})

    # HTML with inline JS — no f-string conflicts because we inject data as a JSON blob
    html = """<!DOCTYPE html>
<html><head>
<meta charset="utf-8">
<meta http-equiv="refresh" content="60">
<title>OpenQuant Live Dashboard</title>
<style>
body{background:#0f172a;color:#e2e8f0;font-family:'JetBrains Mono',monospace;margin:20px}
h1{color:#38bdf8;border-bottom:2px solid #1e3a5f;padding-bottom:10px}
.summary{display:flex;gap:20px;margin:15px 0;font-size:16px}
.summary span{padding:8px 14px;background:#1e293b;border-radius:6px}
.legend{display:flex;gap:20px;font-size:12px;color:#94a3b8;margin:10px 0}
.dot{display:inline-block;width:10px;height:10px;border-radius:2px;margin-right:4px;vertical-align:middle}
.panel{background:#1e293b;border-radius:8px;padding:16px;margin:16px 0}
.header{display:flex;justify-content:space-between;align-items:center;margin-bottom:10px}
.header h2{margin:0;color:#38bdf8;font-size:20px}
.tags{display:flex;gap:10px;font-size:13px}
.tags span{padding:3px 8px;background:#0f172a;border-radius:4px}
.pos{color:#22c55e}.neg{color:#ef4444}
.rev{color:#22c55e;font-weight:bold}.div{color:#ef4444;font-weight:bold}
canvas{background:#0f172a;border-radius:4px;width:100%;margin:4px 0}
.label{color:#94a3b8;font-size:12px;margin:6px 0 2px}
.footer{color:#475569;font-size:11px;margin-top:20px}
</style>
</head><body>
<h1>OpenQuant Live Dashboard</h1>
<div id="app"></div>
<script>
var DATA = """ + data_json + """;

var app = document.getElementById('app');
var h = '<div class="summary">';
h += '<span>Equity: $' + DATA.equity.toLocaleString(undefined,{minimumFractionDigits:2}) + '</span>';
h += '<span>Positions: ' + DATA.pairs.length + '</span>';
h += '<span>' + DATA.updated + '</span></div>';
h += '<div class="legend">';
h += '<span><span class="dot" style="background:#22c55e"></span>R² &gt; 0.85</span>';
h += '<span><span class="dot" style="background:#eab308"></span>R² 0.70-0.85</span>';
h += '<span><span class="dot" style="background:#ef4444"></span>R² &lt; 0.70 (exit!)</span>';
h += '<span><span class="dot" style="background:#555"></span>Exit zone</span></div>';

DATA.pairs.forEach(function(p, idx) {
    var status = 'DIVERGING', sc = 'div';
    if (p.frozen_z !== null) {
        if ((p.direction === 'LONG' && p.frozen_z > p.entry_z) ||
            (p.direction === 'SHORT' && p.frozen_z < p.entry_z)) {
            status = 'REVERTING'; sc = 'rev';
        }
    }
    var pnlCls = p.pnl >= 0 ? 'pos' : 'neg';
    var r2c = p.r2 >= 0.85 ? '#22c55e' : p.r2 >= 0.70 ? '#eab308' : '#ef4444';

    h += '<div class="panel">';
    h += '<div class="header"><h2>' + p.pair + ' ' + p.direction + '</h2>';
    h += '<div class="tags">';
    h += '<span>Entry z: ' + p.entry_z.toFixed(2) + '</span>';
    h += '<span>Now: ' + (p.frozen_z !== null ? p.frozen_z.toFixed(2) : 'N/A') + '</span>';
    h += '<span style="color:' + r2c + '">R²: ' + p.r2.toFixed(3) + '</span>';
    h += '<span>HL: ' + p.hl.toFixed(1) + 'd</span>';
    h += '<span class="' + pnlCls + '">P&L: $' + p.pnl.toFixed(2) + '</span>';
    h += '<span class="' + sc + '">' + status + '</span>';
    h += '</div></div>';

    h += '<div class="label">Price Ratio (' + p.a + '/' + p.b + ') + Bollinger Bands</div>';
    h += '<canvas id="r' + idx + '" width="800" height="150"></canvas>';
    h += '<div class="label">Z-Score (colored by R²) — gray = exit zone</div>';
    h += '<canvas id="z' + idx + '" width="800" height="180"></canvas>';
    h += '</div>';
});

app.innerHTML = h;

function r2color(v) {
    if (v === null) return '#555';
    if (v >= 0.85) return '#22c55e';
    if (v >= 0.70) return '#eab308';
    return '#ef4444';
}

DATA.pairs.forEach(function(p, idx) {
    // Ratio chart
    var c = document.getElementById('r' + idx);
    var ctx = c.getContext('2d');
    var w = c.width, ht = c.height;
    var d = p.ratio_series.filter(function(v){return v!==null});
    var mn = Math.min.apply(null,d), mx = Math.max.apply(null,d);
    var pad = (mx-mn)*0.15||0.01; mn-=pad; mx+=pad;
    var mean = d.reduce(function(a,b){return a+b},0)/d.length;
    var std = Math.sqrt(d.reduce(function(a,b){return a+(b-mean)*(b-mean)},0)/d.length);
    function ry(v){return ht-(v-mn)/(mx-mn)*ht}

    ctx.fillStyle='rgba(59,130,246,0.08)';
    ctx.fillRect(0,ry(mean+2*std),w,ry(mean-2*std)-ry(mean+2*std));
    ctx.strokeStyle='#444';ctx.setLineDash([3,3]);
    ctx.beginPath();ctx.moveTo(0,ry(mean));ctx.lineTo(w,ry(mean));ctx.stroke();
    ctx.setLineDash([]);
    ctx.strokeStyle='#3b82f6';ctx.lineWidth=1;
    ctx.beginPath();ctx.moveTo(0,ry(mean+2*std));ctx.lineTo(w,ry(mean+2*std));ctx.stroke();
    ctx.beginPath();ctx.moveTo(0,ry(mean-2*std));ctx.lineTo(w,ry(mean-2*std));ctx.stroke();
    ctx.strokeStyle='#e2e8f0';ctx.lineWidth=1.5;ctx.beginPath();
    var f=true;
    for(var i=0;i<p.ratio_series.length;i++){
        if(p.ratio_series[i]===null)continue;
        var x=i/p.ratio_series.length*w;
        if(f){ctx.moveTo(x,ry(p.ratio_series[i]));f=false}
        else ctx.lineTo(x,ry(p.ratio_series[i]));
    }
    ctx.stroke();

    // Z-score chart colored by R²
    var c2 = document.getElementById('z' + idx);
    var ctx2 = c2.getContext('2d');
    var w2=c2.width, h2=c2.height;
    var zv = p.z_series.filter(function(v){return v!==null});
    var zMax = Math.max(Math.max.apply(null,zv.map(Math.abs)), 2.5);
    function zy(v){return h2/2 - v/zMax*h2/2}

    // Exit zone
    ctx2.fillStyle='rgba(100,100,100,0.15)';
    ctx2.fillRect(0,zy(0.2),w2,zy(-0.2)-zy(0.2));
    // Zero line
    ctx2.strokeStyle='#444';ctx2.setLineDash([3,3]);
    ctx2.beginPath();ctx2.moveTo(0,zy(0));ctx2.lineTo(w2,zy(0));ctx2.stroke();
    ctx2.setLineDash([]);
    // Entry thresholds
    ctx2.strokeStyle='rgba(239,68,68,0.4)';ctx2.lineWidth=1;
    ctx2.beginPath();ctx2.moveTo(0,zy(1));ctx2.lineTo(w2,zy(1));ctx2.stroke();
    ctx2.beginPath();ctx2.moveTo(0,zy(-1));ctx2.lineTo(w2,zy(-1));ctx2.stroke();
    // Labels
    ctx2.fillStyle='#64748b';ctx2.font='10px monospace';
    ctx2.fillText('+1.0',w2-30,zy(1)-3);ctx2.fillText('-1.0',w2-30,zy(-1)+10);
    ctx2.fillText('EXIT',w2-30,zy(0)+3);

    // Z segments colored by R²
    ctx2.lineWidth=2.5;
    for(var i=1;i<p.z_series.length;i++){
        if(p.z_series[i]===null||p.z_series[i-1]===null)continue;
        ctx2.strokeStyle=r2color(p.r2_series[i]);
        ctx2.beginPath();
        ctx2.moveTo((i-1)/p.z_series.length*w2, zy(p.z_series[i-1]));
        ctx2.lineTo(i/p.z_series.length*w2, zy(p.z_series[i]));
        ctx2.stroke();
    }
    // Current frozen z dot
    if(p.frozen_z!==null){
        ctx2.fillStyle='#fff';ctx2.beginPath();
        ctx2.arc(w2-8,zy(p.frozen_z),5,0,Math.PI*2);ctx2.fill();
        ctx2.fillStyle='#e2e8f0';ctx2.font='11px monospace';
        ctx2.fillText(p.frozen_z.toFixed(2),w2-50,zy(p.frozen_z)-8);
    }
});
</script>
<div class="footer">Auto-refreshes every 60s. All math via Rust pybridge.</div>
</body></html>"""

    output = ROOT / "dashboards" / "live_dashboard.html"
    output.parent.mkdir(exist_ok=True)
    with open(output, "w") as f:
        f.write(html)
    print(f"Dashboard: file://{output}")


def main():
    parser = argparse.ArgumentParser(description="Live pairs trading dashboard")
    parser.add_argument("--watch", type=int, default=0, help="Regenerate every N seconds")
    args = parser.parse_args()

    while True:
        generate()
        if args.watch <= 0:
            break
        time.sleep(args.watch)


if __name__ == "__main__":
    main()
