//! Main basket engine with Bertram symmetric state machine.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use basket_picker::{BasketFit, OuFit};
use serde::{Deserialize, Serialize};
use tracing::{debug, info, trace, warn};

use crate::intent::{PositionIntent, TransitionReason};
use crate::state::BasketState;
use crate::DailyBar;

/// Per-basket frozen parameters (read-only after engine start).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BasketParams {
    /// Basket identifier.
    pub basket_id: String,
    /// Target symbol.
    pub target: String,
    /// Peer symbols.
    pub peers: Vec<String>,
    /// Frozen OU fit parameters.
    pub ou: OuFit,
    /// Bertram threshold k (already clipped).
    pub threshold_k: f64,
}

impl BasketParams {
    /// Create from a validated BasketFit.
    ///
    /// Uses the candidate's canonical ID which includes sector, target,
    /// fit_date, and a hash of the peer members for uniqueness.
    pub fn from_fit(fit: &BasketFit) -> Option<Self> {
        if !fit.valid {
            return None;
        }
        let ou = fit.ou.as_ref()?;
        Some(Self {
            basket_id: fit.candidate.id(),
            target: fit.candidate.target.clone(),
            peers: fit.candidate.members.clone(),
            ou: ou.clone(),
            threshold_k: fit.threshold_k,
        })
    }
}

/// Snapshot of engine state for persistence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineSnapshot {
    /// Per-basket parameters (frozen).
    pub params: Vec<BasketParams>,
    /// Per-basket runtime state.
    pub states: HashMap<String, BasketState>,
}

/// The basket engine: manages state machines for all active baskets.
pub struct BasketEngine {
    /// Per-basket parameters (frozen after construction).
    params: HashMap<String, BasketParams>,
    /// Per-basket runtime state.
    states: HashMap<String, BasketState>,
}

impl BasketEngine {
    /// Create a new engine from validated basket fits.
    pub fn new(fits: &[BasketFit]) -> Self {
        let mut params = HashMap::new();
        let mut states = HashMap::new();

        for fit in fits {
            if let Some(p) = BasketParams::from_fit(fit) {
                let id = p.basket_id.clone();
                params.insert(id.clone(), p);
                states.insert(id, BasketState::new());
            }
        }

        Self { params, states }
    }

    /// Get the number of active baskets.
    pub fn num_baskets(&self) -> usize {
        self.params.len()
    }

