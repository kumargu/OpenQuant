"""
FastAPI webhook receiver for TradingView alerts.

Run: uvicorn paper_trading.server:app --port 8877
"""

import os
from datetime import datetime, timezone

from fastapi import FastAPI, HTTPException
from pydantic import BaseModel, Field

from . import store, portfolio

app = FastAPI(title="OpenQuant Paper Trading")

WEBHOOK_SECRET = os.environ.get("OQ_WEBHOOK_SECRET", "changeme")


class TradeSignal(BaseModel):
    secret: str
    symbol: str
    action: str = Field(description="buy or sell")
    price: float
    qty: float = 1.0
    strategy: str = "manual"


@app.post("/webhook")
def receive_webhook(signal: TradeSignal):
    if signal.secret != WEBHOOK_SECRET:
        raise HTTPException(status_code=403, detail="Invalid secret")

    if signal.action not in ("buy", "sell"):
        raise HTTPException(status_code=400, detail="action must be 'buy' or 'sell'")

    trade = {
        "symbol": signal.symbol.upper(),
        "action": signal.action,
        "price": signal.price,
        "qty": signal.qty,
        "strategy": signal.strategy,
        "source": "tradingview",
        "timestamp": datetime.now(timezone.utc).isoformat(),
        "status": "open",
    }

    trade_id = store.save_trade(trade)
    return {"ok": True, "trade_id": trade_id, "trade": trade}


@app.get("/positions")
def get_positions():
    return portfolio.get_positions()


@app.get("/trades")
def get_trades(symbol: str = None, date: str = None, strategy: str = None):
    return store.find_trades(symbol=symbol, date=date, strategy=strategy)


@app.get("/health")
def health():
    return {"status": "ok"}
