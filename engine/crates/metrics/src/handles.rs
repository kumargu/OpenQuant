//! Pre-registered metric handles for the hot path.
//!
//! The `metrics` crate macros (`counter!`, `histogram!`) perform a registry
//! lookup on every call (~12-15ns). By caching handles after the first call,
//! subsequent operations are pure atomic ops (~1.6ns counter, ~5ns histogram).
//!
//! `SymbolMetrics` holds all handles for a single symbol. The engine creates
//! one per active symbol and reuses it for every bar.

use metrics::{Counter, Histogram};
use std::collections::HashMap;

/// Cached metric handles for one symbol. Clone is cheap (Arc bumps).
#[derive(Clone)]
pub struct SymbolMetrics {
    pub bars_processed: Counter,
    pub on_bar_duration_ns: Histogram,
    pub z_score: Histogram,
    pub relative_volume: Histogram,
    pub signal_buy: Counter,
    pub signal_sell: Counter,
    pub risk_passed: Counter,
    pub risk_rejected: Counter,
}

impl SymbolMetrics {
    /// Register all metric handles for a symbol.
    /// Call once per symbol; the returned handles are `Clone + Send + Sync`.
    pub fn new(symbol: &str) -> Self {
        Self {
            bars_processed: metrics::counter!("engine.bars_processed", "symbol" => symbol.to_string()),
            on_bar_duration_ns: metrics::histogram!("engine.on_bar.duration_ns", "symbol" => symbol.to_string()),
            z_score: metrics::histogram!("features.z_score", "symbol" => symbol.to_string()),
            relative_volume: metrics::histogram!("features.relative_volume", "symbol" => symbol.to_string()),
            signal_buy: metrics::counter!("signal.fired", "symbol" => symbol.to_string(), "side" => "buy"),
            signal_sell: metrics::counter!("signal.fired", "symbol" => symbol.to_string(), "side" => "sell"),
            risk_passed: metrics::counter!("risk.passed", "symbol" => symbol.to_string()),
            risk_rejected: metrics::counter!("risk.rejected", "symbol" => symbol.to_string()),
        }
    }
}

/// Registry of per-symbol cached metric handles.
/// Lazily creates handles on first access per symbol.
pub struct MetricsRegistry {
    symbols: HashMap<String, SymbolMetrics>,
}

impl MetricsRegistry {
    pub fn new() -> Self {
        Self {
            symbols: HashMap::new(),
        }
    }

    /// Get or create cached handles for a symbol.
    pub fn get(&mut self, symbol: &str) -> &SymbolMetrics {
        self.symbols
            .entry(symbol.to_string())
            .or_insert_with(|| SymbolMetrics::new(symbol))
    }
}

impl Default for MetricsRegistry {
    fn default() -> Self {
        Self::new()
    }
}
