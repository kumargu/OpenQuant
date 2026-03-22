//! Shadow trading — track hypothetical P&L for new pairs before committing capital.
//!
//! Replaces the rigid "3-month OOS backtest before going live" with continuous
//! parallel validation. Shadow pairs run the same z-score signals as live pairs
//! but don't emit real orders.
//!
//! ## Lifecycle
//!
//! ```text
//! New pair → Shadow (track hypothetical P&L)
//!   → Promotion criteria met → Promoted (50% size for 5 trades, then full)
//!   → Shadow Sharpe < threshold → Removed
//!
//! Live pair → Rolling Sharpe drops → Demoted to Shadow
//!   → Shadow also degrades → Removed
//! ```

// chrono available if needed for timestamps in future
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::Path;
use tracing::{info, warn};

/// Pair trading mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PairMode {
    /// Tracking hypothetical P&L, no real orders.
    Shadow,
    /// Recently promoted — trading at reduced size (50% for first 5 trades).
    Promoted { trades_at_full: usize },
    /// Fully live — trading at full size.
    Live,
    /// Demoted from live — back to shadow tracking.
    Demoted,
}

impl PairMode {
    /// Position size multiplier for this mode.
    pub fn size_multiplier(&self) -> f64 {
        match self {
            PairMode::Shadow | PairMode::Demoted => 0.0, // no real orders
            PairMode::Promoted { trades_at_full } if *trades_at_full < 5 => 0.5,
            PairMode::Promoted { .. } | PairMode::Live => 1.0,
        }
    }

    /// Whether this mode emits real orders.
    pub fn is_live(&self) -> bool {
        matches!(self, PairMode::Promoted { .. } | PairMode::Live)
    }
}

/// A single shadow (hypothetical) trade.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShadowTrade {
    pub entry_bar: usize,
    pub exit_bar: usize,
    pub entry_z: f64,
    pub exit_z: f64,
    pub return_bps: f64,
}

/// Shadow tracking state for a single pair.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShadowState {
    pub pair_id: String,
    pub mode: PairMode,
    pub shadow_start_bar: usize,
    pub shadow_trades: Vec<ShadowTrade>,
    pub bars_tracked: usize,
}

/// Promotion criteria thresholds.
pub const MIN_SHADOW_TRADES: usize = 10;
pub const MIN_SHADOW_BARS: usize = 20;
pub const MIN_SHADOW_SHARPE: f64 = 1.0;
pub const MAX_SINGLE_LOSS_BPS: f64 = -100.0;

/// Demotion threshold: rolling 20-trade Sharpe below this → demote.
pub const DEMOTION_SHARPE_THRESHOLD: f64 = 0.5;
pub const DEMOTION_WINDOW: usize = 20;

/// Trades at reduced size before full promotion.
pub const PROMOTION_RAMP_TRADES: usize = 5;

impl ShadowState {
    pub fn new(pair_id: String, start_bar: usize) -> Self {
        Self {
            pair_id,
            mode: PairMode::Shadow,
            shadow_start_bar: start_bar,
            shadow_trades: Vec::new(),
            bars_tracked: 0,
        }
    }

    /// Record a shadow trade (hypothetical entry→exit).
    pub fn record_shadow_trade(&mut self, trade: ShadowTrade) {
        info!(
            pair = self.pair_id.as_str(),
            return_bps = format!("{:.1}", trade.return_bps).as_str(),
            total_shadow_trades = self.shadow_trades.len() + 1,
            "Shadow trade closed"
        );
        self.shadow_trades.push(trade);
    }

    /// Tick the bar counter.
    pub fn tick(&mut self) {
        self.bars_tracked += 1;
    }

    /// Check if this pair meets promotion criteria.
    pub fn meets_promotion_criteria(&self) -> bool {
        // Need at least 10 trades OR 20 bars (De Morgan's: fail only when BOTH are below)
        if self.shadow_trades.len() < MIN_SHADOW_TRADES && self.bars_tracked < MIN_SHADOW_BARS {
            return false;
        }
        if self.shadow_trades.is_empty() {
            return false;
        }

        // No single trade loss > MAX_SINGLE_LOSS_BPS
        if self
            .shadow_trades
            .iter()
            .any(|t| t.return_bps < MAX_SINGLE_LOSS_BPS)
        {
            return false;
        }

        // Shadow Sharpe > threshold
        let sharpe = self.shadow_sharpe();
        sharpe >= MIN_SHADOW_SHARPE
    }

