//! Per-basket runtime engine with Bertram symmetric state machine.
//!
//! Consumes validated basket fits from `basket-picker` and produces
//! position intents in response to daily bars.

mod engine;
mod gates;
mod intent;
mod portfolio;
mod state;

pub use engine::{BasketEngine, BasketParams, EngineSnapshot};
pub use gates::{GatePolicyKind, RollingEntryMode, RollingSScoreV1Config};
pub use intent::{PositionIntent, TransitionReason};
pub use portfolio::{
    aggregate_positions, basket_to_legs, diff_to_orders, plan_portfolio, AdmissionScoreKind,
    LegNotional, OrderIntent, OrderReason, PortfolioConfig, PortfolioPlan, Side,
};
pub use state::{BasketState, MAX_SPREAD_HISTORY};

/// A daily bar for a single symbol.
#[derive(Debug, Clone)]
pub struct DailyBar {
    pub symbol: String,
    pub date: chrono::NaiveDate,
    pub close: f64,
}
