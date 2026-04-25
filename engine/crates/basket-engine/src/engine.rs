//! Main basket engine with Bertram symmetric state machine.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::Path;

use basket_picker::{BasketFit, OuFit};
use chrono::NaiveDate;
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
    /// Per-basket runtime state. `BTreeMap` so reload preserves the
    /// engine's deterministic iteration order (#315). JSON-on-disk
    /// shape is unchanged — `BTreeMap` and `HashMap` serialize
    /// identically to `{"key": value, ...}`.
    pub states: BTreeMap<String, BasketState>,
    /// Last trading day that the runner fully processed. Optional so older
    /// snapshots remain readable.
    #[serde(default)]
    pub last_processed_trading_day: Option<NaiveDate>,
}

/// The basket engine: manages state machines for all active baskets.
///
/// `params` and `states` are `BTreeMap`s — not `HashMap`s — because
/// any iteration over them feeds into f64 sums (see
/// `portfolio::plan_portfolio`'s `symbol_notionals += leg.notional`
/// and the per-bar update loop in `on_bars`). `HashMap` iteration is
/// randomized per-process in Rust; ordering-dependent f64 sums then
/// drift across runs and break replay reproducibility (#315).
/// `BTreeMap`'s sorted iteration gives us bit-exact reproducibility
/// at the cost of O(log N) lookups, which is negligible at N ≈ 50.
pub struct BasketEngine {
    /// Per-basket parameters (frozen after construction).
    params: BTreeMap<String, BasketParams>,
    /// Per-basket runtime state.
    states: BTreeMap<String, BasketState>,
    /// Hard cap on how many baskets can be in a non-flat position
    /// simultaneously. `None` = unlimited (test default). When set,
    /// `on_bars` rejects new initial entries (flat → long or flat →
    /// short) once the cap is reached. Flips of existing positions
    /// are always allowed because they don't grow the active set.
    ///
    /// This used to be enforced after the fact by
    /// `plan_portfolio.flatten_baskets`, which ranked all in-position
    /// baskets by current `|z|` and flattened the lower-ranked ones.
    /// That was anti-mean-reversion: a position whose spread was
    /// reverting toward zero would have a SHRINKING `|z|`, get
    /// out-ranked by a freshly dislocated basket with bigger `|z|`,
    /// and be flattened — exactly when it was about to be profitable.
    /// Q4 2025 telemetry showed this directly: 2,153 transitions, 0
    /// of which were flips. Every transition was either an initial
    /// entry or a flatten-by-rank. Moving the cap to entry time
    /// (FCFS-style admission) lets entered positions live until
    /// their own engine-level exit signal fires.
    max_active_positions: Option<usize>,
    /// Adverse-move stop in z-units. If `Some(s)`, an open position
    /// is force-flattened when the spread has moved against the trade
    /// by more than `s` z-units beyond its entry z (long-stop:
    /// `entry_z - z > s`; short-stop: `z - entry_z > s`).
    ///
    /// Without this, a basket whose cointegration breaks during the
    /// walk-forward window sits in a losing position indefinitely —
    /// Bertram's symmetric-flip exit only fires when the spread
    /// returns to the opposite ±k threshold, which never happens once
    /// the OU model no longer describes the data. Q4 2025 replay
    /// observed PNC long entered at z=-1.12 drift to z=-3.91, TFC
    /// short entered at z=+0.49 drift to z=+4.18 — both far from any
    /// flip threshold, both bleeding the entire quarter.
    ///
    /// Lab `stop_loss_experiment.py` swept `[1.5, 2.0, 2.5, 3.0, 4.0,
    /// inf]` across 49 baskets / 9 sectors and found 2.0σ best
    /// (Sharpe 3.36 vs no_stop's much lower terminal equity and
    /// deeper drawdowns). Default here matches.
    stop_loss_z: Option<f64>,
}

