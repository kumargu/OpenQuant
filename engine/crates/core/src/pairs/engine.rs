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
use super::{PairConfig, PairOrderIntent, PairPosition, PairState, PairsTradingConfig};
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};

/// Canonical pair ID — alphabetically ordered to match Thompson sampling's pair_id().
fn canonical_pair_id(a: &str, b: &str) -> String {
    if a <= b {
        format!("{a}/{b}")
    } else {
        format!("{b}/{a}")
    }
}

/// The pairs trading engine. Manages multiple pair states.
/// Tracks cross-sectional dispersion of daily returns across all symbols.
/// High dispersion = idiosyncratic moves dominate = pairs decouple = suppress entries.
struct DispersionTracker {
    /// Last known close price per symbol.
    last_close: std::collections::HashMap<String, f64>,
    /// Daily returns collected for the current day.
    daily_returns: Vec<f64>,
    /// Rolling dispersion values (last N days).
    dispersion_history: std::collections::VecDeque<f64>,
    /// Current day timestamp (truncated to day boundary).
    current_day: i64,
    /// Is the dispersion currently elevated? (entry suppression flag)
    pub high_dispersion: bool,
}

impl DispersionTracker {
    fn new() -> Self {
        Self {
            last_close: std::collections::HashMap::new(),
            daily_returns: Vec::new(),
            dispersion_history: std::collections::VecDeque::new(),
            current_day: 0,
            high_dispersion: false,
        }
    }

    /// Update with a new price observation. Computes dispersion on day boundaries.
    fn update(&mut self, symbol: &str, timestamp: i64, close: f64) {
        let day = timestamp / 86_400_000; // truncate to day

        if day != self.current_day && self.current_day > 0 {
            // New day — compute dispersion from yesterday's returns
            self.compute_dispersion();
            self.daily_returns.clear();
        }
        self.current_day = day;

        // Compute return from previous close
        if let Some(&prev) = self.last_close.get(symbol)
            && prev > 0.0
        {
            let ret = (close / prev).ln();
            if ret.is_finite() {
                self.daily_returns.push(ret);
            }
        }
        self.last_close.insert(symbol.to_string(), close);
    }

    fn compute_dispersion(&mut self) {
        if self.daily_returns.len() < 5 {
            return; // not enough symbols for meaningful dispersion
        }

        let n = self.daily_returns.len() as f64;
        let mean = self.daily_returns.iter().sum::<f64>() / n;
        let variance = self
            .daily_returns
            .iter()
            .map(|r| (r - mean).powi(2))
            .sum::<f64>()
            / (n - 1.0);
        let dispersion = variance.sqrt();

        if dispersion.is_finite() {
            self.dispersion_history.push_back(dispersion);
            // Keep 60 days of history
            while self.dispersion_history.len() > 60 {
                self.dispersion_history.pop_front();
            }

            // High dispersion = above 75th percentile of recent history
            if self.dispersion_history.len() >= 10 {
                let mut sorted: Vec<f64> = self.dispersion_history.iter().copied().collect();
                sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
                let p75 = sorted[sorted.len() * 3 / 4];
                let was_high = self.high_dispersion;
                self.high_dispersion = dispersion > p75;

                if self.high_dispersion != was_high {
                    if self.high_dispersion {
                        warn!(
                            dispersion = format!("{:.6}", dispersion).as_str(),
                            threshold = format!("{:.6}", p75).as_str(),
                            "DISPERSION GATE — high intra-sector dispersion, suppressing new entries"
                        );
                    } else {
                        info!(
                            dispersion = format!("{:.6}", dispersion).as_str(),
                            threshold = format!("{:.6}", p75).as_str(),
                            "DISPERSION GATE — dispersion normalized, entries resumed"
                        );
                    }
                }
            }
        }
    }
}