    /// Compute Sharpe ratio of shadow trades.
    pub fn shadow_sharpe(&self) -> f64 {
        if self.shadow_trades.is_empty() {
            return 0.0;
        }
        let returns: Vec<f64> = self.shadow_trades.iter().map(|t| t.return_bps).collect();
        sharpe_ratio(&returns)
    }

    /// Check if a live pair should be demoted based on rolling performance.
    ///
    /// The caller (PairsEngine) must maintain per-pair live trade returns and pass
    /// them here. ShadowState only tracks hypothetical trades — live returns are
    /// owned by the engine to avoid coupling shadow tracking with order execution.
    pub fn should_demote(&self, recent_returns: &[f64]) -> bool {
        if recent_returns.len() < DEMOTION_WINDOW {
            return false;
        }
        let window = &recent_returns[recent_returns.len() - DEMOTION_WINDOW..];
        let sharpe = sharpe_ratio(window);
        sharpe < DEMOTION_SHARPE_THRESHOLD
    }

    /// Promote from shadow to live trading.
    pub fn promote(&mut self) {
        info!(
            pair = self.pair_id.as_str(),
            shadow_trades = self.shadow_trades.len(),
            shadow_sharpe = format!("{:.2}", self.shadow_sharpe()).as_str(),
            "Pair PROMOTED from shadow to live (50% size for first {} trades)",
            PROMOTION_RAMP_TRADES
        );
        self.mode = PairMode::Promoted { trades_at_full: 0 };
    }

    /// Record a completed live trade (for ramp-up tracking).
    pub fn record_live_trade(&mut self) {
        if let PairMode::Promoted { trades_at_full } = &mut self.mode {
            *trades_at_full += 1;
            if *trades_at_full >= PROMOTION_RAMP_TRADES {
                info!(
                    pair = self.pair_id.as_str(),
                    "Pair fully promoted — now trading at full size"
                );
                self.mode = PairMode::Live;
            }
        }
    }

    /// Demote from live back to shadow.
    pub fn demote(&mut self) {
        warn!(
            pair = self.pair_id.as_str(),
            mode = ?self.mode,
            "Pair DEMOTED to shadow — performance degraded"
        );
        self.mode = PairMode::Demoted;
        self.shadow_trades.clear();
        self.bars_tracked = 0;
    }

    /// Check if this pair should be removed entirely.
    /// Demoted pairs with enough shadow trades and negative Sharpe are beyond recovery.
    pub fn should_remove(&self) -> bool {
        matches!(self.mode, PairMode::Demoted)
            && self.shadow_trades.len() >= MIN_SHADOW_TRADES
            && self.shadow_sharpe() < 0.0
    }
}

/// Compute Sharpe ratio from a return series (annualization not needed for ranking).
fn sharpe_ratio(returns: &[f64]) -> f64 {
    let n = returns.len() as f64;
    if n < 2.0 {
        return 0.0;
    }
    let mean: f64 = returns.iter().sum::<f64>() / n;
    let var: f64 = returns.iter().map(|r| (r - mean).powi(2)).sum::<f64>() / (n - 1.0);
    let std = var.sqrt();
    if std < 1e-10 {
        // Near-zero variance: if mean is positive, treat as very high Sharpe
        return if mean > 0.0 {
            f64::INFINITY
        } else if mean < 0.0 {
            f64::NEG_INFINITY
        } else {
            0.0
        };
    }
    mean / std
}

/// Persistent shadow state across all pairs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShadowRegistry {
    pub pairs: HashMap<String, ShadowState>,
}

const SHADOW_FILE: &str = "shadow_trades.json";

impl ShadowRegistry {
    pub fn new() -> Self {
        Self {
            pairs: HashMap::new(),
        }
    }

    /// Get or create shadow state for a pair.
    pub fn get_or_create(&mut self, pair_id: &str, current_bar: usize) -> &mut ShadowState {
        self.pairs
            .entry(pair_id.to_string())
            .or_insert_with(|| ShadowState::new(pair_id.to_string(), current_bar))
    }

    /// Load from disk.
    pub fn load(data_dir: &Path) -> Self {
        let path = data_dir.join(SHADOW_FILE);
        match fs::read_to_string(&path) {
            Ok(contents) => serde_json::from_str(&contents).unwrap_or_else(|_| Self::new()),
            Err(_) => Self::new(),
        }
    }