impl BasketEngine {
    /// Create a new engine from validated basket fits.
    pub fn new(fits: &[BasketFit]) -> Self {
        let mut params = BTreeMap::new();
        let mut states = BTreeMap::new();

        for fit in fits {
            if let Some(p) = BasketParams::from_fit(fit) {
                let id = p.basket_id.clone();
                params.insert(id.clone(), p);
                states.insert(id, BasketState::new());
            }
        }

        Self {
            params,
            states,
            max_active_positions: None,
            stop_loss_z: None,
        }
    }

    /// Set the maximum number of baskets that can be in a non-flat
    /// position simultaneously. Live/paper/replay should call this
    /// once at startup with `PortfolioConfig.n_active_baskets` so the
    /// engine self-enforces the cap at entry time.
    pub fn set_max_active_positions(&mut self, max: usize) {
        self.max_active_positions = Some(max);
    }

    /// Set the adverse-move stop-loss threshold in z-units. See the
    /// field doc on `stop_loss_z` for behavior. Pass `None` to disable
    /// (degenerates to pure Bertram symmetric-flip).
    pub fn set_stop_loss_z(&mut self, stop_z: Option<f64>) {
        self.stop_loss_z = stop_z;
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
        let mut skipped_at_cap = 0usize;
        let mut stopped_out = 0usize;
        let mut suspended_re_armed = 0usize;
        let mut skipped_suspended = 0usize;

        // Snapshot how many baskets are currently in a non-flat
        // position at the start of this `on_bars`. Updated locally as
        // we admit new entries below; flips don't change the count.
        let mut active_position_count = self.states.values().filter(|s| s.position != 0).count();
        let cap = self.max_active_positions;
        let stop_z = self.stop_loss_z;

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

            // Re-arm the entry gate: a previously stopped-out basket
            // becomes eligible again only once the spread is back
            // inside the Bertram band (`|z| < k`). This prevents the
            // pathology where a basket stops out at z=±s_stop, the
            // very next bar still has |z| > k, and the engine
            // immediately re-enters at the same adverse z and
            // re-stops. Mirrors quant-lab `simulate_bertram_with_stop`.
            if state.suspended && z.abs() < k {
                state.suspended = false;
                suspended_re_armed += 1;
                debug!(
                    basket_id = %basket_id,
                    z = %format!("{:.4}", z),
                    k = %format!("{:.4}", k),
                    "stop-loss re-armed (spread back inside band)"
                );
            }

            // Bertram symmetric state machine + adverse-move stop.
            // The stop check runs FIRST when in a position, so a stop
            // takes precedence over a flip on the same bar.
            let mut at_cap_skip = false;
            let mut stop_triggered = false;
            let (new_pos, reason) = match old_pos {
                0 => {
                    // Flat: enter on threshold breach. Entry-time cap
                    // gates new admissions — see the field doc on
                    // `BasketEngine::max_active_positions`. We count
                    // current active positions BEFORE this branch
                    // mutates `active_position_count`.
                    let entry_allowed = match cap {
                        Some(max) => active_position_count < max,
                        None => true,
                    };
                    if state.suspended {
                        // Still locked out from a prior stop; the band
                        // hasn't re-armed yet. Don't admit even if
                        // |z| > k.
                        if z < -k || z > k {
                            skipped_suspended += 1;
                        }
                        (0, None)
                    } else if !entry_allowed {
                        if z < -k || z > k {
                            // Real signal we're declining due to cap;
                            // log so the operator can see if the cap
                            // is starving the strategy.
                            at_cap_skip = true;
                        }
                        (0, None)
                    } else if z < -k {
                        active_position_count += 1;
                        (1, Some(TransitionReason::InitialEntryLong))
                    } else if z > k {
                        active_position_count += 1;
                        (-1, Some(TransitionReason::InitialEntryShort))
                    } else {
                        (0, None)
                    }
                }
                1 => {
                    // Long: stop out if z has drifted adversely below
                    // entry by more than `stop_z`. Otherwise flip to
                    // short on z > +k. Flips don't grow the active
                    // set (still 1 position) so the cap doesn't apply.
                    let adverse = state.entry_z.map(|ez| ez - z).unwrap_or(0.0);
                    let stopped = stop_z.is_some_and(|s| adverse > s);
                    if stopped {
                        stop_triggered = true;
                        active_position_count = active_position_count.saturating_sub(1);
                        (0, Some(TransitionReason::StopLossLong))
                    } else if z > k {
                        (-1, Some(TransitionReason::FlipLongToShort))
                    } else {
                        (1, None)
                    }
                }
                -1 => {
                    // Short: stop out if z has drifted adversely above
                    // entry by more than `stop_z`. Otherwise flip to
                    // long on z < -k.
                    let adverse = state.entry_z.map(|ez| z - ez).unwrap_or(0.0);
                    let stopped = stop_z.is_some_and(|s| adverse > s);
                    if stopped {
                        stop_triggered = true;
                        active_position_count = active_position_count.saturating_sub(1);
                        (0, Some(TransitionReason::StopLossShort))
                    } else if z < -k {
                        (1, Some(TransitionReason::FlipShortToLong))
                    } else {
                        (-1, None)
                    }
                }
                _ => (old_pos, None),
            };
            if at_cap_skip {
                skipped_at_cap += 1;
            }
            if stop_triggered {
                stopped_out += 1;
            }

            // Apply state transition if any
            if let Some(reason) = reason {
                // Capture the prior position's entry context BEFORE we
                // overwrite it, so the transition log can compare
                // entry-z vs exit-z, entry-spread vs current spread, and
                // compute a holding-period in trading days. This is the
                // load-bearing telemetry for "is the strategy doing
                // mean-reversion or whipsawing on noise?"
                let entry_z = state.entry_z;
                let entry_spread = state.entry_spread;
                let entry_date = state.entry_date;
                if old_pos == 0 {
                    state.enter(new_pos, date, spread, z);
                } else if new_pos == 0 {
                    // Stop-out path: flatten and arm the re-entry gate
                    // so we don't immediately re-enter at the same
                    // adverse z on the next bar.
                    state.stop_out();
                } else {
                    state.flip(date, spread, z);
                }

                transitioned += 1;

                let holding_days = entry_date.and_then(|d| (date - d).num_days().into());
                let spread_move = entry_spread.map(|es| spread - es);
                // Sign convention: spread = log(target) - mean(log(peers)).
                // A LONG basket (position=+1) holds target long and peers
                // short via `basket_to_legs`, so it profits when target
                // outperforms peers — i.e. when spread RISES. SHORT (-1)
                // profits when spread falls. Per-unit-notional P&L is
                // therefore (Δspread) × old_pos. The previous formula
                // negated old_pos and flipped every gain/loss in the
                // telemetry log — masking that flips at z≈±k were
                // actually winning trades while the aggregated portfolio
                // was bleeding from a different cause.
                let pnl_per_unit_notional = entry_spread.map(|es| (spread - es) * (old_pos as f64));
                info!(
                    basket_id = %basket_id,
                    z_score = %format!("{:.4}", z),
                    entry_z = ?entry_z.map(|v| format!("{:.4}", v)),
                    old_pos = old_pos,
                    new_pos = new_pos,
                    spread = %format!("{:.6}", spread),
                    spread_move = ?spread_move.map(|v| format!("{:.6}", v)),
                    pnl_per_unit = ?pnl_per_unit_notional.map(|v| format!("{:.6}", v)),
                    holding_days = ?holding_days,
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
            skipped_at_cap,
            stopped_out,
            suspended_re_armed,
            skipped_suspended,
            active_after = active_position_count,
            cap = ?cap,
            stop_z = ?stop_z,
            intents_emitted = intents.len(),
            "on_bars summary"
        );

        intents
    }

    /// Save engine state to a JSON file.
    pub fn save_state(&self, path: &Path) -> Result<(), String> {
        self.save_state_with_day(path, None)
    }

    /// Save engine state plus the last fully processed trading day.
    pub fn save_state_with_day(
        &self,
        path: &Path,
        last_processed_trading_day: Option<NaiveDate>,
    ) -> Result<(), String> {
        let snapshot = EngineSnapshot {
            params: self.params.values().cloned().collect(),
            states: self.states.clone(),
            last_processed_trading_day,
        };
        let json = serde_json::to_string_pretty(&snapshot)
            .map_err(|e| format!("failed to serialize: {}", e))?;
        fs::write(path, json).map_err(|e| format!("failed to write: {}", e))?;
        Ok(())
    }

    /// Load a raw engine snapshot without trusting persisted params as the
    /// live source of truth. Callers that have a fresh fit artifact should
    /// restore only runtime states onto current params.
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

        let mut params = BTreeMap::new();
        for p in snapshot.params {
            params.insert(p.basket_id.clone(), p);
        }

        Ok(Self {
            params,
            states: snapshot.states,
            max_active_positions: None,
            stop_loss_z: None,
        })
    }