    /// Process a batch of daily bars and return position intents.
    ///
    /// All bars must have the same date. Returns empty if bars have mixed dates.
    pub fn on_bars(&mut self, bars: &[DailyBar]) -> Vec<PositionIntent> {
        if bars.is_empty() {
            return vec![];
        }

        // Validate all bars have the same date
        let date = bars[0].date;
        for bar in &bars[1..] {
            if bar.date != date {
                tracing::warn!(
                    expected = %date,
                    found = %bar.date,
                    symbol = %bar.symbol,
                    "mixed_dates_in_bars"
                );
                return vec![];
            }
        }

        // Build price map from bars
        let mut prices: HashMap<&str, f64> = HashMap::new();
        for bar in bars {
            prices.insert(&bar.symbol, bar.close);
        }

        let mut intents = Vec::new();
        // Per-call cardinality breakdown — emitted as INFO at the end so every
        // `on_bars` session leaves a one-line summary we can grep even at
        // default log level. This is the "did the engine actually evaluate
        // anything meaningful?" counter we did not have yesterday.
        let total_baskets = self.params.len();
        let mut evaluated = 0usize;
        let mut skipped_missing_target = 0usize;
        let mut skipped_missing_peer = 0usize;
        let mut skipped_nan_z = 0usize;
        let mut near_threshold = 0usize;
        let mut transitioned = 0usize;

        for (basket_id, params) in &self.params {
            let state = match self.states.get_mut(basket_id) {
                Some(s) => s,
                None => {
                    tracing::error!(basket_id = %basket_id, "missing_basket_state");
                    continue;
                }
            };

            // Get target and peer prices
            let target_price = match prices.get(params.target.as_str()) {
                Some(&p) if p.is_finite() && p > 0.0 => p,
                _ => {
                    // Silent continue is a real blind spot: if the target's
                    // price never arrives, the basket is inert forever and
                    // nobody notices. DEBUG keeps the cardinality summary
                    // terse while letting deep-dive users see per-basket
                    // detail via `RUST_LOG=basket_engine=debug`.
                    skipped_missing_target += 1;
                    debug!(
                        basket_id = %basket_id,
                        target = params.target.as_str(),
                        date = %date,
                        "skip basket — missing or invalid target price"
                    );
                    continue;
                }
            };

            let mut peer_log_sum = 0.0;
            let mut peer_count = 0;
            let mut all_peers_valid = true;
            let mut missing_peer: Option<&str> = None;

            for peer in &params.peers {
                match prices.get(peer.as_str()) {
                    Some(&p) if p.is_finite() && p > 0.0 => {
                        peer_log_sum += p.ln();
                        peer_count += 1;
                    }
                    _ => {
                        all_peers_valid = false;
                        missing_peer = Some(peer.as_str());
                        break;
                    }
                }
            }

            if !all_peers_valid || peer_count == 0 {
                skipped_missing_peer += 1;
                debug!(
                    basket_id = %basket_id,
                    missing_peer = missing_peer.unwrap_or("<all>"),
                    date = %date,
                    "skip basket — missing or invalid peer price"
                );
                continue;
            }

            // Compute spread = log(target) - mean(log(peers))
            let spread = target_price.ln() - peer_log_sum / peer_count as f64;
            state.record_spread(spread);

            // Compute z-score using frozen OU parameters
            let z = (spread - params.ou.mu) / params.ou.sigma_eq;
            state.last_z = Some(z);

            if !z.is_finite() {
                skipped_nan_z += 1;
                warn!(
                    basket_id = %basket_id,
                    target_price,
                    peer_count,
                    peer_log_sum,
                    mu = params.ou.mu,
                    sigma_eq = params.ou.sigma_eq,
                    "skip basket — z-score not finite"
                );
                continue;
            }

            evaluated += 1;
            let k = params.threshold_k;
            let old_pos = state.position;

            // Near-threshold counter — any basket with |z| > 0.75k is "close
            // to firing." Tracking this gives us a pulse on whether the
            // strategy is actually seeing opportunity vs. sitting flat.
            if z.abs() > 0.75 * k {
                near_threshold += 1;
            }

            // Per-basket TRACE — full detail for deep-dive debugging.
            trace!(
                basket_id = %basket_id,
                target = params.target.as_str(),
                target_price,
                peer_count,
                spread = %format!("{:.6}", spread),
                z = %format!("{:.4}", z),
                threshold_k = k,
                position = old_pos,
                date = %date,
                "basket evaluation"
            );

            // Per-basket DEBUG — lighter, still per-basket-per-call.
            debug!(
                basket_id = %basket_id,
                z = %format!("{:.4}", z),
                k = %format!("{:.4}", k),
                pos = old_pos,
                "basket z-check"
            );

            // Bertram symmetric state machine
            let (new_pos, reason) = match old_pos {
                0 => {
                    // Flat: enter on threshold breach
                    if z < -k {
                        (1, Some(TransitionReason::InitialEntryLong))
                    } else if z > k {
                        (-1, Some(TransitionReason::InitialEntryShort))
                    } else {
                        (0, None)
                    }
                }
                1 => {
                    // Long: flip to short when z > k
                    if z > k {
                        (-1, Some(TransitionReason::FlipLongToShort))
                    } else {
                        (1, None)
                    }
                }
                -1 => {
                    // Short: flip to long when z < -k
                    if z < -k {
                        (1, Some(TransitionReason::FlipShortToLong))
                    } else {
                        (-1, None)
                    }
                }
                _ => (old_pos, None),
            };

            // Apply state transition if any
            if let Some(reason) = reason {
                if old_pos == 0 {
                    state.enter(new_pos, date, spread);
                } else {
                    state.flip(date, spread);
                }

                transitioned += 1;

                info!(
                    basket_id = %basket_id,
                    z_score = %format!("{:.4}", z),
                    old_pos = old_pos,
                    new_pos = new_pos,
                    spread = %format!("{:.6}", spread),
                    reason = %reason.as_str(),
                    "position_transition"
                );

                intents.push(PositionIntent::new(
                    basket_id.clone(),
                    new_pos,
                    reason,
                    z,
                    spread,
                    date,
                ));
            }
        }

        // Per-`on_bars` cardinality summary. Emitted at INFO so it surfaces
        // at the default log level and gives every session a one-line
        // "engine heartbeat" we can grep for without trace spam. Answers:
        // "did the engine actually run, and did anything come close to firing?"
        info!(
            date = %date,
            total_baskets,
            evaluated,
            skipped_missing_target,
            skipped_missing_peer,
            skipped_nan_z,
            near_threshold,
            transitioned,
            intents_emitted = intents.len(),
            "on_bars summary"
        );

        intents
    }

