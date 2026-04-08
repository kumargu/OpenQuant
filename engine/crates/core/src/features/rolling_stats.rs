//! Rolling mean and standard deviation — O(1) per update, numerically stable.
//!
//! Uses Welford's online algorithm over a runtime-sized sliding window backed
//! by `VecDeque<f64>`. Window size is set at construction, not at compile time.
//!
//! # When is this useful?
//!
//! - **Z-score calculation**: z = (value - mean) / std_dev. The z-score is the
//!   core signal for mean-reversion strategies — it measures how many standard
//!   deviations a value is from the rolling average.
//!
//! - **Bollinger Bands**: upper/lower = SMA ± 2 × std_dev(close_prices).
//!
//! - **Relative volume**: current_volume / rolling_mean(volume).
//!
//! - **Volatility estimation**: rolling std_dev of returns for position sizing.
//!
//! # Mathematics — Welford's algorithm (1962)
//!
//! Welford's online algorithm maintains `mean` and `M2` (sum of squared
//! deviations from the current mean) incrementally. For a sliding window,
//! we apply the reverse update when evicting the oldest value.
//!
//! ```text
//!  add(x):
//!    n += 1
//!    delta  = x - mean
//!    mean  += delta / n
//!    delta2 = x - mean        // note: mean has been updated
//!    M2    += delta * delta2
//!
//!  remove(x):
//!    delta  = x - mean
//!    mean  -= delta / (n - 1)  // (n before decrement)
//!    delta2 = x - mean
//!    M2    -= delta * delta2
//!    n -= 1
//!
//!  variance = M2 / n           (population variance)
//!  std_dev  = sqrt(variance)
//! ```
//!
//! This avoids the catastrophic cancellation of the naive `sum_sq/n - mean²`
//! formula, which fails when values are large relative to variance (e.g.,
//! log-spreads of 0.5–5.0 with std 0.01–0.05).
//!
//! Reference: Welford, B. P. (1962). "Note on a Method for Calculating
//! Corrected Sums of Squares and Products."

use std::collections::VecDeque;

#[derive(Clone)]
pub struct RollingStats {
    buf: VecDeque<f64>,
    capacity: usize,
    mean: f64,
    m2: f64,
}

impl RollingStats {
    pub fn new(window: usize) -> Self {
        assert!(window > 0, "RollingStats window must be > 0");
        Self {
            buf: VecDeque::with_capacity(window),
            capacity: window,
            mean: 0.0,
            m2: 0.0,
        }
    }

    #[inline]
    pub fn push(&mut self, value: f64) {
        if self.buf.len() == self.capacity {
            let old = self.buf.pop_front().unwrap();
            // Reverse Welford: remove oldest value
            let n = self.buf.len() as f64; // count after removal from deque
            if n == 0.0 {
                self.mean = 0.0;
                self.m2 = 0.0;
            } else {
                let delta = old - self.mean;
                self.mean -= delta / n;
                let delta2 = old - self.mean;
                self.m2 -= delta * delta2;
                // Guard against floating-point drift making m2 negative
                if self.m2 < 0.0 {
                    self.m2 = 0.0;
                }
            }
        }
        // Forward Welford: add new value
        self.buf.push_back(value);
        let n = self.buf.len() as f64;
        let delta = value - self.mean;
        self.mean += delta / n;
        let delta2 = value - self.mean;
        self.m2 += delta * delta2;
    }

    #[inline]
    pub fn is_ready(&self) -> bool {
        self.buf.len() == self.capacity
    }

    #[inline]
    pub fn mean(&self) -> f64 {
        if self.buf.is_empty() {
            return 0.0;
        }
        self.mean
    }

    #[inline]
    pub fn variance(&self) -> f64 {
        let n = self.buf.len() as f64;
        if n < 2.0 {
            return 0.0;
        }
        (self.m2 / n).max(0.0)
    }

    #[inline]
    pub fn std_dev(&self) -> f64 {
        self.variance().sqrt()
    }

