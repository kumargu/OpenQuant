"""
Build the foundation dataset: 1-min OHLCV bars for S&P 500 2025-2026.

Output: ~/quant-data/bars/v1_sp500_2025-2026_1min/
  AAL.parquet, AAPL.parquet, ... (one file per symbol)
  MANIFEST.json (fetch metadata)
  FAILED.json   (symbols that failed to fetch, if any)

IMMUTABLE: once written, never mutate. If schema changes, bump to v2.

Resumable: if a {symbol}.parquet already exists with valid data, it is
skipped. Re-run the script to pick up where you left off after a crash
or rate-limit.

Schema (Parquet, snappy-compressed):
  timestamp: timestamp[us, tz=UTC]
  open:      float64
  high:      float64
  low:       float64
  close:     float64
  volume:    int64
  trade_count: int64
  vwap:      float64
"""
import json
import os
import sys
import time
import traceback
from datetime import date, datetime, timezone
from pathlib import Path
from typing import List, Optional

import pyarrow as pa
import pyarrow.parquet as pq
from dotenv import load_dotenv

# ── Paths / constants ──
REPO = Path(__file__).parent.parent
DATA_DIR = Path.home() / "quant-data"
DATASET_NAME = "v2_sp500_2025-2026_1min_adjusted"
BARS_DIR = DATA_DIR / "bars" / DATASET_NAME
MANIFEST_PATH = BARS_DIR / "MANIFEST.json"
FAILED_PATH = BARS_DIR / "FAILED.json"

# Date range — we store "everything" and filter at read time
START_DATE = date(2025, 1, 1)
END_DATE = date(2026, 4, 10)  # today; the script records actual end time in MANIFEST

# Symbol source
SECTORS_JSON = REPO / "data" / "sp500_sectors.json"


def load_symbols() -> List[str]:
    with open(SECTORS_JSON) as f:
        d = json.load(f)
    syms: List[str] = []
    for lst in d.values():
        syms.extend(lst)
    syms = sorted(set(syms))
    return syms


def fetch_one(client, symbol: str, start: date, end: date) -> pa.Table:
    """Fetch 1-min bars for a single symbol, return Arrow table."""
    from alpaca.data.enums import Adjustment, DataFeed
    from alpaca.data.requests import StockBarsRequest
    from alpaca.data.timeframe import TimeFrame, TimeFrameUnit

    req = StockBarsRequest(
        symbol_or_symbols=symbol,
        timeframe=TimeFrame(1, TimeFrameUnit.Minute),
        start=datetime.combine(start, datetime.min.time(), tzinfo=timezone.utc),
        end=datetime.combine(end, datetime.max.time(), tzinfo=timezone.utc),
        feed=DataFeed.IEX,  # matches the engine's live-trading feed
        adjustment=Adjustment.ALL,  # adjust for splits AND dividends
    )
    bars_obj = client.get_stock_bars(req)

    # alpaca-py returns a BarSet. .data is dict[str, list[Bar]]
    bars = bars_obj.data.get(symbol, [])
    if not bars:
        return pa.table(
            {
                "timestamp": pa.array([], pa.timestamp("us", tz="UTC")),
                "open": pa.array([], pa.float64()),
                "high": pa.array([], pa.float64()),
                "low": pa.array([], pa.float64()),
                "close": pa.array([], pa.float64()),
                "volume": pa.array([], pa.int64()),
                "trade_count": pa.array([], pa.int64()),
                "vwap": pa.array([], pa.float64()),
            }
        )

    ts = [b.timestamp for b in bars]
    return pa.table(
        {
            "timestamp": pa.array(ts, pa.timestamp("us", tz="UTC")),
            "open": pa.array([float(b.open) for b in bars], pa.float64()),
            "high": pa.array([float(b.high) for b in bars], pa.float64()),
            "low": pa.array([float(b.low) for b in bars], pa.float64()),
            "close": pa.array([float(b.close) for b in bars], pa.float64()),
            "volume": pa.array([int(b.volume) for b in bars], pa.int64()),
            "trade_count": pa.array(
                [int(b.trade_count or 0) for b in bars], pa.int64()
            ),
            "vwap": pa.array([float(b.vwap or 0.0) for b in bars], pa.float64()),
        }
    )


def write_parquet(table: pa.Table, path: Path) -> None:
    pq.write_table(table, path, compression="snappy")


