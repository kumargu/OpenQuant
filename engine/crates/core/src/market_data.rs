//! Market data types — the canonical representation of price data.
//!
//! All data entering the engine is converted to these types first.
//! Bar is the primary unit: one OHLCV candle for a symbol at a timestamp.

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

/// Result of validating a bar sequence.
#[derive(Debug, Default)]
pub struct DataQualityReport {
    pub total_bars: usize,
    pub ohlc_violations: usize,
    pub non_positive_prices: usize,
    pub zero_volume_bars: usize,
    pub timestamp_backwards: usize,
    pub duplicate_timestamps: usize,
    /// Gaps longer than `gap_threshold_ms` (index, gap_ms).
    pub gaps: Vec<(usize, i64)>,
}

impl DataQualityReport {
    pub fn has_critical_issues(&self) -> bool {
        self.ohlc_violations > 0
            || self.non_positive_prices > 0
            || self.timestamp_backwards > 0
            || self.duplicate_timestamps > 0
    }

    pub fn zero_volume_pct(&self) -> f64 {
        if self.total_bars == 0 {
            return 0.0;
        }
        self.zero_volume_bars as f64 / self.total_bars as f64
    }
}

/// Validate a slice of bars and return a quality report.
/// `gap_threshold_ms`: any gap between consecutive bars larger than this is flagged.
pub fn validate_bars(bars: &[Bar], gap_threshold_ms: i64) -> DataQualityReport {
    let mut report = DataQualityReport {
        total_bars: bars.len(),
        ..Default::default()
    };

    let mut prev_ts: Option<i64> = None;

    for (i, bar) in bars.iter().enumerate() {
        // OHLC consistency: high >= max(open, close), low <= min(open, close)
        if bar.high < bar.open || bar.high < bar.close || bar.low > bar.open || bar.low > bar.close
        {
            report.ohlc_violations += 1;
        }

        // Price positivity and finiteness (NaN comparisons are always false,
        // so we must explicitly check with is_finite)
        if !bar.open.is_finite()
            || !bar.high.is_finite()
            || !bar.low.is_finite()
            || !bar.close.is_finite()
            || bar.open <= 0.0
            || bar.high <= 0.0
            || bar.low <= 0.0
            || bar.close <= 0.0
        {
            report.non_positive_prices += 1;
        }

        // Volume
        if bar.volume == 0.0 {
            report.zero_volume_bars += 1;
        }

        // Timestamp ordering
        if let Some(prev) = prev_ts {
            if bar.timestamp < prev {
                report.timestamp_backwards += 1;
            } else if bar.timestamp == prev {
                report.duplicate_timestamps += 1;
            } else {
                let gap = bar.timestamp - prev;
                if gap > gap_threshold_ms {
                    report.gaps.push((i, gap));
                }
            }
        }

        prev_ts = Some(bar.timestamp);
    }

    report
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

    fn tbar(ts: i64, close: f64, vol: f64) -> Bar {
        Bar {
            symbol: "TEST".into(),
            timestamp: ts,
            open: close,
            high: close + 1.0,
            low: close - 1.0,
            close,
            volume: vol,
        }
    }

    #[test]
    fn validate_clean_bars() {
        let bars = vec![
            tbar(1000, 100.0, 10.0),
            tbar(2000, 101.0, 12.0),
            tbar(3000, 99.0, 8.0),
        ];
        let report = validate_bars(&bars, 5000);
        assert_eq!(report.total_bars, 3);
        assert!(!report.has_critical_issues());
        assert_eq!(report.zero_volume_bars, 0);
        assert!(report.gaps.is_empty());
    }

    #[test]
    fn validate_detects_ohlc_violation() {
        let bars = vec![Bar {
            symbol: "TEST".into(),
            timestamp: 1000,
            open: 100.0,
            high: 95.0, // high < open = violation
            low: 90.0,
            close: 92.0,
            volume: 10.0,
        }];
        let report = validate_bars(&bars, 5000);
        assert_eq!(report.ohlc_violations, 1);
        assert!(report.has_critical_issues());
    }

    #[test]
    fn validate_detects_zero_volume() {
        let bars = vec![
            tbar(1000, 100.0, 0.0),
            tbar(2000, 101.0, 10.0),
            tbar(3000, 99.0, 0.0),
        ];
        let report = validate_bars(&bars, 5000);
        assert_eq!(report.zero_volume_bars, 2);
        assert!((report.zero_volume_pct() - 2.0 / 3.0).abs() < 1e-10);
    }

    #[test]
    fn validate_detects_timestamp_issues() {
        let bars = vec![
            tbar(1000, 100.0, 10.0),
            tbar(1000, 101.0, 10.0), // duplicate
            tbar(500, 99.0, 10.0),   // backwards
        ];
        let report = validate_bars(&bars, 5000);
        assert_eq!(report.duplicate_timestamps, 1);
        assert_eq!(report.timestamp_backwards, 1);
        assert!(report.has_critical_issues());
    }

    #[test]
    fn validate_detects_nan_prices() {
        let bars = vec![Bar {
            symbol: "TEST".into(),
            timestamp: 1000,
            open: f64::NAN,
            high: 105.0,
            low: 95.0,
            close: 100.0,
            volume: 10.0,
        }];
        let report = validate_bars(&bars, 5000);
        assert_eq!(report.non_positive_prices, 1);
        assert!(report.has_critical_issues());
    }

    #[test]
    fn validate_detects_infinity_prices() {
        let bars = vec![Bar {
            symbol: "TEST".into(),
            timestamp: 1000,
            open: 100.0,
            high: f64::INFINITY,
            low: 95.0,
            close: 100.0,
            volume: 10.0,
        }];
        let report = validate_bars(&bars, 5000);
        assert_eq!(report.non_positive_prices, 1);
    }

    #[test]
    fn validate_detects_gaps() {
        let bars = vec![
            tbar(1000, 100.0, 10.0),
            tbar(2000, 101.0, 10.0), // 1s gap (ok)
            tbar(10000, 99.0, 10.0), // 8s gap (flagged at 5s threshold)
        ];
        let report = validate_bars(&bars, 5000);
        assert_eq!(report.gaps.len(), 1);
        assert_eq!(report.gaps[0], (2, 8000));
    }
}
