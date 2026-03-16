//! Fixed-size ring buffer — the foundation of all incremental features.
//!
//! A stack-allocated circular buffer with O(1) push, last, ago, and oldest.
//! Capacity must be a power of 2 so index wrapping uses bitwise AND (`& mask`)
//! instead of modulo — this is measurably faster on the hot path.
//!
//! # When is this useful?
//!
//! Any time you need a sliding window over recent values:
//! - SMA: running sum over last N closes
//! - Rolling std: running sum + sum-of-squares over last N returns
//! - Lookback returns: close now vs close N bars ago
//! - Donchian channel (future): tracking max/min over last N bars
//!
//! # Memory layout
//!
//! ```text
//!  capacity = 4, mask = 3 (0b11)
//!
//!  push(A): [A _ _ _]  head=1, len=1
//!  push(B): [A B _ _]  head=2, len=2
//!  push(C): [A B C _]  head=3, len=3
//!  push(D): [A B C D]  head=0, len=4 (full)
//!  push(E): [E B C D]  head=1, len=4 (A overwritten)
//!            ^oldest    ^newest = data[(head-1) & mask]
//! ```

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty() {
        let rb = RingBuf::<4>::new();
        assert_eq!(rb.len(), 0);
        assert!(!rb.is_full());
        assert!(rb.is_empty());
        assert_eq!(rb.last(), None);
        assert_eq!(rb.ago(0), None);
        assert_eq!(rb.oldest(), None);
    }

    #[test]
    fn push_and_access() {
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
    fn wraps_correctly() {
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
    fn full_cycle() {
        let mut rb = RingBuf::<4>::new();
        for i in 0..20 {
            rb.push(i as f64);
        }
        assert_eq!(rb.last(), Some(19.0));
        assert_eq!(rb.ago(1), Some(18.0));
        assert_eq!(rb.ago(3), Some(16.0));
    }
}
