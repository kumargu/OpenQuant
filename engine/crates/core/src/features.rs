//! Incremental feature computation from price bars.
//!
//! Features are the quantitative inputs to strategies. Every feature updates
//! in O(1) per bar using fixed-size stack buffers — zero heap allocation
//! in the hot path.
//!
//! ```text
//!  Bar (close, high, low, volume)
//!   │
//!   ├──► RingBuf<32> (closes)  ──► SMA-20 (running sum, O(1))
//!   │                           ──► N-bar returns (lookback)
//!   │
//!   ├──► RollingStats<32>      ──► return std dev (running sum + sum_sq)
//!   │    (1-bar returns)        ──► z-score = return / std_dev
//!   │
//!   ├──► RollingStats<32>      ──► relative volume = vol / avg_vol
//!   │    (volume)
//!   │
//!   └──► direct math           ──► bar range, close location
//! ```
//!
//! V1 features:
//! - Returns: 1-bar, 5-bar, 20-bar simple returns
//! - SMA: 20-bar simple moving average of close (running sum, not iter)
//! - Volatility: 20-bar rolling std dev of returns
//! - Z-score: current return / rolling volatility
//! - Volume: current volume / 20-bar avg volume
//! - Bar shape: range (high - low), close location within bar

// ---------------------------------------------------------------------------
// Ring buffer — const-generic, stack-allocated, zero-alloc
// ---------------------------------------------------------------------------

/// Fixed-size ring buffer on the stack. Capacity must be a power of 2
/// so index wrapping uses bitwise AND instead of modulo.
///
/// ```text
///  capacity = 4, mask = 3 (0b11)
///
///  push(A): [A _ _ _]  head=1, len=1
///  push(B): [A B _ _]  head=2, len=2
///  push(C): [A B C _]  head=3, len=3
///  push(D): [A B C D]  head=0, len=4 (full)
///  push(E): [E B C D]  head=1, len=4 (A overwritten)
///            ^oldest    ^newest = data[(head-1) & mask]
/// ```
#[derive(Clone)]
pub struct RingBuf<const N: usize> {
    data: [f64; N],
    head: usize,
    len: usize,
}

impl<const N: usize> Default for RingBuf<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> RingBuf<N> {
    const MASK: usize = N - 1; // only valid when N is power of 2

    pub fn new() -> Self {
        assert!(N.is_power_of_two(), "RingBuf capacity must be power of 2");
        Self {
            data: [0.0; N],
            head: 0,
            len: 0,
        }
    }

    #[inline]
    pub fn push(&mut self, value: f64) {
        self.data[self.head] = value;
        self.head = (self.head + 1) & Self::MASK;
        if self.len < N {
            self.len += 1;
        }
    }

    #[inline]
    pub fn is_full(&self) -> bool {
        self.len == N
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Most recent value pushed.
    #[inline]
    pub fn last(&self) -> Option<f64> {
        if self.len == 0 {
            None
        } else {
            Some(self.data[(self.head.wrapping_sub(1)) & Self::MASK])
        }
    }

    /// Value N steps ago (0 = most recent, 1 = previous, ...).
    #[inline]
    pub fn ago(&self, n: usize) -> Option<f64> {
        if n >= self.len {
            None
        } else {
            Some(self.data[(self.head.wrapping_sub(1 + n)) & Self::MASK])
        }
    }

    /// Oldest value in the buffer (will be overwritten on next push when full).
    #[inline]
    pub fn oldest(&self) -> Option<f64> {
        if self.len == 0 {
            None
        } else if self.is_full() {
            Some(self.data[self.head]) // head points to oldest when full
        } else {
            Some(self.data[0])
        }
    }
}

// ---------------------------------------------------------------------------
// Rolling stats — running sum + sum_sq, O(1) per update
// ---------------------------------------------------------------------------

/// Rolling mean and standard deviation over a fixed window.
/// Uses running sum and sum-of-squares — no iteration per update.
///
/// ```text
///  push(new):
///    if full: sum -= oldest; sum_sq -= oldest²
///    sum += new; sum_sq += new²
///    buf.push(new)
///
///  mean     = sum / len
///  variance = sum_sq/len - mean²
///  std_dev  = sqrt(variance)
/// ```
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

// ---------------------------------------------------------------------------
// SMA — running sum, O(1) per update
// ---------------------------------------------------------------------------

/// Simple moving average using a running sum.
/// Does NOT iterate the buffer — just adds new, subtracts oldest.
///
/// ```text
///  push(new):
///    if full: sum -= oldest
///    sum += new
///    sma = sum / len
/// ```
#[derive(Clone)]
pub struct Sma<const N: usize> {
    buf: RingBuf<N>,
    sum: f64,
}

impl<const N: usize> Default for Sma<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> Sma<N> {
    pub fn new() -> Self {
        Self {
            buf: RingBuf::new(),
            sum: 0.0,
        }
    }

