//! Simple Moving Average — running sum, O(1) per update.
//!
//! # When is this useful?
//!
//! - **Trend filter**: close > SMA → bullish regime, close < SMA → bearish.
//!   Mean-reversion strategies use this to avoid catching falling knives —
//!   don't buy the dip if the broader trend is down.
//!
//! - **Bollinger Bands center line**: the SMA is the middle of Bollinger Bands.
//!   Price oscillates around the SMA; extreme deviations revert.
//!
//! - **Support/resistance**: institutional algorithms often key off round SMAs
//!   (50-day, 200-day), creating self-fulfilling support/resistance levels.
//!
//! # Mathematics
//!
//! ```text
//!  push(new):
//!    if full: sum -= oldest
//!    sum += new
//!    sma = sum / len
//! ```
//!
//! Unlike EMA, SMA weights all bars in the window equally. This makes it
//! smoother but slower to react to recent price changes.
//!
//! # Window size note
//!
//! Uses power-of-2 capacity for the underlying ring buffer (bitwise index
//! wrapping). `Sma<32>` gives a 32-bar SMA, not 20. If you need exactly
//! 20 bars, you'd need a non-power-of-2 buffer or separate length cap.

use super::ring_buf::RingBuf;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn before_full() {
        let mut sma = Sma::<4>::new();
        let v = sma.push(10.0);
        assert!((v - 10.0).abs() < 1e-10); // 10/1
        let v = sma.push(20.0);
        assert!((v - 15.0).abs() < 1e-10); // 30/2
    }

    #[test]
    fn full_window() {
        let mut sma = Sma::<4>::new();
        sma.push(10.0);
        sma.push(20.0);
        sma.push(30.0);
        let v = sma.push(40.0);
        assert!((v - 25.0).abs() < 1e-10); // (10+20+30+40)/4
        assert!(sma.is_ready());
    }

    #[test]
    fn rolling_eviction() {
        let mut sma = Sma::<4>::new();
        sma.push(10.0);
        sma.push(20.0);
        sma.push(30.0);
        sma.push(40.0);
        let v = sma.push(50.0);
        // Window: [20,30,40,50], avg=35
        assert!((v - 35.0).abs() < 1e-10);
    }
}
