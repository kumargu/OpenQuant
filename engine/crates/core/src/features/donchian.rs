//! Donchian channels — highest high / lowest low over N bars.
//!
//! Used for breakout detection: price exceeding the N-bar high/low signals
//! a potential breakout from a consolidation range.
//!
//! Implementation: stores highs and lows in separate `RingBuf<N>` and scans
//! on each bar. For N=32, scanning 32 f64s (~256 bytes) fits in a cache line
//! and is practically free.

use super::RingBuf;

/// Donchian channel state — tracks highest high and lowest low over N bars.
///
/// O(N) scan per bar, where N is small (32). Stack-allocated, zero heap.
#[derive(Clone)]
pub struct Donchian<const N: usize> {
    highs: RingBuf<N>,
    lows: RingBuf<N>,
}

/// Donchian channel values for a single bar.
#[derive(Debug, Clone, Copy, Default)]
pub struct DonchianValues {
    /// Highest high over last N bars.
    pub upper: f64,
    /// Lowest low over last N bars.
    pub lower: f64,
    /// Midpoint: (upper + lower) / 2.
    pub mid: f64,
}

impl<const N: usize> Default for Donchian<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> Donchian<N> {
    pub fn new() -> Self {
        Self {
            highs: RingBuf::new(),
            lows: RingBuf::new(),
        }
    }

    /// Update with a new bar's high and low.
    #[inline]
    pub fn update(&mut self, high: f64, low: f64) -> DonchianValues {
        self.highs.push(high);
        self.lows.push(low);

        let mut max_high = f64::NEG_INFINITY;
        let mut min_low = f64::INFINITY;

        let len = self.highs.len();
        for i in 0..len {
            if let Some(h) = self.highs.ago(i)
                && h > max_high
            {
                max_high = h;
            }
            if let Some(l) = self.lows.ago(i)
                && l < min_low
            {
                min_low = l;
            }
        }

        DonchianValues {
            upper: max_high,
            lower: min_low,
            mid: (max_high + min_low) / 2.0,
        }
    }

    /// Whether the channel has a full window of data.
    pub fn is_full(&self) -> bool {
        self.highs.is_full()
    }
}

/// Bandwidth percentile tracker — detects Bollinger squeeze.
///
/// Stores recent bandwidth values and computes the percentile rank
/// of the current value. A low percentile (< 0.20) indicates a squeeze.
#[derive(Clone)]
pub struct BandwidthPercentile<const N: usize> {
    buf: RingBuf<N>,
}

impl<const N: usize> Default for BandwidthPercentile<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> BandwidthPercentile<N> {
    pub fn new() -> Self {
        Self {
            buf: RingBuf::new(),
        }
    }

    /// Push a new bandwidth value and return its percentile rank (0.0-1.0).
    ///
    /// Percentile = fraction of recent values that are <= current value.
    /// Low percentile = bandwidth is tighter than usual = squeeze.
    #[inline]
    pub fn push(&mut self, bandwidth: f64) -> f64 {
        self.buf.push(bandwidth);
        let len = self.buf.len();
        if len < 2 {
            return 0.5; // not enough data
        }

        let mut count_below = 0usize;
        for i in 0..len {
            if let Some(v) = self.buf.ago(i)
                && v <= bandwidth
            {
                count_below += 1;
            }
        }
        count_below as f64 / len as f64
    }

    pub fn is_ready(&self) -> bool {
        self.buf.is_full()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn donchian_single_bar() {
        let mut d = Donchian::<4>::new();
        let v = d.update(105.0, 95.0);
        assert!((v.upper - 105.0).abs() < 1e-10);
        assert!((v.lower - 95.0).abs() < 1e-10);
        assert!((v.mid - 100.0).abs() < 1e-10);
    }

    #[test]
    fn donchian_tracks_extremes() {
        let mut d = Donchian::<4>::new();
        d.update(105.0, 95.0);
        d.update(110.0, 98.0);
        d.update(103.0, 92.0);
        let v = d.update(108.0, 97.0);
        assert!((v.upper - 110.0).abs() < 1e-10); // highest high
        assert!((v.lower - 92.0).abs() < 1e-10); // lowest low
    }

    #[test]
    fn donchian_window_slides() {
        let mut d = Donchian::<4>::new();
        // Fill: highs=[105, 110, 103, 108], lows=[95, 98, 92, 97]
        d.update(105.0, 95.0);
        d.update(110.0, 98.0);
        d.update(103.0, 92.0);
        d.update(108.0, 97.0);

        // Push new bar — oldest (105/95) slides out
        let v = d.update(101.0, 96.0);
        // highs now: [110, 103, 108, 101]
        assert!((v.upper - 110.0).abs() < 1e-10);
        // lows now: [98, 92, 97, 96]
        assert!((v.lower - 92.0).abs() < 1e-10);

        // Push again — oldest (110/98) slides out
        let v = d.update(102.0, 99.0);
        // highs: [103, 108, 101, 102]
        assert!((v.upper - 108.0).abs() < 1e-10);
        // lows: [92, 97, 96, 99]
        assert!((v.lower - 92.0).abs() < 1e-10);
    }

    #[test]
    fn bandwidth_percentile_squeeze() {
        let mut bp = BandwidthPercentile::<16>::new();
        // Feed in decreasing bandwidths (tightening)
        for i in (0..15).rev() {
            bp.push(10.0 + i as f64);
        }
        // Push a very tight bandwidth
        let pct = bp.push(5.0);
        // 5.0 is below all previous values, so percentile should be low
        assert!(pct < 0.15, "squeeze should have low percentile, got {pct}");
    }

    #[test]
    fn bandwidth_percentile_normal() {
        let mut bp = BandwidthPercentile::<16>::new();
        // Feed uniform bandwidths
        for i in 0..16 {
            bp.push(i as f64);
        }
        // Push a middle value
        let pct = bp.push(8.0);
        // ~9 values <= 8 out of 16 ≈ 0.56
        assert!(
            pct > 0.3 && pct < 0.7,
            "middle value should be mid percentile, got {pct}"
        );
    }

    #[test]
    fn bandwidth_percentile_not_enough_data() {
        let mut bp = BandwidthPercentile::<16>::new();
        let pct = bp.push(10.0);
        assert!((pct - 0.5).abs() < 1e-10, "single value should return 0.5");
    }

    #[test]
    fn donchian_is_full() {
        let mut d = Donchian::<4>::new();
        for i in 0..3 {
            d.update(100.0 + i as f64, 99.0);
            assert!(!d.is_full());
        }
        d.update(103.0, 99.0);
        assert!(d.is_full());
    }
}