    /// Save engine state to a JSON file.
    pub fn save_state(&self, path: &Path) -> Result<(), String> {
        let snapshot = EngineSnapshot {
            params: self.params.values().cloned().collect(),
            states: self.states.clone(),
        };
        let json = serde_json::to_string_pretty(&snapshot)
            .map_err(|e| format!("failed to serialize: {}", e))?;
        fs::write(path, json).map_err(|e| format!("failed to write: {}", e))?;
        Ok(())
    }

    /// Load a raw engine snapshot from disk and validate its internal
    /// params/state consistency without trusting the frozen params for runtime.
    pub fn load_snapshot(path: &Path) -> Result<EngineSnapshot, String> {
        let content = fs::read_to_string(path).map_err(|e| format!("failed to read: {}", e))?;
        let snapshot: EngineSnapshot =
            serde_json::from_str(&content).map_err(|e| format!("failed to parse: {}", e))?;
        Self::validate_snapshot(&snapshot)?;
        Ok(snapshot)
    }

    /// Load engine state from a JSON file.
    pub fn load_state(path: &Path) -> Result<Self, String> {
        let snapshot = Self::load_snapshot(path)?;
        let mut params = HashMap::new();
        for p in snapshot.params {
            params.insert(p.basket_id.clone(), p);
        }

        Ok(Self {
            params,
            states: snapshot.states,
        })
    }

    /// Replace runtime states while preserving the engine's current frozen params.
    pub fn apply_states(&mut self, states: HashMap<String, BasketState>) -> Result<(), String> {
        let snapshot = EngineSnapshot {
            params: self.params.values().cloned().collect(),
            states,
        };
        Self::validate_snapshot(&snapshot)?;
        self.states = snapshot.states;
        Ok(())
    }

    /// Get the current state for a basket (for testing/diagnostics).
    pub fn get_state(&self, basket_id: &str) -> Option<&BasketState> {
        self.states.get(basket_id)
    }

    /// Get params for a basket (for testing/diagnostics).
    pub fn get_params(&self, basket_id: &str) -> Option<&BasketParams> {
        self.params.get(basket_id)
    }

    /// Iterate over all basket params.
    pub fn iter_params(&self) -> impl Iterator<Item = (&String, &BasketParams)> {
        self.params.iter()
    }

