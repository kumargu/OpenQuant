//! CUSUM (Cumulative Sum) structural break detector.
//!
//! Detects structurally significant price moves by accumulating positive
//! and negative deviations from the mean. Triggers when cumulative sum
//! exceeds a threshold, then resets. Based on AFML Chapter 2.
//!
//! Used to gate entry signals — only trade when a meaningful price move
//! has occurred, filtering out noise bars where price drifts sideways.

/// CUSUM detector state. Zero heap allocation, O(1) per update.
#[derive(Debug, Clone)]
pub struct CusumDetector {
    s_pos: f64,     // cumulative positive deviation
    s_neg: f64,     // cumulative negative deviation (stored as positive magnitude)
    threshold: f64, // static threshold for triggering
    triggered: bool,
}

impl CusumDetector {
    pub fn new(threshold: f64) -> Self {
        Self {
            s_pos: 0.0,
            s_neg: 0.0,
            threshold,
            triggered: false,
        }
    }

    /// Update with a new return value. Returns true if CUSUM triggered.
    ///
    /// When using dynamic threshold, pass `atr / close` as the threshold
    /// override (normalized ATR as fraction of price).
    #[inline]
    pub fn update(&mut self, return_1: f64, dynamic_threshold: Option<f64>) -> bool {
        let thresh = dynamic_threshold.unwrap_or(self.threshold);

        // Accumulate deviations (CUSUM filter — AFML eq 2.1)
        self.s_pos = (self.s_pos + return_1).max(0.0);
        self.s_neg = (self.s_neg - return_1).max(0.0);

        // Trigger on either side exceeding threshold
        if self.s_pos > thresh || self.s_neg > thresh {
            // Reset on trigger
            self.s_pos = 0.0;
            self.s_neg = 0.0;
            self.triggered = true;
        } else {
            self.triggered = false;
        }

        self.triggered
    }

    /// Whether the CUSUM fired on the last update.
    #[inline]
    pub fn triggered(&self) -> bool {
        self.triggered
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_trigger_on_small_moves() {
        let mut cusum = CusumDetector::new(0.01);
        // Small returns should not trigger
        assert!(!cusum.update(0.001, None));
        assert!(!cusum.update(0.002, None));
        assert!(!cusum.update(-0.001, None));
        assert!(!cusum.triggered());
    }

    #[test]
    fn triggers_on_accumulated_positive() {
        let mut cusum = CusumDetector::new(0.01);
        // Accumulate positive returns until trigger
        assert!(!cusum.update(0.005, None)); // s_pos = 0.005
        assert!(!cusum.update(0.004, None)); // s_pos = 0.009
        assert!(cusum.update(0.003, None)); // s_pos = 0.012 > 0.01 → trigger
        assert!(cusum.triggered());
    }

    #[test]
    fn triggers_on_accumulated_negative() {
        let mut cusum = CusumDetector::new(0.01);
        assert!(!cusum.update(-0.005, None)); // s_neg = 0.005
        assert!(!cusum.update(-0.004, None)); // s_neg = 0.009
        assert!(cusum.update(-0.003, None)); // s_neg = 0.012 > 0.01 → trigger
    }

    #[test]
    fn resets_after_trigger() {
        let mut cusum = CusumDetector::new(0.01);
        cusum.update(0.005, None);
        cusum.update(0.004, None);
        assert!(cusum.update(0.003, None)); // trigger
        // After trigger, sums are reset
        assert!(!cusum.update(0.001, None)); // small move, no trigger
        assert!(!cusum.triggered());
    }

    #[test]
    fn opposing_returns_cancel() {
        let mut cusum = CusumDetector::new(0.01);
        cusum.update(0.005, None); // s_pos = 0.005
        cusum.update(-0.005, None); // s_pos clamps to 0, s_neg = 0.005
        // Neither side should be close to threshold
        assert!(!cusum.update(0.001, None));
    }

    #[test]
    fn dynamic_threshold_overrides_static() {
        let mut cusum = CusumDetector::new(1.0); // very high static threshold
        // With dynamic threshold of 0.01, small moves should trigger
        assert!(!cusum.update(0.005, Some(0.01)));
        assert!(!cusum.update(0.004, Some(0.01)));
        assert!(cusum.update(0.003, Some(0.01))); // 0.012 > 0.01
    }

    #[test]
    fn single_large_move_triggers() {
        let mut cusum = CusumDetector::new(0.01);
        assert!(cusum.update(0.02, None)); // single return > threshold
    }
}