    /// Replace runtime states while preserving the engine's current params.
    pub fn apply_states(&mut self, states: BTreeMap<String, BasketState>) -> Result<(), String> {
        let snapshot = EngineSnapshot {
            params: self.params.values().cloned().collect(),
            states,
            last_processed_trading_day: None,
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

    /// Flatten a set of baskets so portfolio admission and engine state stay aligned.
    pub fn flatten_baskets(&mut self, basket_ids: &[String]) {
        for basket_id in basket_ids {
            if let Some(state) = self.states.get_mut(basket_id) {
                state.flatten();
            }
        }
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
                return Err(format!(
                    "runtime state present for unknown basket_id '{basket_id}'"
                ));
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
    fn test_stop_loss_long_triggers_then_re_arms() {
        // Walk through the full stop-loss lifecycle on a long basket:
        //   1. Enter long when z dips below -k.
        //   2. Spread keeps drifting more negative — z exceeds the
        //      adverse-move stop → flatten + suspend.
        //   3. Spread recovers inside the band (|z| < k) → re-arm.
        //   4. Spread dips below -k again → re-enter long.
        // Without re-arm, step 4 would either re-enter immediately at
        // a still-adverse z (and re-stop) or never re-enter at all.
        let fit = make_test_fit(); // mu=0, sigma_eq=0.032, k=1.25
        let mut engine = BasketEngine::new(&[fit]);
        engine.set_stop_loss_z(Some(2.0));
        let basket_id = test_basket_id();

        // Day 1 — strong dislocation, log spread ≈ -0.105 → z ≈ -3.3.
        // z < -k → enter long. Entry z is captured for the stop check.
        let date1 = NaiveDate::from_ymd_opt(2026, 4, 21).unwrap();
        let bars_day1 = vec![
            DailyBar {
                symbol: "AMD".into(),
                date: date1,
                close: 90.0,
            },
            DailyBar {
                symbol: "NVDA".into(),
                date: date1,
                close: 100.0,
            },
            DailyBar {
                symbol: "INTC".into(),
                date: date1,
                close: 100.0,
            },
        ];
        let intents1 = engine.on_bars(&bars_day1);
        assert_eq!(intents1.len(), 1);
        assert_eq!(intents1[0].reason, TransitionReason::InitialEntryLong);
        assert_eq!(engine.get_state(&basket_id).unwrap().position, 1);
        let entry_z = engine.get_state(&basket_id).unwrap().entry_z.unwrap();
        assert!(entry_z < -1.25, "should have entered with z < -k");

        // Day 2 — spread further away from mean: log(80/100) = -0.223
        // → z ≈ -7.0. adverse = entry_z(-3.3) - z(-7.0) ≈ +3.7 > 2.0.
        // Stop-out fires. State flattens, suspended=true.
        let date2 = NaiveDate::from_ymd_opt(2026, 4, 22).unwrap();
        let bars_day2 = vec![
            DailyBar {
                symbol: "AMD".into(),
                date: date2,
                close: 80.0,
            },
            DailyBar {
                symbol: "NVDA".into(),
                date: date2,
                close: 100.0,
            },
            DailyBar {
                symbol: "INTC".into(),
                date: date2,
                close: 100.0,
            },
        ];
        let intents2 = engine.on_bars(&bars_day2);
        assert_eq!(intents2.len(), 1);
        assert_eq!(intents2[0].reason, TransitionReason::StopLossLong);
        assert_eq!(intents2[0].target_position, 0);
        let st2 = engine.get_state(&basket_id).unwrap();
        assert_eq!(st2.position, 0);
        assert!(st2.suspended, "stop-out must arm the re-entry gate");

        // Day 3 — spread back well inside the band (z ≈ 0). Suspended
        // is cleared the moment |z| < k. No new entry yet.
        let date3 = NaiveDate::from_ymd_opt(2026, 4, 23).unwrap();
        let bars_day3 = vec![
            DailyBar {
                symbol: "AMD".into(),
                date: date3,
                close: 100.0,
            },
            DailyBar {
                symbol: "NVDA".into(),
                date: date3,
                close: 100.0,
            },
            DailyBar {
                symbol: "INTC".into(),
                date: date3,
                close: 100.0,
            },
        ];
        let intents3 = engine.on_bars(&bars_day3);
        assert!(intents3.is_empty(), "no entry while z is inside the band");
        let st3 = engine.get_state(&basket_id).unwrap();
        assert!(!st3.suspended, "must re-arm once |z| < k");

        // Day 4 — fresh dislocation. Re-entry fires.
        let date4 = NaiveDate::from_ymd_opt(2026, 4, 24).unwrap();
        let bars_day4 = bars_day1
            .iter()
            .map(|b| DailyBar {
                date: date4,
                ..b.clone()
            })
            .collect::<Vec<_>>();
        let intents4 = engine.on_bars(&bars_day4);
        assert_eq!(intents4.len(), 1);
        assert_eq!(intents4[0].reason, TransitionReason::InitialEntryLong);
    }

    #[test]
    fn test_stop_loss_disabled_by_default() {
        // No `set_stop_loss_z` call → no stop. Position rides through
        // an arbitrarily large adverse move (Bertram baseline behavior).
        let fit = make_test_fit();
        let mut engine = BasketEngine::new(&[fit]);
        let basket_id = test_basket_id();

        let date1 = NaiveDate::from_ymd_opt(2026, 4, 21).unwrap();
        let _ = engine.on_bars(&[
            DailyBar {
                symbol: "AMD".into(),
                date: date1,
                close: 90.0,
            },
            DailyBar {
                symbol: "NVDA".into(),
                date: date1,
                close: 100.0,
            },
            DailyBar {
                symbol: "INTC".into(),
                date: date1,
                close: 100.0,
            },
        ]);
        assert_eq!(engine.get_state(&basket_id).unwrap().position, 1);

        // Far-adverse move that WOULD have stopped at 2.0σ.
        let date2 = NaiveDate::from_ymd_opt(2026, 4, 22).unwrap();
        let intents2 = engine.on_bars(&[
            DailyBar {
                symbol: "AMD".into(),
                date: date2,
                close: 60.0,
            },
            DailyBar {
                symbol: "NVDA".into(),
                date: date2,
                close: 100.0,
            },
            DailyBar {
                symbol: "INTC".into(),
                date: date2,
                close: 100.0,
            },
        ]);
        assert!(
            intents2.is_empty(),
            "no transitions should fire without stop set"
        );
        assert_eq!(
            engine.get_state(&basket_id).unwrap().position,
            1,
            "position must persist with stop disabled"
        );
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