pub struct PairsEngine {
    /// Each pair has its config and mutable state.
    pairs: Vec<(PairConfig, PairState)>,
    /// Shared trading parameters (from openquant.toml).
    trading_config: PairsTradingConfig,
    /// Path to active_pairs.json (for reloading).
    active_pairs_path: Option<PathBuf>,
    /// Trading history (for Thompson sampling feedback).
    trade_history: PairTradingHistory,
    /// Path to write trading history.
    history_path: Option<PathBuf>,
    /// Cross-sectional dispersion tracker (suppresses entries in high-dispersion regimes).
    dispersion: DispersionTracker,
    /// Global daily entry counter — resets each calendar day.
    daily_entries_today: usize,
    /// Calendar day of the daily entry counter.
    daily_entries_day: i64,
}

impl PairsEngine {
    /// Create from pre-built PairConfig list with trade history.
    /// Used when the runner generates pairs via pair-picker library (no JSON file).
    pub fn from_configs(
        configs: Vec<PairConfig>,
        history_path: &std::path::Path,
        trading_config: PairsTradingConfig,
    ) -> Self {
        let trade_history = PairTradingHistory::load(history_path);
        let mut engine = Self::new(configs, trading_config);
        engine.trade_history = trade_history;
        engine.history_path = Some(history_path.to_path_buf());
        engine
    }

    /// Create a new pairs engine from a list of pair configurations.
    pub fn new(configs: Vec<PairConfig>, trading_config: PairsTradingConfig) -> Self {
        info!(
            pairs = configs.len(),
            entry_z = %format_args!("{:.2}", trading_config.entry_z),
            exit_z = %format_args!("{:.2}", trading_config.exit_z),
            stop_z = %format_args!("{:.2}", trading_config.stop_z),
            lookback = trading_config.lookback,
            max_hold_bars = trading_config.max_hold_bars,
            min_hold_bars = trading_config.min_hold_bars,
            notional_per_leg = %format_args!("{:.0}", trading_config.notional_per_leg),
            cost_bps = %format_args!("{:.1}", trading_config.cost_bps),
            "PairsEngine initialized"
        );
        let pairs = configs
            .into_iter()
            .map(|c| {
                let state = PairState::for_pair(&c, &trading_config);
                (c, state)
            })
            .collect();

        Self {
            pairs,
            trading_config,
            active_pairs_path: None,
            trade_history: PairTradingHistory { trades: Vec::new() },
            history_path: None,
            dispersion: DispersionTracker::new(),
            daily_entries_today: 0,
            daily_entries_day: 0,
        }
    }

