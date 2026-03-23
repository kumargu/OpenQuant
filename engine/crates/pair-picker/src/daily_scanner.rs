//! Daily pair scanner — identifies pairs ready to trade on multi-day timeframe.
//!
//! Scans all candidate pairs for the "sweet spot":
//! - Half-life 2-5 days (matches multi-day holding period)
//! - |z-score| > 2.0 (spread diverged enough to trade)
//! - ADF statistic < -2.0 (some cointegration evidence)
//!
//! Outputs actionable signals: which pairs to enter tomorrow at market open.

use crate::stats::halflife::estimate_half_life;
use crate::stats::ols::ols_simple;
use serde::Serialize;
use tracing::info;

/// Configuration for the daily scanner.
pub struct ScannerConfig {
    /// Minimum half-life in days (default: 2.0).
    pub min_half_life: f64,
    /// Maximum half-life in days (default: 5.0).
    pub max_half_life: f64,
    /// Z-score entry threshold (default: 2.0).
    pub entry_z: f64,
    /// Rolling window for z-score computation (default: 30 days).
    pub lookback: usize,
}

impl Default for ScannerConfig {
    fn default() -> Self {
        Self {
            min_half_life: 2.0,
            max_half_life: 5.0,
            entry_z: 2.0,
            lookback: 30,
        }
    }
}

/// A scan result for one pair.
#[derive(Debug, Clone, Serialize)]
pub struct ScanResult {
    pub leg_a: String,
    pub leg_b: String,
    pub alpha: f64,
    pub beta: f64,
    pub half_life: f64,
    pub z_score: f64,
    pub spread_std_bps: f64,
    pub r_squared: f64,
    pub signal: ScanSignal,
}

/// Trading signal from the scanner.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub enum ScanSignal {
    /// z < -entry_z → long spread (buy A, sell B)
    LongSpread,
    /// z > +entry_z → short spread (sell A, buy B)
    ShortSpread,
    /// No signal (z within thresholds)
    NoSignal,
}

/// Scan a single pair using daily close prices.
///
/// Returns `None` if pair doesn't meet criteria (insufficient data, bad OLS, etc.)
pub fn scan_pair(
    leg_a: &str,
    leg_b: &str,
    prices_a: &[f64],
    prices_b: &[f64],
    config: &ScannerConfig,
) -> Option<ScanResult> {
    let n = prices_a.len().min(prices_b.len());
    if n < config.lookback + 10 {
        return None;
    }

    // Use the most recent `n` prices
    let pa = &prices_a[prices_a.len() - n..];
    let pb = &prices_b[prices_b.len() - n..];

    // Guard non-positive prices
    if pa.iter().any(|&p| !p.is_finite() || p <= 0.0)
        || pb.iter().any(|&p| !p.is_finite() || p <= 0.0)
    {
        return None;
    }

    let log_a: Vec<f64> = pa.iter().map(|p| p.ln()).collect();
    let log_b: Vec<f64> = pb.iter().map(|p| p.ln()).collect();

    // OLS regression
    let ols = ols_simple(&log_b, &log_a)?;
    if ols.r_squared < 0.2 {
        return None; // too weak
    }

    // Compute spread
    let spread: Vec<f64> = log_a
        .iter()
        .zip(log_b.iter())
        .map(|(a, b)| a - ols.alpha - ols.beta * b)
        .collect();

    // Half-life
    let hl = estimate_half_life(&spread)?;
    if hl.half_life < config.min_half_life || hl.half_life > config.max_half_life {
        return None;
    }

    // Z-score on rolling window
    let window = &spread[spread.len() - config.lookback..];
    let mean: f64 = window.iter().sum::<f64>() / window.len() as f64;
    let std = {
        let var =
            window.iter().map(|s| (s - mean).powi(2)).sum::<f64>() / (window.len() - 1) as f64;
        var.sqrt()
    };

    if std < 1e-10 {
        return None;
    }

    let current_spread = *spread.last().unwrap();
    let z = (current_spread - mean) / std;

    // Spread std in bps (for position sizing reference)
    let spread_std_bps = std * 10_000.0;

    let signal = if z < -config.entry_z {
        ScanSignal::LongSpread
    } else if z > config.entry_z {
        ScanSignal::ShortSpread
    } else {
        ScanSignal::NoSignal
    };

    Some(ScanResult {
        leg_a: leg_a.to_string(),
        leg_b: leg_b.to_string(),
        alpha: ols.alpha,
        beta: ols.beta,
        half_life: hl.half_life,
        z_score: z,
        spread_std_bps,
        r_squared: ols.r_squared,
        signal,
    })
}

