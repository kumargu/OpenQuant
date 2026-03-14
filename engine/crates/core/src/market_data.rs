/// A single OHLCV bar.
#[derive(Debug, Clone)]
pub struct Bar {
    pub symbol: String,
    pub timestamp: i64, // unix millis
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: f64,
}

impl Bar {
    /// Bar range: high - low.
    pub fn range(&self) -> f64 {
        self.high - self.low
    }

    /// Where close sits within the bar: 0.0 = at low, 1.0 = at high.
    pub fn close_location(&self) -> f64 {
        let range = self.range();
        if range == 0.0 {
            0.5
        } else {
            (self.close - self.low) / range
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bar(o: f64, h: f64, l: f64, c: f64) -> Bar {
        Bar {
            symbol: "TEST".into(),
            timestamp: 0,
            open: o,
            high: h,
            low: l,
            close: c,
            volume: 100.0,
        }
    }

    #[test]
    fn test_range() {
        assert_eq!(bar(100.0, 105.0, 95.0, 102.0).range(), 10.0);
    }

    #[test]
    fn test_close_location() {
        // Close at midpoint
        let b = bar(100.0, 110.0, 90.0, 100.0);
        assert!((b.close_location() - 0.5).abs() < 1e-10);

        // Close at high
        let b = bar(100.0, 110.0, 90.0, 110.0);
        assert!((b.close_location() - 1.0).abs() < 1e-10);

        // Close at low
        let b = bar(100.0, 110.0, 90.0, 90.0);
        assert!((b.close_location() - 0.0).abs() < 1e-10);

        // Zero range bar
        let b = bar(100.0, 100.0, 100.0, 100.0);
        assert!((b.close_location() - 0.5).abs() < 1e-10);
    }
}
