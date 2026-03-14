/// Incremental feature computation using ring buffers.
/// No full recomputation — each update is O(1).

/// Fixed-size ring buffer for rolling computations.
#[derive(Debug, Clone)]
pub struct RingBuffer {
    data: Vec<f64>,
    head: usize,
    len: usize,
    capacity: usize,
}

impl RingBuffer {
    pub fn new(capacity: usize) -> Self {
        Self {
            data: vec![0.0; capacity],
            head: 0,
            len: 0,
            capacity,
        }
    }

    pub fn push(&mut self, value: f64) {
        self.data[self.head] = value;
        self.head = (self.head + 1) % self.capacity;
        if self.len < self.capacity {
            self.len += 1;
        }
    }

    pub fn is_full(&self) -> bool {
        self.len == self.capacity
    }

    pub fn len(&self) -> usize {
        self.len
    }

    /// Iterate over values oldest to newest.
    pub fn iter(&self) -> impl Iterator<Item = f64> + '_ {
        let start = if self.is_full() {
            self.head
        } else {
            0
        };
        (0..self.len).map(move |i| self.data[(start + i) % self.capacity])
    }

    /// Most recent value.
    pub fn last(&self) -> Option<f64> {
        if self.len == 0 {
            None
        } else {
            let idx = (self.head + self.capacity - 1) % self.capacity;
            Some(self.data[idx])
        }
    }

    /// Value N steps ago (0 = most recent).
    pub fn ago(&self, n: usize) -> Option<f64> {
        if n >= self.len {
            None
        } else {
            let idx = (self.head + self.capacity - 1 - n) % self.capacity;
            Some(self.data[idx])
        }
    }
}

/// Incremental rolling mean and variance (Welford-style running stats).
#[derive(Debug, Clone)]
pub struct RollingStats {
    buf: RingBuffer,
    sum: f64,
    sum_sq: f64, // sum of squares for variance
}

impl RollingStats {
    pub fn new(window: usize) -> Self {
        Self {
            buf: RingBuffer::new(window),
            sum: 0.0,
            sum_sq: 0.0,
        }
    }

    pub fn push(&mut self, value: f64) {
        // Remove oldest if buffer is full
        if self.buf.is_full() {
            let oldest = self.buf.iter().next().unwrap();
            self.sum -= oldest;
            self.sum_sq -= oldest * oldest;
        }
        self.sum += value;
        self.sum_sq += value * value;
        self.buf.push(value);
    }

    pub fn is_ready(&self) -> bool {
        self.buf.is_full()
    }

    pub fn mean(&self) -> f64 {
        if self.buf.len() == 0 {
            return 0.0;
        }
        self.sum / self.buf.len() as f64
    }

    pub fn variance(&self) -> f64 {
        let n = self.buf.len() as f64;
        if n < 2.0 {
            return 0.0;
        }
        // Var = E[X²] - E[X]²
        let mean = self.sum / n;
        (self.sum_sq / n - mean * mean).max(0.0)
    }

    pub fn std_dev(&self) -> f64 {
        self.variance().sqrt()
    }
}

/// All computed features for a single symbol at current bar.
#[derive(Debug, Clone, Default)]
pub struct FeatureValues {
    pub return_1: f64,         // 1-bar return
    pub return_5: f64,         // 5-bar return
    pub return_20: f64,        // 20-bar return
    pub sma_20: f64,           // 20-bar simple moving average of close
    pub return_std_20: f64,    // 20-bar std dev of 1-bar returns
    pub return_z_score: f64,   // return_1 / return_std_20
    pub relative_volume: f64,  // current volume / 20-bar avg volume
    pub bar_range: f64,        // high - low
    pub close_location: f64,   // (close - low) / (high - low)
    pub warmed_up: bool,       // true once all features have enough data
}

/// Per-symbol feature state. Tracks ring buffers and computes features incrementally.
#[derive(Debug, Clone)]
pub struct FeatureState {
    closes: RingBuffer,        // last 20 closes for SMA and returns
    return_stats: RollingStats, // rolling mean/std of 1-bar returns
    volume_stats: RollingStats, // rolling mean of volume
    bar_count: usize,
    warmup_period: usize,
}

impl FeatureState {
    pub fn new() -> Self {
        Self {
            closes: RingBuffer::new(20),
            return_stats: RollingStats::new(20),
            volume_stats: RollingStats::new(20),
            bar_count: 0,
            warmup_period: 20,
        }
    }