    #[inline]
    pub fn push(&mut self, value: f64) -> f64 {
        if self.buf.is_full() {
            self.sum -= self.buf.oldest().unwrap();
        }
        self.sum += value;
        self.buf.push(value);
        self.sum / self.buf.len() as f64
    }

    #[inline]
    pub fn is_ready(&self) -> bool {
        self.buf.is_full()
    }
}

// ---------------------------------------------------------------------------
// Feature output + per-symbol state
// ---------------------------------------------------------------------------

/// All computed features for a single symbol at current bar.
#[derive(Debug, Clone, Default)]
pub struct FeatureValues {
    pub return_1: f64,        // 1-bar return
    pub return_5: f64,        // 5-bar return
    pub return_20: f64,       // 20-bar return
    pub sma_20: f64,          // 20-bar simple moving average of close
    pub sma_50: f64,          // 50-bar simple moving average of close (trend)
    pub atr: f64,             // average true range (14-bar)
    pub return_std_20: f64,   // 20-bar rolling std dev of 1-bar returns
    pub return_z_score: f64,  // return_1 / return_std_20
    pub relative_volume: f64, // current volume / 20-bar avg volume
    pub bar_range: f64,       // high - low
    pub close_location: f64,  // (close - low) / (high - low)
    pub trend_up: bool,       // true when close > sma_50 (bullish trend)
    pub warmed_up: bool,      // true once all features have enough data
}

/// Per-symbol feature state. All buffers are stack-allocated, fixed-size.
/// Uses power-of-2 capacity (32) for a 20-bar lookback window.
#[derive(Clone)]
pub struct FeatureState {
    closes: RingBuf<64>,         // last N closes for lookback returns (64 for SMA-50)
    sma: Sma<32>,                // 20-bar SMA via running sum
    sma_long: Sma<64>,           // 50-bar SMA for trend detection
    atr_stats: RollingStats<16>, // 14-bar ATR via rolling mean of true range
    return_stats: RollingStats<32>, // rolling std of 1-bar returns
    volume_stats: RollingStats<32>, // rolling avg of volume
    prev_close: Option<f64>,     // previous close for true range calculation
    bar_count: usize,
    warmup_period: usize,
}

impl Default for FeatureState {
    fn default() -> Self {
        Self::new()
    }
}

impl FeatureState {
    pub fn new() -> Self {
        Self {
            closes: RingBuf::new(),
            sma: Sma::new(),
            sma_long: Sma::new(),
            atr_stats: RollingStats::new(),
            return_stats: RollingStats::new(),
            volume_stats: RollingStats::new(),
            prev_close: None,
            bar_count: 0,
            warmup_period: 50, // increased from 20 to accommodate SMA-50
        }
    }

    /// Update features with a new bar. Returns computed values.
    /// This is the hot path — zero heap allocation, O(1) per call.
    #[inline]
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

        // N-bar returns via lookback
        let return_5 = self
            .closes
            .ago(5)
            .filter(|&pc| pc != 0.0)
            .map(|pc| (close - pc) / pc)
            .unwrap_or(0.0);

        let return_20 = self
            .closes
            .ago(19)
            .filter(|&pc| pc != 0.0)
            .map(|pc| (close - pc) / pc)
            .unwrap_or(0.0);

        // SMA-20 via running sum (O(1), no iteration)
        let sma_20 = self.sma.push(close);

        // SMA-50 for trend detection
        let sma_50 = self.sma_long.push(close);

        // ATR: True Range = max(H-L, |H-prev_close|, |L-prev_close|)
        let true_range = match self.prev_close {
            Some(pc) => {
                let hl = high - low;
                let hc = (high - pc).abs();
                let lc = (low - pc).abs();
                hl.max(hc).max(lc)
            }
            None => high - low, // first bar: just use range
        };
        self.atr_stats.push(true_range);
        self.prev_close = Some(close);
        let atr = self.atr_stats.mean();

        // Volume
        self.volume_stats.push(volume);
        let avg_volume = self.volume_stats.mean();
        let relative_volume = if avg_volume > 0.0 {
            volume / avg_volume
        } else {
            1.0
        };

        // Z-score
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

