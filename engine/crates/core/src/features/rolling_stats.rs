//! Rolling mean and standard deviation — O(1) per update.
//!
//! Uses running sum and sum-of-squares over a fixed window.
//! No iteration per update — just add new value, subtract oldest.
//!
//! # When is this useful?
//!
//! - **Z-score calculation**: z = (return - mean) / std_dev. The z-score is the
//!   core signal for mean-reversion strategies — it measures how many standard
//!   deviations a return is from the rolling average.
//!
//! - **Bollinger Bands**: upper/lower = SMA ± 2 × std_dev(close_prices).
//!   The squeeze (low bandwidth) predicts upcoming volatility expansion.
//!
//! - **Relative volume**: current_volume / rolling_mean(volume). Confirms
//!   that a price move has real participation behind it.
//!
//! - **Volatility estimation**: rolling std_dev of returns is a simple realized
//!   volatility measure, useful for position sizing and stop placement.
//!
//! # Mathematics
//!
//! ```text
//!  push(new):
//!    if full: sum -= oldest; sum_sq -= oldest²
//!    sum += new; sum_sq += new²
//!    buf.push(new)
//!
//!  mean     = sum / len
//!  variance = sum_sq/len - mean²      (population variance)
//!  std_dev  = sqrt(variance)
//! ```
//!
//! Note: uses population variance (N), not sample variance (N-1).
//! For rolling windows of 20+ bars, the difference is negligible.

use super::ring_buf::RingBuf;

#[derive(Clone)]
pub struct RollingStats<const N: usize> {
    buf: RingBuf<N>,
    sum: f64,
    sum_sq: f64,
}

impl<const N: usize> Default for RollingStats<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> RollingStats<N> {
    pub fn new() -> Self {
        Self {
            buf: RingBuf::new(),
            sum: 0.0,
            sum_sq: 0.0,
        }
    }

    #[inline]
    pub fn push(&mut self, value: f64) {
        if self.buf.is_full() {
            let old = self.buf.oldest().unwrap();
            self.sum -= old;
            self.sum_sq -= old * old;
        }
        self.sum += value;
        self.sum_sq += value * value;
        self.buf.push(value);
    }

    #[inline]
    pub fn is_ready(&self) -> bool {
        self.buf.is_full()
    }

    #[inline]
    pub fn mean(&self) -> f64 {
        if self.buf.is_empty() {
            return 0.0;
        }
        self.sum / self.buf.len() as f64
    }

    #[inline]
    pub fn variance(&self) -> f64 {
        let n = self.buf.len() as f64;
        if n < 2.0 {
            return 0.0;
        }
        let mean = self.sum / n;
        (self.sum_sq / n - mean * mean).max(0.0)
    }

    #[inline]
    pub fn std_dev(&self) -> f64 {
        self.variance().sqrt()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mean() {
        let mut rs = RollingStats::<4>::new();
        rs.push(2.0);
        rs.push(4.0);
        rs.push(6.0);
        assert!((rs.mean() - 4.0).abs() < 1e-10);
    }

    #[test]
    fn variance() {
        let mut rs = RollingStats::<4>::new();
        rs.push(2.0);
        rs.push(4.0);
        rs.push(6.0);
        // Var of [2,4,6]: mean=4, var = ((4+0+4)/3) = 8/3
        assert!((rs.variance() - 8.0 / 3.0).abs() < 1e-10);
    }

    #[test]
    fn window_evicts() {
        let mut rs = RollingStats::<4>::new();
        rs.push(10.0);
        rs.push(10.0);
        rs.push(10.0);
        rs.push(10.0);
        assert!(rs.is_ready());
        assert!((rs.mean() - 10.0).abs() < 1e-10);

        rs.push(20.0);
        // Window is now [10, 10, 10, 20], mean = 12.5
        assert!((rs.mean() - 12.5).abs() < 1e-10);
    }

    #[test]
    fn std_dev_zero_for_constant() {
        let mut rs = RollingStats::<4>::new();
        for _ in 0..4 {
            rs.push(5.0);
        }
        assert!(
            rs.std_dev() < 1e-10,
            "constant values should have zero std dev"
        );
    }

    #[test]
    fn known_std_dev() {
        let mut rs = RollingStats::<4>::new();
        // Window [1, 2, 3, 4]: mean=2.5, var = (1.5²+0.5²+0.5²+1.5²)/4 = 5/4 = 1.25
        rs.push(1.0);
        rs.push(2.0);
        rs.push(3.0);
        rs.push(4.0);
        assert!((rs.variance() - 1.25).abs() < 1e-10);
        assert!((rs.std_dev() - 1.25_f64.sqrt()).abs() < 1e-10);
    }
}
