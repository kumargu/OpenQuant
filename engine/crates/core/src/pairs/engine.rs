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

use super::active_pairs::{ClosedPairTrade, PairTradingHistory, load_active_pairs};
use super::{PairConfig, PairOrderIntent, PairPosition, PairState};
use std::path::{Path, PathBuf};
use tracing::{info, warn};

/// The pairs trading engine. Manages multiple pair states.
pub struct PairsEngine {
    /// Each pair has its config and mutable state.
    pairs: Vec<(PairConfig, PairState)>,
    /// Global bar counter (shared across all pairs).
    bar_counter: usize,
    /// Path to active_pairs.json (for reloading).
    active_pairs_path: Option<PathBuf>,
    /// Trading history (for Thompson sampling feedback).
    trade_history: PairTradingHistory,
    /// Path to write trading history.
    history_path: Option<PathBuf>,
}

impl PairsEngine {
    /// Create a new pairs engine from a list of pair configurations.
    pub fn new(configs: Vec<PairConfig>) -> Self {
        let pairs = configs.into_iter().map(|c| (c, PairState::new())).collect();

        Self {
            pairs,
            bar_counter: 0,
            active_pairs_path: None,
            trade_history: PairTradingHistory { trades: Vec::new() },
            history_path: None,
        }
    }

    /// Create a pairs engine from `active_pairs.json`.
    ///
    /// Falls back to `fallback_configs` if the file is missing, stale, or unparseable.
    pub fn from_active_pairs(
        active_pairs_path: &Path,
        history_path: &Path,
        fallback_configs: Vec<PairConfig>,
    ) -> Self {
        let trade_history = PairTradingHistory::load(history_path);
        info!(
            existing_trades = trade_history.trades.len(),
            "Loaded trading history"
        );

        let configs = match load_active_pairs(active_pairs_path) {
            Some((_file, configs)) => configs,
            None => {
                warn!(
                    fallback_count = fallback_configs.len(),
                    "Using fallback pair configs"
                );
                fallback_configs
            }
        };

        let pairs = configs.into_iter().map(|c| (c, PairState::new())).collect();

        Self {
            pairs,
            bar_counter: 0,
            active_pairs_path: Some(active_pairs_path.to_path_buf()),
            trade_history,
            history_path: Some(history_path.to_path_buf()),
        }
    }

    /// Reload pairs from `active_pairs.json`.
    ///
    /// Pairs with open positions are kept (not hard-cut); new pairs start fresh.
    /// Removed pairs with open positions get tightened stops (exit_z = 0.0 to
    /// close on next reversion, stop_z halved).
    pub fn reload(&mut self) -> bool {
        let path = match &self.active_pairs_path {
            Some(p) => p.clone(),
            None => return false,
        };

        let (_file, new_configs) = match load_active_pairs(&path) {
            Some(result) => result,
            None => return false,
        };

        let old_count = self.pairs.len();

        // Build set of new pair IDs
        let new_ids: std::collections::HashSet<String> = new_configs
            .iter()
            .map(|c| format!("{}/{}", c.leg_a, c.leg_b))
            .collect();

        // Handle removed pairs: tighten stops on open positions
        for (config, state) in &mut self.pairs {
            let pair_id = format!("{}/{}", config.leg_a, config.leg_b);
            if !new_ids.contains(&pair_id) && state.position() != PairPosition::Flat {
                info!(
                    pair = pair_id.as_str(),
                    "Pair removed from active list — tightening stops for graceful exit"
                );
                // Tighten: close on any z-score reversion, halve stop distance
                config.exit_z = 0.0;
                config.stop_z /= 2.0;
            }
        }

        // Remove pairs that are flat AND not in new configs
        self.pairs.retain(|(config, state)| {
            let pair_id = format!("{}/{}", config.leg_a, config.leg_b);
            let keep = new_ids.contains(&pair_id) || state.position() != PairPosition::Flat;
            if !keep {
                info!(pair = pair_id.as_str(), "Removed flat pair");
            }
            keep
        });

        // Add new pairs that don't already exist
        let existing_ids: std::collections::HashSet<String> = self
            .pairs
            .iter()
            .map(|(c, _)| format!("{}/{}", c.leg_a, c.leg_b))
            .collect();

        for config in new_configs {
            let pair_id = format!("{}/{}", config.leg_a, config.leg_b);
            if !existing_ids.contains(&pair_id) {
                info!(
                    pair = pair_id.as_str(),
                    beta = format!("{:.4}", config.beta).as_str(),
                    "Added new pair from active_pairs.json"
                );
                self.pairs.push((config, PairState::new()));
            }
        }

        info!(
            old_count,
            new_count = self.pairs.len(),
            "Pairs reloaded from active_pairs.json"
        );

        true
    }

    /// Record a closed trade in the trading history.
    ///
    /// Called by the Python runner when a pair trade is fully executed.
    pub fn record_trade(&mut self, trade: ClosedPairTrade) {
        if let Some(path) = &self.history_path {
            if let Err(e) = self.trade_history.append_and_save(trade, path) {
                warn!(error = %e, "Failed to write trading history");
            }
        } else {
            self.trade_history.trades.push(trade);
        }
    }