def fetch_all(limit: Optional[int] = None, symbols_override: Optional[List[str]] = None) -> None:
    load_dotenv(REPO / ".env")
    from alpaca.data.historical import StockHistoricalDataClient

    client = StockHistoricalDataClient(
        os.environ["ALPACA_API_KEY"],
        os.environ["ALPACA_SECRET_KEY"],
    )

    BARS_DIR.mkdir(parents=True, exist_ok=True)

    symbols = symbols_override or load_symbols()
    if limit:
        symbols = symbols[:limit]

    print(f"Fetching {len(symbols)} symbols: {START_DATE} .. {END_DATE}", file=sys.stderr)
    print(f"Output: {BARS_DIR}", file=sys.stderr)

    failed = {}
    bar_counts = {}
    skipped = []

    t0 = time.time()
    for i, sym in enumerate(symbols, 1):
        out_path = BARS_DIR / f"{sym}.parquet"
        if out_path.exists():
            try:
                existing = pq.read_metadata(out_path)
                bar_counts[sym] = existing.num_rows
                skipped.append(sym)
                if i % 50 == 0:
                    print(f"  [{i}/{len(symbols)}] skipped (exists) — {sym} ({existing.num_rows} rows)", file=sys.stderr)
                continue
            except Exception:
                # Corrupt, delete and refetch
                out_path.unlink()

        try:
            tbl = fetch_one(client, sym, START_DATE, END_DATE)
            write_parquet(tbl, out_path)
            bar_counts[sym] = tbl.num_rows
            elapsed = time.time() - t0
            rate = i / max(elapsed, 0.001)
            eta = (len(symbols) - i) / max(rate, 0.001)
            print(
                f"  [{i}/{len(symbols)}] {sym}: {tbl.num_rows:,} bars "
                f"({rate:.2f}/s, ETA {eta/60:.1f}m)",
                file=sys.stderr,
            )
        except Exception as e:
            failed[sym] = f"{type(e).__name__}: {e}"
            print(f"  [{i}/{len(symbols)}] {sym}: FAIL {failed[sym]}", file=sys.stderr)

    # Write MANIFEST
    manifest = {
        "version": DATASET_NAME,
        "built_at": datetime.now(timezone.utc).isoformat(),
        "universe": "S&P 500",
        "universe_source": "data/sp500_sectors.json",
        "date_range": {"start": START_DATE.isoformat(), "end": END_DATE.isoformat()},
        "timeframe": "1 minute",
        "timezone": "UTC",
        "session_filter": "none (raw Alpaca output — includes extended hours if present)",
        "adjustment": "Adjustment.ALL (splits + dividends adjusted)",
        "feed": "IEX (matches engine's live-trading feed)",
        "format": "Parquet snappy",
        "schema": {
            "timestamp": "timestamp[us, tz=UTC]",
            "open": "float64",
            "high": "float64",
            "low": "float64",
            "close": "float64",
            "volume": "int64",
            "trade_count": "int64",
            "vwap": "float64",
        },
        "n_symbols_requested": len(symbols),
        "n_symbols_success": len(bar_counts),
        "n_symbols_failed": len(failed),
        "total_bars": int(sum(bar_counts.values())),
        "bar_counts": bar_counts,
        "notes": [
            "Immutable: never mutate. If schema or params change, bump to v2.",
            "Survivorship bias: constituents are current S&P 500 as of build_at.",
            "Some symbols may be missing if Alpaca has no data for them.",
        ],
    }
    with open(MANIFEST_PATH, "w") as f:
        json.dump(manifest, f, indent=2)

    if failed:
        with open(FAILED_PATH, "w") as f:
            json.dump(failed, f, indent=2)

    elapsed = time.time() - t0
    print(
        f"\nDone. {len(bar_counts)}/{len(symbols)} success, {len(failed)} failed, "
        f"{sum(bar_counts.values()):,} total bars, {elapsed/60:.1f} minutes.",
        file=sys.stderr,
    )
    print(f"Manifest: {MANIFEST_PATH}", file=sys.stderr)


if __name__ == "__main__":
    # --limit N  → fetch only first N symbols (for smoke testing)
    # --symbols A,B,C → fetch specific symbols
    limit = None
    syms = None
    args = sys.argv[1:]
    i = 0
    while i < len(args):
        if args[i] == "--limit":
            limit = int(args[i + 1])
            i += 2
        elif args[i] == "--symbols":
            syms = args[i + 1].split(",")
            i += 2
        else:
            i += 1
    fetch_all(limit=limit, symbols_override=syms)