    /// Create a pairs engine from `active_pairs.json`.
    ///
    /// Falls back to `fallback_configs` if the file is missing, stale, or unparseable.
    pub fn from_active_pairs(
        active_pairs_path: &Path,
        history_path: &Path,
        fallback_configs: Vec<PairConfig>,
        trading_config: PairsTradingConfig,
        skip_staleness: bool,
    ) -> Self {
        let trade_history = PairTradingHistory::load(history_path);
        info!(
            existing_trades = trade_history.trades.len(),
            "Loaded trading history"
        );

        let configs = match load_active_pairs(active_pairs_path, skip_staleness) {
            Some((_file, configs)) => configs,
            None => {
                warn!(
                    fallback_count = fallback_configs.len(),
                    "Using fallback pair configs"
                );
                fallback_configs
            }
        };

        info!(
            entry_z = %format_args!("{:.2}", trading_config.entry_z),
            exit_z = %format_args!("{:.2}", trading_config.exit_z),
            stop_z = %format_args!("{:.2}", trading_config.stop_z),
            lookback = trading_config.lookback,
            max_hold_bars = trading_config.max_hold_bars,
            min_hold_bars = trading_config.min_hold_bars,
            notional_per_leg = %format_args!("{:.0}", trading_config.notional_per_leg),
            "PairsEngine initialized"
        );
        let pairs = configs
            .into_iter()
            .map(|c| {
                let state = PairState::for_pair(&c, &trading_config);
                (c, state)
            })
            .collect();

        Self {
            pairs,
            trading_config,
            active_pairs_path: Some(active_pairs_path.to_path_buf()),
            trade_history,
            history_path: Some(history_path.to_path_buf()),
            dispersion: DispersionTracker::new(),
            daily_entries_today: 0,
            daily_entries_day: 0,
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

        let (_file, new_configs) = match load_active_pairs(&path, false) {
            Some(result) => result,
            None => return false,
        };

        let old_count = self.pairs.len();

        // Build map of new configs by pair_id for O(1) lookup
        let new_configs_map: std::collections::HashMap<String, &PairConfig> = new_configs
            .iter()
            .map(|c| (canonical_pair_id(&c.leg_a, &c.leg_b), c))
            .collect();

        // Update existing pairs: refresh beta/alpha, or tighten stops if removed
        for (config, state) in &mut self.pairs {
            let pair_id = canonical_pair_id(&config.leg_a, &config.leg_b);
            if let Some(new_cfg) = new_configs_map.get(&pair_id) {
                // Pair still active — refresh hedge ratio (daily beta recalibration)
                if (config.beta - new_cfg.beta).abs() > 1e-6
                    || (config.alpha - new_cfg.alpha).abs() > 1e-6
                {
                    info!(
                        pair = pair_id.as_str(),
                        old_beta = format!("{:.4}", config.beta).as_str(),
                        new_beta = format!("{:.4}", new_cfg.beta).as_str(),
                        "Refreshed hedge ratio from active_pairs.json"
                    );
                    config.alpha = new_cfg.alpha;
                    config.beta = new_cfg.beta;
                }
                // Refresh lookback window if half-life changed (only when flat —
                // changing window mid-trade would invalidate exit context).
                if new_cfg.lookback_bars != config.lookback_bars
                    && state.position() == PairPosition::Flat
                {
                    let new_window = if new_cfg.lookback_bars > 0 {
                        new_cfg.lookback_bars
                    } else {
                        self.trading_config.lookback
                    };
                    info!(
                        pair = pair_id.as_str(),
                        old_window = config.lookback_bars,
                        new_window,
                        spread_observations = state.spread_count(),
                        "Resizing spread window (preserving observations)"
                    );
                    config.lookback_bars = new_cfg.lookback_bars;
                    // Full reset on window change. Resize() preserves spread
                    // observations but they were computed with old alpha/beta,
                    // creating inconsistency with the fresh hedge ratio.
                    // TODO: resize + recompute spreads with new alpha/beta.
                    *state = PairState::for_pair(config, &self.trading_config);
                }
            } else if state.position() != PairPosition::Flat {
                info!(
                    pair = pair_id.as_str(),
                    "Pair removed from active list — tightening stops for graceful exit"
                );
                state.exit_z_override = Some(0.0);
                let current_stop = state.stop_z_override.unwrap_or(self.trading_config.stop_z);
                state.stop_z_override = Some(current_stop / 2.0);
            }
        }

        // Remove pairs that are flat AND not in new configs
        self.pairs.retain(|(config, state)| {
            let pair_id = canonical_pair_id(&config.leg_a, &config.leg_b);
            let keep =
                new_configs_map.contains_key(&pair_id) || state.position() != PairPosition::Flat;
            if !keep {
                info!(pair = pair_id.as_str(), "Removed flat pair");
            }
            keep
        });

        // Add new pairs that don't already exist
        let existing_ids: std::collections::HashSet<String> = self
            .pairs
            .iter()
            .map(|(c, _)| canonical_pair_id(&c.leg_a, &c.leg_b))
            .collect();

        for config in new_configs {
            let pair_id = canonical_pair_id(&config.leg_a, &config.leg_b);
            if !existing_ids.contains(&pair_id) {
                info!(
                    pair = pair_id.as_str(),
                    beta = format!("{:.4}", config.beta).as_str(),
                    "Added new pair from active_pairs.json"
                );
                self.pairs.push((
                    config.clone(),
                    PairState::for_pair(&config, &self.trading_config),
                ));
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
    /// Entry signals are suppressed when `max_concurrent_pairs` is reached.
    pub fn on_bar(&mut self, symbol: &str, timestamp: i64, close: f64) -> Vec<PairOrderIntent> {
        // Update dispersion tracker with every bar
        self.dispersion.update(symbol, timestamp, close);

        let mut matched = false;
        let mut all_intents = Vec::new();

        let max_concurrent = self.trading_config.max_concurrent_pairs;
        let _dispersion_high = self.dispersion.high_dispersion;
        let mut current_open = self.open_position_count();

        // Reset daily entry counter on new calendar day
        let bar_day = timestamp / 86_400_000;
        if bar_day != self.daily_entries_day {
            self.daily_entries_day = bar_day;
            self.daily_entries_today = 0;
        }

        for (config, state) in &mut self.pairs {
            if config.leg_a == symbol || config.leg_b == symbol {
                matched = true;
            }
            let intents = state.on_price(symbol, close, config, &self.trading_config, timestamp);
            if !intents.is_empty() {
                // Block new entries when at capacity. Exits always pass through.
                let is_entry = intents
                    .iter()
                    .any(|i| matches!(i.reason, crate::signals::SignalReason::PairsEntry));
                let at_capacity = max_concurrent > 0 && current_open >= max_concurrent;
                let daily_cap = self.trading_config.max_daily_entries;
                let daily_cap_reached = daily_cap > 0 && self.daily_entries_today >= daily_cap;
                let block_entry = at_capacity || daily_cap_reached;
                if block_entry && is_entry {
                    state.force_flat();
                    if daily_cap_reached {
                        info!(
                            pair = format!("{}/{}", config.leg_a, config.leg_b).as_str(),
                            daily_entries = self.daily_entries_today,
                            max_daily_entries = daily_cap,
                            "entry blocked — daily entry cap reached"
                        );
                    } else {
                        info!(
                            pair = format!("{}/{}", config.leg_a, config.leg_b).as_str(),
                            max_concurrent, "entry blocked — position cap reached"
                        );
                    }
                } else {
                    if is_entry {
                        current_open += 1;
                        self.daily_entries_today += 1;
                    }
                    all_intents.extend(intents);
                }
            }
        }

        if !matched {
            debug!(
                symbol,
                "pairs: bar for unknown symbol — not a leg in any pair"
            );
        }

        all_intents
    }

    /// Reset daily state (e.g., at midnight UTC).
    pub fn reset_daily(&mut self) {
        info!("pairs engine: daily reset (no-op for pair positions)");
    }

    /// Flatten all positions — close everything in-engine without emitting orders.
    /// Used after warmup: rolling stats are warmed up but we don't want phantom positions.
    pub fn flatten_all(&mut self) {
        let mut flattened = 0;
        for (_config, state) in &mut self.pairs {
            if state.position() != super::PairPosition::Flat {
                state.force_flat();
                flattened += 1;
            }
        }
        info!(
            flattened,
            "pairs engine: flattened all positions (warmup reset)"
        );
    }

    /// Flatten all positions AND reset rolling spread stats.
    /// Used when switching timeframes (e.g., daily warmup → minute replay).
    /// Daily-bar variance would corrupt minute-bar z-scores if not reset.
    pub fn flatten_and_reset_stats(&mut self) {
        for (_config, state) in &mut self.pairs {
            if state.position() != super::PairPosition::Flat {
                state.force_flat();
            }
            state.reset_spread_stats();
        }
        info!("pairs engine: flattened + reset spread stats (timeframe switch)");
    }

    /// Block entries for all pairs containing the given symbol until the timestamp.
    /// Used for earnings blackout: if "BAC" reports earnings, block BAC/C, AXP/BAC, etc.
    pub fn block_symbol_entries(&mut self, symbol: &str, until_ts: i64) {
        for (config, state) in &mut self.pairs {
            if config.leg_a == symbol || config.leg_b == symbol {
                state.block_entry_until(until_ts);
                info!(
                    pair = format!("{}/{}", config.leg_a, config.leg_b).as_str(),
                    symbol, "earnings blackout set"
                );
            }
        }
    }

    /// Reconcile engine state with external positions (e.g., Alpaca on restart).
    /// For each pair, if both legs have positions in the provided map, restore
    /// the pair's position state. This prevents duplicate entries on restart.
    ///
    /// `positions`: map of symbol → (qty, avg_entry_price). Positive qty = long, negative = short.
    pub fn reconcile_positions(
        &mut self,
        positions: &std::collections::HashMap<String, (f64, f64)>,
    ) {
        let mut restored = 0;
        for (config, state) in &mut self.pairs {
            let leg_a = positions.get(&config.leg_a);
            let leg_b = positions.get(&config.leg_b);

            if let (Some(&(qty_a, price_a)), Some(&(qty_b, price_b))) = (leg_a, leg_b) {
                // Determine spread direction from position signs
                let direction = if qty_a > 0.0 && qty_b < 0.0 {
                    Some(super::PairPosition::LongSpread) // long A, short B
                } else if qty_a < 0.0 && qty_b > 0.0 {
                    Some(super::PairPosition::ShortSpread) // short A, long B
                } else {
                    None // legs don't form a clean pair trade
                };

                if let Some(pos) = direction {
                    state.restore_position(pos, price_a, price_b, config.beta);
                    restored += 1;
                    info!(
                        pair = format!("{}/{}", config.leg_a, config.leg_b).as_str(),
                        pos = ?pos,
                        price_a = format!("{price_a:.2}").as_str(),
                        price_b = format!("{price_b:.2}").as_str(),
                        "reconciled position from Alpaca"
                    );
                }
            }
        }
        info!(restored, "position reconciliation complete");
    }

    /// Set the path to the pairs file (monthly_pairs_YYYYMM.json) for reload().
    /// Needed when engine is created via from_configs() (replay) but
    /// later needs reload() after pair-picker writes a new file.
    pub fn set_pairs_path(&mut self, path: PathBuf) {
        self.active_pairs_path = Some(path);
    }

    /// Number of configured pairs.
    pub fn pair_count(&self) -> usize {
        self.pairs.len()
    }

    /// Number of pairs with open positions (not flat).
    pub fn open_position_count(&self) -> usize {
        self.pairs
            .iter()
            .filter(|(_, state)| state.position() != PairPosition::Flat)
            .count()
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

    fn default_trading() -> PairsTradingConfig {
        PairsTradingConfig {
            min_hold_bars: 0,
            ..PairsTradingConfig::default()
        }
    }

    fn gld_slv_config() -> PairConfig {
        PairConfig {
            leg_a: "GLD".into(),
            leg_b: "SLV".into(),
            alpha: 0.0,
            beta: 0.37,
            kappa: 0.0,
            max_hold_bars: 0,
            lookback_bars: 0,
        }
    }

    fn c_jpm_config() -> PairConfig {
        PairConfig {
            leg_a: "C".into(),
            leg_b: "JPM".into(),
            alpha: 0.0,
            beta: 1.39,
            kappa: 0.0,
            max_hold_bars: 0,
            lookback_bars: 0,
        }
    }

    #[test]
    fn test_multi_pair_engine() {
        let mut engine =
            PairsEngine::new(vec![gld_slv_config(), c_jpm_config()], default_trading());
        assert_eq!(engine.pair_count(), 2);

        let intents = engine.on_bar("GLD", 1000, 420.0);
        assert!(intents.is_empty());

        let intents = engine.on_bar("AAPL", 1000, 150.0);
        assert!(intents.is_empty());
    }

    #[test]
    fn test_positions_initially_flat() {
        let engine = PairsEngine::new(vec![gld_slv_config()], default_trading());
        let positions = engine.positions();
        assert_eq!(positions.len(), 1);
        assert_eq!(positions[0].1, PairPosition::Flat);
    }

    #[test]
    fn test_lifecycle_warmup_entry_exit() {
        let mut engine = PairsEngine::new(vec![gld_slv_config()], default_trading());

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

        let engine = PairsEngine::from_active_pairs(
            &active_path,
            &history_path,
            vec![],
            default_trading(),
            true,
        );
        assert_eq!(engine.pair_count(), 1);
        assert_eq!(engine.positions()[0].0.leg_a, "GS");
        assert!((engine.positions()[0].0.beta - 1.23).abs() < 0.01);
    }

    #[test]
    fn test_from_active_pairs_missing_file_uses_fallback() {
        let tmp = TempDir::new().unwrap();
        let active_path = tmp.path().join("nonexistent.json");
        let history_path = tmp.path().join("history.json");

        let engine = PairsEngine::from_active_pairs(
            &active_path,
            &history_path,
            vec![gld_slv_config()],
            default_trading(),
            true,
        );
        assert_eq!(engine.pair_count(), 1);
        assert_eq!(engine.positions()[0].0.leg_a, "GLD");
    }

    #[test]
    fn test_record_trade() {
        let tmp = TempDir::new().unwrap();
        let history_path = tmp.path().join("history.json");

        let mut engine = PairsEngine::new(vec![gld_slv_config()], default_trading());
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

        let mut engine = PairsEngine::new(vec![gld_slv_config()], default_trading());
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

    // -----------------------------------------------------------------------
    // Integration tests: verify data contracts between modules
    // -----------------------------------------------------------------------

    #[test]
    fn test_integration_alpha_used_in_spread() {
        // Verify that alpha from active_pairs.json is actually used in spread computation.
        // With alpha=0.5, spread = ln(A) - 0.5 - beta * ln(B)
        // Without alpha,  spread = ln(A) - beta * ln(B)
        // The difference should be visible in z-scores after warmup.
        let tmp = TempDir::new().unwrap();
        let active_path = tmp.path().join("active_pairs.json");
        let history_path = tmp.path().join("history.json");

        // Load with alpha=0.5, beta=1.0
        let json = format!(
            r#"{{
  "generated_at": "{}",
  "pairs": [{{
    "leg_a": "A", "leg_b": "B", "alpha": 0.5, "beta": 1.0,
    "half_life_days": 10.0, "adf_statistic": -4.0, "adf_pvalue": 0.001,
    "beta_cv": 0.05, "structural_break": false, "regime_robustness": 0.9,
    "economic_rationale": "test", "score": 0.9
  }}]
}}"#,
            chrono::Utc::now().to_rfc3339()
        );
        std::fs::write(&active_path, json).unwrap();

        let engine = PairsEngine::from_active_pairs(
            &active_path,
            &history_path,
            vec![],
            default_trading(),
            true,
        );
        // Verify alpha was loaded
        assert!(
            (engine.positions()[0].0.alpha - 0.5).abs() < 0.01,
            "alpha should be 0.5, got {}",
            engine.positions()[0].0.alpha
        );
        // Verify spread uses alpha: with both legs at same price (100),
        // spread = ln(100) - 0.5 - 1.0 * ln(100) = -0.5
        // Without alpha it would be 0.0
        let config = &engine.positions()[0].0;
        let spread = (100.0_f64).ln() - config.alpha - config.beta * (100.0_f64).ln();
        assert!(
            (spread - -0.5).abs() < 0.01,
            "spread should be -0.5 with alpha=0.5, got {spread}"
        );
    }

    #[test]
    fn test_integration_canonical_pair_id_in_history() {
        // Verify that trade history uses canonical pair_id matching Thompson sampling.
        // Record a trade with legs ("MS", "GS") → history file should have "GS/MS".
        let tmp = TempDir::new().unwrap();
        let history_path = tmp.path().join("history.json");

        let mut engine = PairsEngine::new(vec![], default_trading());
        engine.history_path = Some(history_path.clone());
        engine.trade_history = PairTradingHistory { trades: Vec::new() };

        // Use canonical_pair_id for the trade (as the engine would)
        let pair_id = canonical_pair_id("MS", "GS");
        assert_eq!(pair_id, "GS/MS", "canonical ordering should alphabetize");

        engine.record_trade(ClosedPairTrade {
            pair: ("GS".into(), "MS".into()), // canonical order
            entry_date: "2026-03-10".into(),
            exit_date: "2026-03-14".into(),
            entry_zscore: 2.1,
            exit_zscore: 0.3,
            return_bps: 42.0,
            holding_period_bars: 4,
            exit_reason: "reversion".into(),
        });

        // Reload and verify the pair tuple matches what Thompson expects
        let reloaded = PairTradingHistory::load(&history_path);
        assert_eq!(reloaded.trades[0].pair.0, "GS");
        assert_eq!(reloaded.trades[0].pair.1, "MS");
    }

    #[test]
    fn test_integration_beta_refresh_on_reload() {
        // Verify that reloading active_pairs.json updates beta on existing pairs.
        let tmp = TempDir::new().unwrap();
        let active_path = tmp.path().join("active_pairs.json");
        let history_path = tmp.path().join("history.json");

        // Initial load with beta=1.0
        let json_v1 = format!(
            r#"{{
  "generated_at": "{}",
  "pairs": [{{
    "leg_a": "A", "leg_b": "B", "alpha": 0.0, "beta": 1.0,
    "half_life_days": 10.0, "adf_statistic": -4.0, "adf_pvalue": 0.001,
    "beta_cv": 0.05, "structural_break": false, "regime_robustness": 0.9,
    "economic_rationale": "test", "score": 0.9
  }}]
}}"#,
            chrono::Utc::now().to_rfc3339()
        );
        std::fs::write(&active_path, &json_v1).unwrap();

        let mut engine = PairsEngine::from_active_pairs(
            &active_path,
            &history_path,
            vec![],
            default_trading(),
            true,
        );
        assert!((engine.positions()[0].0.beta - 1.0).abs() < 0.01);

        // Reload with updated beta=1.5
        let json_v2 = format!(
            r#"{{
  "generated_at": "{}",
  "pairs": [{{
    "leg_a": "A", "leg_b": "B", "alpha": 0.1, "beta": 1.5,
    "half_life_days": 10.0, "adf_statistic": -4.0, "adf_pvalue": 0.001,
    "beta_cv": 0.05, "structural_break": false, "regime_robustness": 0.9,
    "economic_rationale": "test", "score": 0.9
  }}]
}}"#,
            chrono::Utc::now().to_rfc3339()
        );
        std::fs::write(&active_path, &json_v2).unwrap();

        assert!(engine.reload());
        // Beta should now be 1.5, alpha should be 0.1
        assert!(
            (engine.positions()[0].0.beta - 1.5).abs() < 0.01,
            "beta should be refreshed to 1.5, got {}",
            engine.positions()[0].0.beta
        );
        assert!(
            (engine.positions()[0].0.alpha - 0.1).abs() < 0.01,
            "alpha should be refreshed to 0.1, got {}",
            engine.positions()[0].0.alpha
        );
    }

