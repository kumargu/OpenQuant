use serde::{Deserialize, Serialize};

use crate::engine::BasketParams;
use crate::intent::TransitionReason;
use crate::state::{BasketState, MAX_SPREAD_HISTORY};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub enum GatePolicyKind {
    #[default]
    BertramFrozen,
    RollingSScoreV1(RollingSScoreV1Config),
}

impl GatePolicyKind {
    pub fn history_lookback(&self) -> Option<usize> {
        match self {
            Self::BertramFrozen => None,
            Self::RollingSScoreV1(config) => Some(config.lookback),
        }
    }

    pub fn signal_label(&self) -> &'static str {
        match self {
            Self::BertramFrozen => "z_score",
            Self::RollingSScoreV1(config) => match config.entry_mode {
                RollingEntryMode::RollingScore => "s_score",
                RollingEntryMode::RawZScore => "z_score",
            },
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::BertramFrozen => Ok(()),
            Self::RollingSScoreV1(config) => {
                if config.lookback == 0 || config.lookback > MAX_SPREAD_HISTORY {
                    return Err(format!(
                        "rolling_s_score_v1 lookback must be in [1, {MAX_SPREAD_HISTORY}]"
                    ));
                }
                if config.min_history == 0 || config.min_history > config.lookback {
                    return Err(
                        "rolling_s_score_v1 min_history must be in [1, lookback]".to_string()
                    );
                }
                if config.entry_confirmation_bars == 0
                    || config.entry_confirmation_bars > MAX_SPREAD_HISTORY
                {
                    return Err(format!(
                        "rolling_s_score_v1 entry_confirmation_bars must be in [1, {MAX_SPREAD_HISTORY}]"
                    ));
                }
                if !config.exit_threshold.is_finite() || config.exit_threshold < 0.0 {
                    return Err(
                        "rolling_s_score_v1 exit_threshold must be finite and non-negative"
                            .to_string(),
                    );
                }
                Ok(())
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RollingSScoreV1Config {
    pub lookback: usize,
    pub min_history: usize,
    pub exit_threshold: f64,
    pub direct_flip: bool,
    pub entry_mode: RollingEntryMode,
    pub entry_confirmation_bars: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RollingEntryMode {
    RollingScore,
    RawZScore,
}

impl Default for RollingSScoreV1Config {
    fn default() -> Self {
        Self {
            lookback: 20,
            min_history: 20,
            exit_threshold: 0.5,
            direct_flip: true,
            entry_mode: RollingEntryMode::RollingScore,
            entry_confirmation_bars: 1,
        }
    }
}

pub trait GatePolicy {
    fn evaluate(
        &self,
        state: &BasketState,
        params: &BasketParams,
        spread: f64,
        raw_z: f64,
    ) -> GateEvaluation;
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GateEvaluation {
    pub signal_score: Option<f64>,
    pub entry_threshold: f64,
    pub exit_threshold: Option<f64>,
    pub next_position: i8,
    pub reason: Option<TransitionReason>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct BertramFrozenPolicy;

#[derive(Debug, Clone, Copy)]
pub struct RollingSScoreV1Policy {
    config: RollingSScoreV1Config,
}

impl RollingSScoreV1Policy {
    pub fn new(config: RollingSScoreV1Config) -> Self {
        Self { config }
    }

    fn compute_recent_entry_scores(
        &self,
        state: &BasketState,
        params: &BasketParams,
        current_spread: f64,
    ) -> Vec<f64> {
        let mut spreads: Vec<f64> = state.spread_history.iter().copied().collect();
        spreads.push(current_spread);
        let want = self.config.entry_confirmation_bars.max(1);
        match self.config.entry_mode {
            RollingEntryMode::RawZScore => spreads
                .iter()
                .rev()
                .take(want)
                .map(|spread| (spread - params.ou.mu) / params.ou.sigma_eq)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect(),
            RollingEntryMode::RollingScore => {
                let mut scores = Vec::new();
                for idx in 0..spreads.len() {
                    let start = idx.saturating_sub(self.config.lookback);
                    let history = &spreads[start..idx];
                    if history.len() < self.config.min_history {
                        continue;
                    }
                    let mean = history.iter().sum::<f64>() / history.len() as f64;
                    let variance = history
                        .iter()
                        .map(|v| {
                            let d = *v - mean;
                            d * d
                        })
                        .sum::<f64>()
                        / history.len() as f64;
                    let std_dev = variance.sqrt();
                    if !std_dev.is_finite() || std_dev <= f64::EPSILON {
                        continue;
                    }
                    scores.push((spreads[idx] - mean) / std_dev);
                }
                scores
                    .into_iter()
                    .rev()
                    .take(want)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect()
            }
        }
    }

    fn entry_confirmation_passes(&self, scores: &[f64], threshold: f64, direction: i8) -> bool {
        let need = self.config.entry_confirmation_bars.max(1);
        if scores.len() < need {
            return false;
        }
        match direction {
            1 => scores.iter().all(|score| *score <= -threshold),
            -1 => scores.iter().all(|score| *score >= threshold),
            _ => false,
        }
    }
}

impl GatePolicy for BertramFrozenPolicy {
    fn evaluate(
        &self,
        state: &BasketState,
        params: &BasketParams,
        _spread: f64,
        raw_z: f64,
    ) -> GateEvaluation {
        let k = params.threshold_k;
        let old_pos = state.position;
        let (next_position, reason) = match old_pos {
            0 => {
                if raw_z < -k {
                    (1, Some(TransitionReason::InitialEntryLong))
                } else if raw_z > k {
                    (-1, Some(TransitionReason::InitialEntryShort))
                } else {
                    (0, None)
                }
            }
            1 => {
                if raw_z > k {
                    (-1, Some(TransitionReason::FlipLongToShort))
                } else {
                    (1, None)
                }
            }
            -1 => {
                if raw_z < -k {
                    (1, Some(TransitionReason::FlipShortToLong))
                } else {
                    (-1, None)
                }
            }
            _ => (old_pos, None),
        };

        GateEvaluation {
            signal_score: Some(raw_z),
            entry_threshold: k,
            exit_threshold: None,
            next_position,
            reason,
        }
    }
}

impl GatePolicy for RollingSScoreV1Policy {
    fn evaluate(
        &self,
        state: &BasketState,
        params: &BasketParams,
        spread: f64,
        raw_z: f64,
    ) -> GateEvaluation {
        let history: Vec<f64> = state
            .spread_history
            .iter()
            .rev()
            .take(self.config.lookback)
            .copied()
            .collect();
        if history.len() < self.config.min_history {
            return GateEvaluation {
                signal_score: None,
                entry_threshold: params.threshold_k,
                exit_threshold: Some(self.config.exit_threshold),
                next_position: state.position,
                reason: None,
            };
        }

        let mean = history.iter().sum::<f64>() / history.len() as f64;
        let variance = history
            .iter()
            .map(|v| {
                let d = *v - mean;
                d * d
            })
            .sum::<f64>()
            / history.len() as f64;
        let std_dev = variance.sqrt();
        if !std_dev.is_finite() || std_dev <= f64::EPSILON {
            return GateEvaluation {
                signal_score: None,
                entry_threshold: params.threshold_k,
                exit_threshold: Some(self.config.exit_threshold),
                next_position: state.position,
                reason: None,
            };
        }

        let s_score = (spread - mean) / std_dev;
        let old_pos = state.position;
        let k = params.threshold_k;
        let exit = self.config.exit_threshold;
        let entry_score = match self.config.entry_mode {
            RollingEntryMode::RollingScore => s_score,
            RollingEntryMode::RawZScore => raw_z,
        };
        let recent_entry_scores = self.compute_recent_entry_scores(state, params, spread);
        let long_confirmed = self.entry_confirmation_passes(&recent_entry_scores, k, 1);
        let short_confirmed = self.entry_confirmation_passes(&recent_entry_scores, k, -1);
        let (next_position, reason, signal_score) = match old_pos {
            0 => {
                if entry_score <= -k && long_confirmed {
                    (
                        1,
                        Some(TransitionReason::InitialEntryLong),
                        Some(entry_score),
                    )
                } else if entry_score >= k && short_confirmed {
                    (
                        -1,
                        Some(TransitionReason::InitialEntryShort),
                        Some(entry_score),
                    )
                } else {
                    (0, None, Some(entry_score))
                }
            }
            1 => {
                if self.config.direct_flip && entry_score >= k && short_confirmed {
                    (
                        -1,
                        Some(TransitionReason::FlipLongToShort),
                        Some(entry_score),
                    )
                } else if s_score >= -exit {
                    (0, Some(TransitionReason::ExitLong), Some(s_score))
                } else {
                    (1, None, Some(s_score))
                }
            }
            -1 => {
                if self.config.direct_flip && entry_score <= -k && long_confirmed {
                    (
                        1,
                        Some(TransitionReason::FlipShortToLong),
                        Some(entry_score),
                    )
                } else if s_score <= exit {
                    (0, Some(TransitionReason::ExitShort), Some(s_score))
                } else {
                    (-1, None, Some(s_score))
                }
            }
            _ => (old_pos, None, None),
        };

        GateEvaluation {
            signal_score,
            entry_threshold: k,
            exit_threshold: Some(exit),
            next_position,
            reason,
        }
    }
}

pub fn evaluate_gate(
    policy: &GatePolicyKind,
    state: &BasketState,
    params: &BasketParams,
    spread: f64,
    raw_z: f64,
) -> GateEvaluation {
    match policy {
        GatePolicyKind::BertramFrozen => BertramFrozenPolicy.evaluate(state, params, spread, raw_z),
        GatePolicyKind::RollingSScoreV1(config) => {
            RollingSScoreV1Policy::new(*config).evaluate(state, params, spread, raw_z)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use basket_picker::OuFit;
    use std::collections::VecDeque;

    fn params() -> BasketParams {
        BasketParams {
            basket_id: "test".to_string(),
            target: "AAA".to_string(),
            peers: vec!["BBB".to_string()],
            ou: OuFit {
                mu: 0.0,
                sigma_eq: 1.0,
                half_life_days: 5.0,
                a: 0.0,
                b: 0.0,
                kappa: 0.1,
                sigma: 1.0,
            },
            threshold_k: 1.25,
        }
    }

    #[test]
    fn bertram_policy_keeps_legacy_flip_behavior() {
        let mut state = BasketState::default();
        state.position = 1;
        let out = BertramFrozenPolicy.evaluate(&state, &params(), 0.0, 2.0);
        assert_eq!(out.reason, Some(TransitionReason::FlipLongToShort));
        assert_eq!(out.next_position, -1);
        assert_eq!(out.signal_score, Some(2.0));
    }

    #[test]
    fn rolling_policy_waits_for_min_history() {
        let mut state = BasketState::default();
        state.spread_history = VecDeque::from(vec![0.0; 5]);
        let out = RollingSScoreV1Policy::new(RollingSScoreV1Config::default()).evaluate(
            &state,
            &params(),
            -3.0,
            -3.0,
        );
        assert_eq!(out.reason, None);
        assert_eq!(out.signal_score, None);
    }

    #[test]
    fn rolling_policy_enters_and_exits_using_bands() {
        let cfg = RollingSScoreV1Config {
            lookback: 20,
            min_history: 20,
            exit_threshold: 0.5,
            direct_flip: true,
            entry_mode: RollingEntryMode::RollingScore,
            entry_confirmation_bars: 1,
        };
        let mut state = BasketState::default();
        state.spread_history = (0..20).map(|i| i as f64 * 0.1).collect();
        let enter = RollingSScoreV1Policy::new(cfg).evaluate(&state, &params(), -2.0, -2.0);
        assert_eq!(enter.reason, Some(TransitionReason::InitialEntryLong));
        assert_eq!(enter.next_position, 1);

        state.position = 1;
        let exit = RollingSScoreV1Policy::new(cfg).evaluate(&state, &params(), 0.8, 0.8);
        assert_eq!(exit.reason, Some(TransitionReason::ExitLong));
        assert_eq!(exit.next_position, 0);
    }

    #[test]
    fn rolling_policy_raw_z_entry_mode_reports_raw_z_on_entry() {
        let cfg = RollingSScoreV1Config {
            lookback: 20,
            min_history: 20,
            exit_threshold: 0.5,
            direct_flip: true,
            entry_mode: RollingEntryMode::RawZScore,
            entry_confirmation_bars: 1,
        };
        let mut state = BasketState::default();
        state.spread_history = (0..20).map(|i| i as f64 * 0.1).collect();
        let enter = RollingSScoreV1Policy::new(cfg).evaluate(&state, &params(), -2.0, -2.5);
        assert_eq!(enter.reason, Some(TransitionReason::InitialEntryLong));
        assert_eq!(enter.signal_score, Some(-2.5));
    }

    #[test]
    fn rolling_policy_requires_consecutive_raw_z_confirmation_for_entry() {
        let cfg = RollingSScoreV1Config {
            lookback: 20,
            min_history: 20,
            exit_threshold: 0.5,
            direct_flip: true,
            entry_mode: RollingEntryMode::RawZScore,
            entry_confirmation_bars: 2,
        };
        let mut state = BasketState::default();
        state.spread_history = VecDeque::from(vec![
            0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0,
            0.0, 0.2, 0.1,
        ]);
        let blocked = RollingSScoreV1Policy::new(cfg).evaluate(&state, &params(), -2.0, -2.0);
        assert_eq!(blocked.reason, None);

        state.spread_history.pop_front();
        state.spread_history.push_back(-2.0);
        let allowed = RollingSScoreV1Policy::new(cfg).evaluate(&state, &params(), -2.1, -2.1);
        assert_eq!(allowed.reason, Some(TransitionReason::InitialEntryLong));
    }
}
