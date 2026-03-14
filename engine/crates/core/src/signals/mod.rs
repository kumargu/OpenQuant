//! Signal generation: features → trade decisions.
//!
//! Each strategy lives in its own module and implements the `Strategy` trait.
//! The engine picks which strategy to run. Strategies are independent and
//! testable in isolation.
//!
//! Adding a new strategy:
//! 1. Create a new file in `signals/` (e.g., `momentum.rs`)
//! 2. Implement the `Strategy` trait
//! 3. Re-export it from this module

pub mod mean_reversion;

use crate::features::FeatureValues;

/// Side of a trade.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Side {
    Buy,
    Sell,
}

/// Output from a strategy's scoring function.
#[derive(Debug, Clone)]
pub struct SignalOutput {
    /// Buy or sell.
    pub side: Side,
    /// Conviction strength — higher means stronger signal.
    pub score: f64,
    /// Human-readable explanation of why this signal fired.
    pub reason: String,
}

/// Trait that all strategies implement.
///
/// A strategy takes current feature values and position state,
/// and optionally returns a trade signal.
pub trait Strategy: Send + Sync {
    /// Score the current bar. Returns Some(signal) if a trade should be taken.
    fn score(&self, features: &FeatureValues, has_position: bool) -> Option<SignalOutput>;
}