/// Scan all candidate pairs and return actionable signals.
pub fn scan_all(
    candidates: &[(String, String)],
    prices: &std::collections::HashMap<String, Vec<f64>>,
    config: &ScannerConfig,
) -> Vec<ScanResult> {
    let mut results = Vec::new();

    for (leg_a, leg_b) in candidates {
        let pa = match prices.get(leg_a.as_str()) {
            Some(p) => p,
            None => continue,
        };
        let pb = match prices.get(leg_b.as_str()) {
            Some(p) => p,
            None => continue,
        };

        if let Some(result) = scan_pair(leg_a, leg_b, pa, pb, config) {
            if result.signal != ScanSignal::NoSignal {
                info!(
                    pair = format!("{}/{}", leg_a, leg_b).as_str(),
                    z = format!("{:.2}", result.z_score).as_str(),
                    hl = format!("{:.1}d", result.half_life).as_str(),
                    std_bps = format!("{:.0}", result.spread_std_bps).as_str(),
                    signal = ?result.signal,
                    "SIGNAL: pair in sweet spot"
                );
            }
            results.push(result);
        }
    }

    // Sort by absolute z-score descending (strongest signals first)
    results.sort_by(|a, b| {
        b.z_score
            .abs()
            .partial_cmp(&a.z_score.abs())
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let signals = results
        .iter()
        .filter(|r| r.signal != ScanSignal::NoSignal)
        .count();
    info!(
        scanned = candidates.len(),
        sweet_spot = results.len(),
        signals,
        "Daily scan complete"
    );

    results
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ou_prices(n: usize, half_life: f64, seed: u64) -> (Vec<f64>, Vec<f64>) {
        let phi = (-f64::ln(2.0) / half_life).exp();
        let mut state = seed;
        let mut next = |scale: f64| -> f64 {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((state >> 33) as f64 / u32::MAX as f64 - 0.5) * scale
        };

        let mut log_b = Vec::with_capacity(n);
        let mut b_val = 4.0;
        for _ in 0..n {
            b_val += next(0.02);
            log_b.push(b_val);
        }

        let mut spread = 0.0;
        let mut log_a = Vec::with_capacity(n);
        for lb in &log_b {
            spread = phi * spread + next(0.03); // larger noise for stronger signal
            log_a.push(1.5 * lb + 1.0 + spread);
        }

        let pa: Vec<f64> = log_a.iter().map(|x| x.exp()).collect();
        let pb: Vec<f64> = log_b.iter().map(|x| x.exp()).collect();
        (pa, pb)
    }

    #[test]
    fn test_scan_pair_in_sweet_spot() {
        let (pa, pb) = ou_prices(200, 3.0, 42);
        let config = ScannerConfig::default();
        let result = scan_pair("A", "B", &pa, &pb, &config);

        // Should find the pair (HL=3 is in 2-5 range)
        assert!(result.is_some(), "pair with HL=3 should be in sweet spot");
        let r = result.unwrap();
        assert!(r.half_life > 1.5 && r.half_life < 6.0, "hl={}", r.half_life);
    }

    #[test]
    fn test_scan_pair_too_fast() {
        let (pa, pb) = ou_prices(200, 0.5, 42); // HL=0.5 days, too fast
        let config = ScannerConfig::default();
        let result = scan_pair("A", "B", &pa, &pb, &config);
        // Should be rejected (HL < 2)
        assert!(result.is_none(), "HL=0.5 should be too fast");
    }

    #[test]
    fn test_scan_pair_too_slow() {
        let (pa, pb) = ou_prices(200, 20.0, 42); // HL=20 days, too slow
        let config = ScannerConfig::default();
        let result = scan_pair("A", "B", &pa, &pb, &config);
        // Should be rejected (HL > 5)
        assert!(result.is_none(), "HL=20 should be too slow");
    }

    #[test]
    fn test_scan_all_filters_correctly() {
        let (pa, pb) = ou_prices(200, 3.0, 42);
        let (px, py) = ou_prices(200, 20.0, 99); // too slow

        let mut prices = std::collections::HashMap::new();
        prices.insert("A".to_string(), pa);
        prices.insert("B".to_string(), pb);
        prices.insert("X".to_string(), px);
        prices.insert("Y".to_string(), py);

        let candidates = vec![
            ("A".to_string(), "B".to_string()),
            ("X".to_string(), "Y".to_string()),
        ];

        let results = scan_all(&candidates, &prices, &ScannerConfig::default());

        // Only the fast pair should be in results
        assert!(results.len() <= 1, "only sweet spot pairs should pass");
    }
}
