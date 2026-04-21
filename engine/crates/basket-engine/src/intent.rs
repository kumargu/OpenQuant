//! Position intent types for basket trading.

use serde::{Deserialize, Serialize};

/// Reason for a position transition.
///
/// Deliberately limited to exactly 4 valid transitions per Bertram symmetric.
/// No stop-loss, time-exit, or de-risk variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TransitionReason {
    /// First entry into a long position (from flat).
    InitialEntryLong,
    /// First entry into a short position (from flat).
    InitialEntryShort,
    /// Flip from long to short.
    FlipLongToShort,
    /// Flip from short to long.
    FlipShortToLong,
}

impl TransitionReason {
    /// Get short description for logging.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::InitialEntryLong => "initial_entry_long",
            Self::InitialEntryShort => "initial_entry_short",
            Self::FlipLongToShort => "flip_long_to_short",
            Self::FlipShortToLong => "flip_short_to_long",
        }
    }
}

/// A position intent produced by the engine on direction change.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PositionIntent {
    /// Basket identifier (sector:target format).
    pub basket_id: String,
    /// Target position: -1 (short), +1 (long). Never 0 after first entry.
    pub target_position: i8,
    /// Reason for this transition.
    pub reason: TransitionReason,
    /// Z-score that triggered this transition.
    pub z_score: f64,
    /// Spread value at transition.
    pub spread: f64,
    /// Date of the bar that triggered this transition.
    pub date: chrono::NaiveDate,
}

impl PositionIntent {
    /// Create a new position intent.
    pub fn new(
        basket_id: String,
        target_position: i8,
        reason: TransitionReason,
        z_score: f64,
        spread: f64,
        date: chrono::NaiveDate,
    ) -> Self {
        Self {
            basket_id,
            target_position,
            reason,
            z_score,
            spread,
            date,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_transition_reason_as_str() {
        assert_eq!(
            TransitionReason::InitialEntryLong.as_str(),
            "initial_entry_long"
        );
        assert_eq!(
            TransitionReason::FlipShortToLong.as_str(),
            "flip_short_to_long"
        );
    }

    #[test]
    fn test_position_intent_creation() {
        let intent = PositionIntent::new(
            "chips:AMD".to_string(),
            1,
            TransitionReason::InitialEntryLong,
            -1.5,
            0.03,
            chrono::NaiveDate::from_ymd_opt(2026, 4, 21).unwrap(),
        );
        assert_eq!(intent.basket_id, "chips:AMD");
        assert_eq!(intent.target_position, 1);
    }
}
