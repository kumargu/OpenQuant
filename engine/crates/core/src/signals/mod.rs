//! Signal generation: features → trade decisions.
//!
//! ```text
//!  ┌──────────────┐
//!  │ FeatureValues │   (z-score, volume, close_loc, etc.)
//!  └──────┬───────┘
//!         │
//!         ▼
//!  ┌──────────────┐
//!  │   Strategy    │   (mean_reversion, momentum, etc.)
//!  │   .score()    │
//!  └──────┬───────┘
//!         │
//!         ▼
//!   Some(SignalOutput)  or  None
//!    │  side: Buy/Sell
//!    │  score: conviction strength
//!    │  reason: why it fired
//! ```
//!
//! Each strategy lives in its own module and implements the `Strategy` trait.
//! Strategies are independent, testable in isolation, and swappable.
//!
//! Adding a new strategy:
//! 1. Create `signals/my_strategy.rs`
//! 2. Implement the `Strategy` trait
//! 3. Add `pub mod my_strategy;` here

pub mod mean_reversion;
pub mod momentum;

use crate::features::FeatureValues;

/// Side of a trade.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Side {
    Buy,
    Sell,
}

/// Why a signal fired — enum to avoid String allocation in hot path.
/// Use `.describe()` to get a human-readable string when needed (logging, display).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SignalReason {
    MeanReversionBuy,
    MeanReversionSell,
    MomentumBuy,
    MomentumSell,
    StopLoss,
    TakeProfit,
    MaxHoldTime,
}

impl SignalReason {
    pub fn describe(&self) -> &'static str {
        match self {
            Self::MeanReversionBuy => "mean-reversion buy: oversold + volume confirmation",
            Self::MeanReversionSell => "mean-reversion sell: overbought reversion",
            Self::MomentumBuy => "momentum buy: EMA crossover + ADX trend confirmation",
            Self::MomentumSell => "momentum sell: trend reversal (fast EMA below slow)",
            Self::StopLoss => "stop loss: price dropped below threshold",
            Self::TakeProfit => "take profit: price rose above threshold",
            Self::MaxHoldTime => "max hold time: position held too long",
        }
    }
}

/// Output from a strategy's scoring function.
#[derive(Debug, Clone)]
pub struct SignalOutput {
    /// Buy or sell.
    pub side: Side,
    /// Conviction strength — higher means stronger signal.
    pub score: f64,
    /// Why this signal fired (enum, zero-alloc).
    pub reason: SignalReason,
    /// Feature snapshot for logging (z-score, relative volume).
    /// Cheap to copy (two f64s), useful for trade journals.
    pub z_score: f64,
    pub relative_volume: f64,
}

/// Trait that all strategies implement.
///
/// A strategy takes current feature values and position state,
/// and optionally returns a trade signal.
pub trait Strategy: Send + Sync {
    /// Score the current bar. Returns Some(signal) if a trade should be taken.
    fn score(&self, features: &FeatureValues, has_position: bool) -> Option<SignalOutput>;
}
