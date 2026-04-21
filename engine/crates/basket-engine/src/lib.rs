//! Per-basket runtime engine with Bertram symmetric state machine.
//!
//! Consumes validated basket fits from `basket-picker` and produces
//! position intents in response to daily bars.

mod engine;
mod intent;
mod state;

pub use engine::BasketEngine;
pub use intent::{PositionIntent, TransitionReason};
pub use state::BasketState;

/// A daily bar for a single symbol.
#[derive(Debug, Clone)]
pub struct DailyBar {
    pub symbol: String,
    pub date: chrono::NaiveDate,
    pub close: f64,
}
