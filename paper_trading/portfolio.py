"""
Portfolio tracker — computes positions from trade history.

Positions are derived (not stored) by replaying trades for a symbol.
"""

from . import store


def get_positions() -> dict[str, dict]:
    """Return current open positions keyed by symbol.

    Each position: {symbol, qty, avg_price, side, trades: [ids]}
    """
    trades = store.find_trades(status="open")
    positions: dict[str, dict] = {}

    for t in trades:
        sym = t["symbol"]
        if sym not in positions:
            positions[sym] = {
                "symbol": sym,
                "qty": 0,
                "cost_basis": 0.0,
                "trades": [],
            }

        pos = positions[sym]
        qty = t["qty"]
        price = t["price"]

        if t["action"] == "buy":
            pos["cost_basis"] += qty * price
            pos["qty"] += qty
        elif t["action"] == "sell":
            pos["cost_basis"] -= qty * price
            pos["qty"] -= qty

        pos["trades"].append(t["id"])

    # Compute avg price and side
    for sym, pos in list(positions.items()):
        if pos["qty"] == 0:
            del positions[sym]
            continue
        pos["side"] = "long" if pos["qty"] > 0 else "short"
        pos["avg_price"] = abs(pos["cost_basis"] / pos["qty"]) if pos["qty"] != 0 else 0

    return positions


def summary() -> str:
    """Human-readable position summary."""
    positions = get_positions()
    if not positions:
        return "No open positions."

    lines = ["Symbol     Side   Qty     Avg Price"]
    lines.append("-" * 40)
    for sym, pos in sorted(positions.items()):
        lines.append(
            f"{sym:<10} {pos['side']:<6} {abs(pos['qty']):<7} {pos['avg_price']:.2f}"
        )
    return "\n".join(lines)
