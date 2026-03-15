//! Cached metric handles for the engine hot path.
//!
//! Pre-registers all metric handles per symbol so subsequent calls are
//! pure atomic ops (~1.6ns counter, ~5ns histogram) instead of registry
//! lookups (~15ns per call).
//!
//! When no recorder is installed, all operations are noop (~1ns).

use metrics::{Counter, Histogram};
use std::collections::HashMap;

/// All cached metric handles for a single symbol on the hot path.
#[derive(Clone)]
pub struct SymbolHandles {
    // Engine-level
    pub bars_processed: Counter,
    pub on_bar_duration_ns: Histogram,
    pub stale_bars_skipped: Counter,

    // Features
    pub z_score: Histogram,
    pub relative_volume: Histogram,

    // Signal
    pub signal_buy: Counter,
    pub signal_sell: Counter,
    pub signal_score: Histogram,
    pub signal_rejected_no_warmup: Counter,
    pub signal_rejected_trend_filter: Counter,
    pub signal_rejected_volume_filter: Counter,

    // Risk
    pub risk_passed: Counter,
    pub risk_rejected_kill_switch: Counter,
    pub risk_rejected_cost_filter: Counter,
    pub risk_rejected_position_sizing: Counter,

    // Exit
    pub exit_stop_loss: Counter,
    pub exit_take_profit: Counter,
    pub exit_max_hold: Counter,
}

impl SymbolHandles {
    /// Register all metric handles for a symbol.
    /// Call once per symbol; returned handles are `Clone + Send + Sync`.
    pub fn new(symbol: &str) -> Self {
        let s = symbol.to_string();
        Self {
            bars_processed: metrics::counter!("engine.bars_processed", "symbol" => s.clone()),
            on_bar_duration_ns: metrics::histogram!("engine.on_bar.duration_ns", "symbol" => s.clone()),
            stale_bars_skipped: metrics::counter!("engine.stale_bars_skipped", "symbol" => s.clone()),

            z_score: metrics::histogram!("features.z_score", "symbol" => s.clone()),
            relative_volume: metrics::histogram!("features.relative_volume", "symbol" => s.clone()),

            signal_buy: metrics::counter!("signal.fired", "symbol" => s.clone(), "side" => "buy"),
            signal_sell: metrics::counter!("signal.fired", "symbol" => s.clone(), "side" => "sell"),
            signal_score: metrics::histogram!("signal.score", "symbol" => s.clone()),
            signal_rejected_no_warmup: metrics::counter!("signal.rejected", "symbol" => s.clone(), "reason" => "no_warmup"),
            signal_rejected_trend_filter: metrics::counter!("signal.rejected", "symbol" => s.clone(), "reason" => "trend_filter"),
            signal_rejected_volume_filter: metrics::counter!("signal.rejected", "symbol" => s.clone(), "reason" => "volume_filter"),

            risk_passed: metrics::counter!("risk.passed", "symbol" => s.clone()),
            risk_rejected_kill_switch: metrics::counter!("risk.rejected", "symbol" => s.clone(), "reason" => "kill_switch"),
            risk_rejected_cost_filter: metrics::counter!("risk.rejected", "symbol" => s.clone(), "reason" => "cost_filter"),
            risk_rejected_position_sizing: metrics::counter!("risk.rejected", "symbol" => s.clone(), "reason" => "position_sizing"),

            exit_stop_loss: metrics::counter!("exit.triggered", "symbol" => s.clone(), "reason" => "stop_loss"),
            exit_take_profit: metrics::counter!("exit.triggered", "symbol" => s.clone(), "reason" => "take_profit"),
            exit_max_hold: metrics::counter!("exit.triggered", "symbol" => s, "reason" => "max_hold"),
        }
    }
}

/// Registry of per-symbol cached metric handles.
/// Lazily creates handles on first access per symbol.
pub struct HotMetrics {
    symbols: HashMap<String, SymbolHandles>,
    enabled: bool,
}

impl HotMetrics {
    pub fn new(enabled: bool) -> Self {
        Self {
            symbols: HashMap::new(),
            enabled,
        }
    }

    /// Get or create cached handles for a symbol.
    /// Returns None if metrics are disabled.
    /// Uses borrowed lookup for existing symbols to avoid heap allocation
    /// on the hot path; only allocates when inserting a new symbol.
    #[inline]
    pub fn get(&mut self, symbol: &str) -> Option<&SymbolHandles> {
        if !self.enabled {
            return None;
        }
        if !self.symbols.contains_key(symbol) {
            self.symbols
                .insert(symbol.to_string(), SymbolHandles::new(symbol));
        }
        self.symbols.get(symbol)
    }
}

impl Default for HotMetrics {
    fn default() -> Self {
        Self::new(false)
    }
}