    /// Current number of observations in the window.
    #[inline]
    pub fn len(&self) -> usize {
        self.buf.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// Window capacity (set at construction).
    #[inline]
    pub fn window(&self) -> usize {
        self.capacity
    }

    /// Resize the window without losing existing observations.
    /// If shrinking, drops oldest values and recomputes stats.
    /// If growing, keeps all values and waits for more to fill.
    pub fn resize(&mut self, new_window: usize) {
        if new_window == 0 {
            return; // graceful no-op instead of panic in production
        }
        if new_window == self.capacity {
            return;
        }
        if new_window < self.buf.len() {
            // Shrinking: drop oldest values, recompute from remaining
            while self.buf.len() > new_window {
                self.buf.pop_front();
            }
            // Recompute mean/m2 from scratch (two-pass for stability)
            let n = self.buf.len() as f64;
            if n > 0.0 {
                self.mean = self.buf.iter().sum::<f64>() / n;
                self.m2 = self.buf.iter().map(|x| (x - self.mean).powi(2)).sum();
            } else {
                self.mean = 0.0;
                self.m2 = 0.0;
            }
        }
        // For growing: existing observations remain, just expand capacity
        self.capacity = new_window;
        // Pre-allocate if growing beyond current VecDeque capacity
        if new_window > self.buf.capacity() {
            self.buf.reserve(new_window - self.buf.capacity());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mean() {
        let mut rs = RollingStats::new(4);
        rs.push(2.0);
        rs.push(4.0);
        rs.push(6.0);
        assert!((rs.mean() - 4.0).abs() < 1e-10);
    }

    #[test]
    fn variance() {
        let mut rs = RollingStats::new(4);
        rs.push(2.0);
        rs.push(4.0);
        rs.push(6.0);
        // Var of [2,4,6]: mean=4, var = ((4+0+4)/3) = 8/3
        assert!((rs.variance() - 8.0 / 3.0).abs() < 1e-10);
    }

    #[test]
    fn window_evicts() {
        let mut rs = RollingStats::new(4);
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
        let mut rs = RollingStats::new(4);
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
        let mut rs = RollingStats::new(4);
        // Window [1, 2, 3, 4]: mean=2.5, var = (1.5²+0.5²+0.5²+1.5²)/4 = 5/4 = 1.25
        rs.push(1.0);
        rs.push(2.0);
        rs.push(3.0);
        rs.push(4.0);
        assert!((rs.variance() - 1.25).abs() < 1e-10);
        assert!((rs.std_dev() - 1.25_f64.sqrt()).abs() < 1e-10);
    }

    #[test]
    fn runtime_window_sizes() {
        // Verify various runtime window sizes work correctly
        for window in [1, 2, 3, 5, 7, 10, 16, 30, 32, 60, 100, 1000] {
            let mut rs = RollingStats::new(window);
            for i in 0..(window * 2) {
                rs.push(i as f64);
            }
            assert!(rs.is_ready());
            assert_eq!(rs.len(), window);
        }
    }

    /// Property test: Welford's matches naive two-pass on small inputs.
    #[test]
    fn welford_matches_naive_two_pass() {
        let data = vec![1.0, 3.0, 5.0, 7.0, 9.0, 2.0, 4.0, 6.0, 8.0, 10.0];
        let window = 5;
        let mut rs = RollingStats::new(window);

        for &v in &data {
            rs.push(v);
        }
        // Window should be [2, 4, 6, 8, 10]
        let window_data = &data[data.len() - window..];
        let naive_mean: f64 = window_data.iter().sum::<f64>() / window as f64;
        let naive_var: f64 = window_data
            .iter()
            .map(|x| (x - naive_mean).powi(2))
            .sum::<f64>()
            / window as f64;

        assert!(
            (rs.mean() - naive_mean).abs() < 1e-10,
            "mean: welford={} naive={}",
            rs.mean(),
            naive_mean
        );
        assert!(
            (rs.variance() - naive_var).abs() < 1e-10,
            "var: welford={} naive={}",
            rs.variance(),
            naive_var
        );
    }

    /// Property test: large offset values don't cause catastrophic cancellation.
    /// This is the key improvement over the naive sum_sq/n - mean² formula.
    #[test]
    fn stable_with_large_offset() {
        let mut rs = RollingStats::new(100);
        // Values clustered around 1_000_000 with tiny variance
        // Naive formula would fail here due to catastrophic cancellation
        for i in 0..100 {
            rs.push(1_000_000.0 + 0.001 * (i as f64));
        }
        let expected_mean = 1_000_000.0 + 0.001 * 49.5;
        assert!(
            (rs.mean() - expected_mean).abs() < 1e-6,
            "mean: got={} expected={}",
            rs.mean(),
            expected_mean
        );
        // Variance of 0.000, 0.001, ..., 0.099 = var of 0..99 * 0.001²
        // var(0..99) = (99² - 1) / 12 ≈ 833.25 → * 0.001² = 0.00083325
        let expected_var = (0..100)
            .map(|i| {
                let x = 1_000_000.0 + 0.001 * (i as f64);
                (x - expected_mean).powi(2)
            })
            .sum::<f64>()
            / 100.0;
        assert!(
            (rs.variance() - expected_var).abs() < 1e-10,
            "var: got={} expected={}",
            rs.variance(),
            expected_var
        );
    }

    /// Verify the eviction path maintains accuracy across many cycles.
    #[test]
    fn accuracy_after_many_evictions() {
        let mut rs = RollingStats::new(10);
        // Push 10_000 values, then verify the last window is accurate
        for i in 0..10_000 {
            rs.push(i as f64);
        }
        // Window should be [9990, 9991, ..., 9999]
        let expected_mean = 9994.5;
        let naive_var: f64 = (9990..10000)
            .map(|i| (i as f64 - expected_mean).powi(2))
            .sum::<f64>()
            / 10.0;
        assert!(
            (rs.mean() - expected_mean).abs() < 1e-6,
            "mean after 10k: got={} expected={}",
            rs.mean(),
            expected_mean
        );
        assert!(
            (rs.variance() - naive_var).abs() < 1e-6,
            "var after 10k: got={} expected={}",
            rs.variance(),
            naive_var
        );
    }
}