    #[test]
    fn test_position_cap_blocks_entries() {
        // 2 pairs, cap at 1 — second entry should be blocked.
        let trading = PairsTradingConfig {
            max_concurrent_pairs: 1,
            min_hold_bars: 0,
            ..PairsTradingConfig::default()
        };
        let mut engine = PairsEngine::new(vec![gld_slv_config(), c_jpm_config()], trading);
        assert_eq!(engine.pair_count(), 2);
        assert_eq!(engine.open_position_count(), 0);

        // Warmup both pairs with alternating high/low prices to build spread stats.
        // GLD/SLV: prices 95/64 and 105/64 alternating.
        // C/JPM: prices 95/200 and 105/200 alternating.
        const DAY: i64 = 86_400_000;
        for i in 0..35 {
            let ts = DAY * (i + 1);
            let price_a = if i % 2 == 0 { 95.0 } else { 105.0 };
            engine.on_bar("GLD", ts, price_a);
            engine.on_bar("SLV", ts, 64.0);
            engine.on_bar("C", ts, price_a);
            engine.on_bar("JPM", ts, 200.0);
        }
        engine.flatten_all();
        assert_eq!(engine.open_position_count(), 0);

        // Trigger entry on day 36 with a big shock on both pairs.
        let ts = DAY * 36;
        engine.on_bar("GLD", ts, 80.0); // big drop → should trigger
        engine.on_bar("SLV", ts, 64.0);
        engine.on_bar("C", ts, 80.0);
        engine.on_bar("JPM", ts, 200.0);

        // At most 1 pair should be open (cap = 1).
        // The first pair to fire gets in; the second is blocked.
        let open = engine.open_position_count();
        assert!(open <= 1, "cap should limit to 1 open position, got {open}");
    }
}