    /// Update features with a new bar. Returns computed feature values.
    pub fn update(&mut self, close: f64, high: f64, low: f64, volume: f64) -> FeatureValues {
        let prev_close = self.closes.last();
        self.closes.push(close);
        self.bar_count += 1;

        // 1-bar return
        let return_1 = match prev_close {
            Some(pc) if pc != 0.0 => (close - pc) / pc,
            _ => 0.0,
        };
        self.return_stats.push(return_1);

        // N-bar returns
        let return_5 = self
            .closes
            .ago(5)
            .filter(|&pc| pc != 0.0)
            .map(|pc| (close - pc) / pc)
            .unwrap_or(0.0);

        let return_20 = self
            .closes
            .ago(19) // 19 because ago(0) is current
            .filter(|&pc| pc != 0.0)
            .map(|pc| (close - pc) / pc)
            .unwrap_or(0.0);

        // Volume stats
        self.volume_stats.push(volume);
        let avg_volume = self.volume_stats.mean();
        let relative_volume = if avg_volume > 0.0 {
            volume / avg_volume
        } else {
            1.0
        };

        // SMA 20
        let sma_20 = if self.closes.is_full() {
            self.closes.iter().sum::<f64>() / 20.0
        } else {
            close
        };

        // Return z-score
        let std_dev = self.return_stats.std_dev();
        let return_z_score = if std_dev > 1e-10 {
            return_1 / std_dev
        } else {
            0.0
        };

        // Bar shape
        let range = high - low;
        let close_location = if range > 0.0 {
            (close - low) / range
        } else {
            0.5
        };

        FeatureValues {
            return_1,
            return_5,
            return_20,
            sma_20,
            return_std_20: std_dev,
            return_z_score,
            relative_volume,
            bar_range: range,
            close_location,
            warmed_up: self.bar_count >= self.warmup_period,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ring_buffer_basics() {
        let mut rb = RingBuffer::new(3);
        rb.push(1.0);
        rb.push(2.0);
        rb.push(3.0);
        assert!(rb.is_full());
        assert_eq!(rb.last(), Some(3.0));
        assert_eq!(rb.ago(0), Some(3.0));
        assert_eq!(rb.ago(1), Some(2.0));
        assert_eq!(rb.ago(2), Some(1.0));
        assert_eq!(rb.ago(3), None);

        // Overwrite oldest
        rb.push(4.0);
        assert_eq!(rb.last(), Some(4.0));
        assert_eq!(rb.ago(2), Some(2.0)); // 1.0 is gone
        let vals: Vec<f64> = rb.iter().collect();
        assert_eq!(vals, vec![2.0, 3.0, 4.0]);
    }

    #[test]
    fn test_rolling_stats() {
        let mut rs = RollingStats::new(3);
        rs.push(2.0);
        rs.push(4.0);
        rs.push(6.0);
        assert!(rs.is_ready());
        assert!((rs.mean() - 4.0).abs() < 1e-10);

        // Variance of [2,4,6]: mean=4, var = ((4+0+4)/3) = 8/3
        let expected_var = 8.0 / 3.0;
        assert!((rs.variance() - expected_var).abs() < 1e-10);
    }

    #[test]
    fn test_feature_warmup() {
        let mut state = FeatureState::new();
        for i in 0..19 {
            let f = state.update(100.0 + i as f64, 101.0 + i as f64, 99.0 + i as f64, 1000.0);
            assert!(!f.warmed_up, "should not be warmed up at bar {i}");
        }
        let f = state.update(120.0, 121.0, 119.0, 1000.0);
        assert!(f.warmed_up, "should be warmed up at bar 20");
    }

    #[test]
    fn test_return_computation() {
        let mut state = FeatureState::new();
        state.update(100.0, 101.0, 99.0, 1000.0);
        let f = state.update(105.0, 106.0, 104.0, 1000.0);
        assert!((f.return_1 - 0.05).abs() < 1e-10, "expected 5% return");
    }

    #[test]
    fn test_relative_volume() {
        let mut state = FeatureState::new();
        // Feed 20 bars at volume 1000
        for _ in 0..20 {
            state.update(100.0, 101.0, 99.0, 1000.0);
        }
        // Now feed a bar at volume 2000 — should be 2x relative
        let f = state.update(100.0, 101.0, 99.0, 2000.0);
        // Not exactly 2.0 because the 2000 bar is included in the rolling avg
        // Avg = (19*1000 + 2000) / 20 = 21000/20 = 1050
        // Relative = 2000/1050 ≈ 1.905
        assert!(f.relative_volume > 1.5, "expected high relative volume, got {}", f.relative_volume);
    }

    #[test]
    fn test_z_score_extreme_move() {
        let mut state = FeatureState::new();
        // Feed 20 bars of steady prices
        for _ in 0..20 {
            state.update(100.0, 100.5, 99.5, 1000.0);
        }
        // Big drop
        let f = state.update(95.0, 100.0, 94.0, 1500.0);
        // z-score should be strongly negative
        assert!(f.return_z_score < -2.0, "expected z < -2, got {}", f.return_z_score);
    }
}
