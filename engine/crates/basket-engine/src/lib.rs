//! Per-basket runtime engine with Bertram symmetric state machine.
//!
//! Consumes validated basket fits from `basket-picker` and produces
//! position intents in response to daily bars.

mod engine;
mod intent;
mod portfolio;
mod state;

pub use engine::{BasketEngine, BasketParams, EngineSnapshot};
pub use intent::{PositionIntent, TransitionReason};
pub use portfolio::{
    aggregate_positions, basket_to_legs, diff_to_orders, target_shares_from_notionals,
    LegNotional, OrderIntent, OrderReason, PortfolioConfig, Side,
};
pub use state::BasketState;

/// A daily bar for a single symbol.
#[derive(Debug, Clone)]
pub struct DailyBar {
    pub symbol: String,
    pub date: chrono::NaiveDate,
    pub close: f64,
}
