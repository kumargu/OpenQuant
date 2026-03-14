"""
File-based trade store with index for quick search.

Trades are stored as individual JSON files: data/paper_trades/{id}.json
An index file (data/paper_trades/_index.json) maps symbols, dates, and strategies
to trade IDs for fast lookups without scanning all files.
"""

import json
import os
import uuid
from datetime import datetime, timezone
from pathlib import Path
from typing import Optional

DATA_DIR = Path(__file__).parent.parent / "data" / "paper_trades"
INDEX_PATH = DATA_DIR / "_index.json"


def _ensure_dirs():
    DATA_DIR.mkdir(parents=True, exist_ok=True)


def _load_index() -> dict:
    if INDEX_PATH.exists():
        return json.loads(INDEX_PATH.read_text())
    return {
        "by_symbol": {},
        "by_date": {},
        "by_strategy": {},
        "by_status": {"open": [], "closed": []},
        "all": [],
    }


def _save_index(index: dict):
    INDEX_PATH.write_text(json.dumps(index, indent=2))


def _index_trade(index: dict, trade: dict):
    tid = trade["id"]
    symbol = trade["symbol"]
    date = trade["timestamp"][:10]
    strategy = trade.get("strategy", "manual")
    status = trade.get("status", "open")

    index["all"].append(tid)
    index["by_symbol"].setdefault(symbol, []).append(tid)
    index["by_date"].setdefault(date, []).append(tid)
    index["by_strategy"].setdefault(strategy, []).append(tid)
    index["by_status"].setdefault(status, []).append(tid)


def save_trade(trade: dict) -> str:
    _ensure_dirs()
    if "id" not in trade:
        trade["id"] = uuid.uuid4().hex[:12]
    if "timestamp" not in trade:
        trade["timestamp"] = datetime.now(timezone.utc).isoformat()
    if "status" not in trade:
        trade["status"] = "open"

    # Write trade file
    trade_path = DATA_DIR / f"{trade['id']}.json"
    trade_path.write_text(json.dumps(trade, indent=2))

    # Update index
    index = _load_index()
    _index_trade(index, trade)
    _save_index(index)

    return trade["id"]


def load_trade(trade_id: str) -> Optional[dict]:
    trade_path = DATA_DIR / f"{trade_id}.json"
    if trade_path.exists():
        return json.loads(trade_path.read_text())
    return None


def update_trade(trade_id: str, updates: dict):
    trade = load_trade(trade_id)
    if not trade:
        raise ValueError(f"Trade {trade_id} not found")

    old_status = trade.get("status")
    trade.update(updates)

    trade_path = DATA_DIR / f"{trade_id}.json"
    trade_path.write_text(json.dumps(trade, indent=2))

    # Update status index if status changed
    new_status = trade.get("status")
    if old_status != new_status:
        index = _load_index()
        if trade_id in index["by_status"].get(old_status, []):
            index["by_status"][old_status].remove(trade_id)
        index["by_status"].setdefault(new_status, []).append(trade_id)
        _save_index(index)


def find_trades(
    symbol: Optional[str] = None,
    date: Optional[str] = None,
    strategy: Optional[str] = None,
    status: Optional[str] = None,
) -> list[dict]:
    """Find trades by any combination of filters. Returns full trade objects."""
    index = _load_index()

    # Start with all trade IDs, then intersect with each filter
    candidates = set(index["all"])

    if symbol:
        candidates &= set(index["by_symbol"].get(symbol.upper(), []))
    if date:
        candidates &= set(index["by_date"].get(date, []))
    if strategy:
        candidates &= set(index["by_strategy"].get(strategy, []))
    if status:
        candidates &= set(index["by_status"].get(status, []))

    trades = []
    for tid in candidates:
        trade = load_trade(tid)
        if trade:
            trades.append(trade)

    return sorted(trades, key=lambda t: t["timestamp"])


def all_trades() -> list[dict]:
    return find_trades()