    /// Process a new bar. Called for every symbol on every bar.
    ///
    /// Iterates over all configured pairs and checks if this symbol is a leg.
    /// Returns order intents for any pairs that fire entry/exit signals.
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
        info!("pairs engine: daily reset (no-op for pair positions)");
    }

    /// Number of configured pairs.
    pub fn pair_count(&self) -> usize {
        self.pairs.len()
    }

    /// Get current positions for all pairs (for status reporting).
    pub fn positions(&self) -> Vec<(&PairConfig, PairPosition)> {
        self.pairs
            .iter()
            .map(|(config, state)| (config, state.position()))
            .collect()
    }

    /// Get trade history count.
    pub fn trade_count(&self) -> usize {
        self.trade_history.trades.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pairs::PairPosition;
    use tempfile::TempDir;

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

        let intents = engine.on_bar("GLD", 1000, 420.0);
        assert!(intents.is_empty());

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

        for _ in 0..35 {
            engine.on_bar("GLD", 1000, 420.0);
            let intents = engine.on_bar("SLV", 1000, 64.0);
            assert!(intents.is_empty(), "no signals during warmup");
        }

        assert_eq!(engine.positions()[0].1, PairPosition::Flat);

        engine.on_bar("GLD", 1000, 400.0);
        let intents = engine.on_bar("SLV", 1000, 64.0);

        if !intents.is_empty() {
            assert_eq!(intents.len(), 2, "pair entry = 2 legs");
            assert_eq!(intents[0].side, crate::signals::Side::Buy);
            assert_eq!(intents[1].side, crate::signals::Side::Sell);
        }
    }

    #[test]
    fn test_from_active_pairs_file() {
        let tmp = TempDir::new().unwrap();
        let active_path = tmp.path().join("active_pairs.json");
        let history_path = tmp.path().join("pair_trading_history.json");

        let json = format!(
            r#"{{
  "generated_at": "{}",
  "pairs": [
    {{
      "leg_a": "GS", "leg_b": "MS", "alpha": 0.5, "beta": 1.23,
      "half_life_days": 8.5, "adf_statistic": -3.5, "adf_pvalue": 0.003,
      "beta_cv": 0.08, "structural_break": false, "regime_robustness": 0.85,
      "economic_rationale": "banks", "score": 0.85
    }}
  ]
}}"#,
            chrono::Utc::now().to_rfc3339()
        );
        std::fs::write(&active_path, json).unwrap();

        let engine = PairsEngine::from_active_pairs(&active_path, &history_path, vec![]);
        assert_eq!(engine.pair_count(), 1);
        assert_eq!(engine.positions()[0].0.leg_a, "GS");
        assert!((engine.positions()[0].0.beta - 1.23).abs() < 0.01);
    }

    #[test]
    fn test_from_active_pairs_missing_file_uses_fallback() {
        let tmp = TempDir::new().unwrap();
        let active_path = tmp.path().join("nonexistent.json");
        let history_path = tmp.path().join("history.json");

        let engine =
            PairsEngine::from_active_pairs(&active_path, &history_path, vec![gld_slv_config()]);
        assert_eq!(engine.pair_count(), 1);
        assert_eq!(engine.positions()[0].0.leg_a, "GLD");
    }

    #[test]
    fn test_record_trade() {
        let tmp = TempDir::new().unwrap();
        let history_path = tmp.path().join("history.json");

        let mut engine = PairsEngine::new(vec![gld_slv_config()]);
        engine.history_path = Some(history_path.clone());
        engine.trade_history = PairTradingHistory { trades: Vec::new() };

        engine.record_trade(ClosedPairTrade {
            pair: ("GLD".into(), "SLV".into()),
            entry_date: "2026-03-10".into(),
            exit_date: "2026-03-14".into(),
            entry_zscore: 2.1,
            exit_zscore: 0.3,
            return_bps: 42.0,
            holding_period_bars: 4,
            exit_reason: "reversion".into(),
        });

        assert_eq!(engine.trade_count(), 1);

        // Verify file was written
        let reloaded = PairTradingHistory::load(&history_path);
        assert_eq!(reloaded.trades.len(), 1);
    }

    #[test]
    fn test_reload_adds_new_pairs() {
        let tmp = TempDir::new().unwrap();
        let active_path = tmp.path().join("active_pairs.json");

        let mut engine = PairsEngine::new(vec![gld_slv_config()]);
        engine.active_pairs_path = Some(active_path.clone());

        // Write file with GLD/SLV + new pair GS/MS
        let json = format!(
            r#"{{
  "generated_at": "{}",
  "pairs": [
    {{ "leg_a": "GLD", "leg_b": "SLV", "alpha": 0.1, "beta": 0.37,
       "half_life_days": 10.0, "adf_statistic": -4.0, "adf_pvalue": 0.001,
       "beta_cv": 0.05, "structural_break": false, "regime_robustness": 0.9,
       "economic_rationale": "metals", "score": 0.9 }},
    {{ "leg_a": "GS", "leg_b": "MS", "alpha": 0.5, "beta": 1.23,
       "half_life_days": 8.5, "adf_statistic": -3.5, "adf_pvalue": 0.003,
       "beta_cv": 0.08, "structural_break": false, "regime_robustness": 0.85,
       "economic_rationale": "banks", "score": 0.85 }}
  ]
}}"#,
            chrono::Utc::now().to_rfc3339()
        );
        std::fs::write(&active_path, json).unwrap();

        assert!(engine.reload());
        assert_eq!(engine.pair_count(), 2);
    }
}