    fn validate_snapshot(snapshot: &EngineSnapshot) -> Result<(), String> {
        let mut params = HashMap::new();
        for p in &snapshot.params {
            params.insert(p.basket_id.clone(), p);
        }

        for basket_id in params.keys() {
            if !snapshot.states.contains_key(basket_id) {
                return Err(format!("missing runtime state for basket_id '{basket_id}'"));
            }
        }
        for basket_id in snapshot.states.keys() {
            if !params.contains_key(basket_id) {
                return Err(format!("runtime state present for unknown basket_id '{basket_id}'"));
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use basket_picker::{BasketCandidate, BertramResult};
    use chrono::NaiveDate;

    fn make_test_fit() -> BasketFit {
        let candidate = BasketCandidate {
            target: "AMD".to_string(),
            members: vec!["NVDA".to_string(), "INTC".to_string()],
            sector: "chips".to_string(),
            fit_date: NaiveDate::from_ymd_opt(2026, 4, 20).unwrap(),
        };
        let ou = OuFit {
            a: 0.001,
            b: 0.95,
            kappa: 12.92,
            mu: 0.0,
            sigma: 0.01,
            sigma_eq: 0.032,
            half_life_days: 13.51,
        };
        let bertram = BertramResult {
            a: -0.04,
            m: 0.04,
            k: 1.25,
            expected_return_rate: 0.1,
            expected_trade_length_days: 10.0,
            sigma_cont: 0.05,
        };
        BasketFit {
            candidate,
            ou: Some(ou),
            bertram: Some(bertram),
            threshold_k: 1.25,
            valid: true,
            reject_reason: None,
        }
    }

    fn test_basket_id() -> String {
        make_test_fit().candidate.id()
    }

    #[test]
    fn test_engine_creation() {
        let fit = make_test_fit();
        let engine = BasketEngine::new(&[fit]);
        assert_eq!(engine.num_baskets(), 1);
    }

    #[test]
    fn test_initial_entry_long() {
        let fit = make_test_fit();
        let mut engine = BasketEngine::new(&[fit]);

        // z < -k should trigger long entry
        // spread = log(AMD) - mean(log(NVDA), log(INTC))
        // We need z = (spread - mu) / sigma_eq < -1.25
        // With mu=0, sigma_eq=0.032, need spread < -0.04
        // log(90) - (log(100) + log(100))/2 ≈ -0.105
        let bars = vec![
            DailyBar {
                symbol: "AMD".to_string(),
                date: NaiveDate::from_ymd_opt(2026, 4, 21).unwrap(),
                close: 90.0,
            },
            DailyBar {
                symbol: "NVDA".to_string(),
                date: NaiveDate::from_ymd_opt(2026, 4, 21).unwrap(),
                close: 100.0,
            },
            DailyBar {
                symbol: "INTC".to_string(),
                date: NaiveDate::from_ymd_opt(2026, 4, 21).unwrap(),
                close: 100.0,
            },
        ];

        let intents = engine.on_bars(&bars);
        assert_eq!(intents.len(), 1);
        assert_eq!(intents[0].target_position, 1);
        assert_eq!(intents[0].reason, TransitionReason::InitialEntryLong);

        let state = engine.get_state(&test_basket_id()).unwrap();
        assert_eq!(state.position, 1);
    }

    #[test]
    fn test_initial_entry_short() {
        let fit = make_test_fit();
        let mut engine = BasketEngine::new(&[fit]);

        // z > k should trigger short entry
        // log(110) - (log(100) + log(100))/2 ≈ 0.095 > 0.04 (k * sigma_eq)
        let bars = vec![
            DailyBar {
                symbol: "AMD".to_string(),
                date: NaiveDate::from_ymd_opt(2026, 4, 21).unwrap(),
                close: 110.0,
            },
            DailyBar {
                symbol: "NVDA".to_string(),
                date: NaiveDate::from_ymd_opt(2026, 4, 21).unwrap(),
                close: 100.0,
            },
            DailyBar {
                symbol: "INTC".to_string(),
                date: NaiveDate::from_ymd_opt(2026, 4, 21).unwrap(),
                close: 100.0,
            },
        ];

        let intents = engine.on_bars(&bars);
        assert_eq!(intents.len(), 1);
        assert_eq!(intents[0].target_position, -1);
        assert_eq!(intents[0].reason, TransitionReason::InitialEntryShort);
    }

    #[test]
    fn test_flip_long_to_short() {
        let fit = make_test_fit();
        let mut engine = BasketEngine::new(&[fit]);

        // First enter long
        let bars1 = vec![
            DailyBar {
                symbol: "AMD".to_string(),
                date: NaiveDate::from_ymd_opt(2026, 4, 21).unwrap(),
                close: 90.0,
            },
            DailyBar {
                symbol: "NVDA".to_string(),
                date: NaiveDate::from_ymd_opt(2026, 4, 21).unwrap(),
                close: 100.0,
            },
            DailyBar {
                symbol: "INTC".to_string(),
                date: NaiveDate::from_ymd_opt(2026, 4, 21).unwrap(),
                close: 100.0,
            },
        ];
        engine.on_bars(&bars1);
        assert_eq!(engine.get_state(&test_basket_id()).unwrap().position, 1);

        // Then flip to short
        let bars2 = vec![
            DailyBar {
                symbol: "AMD".to_string(),
                date: NaiveDate::from_ymd_opt(2026, 4, 22).unwrap(),
                close: 110.0,
            },
            DailyBar {
                symbol: "NVDA".to_string(),
                date: NaiveDate::from_ymd_opt(2026, 4, 22).unwrap(),
                close: 100.0,
            },
            DailyBar {
                symbol: "INTC".to_string(),
                date: NaiveDate::from_ymd_opt(2026, 4, 22).unwrap(),
                close: 100.0,
            },
        ];
        let intents = engine.on_bars(&bars2);
        assert_eq!(intents.len(), 1);
        assert_eq!(intents[0].reason, TransitionReason::FlipLongToShort);
        assert_eq!(engine.get_state(&test_basket_id()).unwrap().position, -1);
    }

    #[test]
    fn test_no_transition_within_band() {
        let fit = make_test_fit();
        let mut engine = BasketEngine::new(&[fit]);

        // Price ratio close to 1:1, z within band
        let bars = vec![
            DailyBar {
                symbol: "AMD".to_string(),
                date: NaiveDate::from_ymd_opt(2026, 4, 21).unwrap(),
                close: 100.0,
            },
            DailyBar {
                symbol: "NVDA".to_string(),
                date: NaiveDate::from_ymd_opt(2026, 4, 21).unwrap(),
                close: 100.0,
            },
            DailyBar {
                symbol: "INTC".to_string(),
                date: NaiveDate::from_ymd_opt(2026, 4, 21).unwrap(),
                close: 100.0,
            },
        ];

        let intents = engine.on_bars(&bars);
        assert!(intents.is_empty());
        assert_eq!(engine.get_state(&test_basket_id()).unwrap().position, 0);
    }

    #[test]
    fn test_nan_bar_no_transition() {
        let fit = make_test_fit();
        let mut engine = BasketEngine::new(&[fit]);

        let bars = vec![
            DailyBar {
                symbol: "AMD".to_string(),
                date: NaiveDate::from_ymd_opt(2026, 4, 21).unwrap(),
                close: f64::NAN,
            },
            DailyBar {
                symbol: "NVDA".to_string(),
                date: NaiveDate::from_ymd_opt(2026, 4, 21).unwrap(),
                close: 100.0,
            },
            DailyBar {
                symbol: "INTC".to_string(),
                date: NaiveDate::from_ymd_opt(2026, 4, 21).unwrap(),
                close: 100.0,
            },
        ];

        let intents = engine.on_bars(&bars);
        assert!(intents.is_empty());
    }

    #[test]
    fn test_state_persistence_roundtrip() {
        let fit = make_test_fit();
        let mut engine = BasketEngine::new(&[fit]);

        // Enter a position
        let bars = vec![
            DailyBar {
                symbol: "AMD".to_string(),
                date: NaiveDate::from_ymd_opt(2026, 4, 21).unwrap(),
                close: 90.0,
            },
            DailyBar {
                symbol: "NVDA".to_string(),
                date: NaiveDate::from_ymd_opt(2026, 4, 21).unwrap(),
                close: 100.0,
            },
            DailyBar {
                symbol: "INTC".to_string(),
                date: NaiveDate::from_ymd_opt(2026, 4, 21).unwrap(),
                close: 100.0,
            },
        ];
        engine.on_bars(&bars);

        // Save and reload
        let tmp = tempfile::NamedTempFile::new().unwrap();
        engine.save_state(tmp.path()).unwrap();
        let loaded = BasketEngine::load_state(tmp.path()).unwrap();

        // Verify state matches
        let orig_state = engine.get_state(&test_basket_id()).unwrap();
        let loaded_state = loaded.get_state(&test_basket_id()).unwrap();
        assert_eq!(orig_state.position, loaded_state.position);
        assert_eq!(orig_state.entry_date, loaded_state.entry_date);
    }

    #[test]
    fn test_mixed_dates_rejected() {
        let fit = make_test_fit();
        let mut engine = BasketEngine::new(&[fit]);

        // Bars with different dates should be rejected
        let bars = vec![
            DailyBar {
                symbol: "AMD".to_string(),
                date: NaiveDate::from_ymd_opt(2026, 4, 21).unwrap(),
                close: 90.0,
            },
            DailyBar {
                symbol: "NVDA".to_string(),
                date: NaiveDate::from_ymd_opt(2026, 4, 22).unwrap(), // Different date!
                close: 100.0,
            },
            DailyBar {
                symbol: "INTC".to_string(),
                date: NaiveDate::from_ymd_opt(2026, 4, 21).unwrap(),
                close: 100.0,
            },
        ];

        let intents = engine.on_bars(&bars);
        assert!(intents.is_empty(), "mixed dates should return no intents");
        assert_eq!(
            engine.get_state(&test_basket_id()).unwrap().position,
            0,
            "state should remain flat"
        );
    }
}
