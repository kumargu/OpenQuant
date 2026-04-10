"""
Reader utilities for the foundation dataset at ~/quant-data/bars/.

Every downstream tool (oracle, pickers, eval) uses this module to load
bars. Don't re-implement Parquet reading elsewhere. Keep the interface
narrow and the implementation boring.

Usage:
    from autoresearch.dataset import Dataset

    ds = Dataset.latest()                    # auto-detects v1_sp500_...
    syms = ds.symbols()                      # list of all symbols
    df = ds.load("AAL")                      # polars DataFrame (or pandas)
    df = ds.load("AAL", start="2025-09-01", end="2025-12-01")
    df = ds.load_regular_session("AAL")      # 13:30-20:00 UTC filter
    pair = ds.load_pair("AAL", "DAL")        # inner-joined on timestamp
"""
from __future__ import annotations

import json
from dataclasses import dataclass
from datetime import date, datetime, timezone
from pathlib import Path
from typing import List, Optional, Tuple, Union

import pyarrow.parquet as pq

try:
    import polars as pl
    HAS_POLARS = True
except ImportError:
    HAS_POLARS = False

try:
    import pandas as pd
    HAS_PANDAS = True
except ImportError:
    HAS_PANDAS = False


DataFrameLike = Union["pl.DataFrame", "pd.DataFrame"]  # runtime-resolved

DEFAULT_ROOT = Path.home() / "quant-data"


@dataclass
class Dataset:
    """Immutable view of a single foundation-dataset version."""

    root: Path  # e.g. ~/quant-data/bars/v1_sp500_2025-2026_1min

    @classmethod
    def latest(cls, flavor: str = "sp500_2025-2026_1min") -> "Dataset":
        """Find the newest version matching the flavor pattern."""
        bars_root = DEFAULT_ROOT / "bars"
        if not bars_root.exists():
            raise FileNotFoundError(f"No dataset root at {bars_root}")
        candidates = sorted(
            [p for p in bars_root.iterdir() if p.is_dir() and flavor in p.name],
            reverse=True,
        )
        if not candidates:
            raise FileNotFoundError(f"No dataset matching '{flavor}' in {bars_root}")
        return cls(root=candidates[0])

    @classmethod
    def at(cls, version: str) -> "Dataset":
        """Pin to an exact version string."""
        path = DEFAULT_ROOT / "bars" / version
        if not path.exists():
            raise FileNotFoundError(path)
        return cls(root=path)

    @property
    def manifest(self) -> dict:
        with open(self.root / "MANIFEST.json") as f:
            return json.load(f)

    def symbols(self) -> List[str]:
        return sorted(
            p.stem for p in self.root.glob("*.parquet") if p.is_file()
        )

    def _path(self, symbol: str) -> Path:
        return self.root / f"{symbol}.parquet"

    def has(self, symbol: str) -> bool:
        return self._path(symbol).exists()

    def load(
        self,
        symbol: str,
        start: Optional[Union[str, date, datetime]] = None,
        end: Optional[Union[str, date, datetime]] = None,
    ) -> DataFrameLike:
        """Load one symbol as a DataFrame (polars if available, else pandas).

        `start` and `end` are inclusive ISO date strings or datetime objects.
        Both are interpreted as UTC if naive.
        """
        path = self._path(symbol)
        if not path.exists():
            raise FileNotFoundError(f"No data for {symbol} in {self.root}")

        table = pq.read_table(path)

        if HAS_POLARS:
            df = pl.from_arrow(table)
        else:
            df = table.to_pandas()

        if start is not None or end is not None:
            df = _filter_time(df, start, end)

        return df

    def load_regular_session(
        self,
        symbol: str,
        start: Optional[Union[str, date, datetime]] = None,
        end: Optional[Union[str, date, datetime]] = None,
    ) -> DataFrameLike:
        """Load one symbol, filtered to regular session (13:30-20:00 UTC)."""
        df = self.load(symbol, start=start, end=end)
        return _filter_regular_session(df)

    def load_pair(
        self,
        leg_a: str,
        leg_b: str,
        start: Optional[Union[str, date, datetime]] = None,
        end: Optional[Union[str, date, datetime]] = None,
        regular_session: bool = True,
    ) -> DataFrameLike:
        """Load two symbols and inner-join on timestamp.

        Returns a DataFrame with columns:
          timestamp, {leg_a}_close, {leg_b}_close, {leg_a}_volume, {leg_b}_volume
        """
        load_fn = self.load_regular_session if regular_session else self.load
        a = load_fn(leg_a, start=start, end=end)
        b = load_fn(leg_b, start=start, end=end)

        if HAS_POLARS and isinstance(a, pl.DataFrame):
            a2 = a.select(
                pl.col("timestamp"),
                pl.col("close").alias(f"{leg_a}_close"),
                pl.col("volume").alias(f"{leg_a}_volume"),
            )
            b2 = b.select(
                pl.col("timestamp"),
                pl.col("close").alias(f"{leg_b}_close"),
                pl.col("volume").alias(f"{leg_b}_volume"),
            )
            return a2.join(b2, on="timestamp", how="inner")
        else:
            a2 = a[["timestamp", "close", "volume"]].rename(
                columns={"close": f"{leg_a}_close", "volume": f"{leg_a}_volume"}
            )
            b2 = b[["timestamp", "close", "volume"]].rename(
                columns={"close": f"{leg_b}_close", "volume": f"{leg_b}_volume"}
            )
            return a2.merge(b2, on="timestamp", how="inner")


# ── Helpers ──

def _to_utc_datetime(x: Union[str, date, datetime]) -> datetime:
    if isinstance(x, datetime):
        return x if x.tzinfo else x.replace(tzinfo=timezone.utc)
    if isinstance(x, date):
        return datetime(x.year, x.month, x.day, tzinfo=timezone.utc)
    if isinstance(x, str):
        # Accept 'YYYY-MM-DD' or ISO datetime
        if len(x) == 10:
            return datetime.fromisoformat(x).replace(tzinfo=timezone.utc)
        dt = datetime.fromisoformat(x)
        return dt if dt.tzinfo else dt.replace(tzinfo=timezone.utc)
    raise TypeError(f"Cannot convert {type(x)} to UTC datetime")


def _filter_time(df, start, end):
    s = _to_utc_datetime(start) if start is not None else None
    e = _to_utc_datetime(end) if end is not None else None

    if HAS_POLARS and isinstance(df, pl.DataFrame):
        if s is not None:
            df = df.filter(pl.col("timestamp") >= s)
        if e is not None:
            df = df.filter(pl.col("timestamp") <= e)
        return df
    else:
        # pandas
        mask = None
        if s is not None:
            mask = df["timestamp"] >= s
        if e is not None:
            m2 = df["timestamp"] <= e
            mask = m2 if mask is None else mask & m2
        return df[mask] if mask is not None else df


def _filter_regular_session(df):
    """Keep only bars with UTC time in [13:30, 20:00)."""
    if HAS_POLARS and isinstance(df, pl.DataFrame):
        # Extract hour and minute from timestamp
        return df.filter(
            (pl.col("timestamp").dt.hour() * 60 + pl.col("timestamp").dt.minute() >= 13 * 60 + 30)
            & (pl.col("timestamp").dt.hour() * 60 + pl.col("timestamp").dt.minute() < 20 * 60)
        )
    else:
        ts = df["timestamp"]
        mins = ts.dt.hour * 60 + ts.dt.minute
        return df[(mins >= 13 * 60 + 30) & (mins < 20 * 60)]
