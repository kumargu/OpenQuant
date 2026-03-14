"""
Alpaca paper trading client.

Wraps alpaca-py SDK for paper trading operations.
Trades placed here will show up on TradingView when your
Alpaca paper account is connected.
"""

import os
from dotenv import load_dotenv
from alpaca.trading.client import TradingClient
from alpaca.trading.requests import MarketOrderRequest, LimitOrderRequest
from alpaca.trading.enums import OrderSide, TimeInForce

load_dotenv()

API_KEY = os.environ["ALPACA_API_KEY"]
SECRET_KEY = os.environ["ALPACA_SECRET_KEY"]

# paper=True routes to paper-api.alpaca.markets
client = TradingClient(API_KEY, SECRET_KEY, paper=True)


def get_account():
    """Return account info (balance, equity, buying power)."""
    acct = client.get_account()
    return {
        "equity": float(acct.equity),
        "cash": float(acct.cash),
        "buying_power": float(acct.buying_power),
        "currency": acct.currency,
        "status": acct.status.value if hasattr(acct.status, 'value') else str(acct.status),
    }


def buy(symbol: str, qty: float, order_type: str = "market", limit_price: float = None):
    """Place a paper buy order."""
    return _place_order(symbol, qty, OrderSide.BUY, order_type, limit_price)


def sell(symbol: str, qty: float, order_type: str = "market", limit_price: float = None):
    """Place a paper sell order."""
    return _place_order(symbol, qty, OrderSide.SELL, order_type, limit_price)


def _place_order(symbol: str, qty: float, side: OrderSide, order_type: str, limit_price: float):
    if order_type == "limit" and limit_price:
        req = LimitOrderRequest(
            symbol=symbol,
            qty=qty,
            side=side,
            time_in_force=TimeInForce.GTC,
            limit_price=limit_price,
        )
    else:
        req = MarketOrderRequest(
            symbol=symbol,
            qty=qty,
            side=side,
            time_in_force=TimeInForce.GTC,
        )

    order = client.submit_order(req)
    return {
        "id": str(order.id),
        "symbol": order.symbol,
        "side": order.side.value if hasattr(order.side, 'value') else str(order.side),
        "qty": str(order.qty),
        "type": order.type.value if hasattr(order.type, 'value') else str(order.type),
        "status": order.status.value if hasattr(order.status, 'value') else str(order.status),
        "submitted_at": str(order.submitted_at),
    }


def get_positions():
    """Return all open positions."""
    positions = client.get_all_positions()
    return [
        {
            "symbol": p.symbol,
            "qty": float(p.qty),
            "side": p.side.value if hasattr(p.side, 'value') else str(p.side),
            "avg_entry": float(p.avg_entry_price),
            "current_price": float(p.current_price),
            "unrealized_pl": float(p.unrealized_pl),
            "unrealized_plpc": float(p.unrealized_plpc),
            "market_value": float(p.market_value),
        }
        for p in positions
    ]


def get_orders(status: str = "open"):
    """Return orders by status (open, closed, all)."""
    from alpaca.trading.requests import GetOrdersRequest
    from alpaca.trading.enums import QueryOrderStatus

    status_map = {
        "open": QueryOrderStatus.OPEN,
        "closed": QueryOrderStatus.CLOSED,
        "all": QueryOrderStatus.ALL,
    }
    req = GetOrdersRequest(status=status_map.get(status, QueryOrderStatus.OPEN))
    orders = client.get_orders(req)
    return [
        {
            "id": str(o.id),
            "symbol": o.symbol,
            "side": o.side.value if hasattr(o.side, 'value') else str(o.side),
            "qty": str(o.qty),
            "type": o.type.value if hasattr(o.type, 'value') else str(o.type),
            "status": o.status.value if hasattr(o.status, 'value') else str(o.status),
            "filled_avg_price": str(o.filled_avg_price) if o.filled_avg_price else None,
            "submitted_at": str(o.submitted_at),
        }
        for o in orders
    ]
