"""
Mock Alpaca market-data server — serves bars from the quant-data parquets.

Bypasses the real Alpaca API during offline replay. The runner's AlpacaClient
(engine/crates/runner/src/alpaca.rs) can be pointed here via the
`ALPACA_DATA_URL` env var. Nothing else in the runner needs to change.

Endpoint: GET /v2/stocks/bars
Query:    symbols=A,B,C  timeframe=1Day|1Min  start=YYYY-MM-DD  end=YYYY-MM-DD[TZ]
Response: {"bars": {"SYM": [{"t": "...", "o":..., "h":..., "l":..., "c":..., "v":...}]}, "next_page_token": null}

Data source: ~/quant-data/bars/v3_sp500_2024-2026_1min_adjusted/{SYMBOL}.parquet
Each parquet holds 1-min OHLCV bars with UTC timestamps.

For 1Min requests we return the raw 1-min bars filtered by [start, end).
For 1Day requests we aggregate to RTH daily bars (session: 13:30..20:00 UTC,
close = last tick of each session). Matches quant-lab's own aggregation, so
pair-picker stats fit on the same prices quant-lab trained on.

Usage:
  python3 scripts/mock_alpaca.py --port 8787
"""
from __future__ import annotations

import argparse
import json
import re
import sys
from datetime import datetime, timezone
from functools import lru_cache
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import parse_qs, urlparse

import pyarrow.parquet as pq

BARS_DIR = Path.home() / "quant-data/bars/v3_sp500_2024-2026_1min_adjusted"


def _parse_dt(s: str) -> datetime:
    """Parse Alpaca's start/end — accepts plain date or RFC3339 with Z."""
    s = s.strip()
    if re.fullmatch(r"\d{4}-\d{2}-\d{2}", s):
        return datetime.fromisoformat(s).replace(tzinfo=timezone.utc)
    if s.endswith("Z"):
        s = s[:-1] + "+00:00"
    return datetime.fromisoformat(s).astimezone(timezone.utc)


@lru_cache(maxsize=600)
def _load_1min(symbol: str):
    """Return a list of (ts_rfc3339, o, h, l, c, v) tuples in UTC order.
    LRU-cached so repeated days on the same symbol are cheap."""
    path = BARS_DIR / f"{symbol}.parquet"
    if not path.exists():
        return []
    t = pq.read_table(
        path, columns=["timestamp", "open", "high", "low", "close", "volume"]
    )
    ts = t.column("timestamp").to_pylist()  # list of pd.Timestamp
    o = t.column("open").to_pylist()
    h = t.column("high").to_pylist()
    l = t.column("low").to_pylist()
    c = t.column("close").to_pylist()
    v = t.column("volume").to_pylist()
    out = []
    for i in range(len(ts)):
        # pyarrow may return naive datetime in UTC; force tz
        dt = ts[i]
        if getattr(dt, "tzinfo", None) is None:
            dt = dt.replace(tzinfo=timezone.utc)
        out.append(
            (
                dt.strftime("%Y-%m-%dT%H:%M:%SZ"),
                float(o[i] or 0.0),
                float(h[i] or 0.0),
                float(l[i] or 0.0),
                float(c[i] or 0.0),
                float(v[i] or 0.0),
                dt,
            )
        )
    return out


def _daily_bars(symbol: str, start: datetime, end: datetime):
    """RTH daily close bars in [start, end). Session: 13:30..20:00 UTC
    (9:30 EST .. 16:00 EST, roughly). Timestamp is the last 1-min bar of the
    session, in UTC, matching Alpaca's `1Day` convention of a day-open
    timestamp (we use last-close for simplicity — pair-picker only reads
    `close` anyway)."""
    rows = _load_1min(symbol)
    if not rows:
        return []
    by_date: dict[str, tuple] = {}
    for ts_str, o, h, l, c, v, dt in rows:
        if dt < start or dt >= end:
            continue
        minute = dt.hour * 60 + dt.minute
        if minute < 13 * 60 + 30 or minute >= 20 * 60:
            continue
        key = dt.date().isoformat()
        existing = by_date.get(key)
        if existing is None:
            by_date[key] = (ts_str, o, h, l, c, v, dt)
        else:
            _, eo, eh, el, ec, ev, _ = existing
            by_date[key] = (
                ts_str,
                eo,
                max(eh, h),
                min(el, l),
                c,  # last tick of the session
                ev + v,
                dt,
            )
    out = []
    for _, row in sorted(by_date.items()):
        ts_str, o, h, l, c, v, dt = row
        # Alpaca reports 1Day bars with ts at 00:00 UTC of that date
        midnight = datetime(dt.year, dt.month, dt.day, tzinfo=timezone.utc)
        out.append(
            {
                "t": midnight.strftime("%Y-%m-%dT%H:%M:%SZ"),
                "o": o,
                "h": h,
                "l": l,
                "c": c,
                "v": v,
            }
        )
    return out


def _minute_bars(symbol: str, start: datetime, end: datetime):
    """Raw 1-min bars in [start, end). Filters to RTH (13:30..20:00 UTC)
    same as quant-lab, avoiding extended-hours noise."""
    rows = _load_1min(symbol)
    out = []
    for ts_str, o, h, l, c, v, dt in rows:
        if dt < start or dt >= end:
            continue
        minute = dt.hour * 60 + dt.minute
        if minute < 13 * 60 + 30 or minute >= 20 * 60:
            continue
        out.append({"t": ts_str, "o": o, "h": h, "l": l, "c": c, "v": v})
    return out


class Handler(BaseHTTPRequestHandler):
    def log_message(self, format, *args):
        return  # silence per-request logs (too noisy)

    def do_GET(self):
        url = urlparse(self.path)
        if url.path != "/v2/stocks/bars":
            self.send_error(404, "only /v2/stocks/bars is served")
            return

        q = parse_qs(url.query)
        symbols = q.get("symbols", [""])[0].split(",")
        timeframe = q.get("timeframe", ["1Day"])[0]
        start_s = q.get("start", [""])[0]
        end_s = q.get("end", [""])[0]

        try:
            start = _parse_dt(start_s)
            end = _parse_dt(end_s)
        except Exception as e:
            self.send_error(400, f"bad date: {e}")
            return

        bars: dict[str, list] = {}
        for sym in symbols:
            sym = sym.strip()
            if not sym:
                continue
            if timeframe == "1Day":
                bars[sym] = _daily_bars(sym, start, end)
            elif timeframe == "1Min":
                bars[sym] = _minute_bars(sym, start, end)
            else:
                self.send_error(400, f"unsupported timeframe: {timeframe}")
                return

        payload = json.dumps({"bars": bars, "next_page_token": None}).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

        total = sum(len(v) for v in bars.values())
        print(
            f"  served {timeframe:<5}  symbols={len(symbols):>3}  "
            f"bars={total:>7}  [{start.date()}..{end.date()}]",
            file=sys.stderr,
            flush=True,
        )


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, default=8787)
    ap.add_argument("--host", default="127.0.0.1")
    args = ap.parse_args()

    srv = ThreadingHTTPServer((args.host, args.port), Handler)
    url = f"http://{args.host}:{args.port}/v2/stocks/bars"
    print(f"mock-alpaca serving {BARS_DIR}", file=sys.stderr)
    print(f"              at {url}", file=sys.stderr)
    print("              ALPACA_DATA_URL=" + url, file=sys.stderr)
    try:
        srv.serve_forever()
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
