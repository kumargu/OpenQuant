//! Portfolio accounting — tracks positions and P&L.
//!
//! Same logic is used in backtesting, paper trading, and live trading.
//! Positions are computed from fills, not stored separately.
//! Supports averaging up/down and partial closes with realized P&L.

use crate::signals::Side;

/// A single open position.
#[derive(Debug, Clone)]
pub struct Position {
    pub symbol: String,
    pub qty: f64,
    pub avg_entry_price: f64,
    pub unrealized_pnl: f64,
}

impl Position {
    /// Mark position to current market price.
    pub fn mark_to_market(&mut self, current_price: f64) {
        self.unrealized_pnl = self.qty * (current_price - self.avg_entry_price);
    }
}

/// Tracks all positions for the portfolio.
#[derive(Debug, Clone)]
pub struct Portfolio {
    positions: Vec<Position>,
}

impl Default for Portfolio {
    fn default() -> Self {
        Self::new()
    }
}

impl Portfolio {
    pub fn new() -> Self {
        Self {
            positions: Vec::new(),
        }
    }

    /// Get position for a symbol, if any.
    pub fn get_position(&self, symbol: &str) -> Option<&Position> {
        self.positions.iter().find(|p| p.symbol == symbol)
    }

    /// Current quantity held for a symbol.
    pub fn position_qty(&self, symbol: &str) -> f64 {
        self.get_position(symbol).map(|p| p.qty).unwrap_or(0.0)
    }

    /// Returns true if holding any qty of this symbol.
    pub fn has_position(&self, symbol: &str) -> bool {
        self.position_qty(symbol) > 0.0
    }

    /// Record a fill. Returns realized P&L if closing/reducing a position.
    pub fn on_fill(&mut self, symbol: &str, side: Side, qty: f64, fill_price: f64) -> f64 {
        let pos = self.positions.iter_mut().find(|p| p.symbol == symbol);

        match side {
            Side::Buy => {
                match pos {
                    Some(p) => {
                        // Add to existing position
                        let total_cost = p.avg_entry_price * p.qty + fill_price * qty;
                        p.qty += qty;
                        p.avg_entry_price = total_cost / p.qty;
                        0.0 // no realized P&L on adding
                    }
                    None => {
                        self.positions.push(Position {
                            symbol: symbol.to_string(),
                            qty,
                            avg_entry_price: fill_price,
                            unrealized_pnl: 0.0,
                        });
                        0.0
                    }
                }
            }
            Side::Sell => {
                match pos {
                    Some(p) => {
                        let sell_qty = qty.min(p.qty);
                        let realized_pnl = sell_qty * (fill_price - p.avg_entry_price);
                        p.qty -= sell_qty;

                        // Remove position if fully closed
                        if p.qty <= 1e-10 {
                            self.positions.retain(|p| p.symbol != symbol);
                        }

                        realized_pnl
                    }
                    None => 0.0, // nothing to sell
                }
            }
        }
    }

    /// All open positions.
    pub fn positions(&self) -> &[Position] {
        &self.positions
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_buy_creates_position() {
        let mut pf = Portfolio::new();
        pf.on_fill("AAPL", Side::Buy, 10.0, 150.0);
        assert!(pf.has_position("AAPL"));
        assert_eq!(pf.position_qty("AAPL"), 10.0);
        assert_eq!(pf.get_position("AAPL").unwrap().avg_entry_price, 150.0);
    }

    #[test]
    fn test_buy_averages_up() {
        let mut pf = Portfolio::new();
        pf.on_fill("AAPL", Side::Buy, 10.0, 100.0);
        pf.on_fill("AAPL", Side::Buy, 10.0, 120.0);
        let pos = pf.get_position("AAPL").unwrap();
        assert_eq!(pos.qty, 20.0);
        assert!((pos.avg_entry_price - 110.0).abs() < 1e-10);
    }

    #[test]
    fn test_sell_realizes_pnl() {
        let mut pf = Portfolio::new();
        pf.on_fill("AAPL", Side::Buy, 10.0, 100.0);
        let pnl = pf.on_fill("AAPL", Side::Sell, 10.0, 110.0);
        assert!((pnl - 100.0).abs() < 1e-10); // 10 * (110-100) = 100
        assert!(!pf.has_position("AAPL"));
    }

    #[test]
    fn test_partial_sell() {
        let mut pf = Portfolio::new();
        pf.on_fill("AAPL", Side::Buy, 10.0, 100.0);
        let pnl = pf.on_fill("AAPL", Side::Sell, 5.0, 110.0);
        assert!((pnl - 50.0).abs() < 1e-10); // 5 * 10 = 50
        assert_eq!(pf.position_qty("AAPL"), 5.0);
    }

    #[test]
    fn test_mark_to_market() {
        let mut pf = Portfolio::new();
        pf.on_fill("AAPL", Side::Buy, 10.0, 100.0);
        let pos = pf
            .positions
            .iter_mut()
            .find(|p| p.symbol == "AAPL")
            .unwrap();
        pos.mark_to_market(115.0);
        assert!((pos.unrealized_pnl - 150.0).abs() < 1e-10);
    }
}
