"""
CLI for paper trading via Alpaca. Trades show up on TradingView.

Usage:
  python -m paper_trading account
  python -m paper_trading positions
  python -m paper_trading orders [--status open|closed|all]
  python -m paper_trading buy AAPL 10
  python -m paper_trading buy AAPL 10 --limit 150.00
  python -m paper_trading sell AAPL 10
  python -m paper_trading sell BTC/USD 0.001
"""

import argparse
import sys

from . import alpaca_client as alpaca


def cmd_account(args):
    acct = alpaca.get_account()
    print(f"Status:       {acct['status']}")
    print(f"Equity:       ${acct['equity']:,.2f}")
    print(f"Cash:         ${acct['cash']:,.2f}")
    print(f"Buying Power: ${acct['buying_power']:,.2f}")


def cmd_positions(args):
    positions = alpaca.get_positions()
    if not positions:
        print("No open positions.")
        return

    print(f"{'Symbol':<10} {'Side':<6} {'Qty':<10} {'Avg Entry':<12} {'Current':<12} {'P&L':<12} {'P&L %'}")
    print("-" * 75)
    for p in positions:
        print(
            f"{p['symbol']:<10} {p['side']:<6} {p['qty']:<10g} "
            f"${p['avg_entry']:<11,.2f} ${p['current_price']:<11,.2f} "
            f"${p['unrealized_pl']:<11,.2f} {p['unrealized_plpc']:>6.2%}"
        )


def cmd_orders(args):
    orders = alpaca.get_orders(args.status)
    if not orders:
        print(f"No {args.status} orders.")
        return

    print(f"{'ID':<12} {'Time':<22} {'Symbol':<10} {'Side':<6} {'Qty':<7} {'Type':<8} {'Status':<12} {'Fill Price'}")
    print("-" * 95)
    for o in orders:
        ts = o["submitted_at"][:19].replace("T", " ")
        fill = f"${float(o['filled_avg_price']):,.2f}" if o["filled_avg_price"] else "-"
        print(
            f"{o['id'][:12]:<12} {ts:<22} {o['symbol']:<10} {o['side']:<6} "
            f"{o['qty']:<7} {o['type']:<8} {o['status']:<12} {fill}"
        )


def cmd_buy(args):
    order_type = "limit" if args.limit else "market"
    result = alpaca.buy(args.symbol, args.qty, order_type=order_type, limit_price=args.limit)
    _print_order(result, "BUY")


def cmd_sell(args):
    order_type = "limit" if args.limit else "market"
    result = alpaca.sell(args.symbol, args.qty, order_type=order_type, limit_price=args.limit)
    _print_order(result, "SELL")


def _print_order(result, action):
    print(f"{action} {result['qty']} {result['symbol']} ({result['type']}) — status: {result['status']}")
    print(f"Order ID: {result['id']}")


def main():
    parser = argparse.ArgumentParser(prog="paper_trading", description="OpenQuant Paper Trading (Alpaca)")
    sub = parser.add_subparsers(dest="command")

    sub.add_parser("account", help="Show account info")
    sub.add_parser("positions", help="Show open positions")

    p_orders = sub.add_parser("orders", help="List orders")
    p_orders.add_argument("--status", default="all", choices=["open", "closed", "all"])

    p_buy = sub.add_parser("buy", help="Place a paper buy")
    p_buy.add_argument("symbol")
    p_buy.add_argument("qty", type=float)
    p_buy.add_argument("--limit", type=float, help="Limit price (omit for market order)")

    p_sell = sub.add_parser("sell", help="Place a paper sell")
    p_sell.add_argument("symbol")
    p_sell.add_argument("qty", type=float)
    p_sell.add_argument("--limit", type=float, help="Limit price (omit for market order)")

    args = parser.parse_args()
    if not args.command:
        parser.print_help()
        sys.exit(1)

    cmds = {
        "account": cmd_account,
        "positions": cmd_positions,
        "orders": cmd_orders,
        "buy": cmd_buy,
        "sell": cmd_sell,
    }
    cmds[args.command](args)


if __name__ == "__main__":
    main()
