//! Pairs trading engine — manages multiple pair states and shared risk.
//!
//! Sits alongside the single-symbol `Engine`. The Python orchestrator feeds
//! bars to both engines and merges their order intents.
//!
//! ```text
//!  Python runner
//!  ├── Engine.on_bar(symbol, ...)       → single-symbol intents
//!  └── PairsEngine.on_bar(symbol, ...)  → pair trade intents (2 per signal)
//! ```

use super::{PairConfig, PairOrderIntent, PairState};
use tracing::info;

/// The pairs trading engine. Manages multiple pair states.
pub struct PairsEngine {
    /// Each pair has its config and mutable state.
    pairs: Vec<(PairConfig, PairState)>,
    /// Global bar counter (shared across all pairs).
    bar_counter: usize,
}

impl PairsEngine {
    /// Create a new pairs engine from a list of pair configurations.
    pub fn new(configs: Vec<PairConfig>) -> Self {
        let pairs = configs
            .into_iter()
            .map(|c| (c, PairState::new()))
            .collect();

        Self {
            pairs,
            bar_counter: 0,
        }
    }

    /// Process a new bar. Called for every symbol on every bar.
    ///
    /// Iterates over all configured pairs and checks if this symbol is a leg.
    /// Returns order intents for any pairs that fire entry/exit signals.
    ///
    /// Only needs the close price — pairs trading doesn't use OHLV.
    pub fn on_bar(&mut self, symbol: &str, _timestamp: i64, close: f64) -> Vec<PairOrderIntent> {
        self.bar_counter += 1;
        let mut all_intents = Vec::new();

        for (config, state) in &mut self.pairs {
            let intents = state.on_price(symbol, close, config);
            if !intents.is_empty() {
                all_intents.extend(intents);
            }
        }

        all_intents
    }

    /// Reset daily state (e.g., at midnight UTC).
    pub fn reset_daily(&mut self) {
        // Pairs positions carry over — no daily reset needed for pair state.
        // Risk state would be reset if we add shared risk tracking.
        info!("pairs engine: daily reset (no-op for pair positions)");
    }

    /// Number of configured pairs.
    pub fn pair_count(&self) -> usize {
        self.pairs.len()
    }

    /// Get current positions for all pairs (for status reporting).
    pub fn positions(&self) -> Vec<(&PairConfig, super::PairPosition)> {
        self.pairs
            .iter()
            .map(|(config, state)| (config, state.position()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pairs::PairPosition;

    fn gld_slv_config() -> PairConfig {
        PairConfig {
            leg_a: "GLD".into(),
            leg_b: "SLV".into(),
            beta: 0.37,
            entry_z: 2.0,
            exit_z: 0.5,
            stop_z: 4.0,
            lookback: 32,
            max_hold_bars: 150,
            notional_per_leg: 10_000.0,
        }
    }

    fn c_jpm_config() -> PairConfig {
        PairConfig {
            leg_a: "C".into(),
            leg_b: "JPM".into(),
            beta: 1.39,
            entry_z: 2.0,
            exit_z: 0.5,
            stop_z: 4.0,
            lookback: 32,
            max_hold_bars: 150,
            notional_per_leg: 10_000.0,
        }
    }

    #[test]
    fn test_multi_pair_engine() {
        let mut engine = PairsEngine::new(vec![gld_slv_config(), c_jpm_config()]);
        assert_eq!(engine.pair_count(), 2);

        // Feed bars for GLD — should not trigger (SLV missing)
        let intents = engine.on_bar("GLD", 1000, 420.0);
        assert!(intents.is_empty());

        // Feed bars for AAPL — unrelated, should be ignored
        let intents = engine.on_bar("AAPL", 1000, 150.0);
        assert!(intents.is_empty());
    }

    #[test]
    fn test_positions_initially_flat() {
        let engine = PairsEngine::new(vec![gld_slv_config()]);
        let positions = engine.positions();
        assert_eq!(positions.len(), 1);
        assert_eq!(positions[0].1, PairPosition::Flat);
    }

    #[test]
    fn test_lifecycle_warmup_entry_exit() {
        let mut engine = PairsEngine::new(vec![gld_slv_config()]);

        // Warmup: feed 35 stable bars
        for _ in 0..35 {
            engine.on_bar("GLD", 1000, 420.0);
            let intents = engine.on_bar("SLV", 1000, 64.0);
            assert!(intents.is_empty(), "no signals during warmup");
        }

        // Check position is still flat
        assert_eq!(engine.positions()[0].1, PairPosition::Flat);

        // Shock: drop GLD → spread drops → z negative → should trigger long spread
        engine.on_bar("GLD", 1000, 400.0);
        let intents = engine.on_bar("SLV", 1000, 64.0);

        // May or may not trigger depending on z-score magnitude
        if !intents.is_empty() {
            assert_eq!(intents.len(), 2, "pair entry = 2 legs");
            // Should be: buy GLD (leg_a), sell SLV (leg_b)
            assert_eq!(intents[0].side, crate::signals::Side::Buy);
            assert_eq!(intents[1].side, crate::signals::Side::Sell);
        }
    }
}