    /// Save to disk.
    pub fn save(&self, data_dir: &Path) -> std::io::Result<()> {
        let path = data_dir.join(SHADOW_FILE);
        let json = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        fs::write(path, json)
    }

    /// Get pairs ready for promotion (includes demoted pairs that have recovered).
    pub fn promotable_pairs(&self) -> Vec<&str> {
        self.pairs
            .iter()
            .filter(|(_, s)| {
                matches!(s.mode, PairMode::Shadow | PairMode::Demoted)
                    && s.meets_promotion_criteria()
            })
            .map(|(id, _)| id.as_str())
            .collect()
    }

    /// Remove pairs that have degraded beyond recovery.
    /// Demoted pairs with enough shadow trades and negative Sharpe are removed entirely.
    pub fn cleanup(&mut self) -> Vec<String> {
        let removed: Vec<String> = self
            .pairs
            .iter()
            .filter(|(_, s)| s.should_remove())
            .map(|(id, _)| id.clone())
            .collect();

        for id in &removed {
            warn!(
                pair = id.as_str(),
                "Removing degraded pair from shadow registry"
            );
            self.pairs.remove(id);
        }

        removed
    }
}

impl Default for ShadowRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shadow_lifecycle() {
        let state = ShadowState::new("GS/MS".into(), 0);
        assert_eq!(state.mode, PairMode::Shadow);
        assert_eq!(state.mode.size_multiplier(), 0.0);
        assert!(!state.mode.is_live());
    }

    #[test]
    fn test_promotion_criteria_not_met_early() {
        let mut state = ShadowState::new("GS/MS".into(), 0);
        // Too few trades
        for i in 0..5 {
            state.record_shadow_trade(ShadowTrade {
                entry_bar: i * 10,
                exit_bar: i * 10 + 5,
                entry_z: 2.0,
                exit_z: 0.3,
                return_bps: 20.0,
            });
        }
        assert!(!state.meets_promotion_criteria());
    }

    #[test]
    fn test_promotion_criteria_met() {
        let mut state = ShadowState::new("GS/MS".into(), 0);
        for i in 0..12 {
            state.record_shadow_trade(ShadowTrade {
                entry_bar: i * 10,
                exit_bar: i * 10 + 5,
                entry_z: 2.0,
                exit_z: 0.3,
                return_bps: 20.0 + (i as f64), // consistent positive returns
            });
        }
        assert!(state.meets_promotion_criteria());
    }

    #[test]
    fn test_promotion_blocked_by_large_loss() {
        let mut state = ShadowState::new("GS/MS".into(), 0);
        for i in 0..12 {
            let ret = if i == 5 { -150.0 } else { 20.0 }; // one large loss
            state.record_shadow_trade(ShadowTrade {
                entry_bar: i * 10,
                exit_bar: i * 10 + 5,
                entry_z: 2.0,
                exit_z: 0.3,
                return_bps: ret,
            });
        }
        assert!(!state.meets_promotion_criteria());
    }

    #[test]
    fn test_promote_and_ramp_up() {
        let mut state = ShadowState::new("GS/MS".into(), 0);
        state.promote();
        assert_eq!(state.mode, PairMode::Promoted { trades_at_full: 0 });
        assert_eq!(state.mode.size_multiplier(), 0.5);
        assert!(state.mode.is_live());

        // Ramp up through 5 trades
        for _ in 0..4 {
            state.record_live_trade();
        }
        assert_eq!(state.mode.size_multiplier(), 0.5); // still 50%

        state.record_live_trade(); // 5th trade
        assert_eq!(state.mode, PairMode::Live);
        assert_eq!(state.mode.size_multiplier(), 1.0);
    }

    #[test]
    fn test_demotion() {
        let state = ShadowState::new("GS/MS".into(), 0);
        // Bad rolling returns
        let returns = vec![-5.0; 20];
        assert!(state.should_demote(&returns));
    }

    #[test]
    fn test_no_demotion_with_good_returns() {
        let state = ShadowState::new("GS/MS".into(), 0);
        let returns: Vec<f64> = (0..20).map(|i| 10.0 + (i as f64) * 0.5).collect();
        assert!(!state.should_demote(&returns));
    }

    #[test]
    fn test_no_demotion_insufficient_data() {
        let state = ShadowState::new("GS/MS".into(), 0);
        let returns = vec![15.0; 5]; // too few
        assert!(!state.should_demote(&returns));
    }

    #[test]
    fn test_demote_clears_state() {
        let mut state = ShadowState::new("GS/MS".into(), 0);
        state.mode = PairMode::Live;
        state.shadow_trades.push(ShadowTrade {
            entry_bar: 0,
            exit_bar: 5,
            entry_z: 2.0,
            exit_z: 0.3,
            return_bps: 20.0,
        });
        state.bars_tracked = 100;

        state.demote();
        assert_eq!(state.mode, PairMode::Demoted);
        assert!(state.shadow_trades.is_empty());
        assert_eq!(state.bars_tracked, 0);
    }

    #[test]
    fn test_sharpe_ratio() {
        assert_eq!(sharpe_ratio(&[]), 0.0);
        assert_eq!(sharpe_ratio(&[10.0]), 0.0);
        // Constant positive returns → infinite Sharpe (zero variance, positive mean)
        assert!(sharpe_ratio(&[10.0, 10.0, 10.0]).is_infinite());
        // Positive with variance
        let s = sharpe_ratio(&[10.0, 15.0, 20.0, 12.0, 18.0]);
        assert!(s > 0.0, "sharpe={s}");
    }

    #[test]
    fn test_shadow_registry_persistence() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path();

        let mut reg = ShadowRegistry::new();
        let state = reg.get_or_create("GS/MS", 0);
        state.record_shadow_trade(ShadowTrade {
            entry_bar: 0,
            exit_bar: 5,
            entry_z: 2.0,
            exit_z: 0.3,
            return_bps: 42.0,
        });
        reg.save(dir).unwrap();

        let loaded = ShadowRegistry::load(dir);
        assert_eq!(loaded.pairs.len(), 1);
        assert_eq!(loaded.pairs["GS/MS"].shadow_trades.len(), 1);
    }

    #[test]
    fn test_promotable_pairs() {
        let mut reg = ShadowRegistry::new();

        // Good pair — meets criteria
        let good = reg.get_or_create("GS/MS", 0);
        for i in 0..12 {
            good.record_shadow_trade(ShadowTrade {
                entry_bar: i * 10,
                exit_bar: i * 10 + 5,
                entry_z: 2.0,
                exit_z: 0.3,
                return_bps: 20.0,
            });
        }

        // Bad pair — not enough trades
        let _bad = reg.get_or_create("X/Y", 0);

        let promotable = reg.promotable_pairs();
        assert_eq!(promotable.len(), 1);
        assert_eq!(promotable[0], "GS/MS");
    }

    #[test]
    fn test_demoted_pair_can_recover() {
        let mut reg = ShadowRegistry::new();
        let state = reg.get_or_create("GS/MS", 0);
        state.mode = PairMode::Demoted;
        for i in 0..12 {
            state.record_shadow_trade(ShadowTrade {
                entry_bar: i * 10,
                exit_bar: i * 10 + 5,
                entry_z: 2.0,
                exit_z: 0.3,
                return_bps: 20.0 + (i as f64),
            });
        }
        let promotable = reg.promotable_pairs();
        assert_eq!(promotable.len(), 1, "Demoted pair should be re-promotable");
    }

    #[test]
    fn test_should_remove_degraded_demoted() {
        let mut state = ShadowState::new("BAD/PAIR".into(), 0);
        state.mode = PairMode::Demoted;
        for i in 0..12 {
            state.record_shadow_trade(ShadowTrade {
                entry_bar: i * 10,
                exit_bar: i * 10 + 5,
                entry_z: 2.0,
                exit_z: 0.3,
                return_bps: -20.0,
            });
        }
        assert!(
            state.should_remove(),
            "Degraded demoted pair should be removed"
        );
    }

    #[test]
    fn test_shadow_pair_not_removed() {
        let state = ShadowState::new("GS/MS".into(), 0);
        assert!(!state.should_remove());
    }

    #[test]
    fn test_cleanup_removes_degraded() {
        let mut reg = ShadowRegistry::new();

        let bad = reg.get_or_create("BAD/PAIR", 0);
        bad.mode = PairMode::Demoted;
        for i in 0..12 {
            bad.record_shadow_trade(ShadowTrade {
                entry_bar: i * 10,
                exit_bar: i * 10 + 5,
                entry_z: 2.0,
                exit_z: 0.3,
                return_bps: -20.0,
            });
        }

        let _good = reg.get_or_create("GS/MS", 0);

        let removed = reg.cleanup();
        assert_eq!(removed.len(), 1);
        assert_eq!(removed[0], "BAD/PAIR");
        assert_eq!(reg.pairs.len(), 1);
    }
}