        // Trend: close above SMA-50 = bullish
        let trend_up = close > sma_50;

        FeatureValues {
            return_1,
            return_5,
            return_20,
            sma_20,
            sma_50,
            atr,
            return_std_20: std_dev,
            return_z_score,
            relative_volume,
            bar_range: range,
            close_location,
            trend_up,
            warmed_up: self.bar_count >= self.warmup_period,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // --- RingBuf tests ---

    #[test]
    fn ringbuf_empty() {
        let rb = RingBuf::<4>::new();
        assert_eq!(rb.len(), 0);
        assert!(!rb.is_full());
        assert_eq!(rb.last(), None);
        assert_eq!(rb.ago(0), None);
        assert_eq!(rb.oldest(), None);
    }

    #[test]
    fn ringbuf_push_and_access() {
        let mut rb = RingBuf::<4>::new();
        rb.push(10.0);
        rb.push(20.0);
        rb.push(30.0);
        assert_eq!(rb.len(), 3);
        assert!(!rb.is_full());
        assert_eq!(rb.last(), Some(30.0));
        assert_eq!(rb.ago(0), Some(30.0));
        assert_eq!(rb.ago(1), Some(20.0));
        assert_eq!(rb.ago(2), Some(10.0));
        assert_eq!(rb.ago(3), None);
    }

    #[test]
    fn ringbuf_wraps_correctly() {
        let mut rb = RingBuf::<4>::new();
        rb.push(1.0);
        rb.push(2.0);
        rb.push(3.0);
        rb.push(4.0);
        assert!(rb.is_full());
        assert_eq!(rb.oldest(), Some(1.0));

        // Push overwrites oldest
        rb.push(5.0);
        assert_eq!(rb.last(), Some(5.0));
        assert_eq!(rb.oldest(), Some(2.0)); // 1.0 is gone
        assert_eq!(rb.ago(0), Some(5.0));
        assert_eq!(rb.ago(1), Some(4.0));
        assert_eq!(rb.ago(2), Some(3.0));
        assert_eq!(rb.ago(3), Some(2.0));
    }

    #[test]
    fn ringbuf_full_cycle() {
        // Push more than capacity to test multiple wraps
        let mut rb = RingBuf::<4>::new();
        for i in 0..20 {
            rb.push(i as f64);
        }
        assert_eq!(rb.last(), Some(19.0));
        assert_eq!(rb.ago(1), Some(18.0));
        assert_eq!(rb.ago(3), Some(16.0));
    }

    // --- RollingStats tests ---

    #[test]
    fn rolling_stats_mean() {
        let mut rs = RollingStats::<4>::new();
        rs.push(2.0);
        rs.push(4.0);
        rs.push(6.0);
        assert!((rs.mean() - 4.0).abs() < 1e-10);
    }

    #[test]
    fn rolling_stats_variance() {
        let mut rs = RollingStats::<4>::new();
        rs.push(2.0);
        rs.push(4.0);
        rs.push(6.0);
        // Var of [2,4,6]: mean=4, var = ((4+0+4)/3) = 8/3
        assert!((rs.variance() - 8.0 / 3.0).abs() < 1e-10);
    }

    #[test]
    fn rolling_stats_window_evicts() {
        let mut rs = RollingStats::<4>::new();
        rs.push(10.0);
        rs.push(10.0);
        rs.push(10.0);
        rs.push(10.0);
        assert!(rs.is_ready());
        assert!((rs.mean() - 10.0).abs() < 1e-10);

        // Push a different value — oldest 10.0 is evicted
        rs.push(20.0);
        // Window is now [10, 10, 10, 20], mean = 12.5
        assert!((rs.mean() - 12.5).abs() < 1e-10);
    }

    #[test]
    fn rolling_stats_std_dev_zero_for_constant() {
        let mut rs = RollingStats::<4>::new();
        for _ in 0..4 {
            rs.push(5.0);
        }
        assert!(
            rs.std_dev() < 1e-10,
            "constant values should have zero std dev"
        );
    }

    // --- SMA tests ---

    #[test]
    fn sma_before_full() {
        let mut sma = Sma::<4>::new();
        let v = sma.push(10.0);
        assert!((v - 10.0).abs() < 1e-10); // 10/1
        let v = sma.push(20.0);
        assert!((v - 15.0).abs() < 1e-10); // 30/2
    }

    #[test]
    fn sma_full_window() {
        let mut sma = Sma::<4>::new();
        sma.push(10.0);
        sma.push(20.0);
        sma.push(30.0);
        let v = sma.push(40.0);
        assert!((v - 25.0).abs() < 1e-10); // (10+20+30+40)/4
        assert!(sma.is_ready());
    }

    #[test]
    fn sma_rolling_eviction() {
        let mut sma = Sma::<4>::new();
        sma.push(10.0);
        sma.push(20.0);
        sma.push(30.0);
        sma.push(40.0);
        // Window: [10,20,30,40], avg=25
        let v = sma.push(50.0);
        // Window: [20,30,40,50], avg=35
        assert!((v - 35.0).abs() < 1e-10);
    }

    // --- FeatureState tests ---

    #[test]
    fn feature_warmup() {
        let mut state = FeatureState::new();
        for i in 0..49 {
            let f = state.update(100.0 + i as f64, 101.0 + i as f64, 99.0 + i as f64, 1000.0);
            assert!(!f.warmed_up, "should not be warmed up at bar {i}");
        }
        let f = state.update(120.0, 121.0, 119.0, 1000.0);
        assert!(f.warmed_up, "should be warmed up at bar 50");
    }

    #[test]
    fn return_1_computation() {
        let mut state = FeatureState::new();
        state.update(100.0, 101.0, 99.0, 1000.0);
        let f = state.update(105.0, 106.0, 104.0, 1000.0);
        // (105 - 100) / 100 = 0.05
        assert!((f.return_1 - 0.05).abs() < 1e-10, "expected 5% return");
    }

    #[test]
    fn return_1_first_bar_is_zero() {
        let mut state = FeatureState::new();
        let f = state.update(100.0, 101.0, 99.0, 1000.0);
        assert_eq!(f.return_1, 0.0, "first bar has no previous close");
    }

    #[test]
    fn relative_volume_spike() {
        let mut state = FeatureState::new();
        for _ in 0..20 {
            state.update(100.0, 101.0, 99.0, 1000.0);
        }
        let f = state.update(100.0, 101.0, 99.0, 2000.0);
        // 2000 bar is included in rolling avg: (19*1000 + 2000) / 20 = 1050
        // Relative = 2000/1050 ≈ 1.905
        assert!(
            f.relative_volume > 1.5,
            "expected high relative volume, got {}",
            f.relative_volume
        );
    }

    #[test]
    fn z_score_extreme_drop() {
        let mut state = FeatureState::new();
        for _ in 0..20 {
            state.update(100.0, 100.5, 99.5, 1000.0);
        }
        let f = state.update(95.0, 100.0, 94.0, 1500.0);
        assert!(
            f.return_z_score < -2.0,
            "expected z < -2, got {}",
            f.return_z_score
        );
    }

    #[test]
    fn z_score_zero_for_constant_prices() {
        let mut state = FeatureState::new();
        for _ in 0..25 {
            let f = state.update(100.0, 100.0, 100.0, 1000.0);
            assert!(
                f.return_z_score.abs() < 1e-10,
                "constant prices should give z=0, got {}",
                f.return_z_score
            );
        }
    }

    #[test]
    fn bar_range_and_close_location() {
        let mut state = FeatureState::new();
        // Close at high
        let f = state.update(110.0, 110.0, 90.0, 1000.0);
        assert!((f.bar_range - 20.0).abs() < 1e-10);
        assert!((f.close_location - 1.0).abs() < 1e-10);

        // Close at low
        let f = state.update(90.0, 110.0, 90.0, 1000.0);
        assert!((f.close_location - 0.0).abs() < 1e-10);

        // Close at midpoint
        let f = state.update(100.0, 110.0, 90.0, 1000.0);
        assert!((f.close_location - 0.5).abs() < 1e-10);
    }

    #[test]
    fn zero_range_bar_close_location() {
        let mut state = FeatureState::new();
        let f = state.update(100.0, 100.0, 100.0, 1000.0);
        assert!(
            (f.close_location - 0.5).abs() < 1e-10,
            "zero range bar should default to 0.5"
        );
    }

    #[test]
    fn sma_matches_manual_calculation() {
        let mut state = FeatureState::new();
        let prices = [100.0, 102.0, 104.0, 103.0, 101.0];
        let mut f = FeatureValues::default();
        for &p in &prices {
            f = state.update(p, p + 1.0, p - 1.0, 1000.0);
        }
        // SMA of 5 values = (100+102+104+103+101)/5 = 102.0
        // But our SMA window is 32 (not 5), so it won't be full yet.
        // With 5 values in a 32-window, SMA = sum/5 = 102.0
        assert!((f.sma_20 - 102.0).abs() < 1e-10);
    }
}
