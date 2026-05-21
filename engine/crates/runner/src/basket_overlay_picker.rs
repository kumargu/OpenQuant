use std::collections::HashSet;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BasketOverlayMode {
    BasketOnly,
    SuppressShorts,
    ReplaceWithLongOnly,
    AddCappedLongSleeve,
}

impl BasketOverlayMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::BasketOnly => "basket_only",
            Self::SuppressShorts => "suppress_shorts",
            Self::ReplaceWithLongOnly => "replace_with_long_only",
            Self::AddCappedLongSleeve => "add_capped_long_sleeve",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BasketOverlayPickerKind {
    Fixed,
    RuleV1,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct BasketOverlayPickerFeatures {
    pub active_sectors: HashSet<String>,
    pub long_symbols: Vec<String>,
    pub leadership_short_conflict_ratio: f64,
    pub strategy_return_20d: f64,
    pub strategy_drawdown_20d: f64,
    pub basket_only_scale_if_sleeve: f64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct BasketOverlayPickerDecision {
    pub mode: BasketOverlayMode,
    pub reason: &'static str,
    pub active_sectors: HashSet<String>,
    pub long_symbols: Vec<String>,
    pub sleeve_leverage_scale: f64,
}

impl BasketOverlayPickerDecision {
    pub fn basket_only(reason: &'static str) -> Self {
        Self {
            mode: BasketOverlayMode::BasketOnly,
            reason,
            active_sectors: HashSet::new(),
            long_symbols: Vec::new(),
            sleeve_leverage_scale: 1.0,
        }
    }

    fn from_mode(
        mode: BasketOverlayMode,
        reason: &'static str,
        features: &BasketOverlayPickerFeatures,
    ) -> Self {
        if mode == BasketOverlayMode::BasketOnly {
            return Self::basket_only(reason);
        }
        Self {
            mode,
            reason,
            active_sectors: features.active_sectors.clone(),
            long_symbols: features.long_symbols.clone(),
            sleeve_leverage_scale: 1.0,
        }
    }

    fn with_sleeve_scale(mut self, scale: f64) -> Self {
        self.sleeve_leverage_scale = if scale.is_finite() {
            scale.clamp(0.0, 1.0)
        } else {
            1.0
        };
        self
    }
}

pub trait BasketOverlayPicker {
    fn id(&self) -> &'static str;

    fn decide(&mut self, features: &BasketOverlayPickerFeatures) -> BasketOverlayPickerDecision;
}

#[derive(Debug, Default)]
pub struct BasketOnlyOverlayPicker;

impl BasketOverlayPicker for BasketOnlyOverlayPicker {
    fn id(&self) -> &'static str {
        "basket_only"
    }

    fn decide(&mut self, _features: &BasketOverlayPickerFeatures) -> BasketOverlayPickerDecision {
        BasketOverlayPickerDecision::basket_only("no_picker_configured")
    }
}

#[derive(Debug, Clone, Copy)]
pub struct FixedOverlayPicker {
    mode: BasketOverlayMode,
}

impl FixedOverlayPicker {
    pub fn new(mode: BasketOverlayMode) -> Self {
        Self { mode }
    }
}

impl BasketOverlayPicker for FixedOverlayPicker {
    fn id(&self) -> &'static str {
        "fixed_overlay"
    }

    fn decide(&mut self, features: &BasketOverlayPickerFeatures) -> BasketOverlayPickerDecision {
        BasketOverlayPickerDecision::from_mode(self.mode, "configured_overlay_mode", features)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuleV1OverlayPickerConfig {
    pub min_dwell_days: usize,
    pub off_confirmation_days: usize,
    pub suppress_conflict_on_threshold: f64,
    pub suppress_conflict_off_threshold: f64,
    pub weak_return_threshold: f64,
    pub drawdown_on_threshold: f64,
    pub recovered_return_threshold: f64,
    pub recovered_drawdown_threshold: f64,
    pub sleeve_return_ceiling: f64,
    pub min_basket_only_scale_for_sleeve: f64,
    pub opportunistic_sleeve_min_basket_only_scale: f64,
    pub opportunistic_sleeve_return_ceiling: f64,
    pub halve_sleeve_drawdown_threshold: f64,
    pub quarter_sleeve_drawdown_threshold: f64,
}

impl Default for RuleV1OverlayPickerConfig {
    fn default() -> Self {
        Self {
            min_dwell_days: 5,
            off_confirmation_days: 2,
            suppress_conflict_on_threshold: 0.15,
            suppress_conflict_off_threshold: 0.05,
            weak_return_threshold: 0.0,
            drawdown_on_threshold: 0.05,
            recovered_return_threshold: 0.03,
            recovered_drawdown_threshold: 0.03,
            sleeve_return_ceiling: 0.03,
            min_basket_only_scale_for_sleeve: 0.70,
            opportunistic_sleeve_min_basket_only_scale: 0.85,
            opportunistic_sleeve_return_ceiling: 0.10,
            halve_sleeve_drawdown_threshold: 0.05,
            quarter_sleeve_drawdown_threshold: 0.10,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuleV1OverlayPicker {
    config: RuleV1OverlayPickerConfig,
    current_mode: BasketOverlayMode,
    dwell_remaining_days: usize,
    off_signal_days: usize,
}

impl RuleV1OverlayPicker {
    pub fn new(config: RuleV1OverlayPickerConfig) -> Self {
        Self {
            config,
            current_mode: BasketOverlayMode::BasketOnly,
            dwell_remaining_days: 0,
            off_signal_days: 0,
        }
    }

    fn leadership_on(features: &BasketOverlayPickerFeatures) -> bool {
        !features.active_sectors.is_empty() && !features.long_symbols.is_empty()
    }

    fn strategy_weak(&self, features: &BasketOverlayPickerFeatures) -> bool {
        features.strategy_return_20d <= self.config.weak_return_threshold
            || features.strategy_drawdown_20d >= self.config.drawdown_on_threshold
    }

    fn strategy_recovered(&self, features: &BasketOverlayPickerFeatures) -> bool {
        features.strategy_return_20d >= self.config.recovered_return_threshold
            && features.strategy_drawdown_20d <= self.config.recovered_drawdown_threshold
    }

    fn sleeve_allowed(&self, features: &BasketOverlayPickerFeatures) -> bool {
        features.basket_only_scale_if_sleeve >= self.config.min_basket_only_scale_for_sleeve
            && (self.strategy_weak(features)
                || features.strategy_return_20d <= self.config.sleeve_return_ceiling)
    }

    fn opportunistic_sleeve_allowed(&self, features: &BasketOverlayPickerFeatures) -> bool {
        features.basket_only_scale_if_sleeve
            >= self.config.opportunistic_sleeve_min_basket_only_scale
            && features.strategy_return_20d <= self.config.opportunistic_sleeve_return_ceiling
            && features.leadership_short_conflict_ratio < self.config.suppress_conflict_on_threshold
    }

    fn sleeve_scale(&self, features: &BasketOverlayPickerFeatures) -> f64 {
        if features.strategy_drawdown_20d >= self.config.quarter_sleeve_drawdown_threshold {
            0.25
        } else if features.strategy_drawdown_20d >= self.config.halve_sleeve_drawdown_threshold
            && features.strategy_return_20d >= self.config.weak_return_threshold
        {
            0.5
        } else {
            1.0
        }
    }

    fn enter(
        &mut self,
        mode: BasketOverlayMode,
        reason: &'static str,
        features: &BasketOverlayPickerFeatures,
    ) -> BasketOverlayPickerDecision {
        self.current_mode = mode;
        self.dwell_remaining_days = self.config.min_dwell_days;
        self.off_signal_days = 0;
        BasketOverlayPickerDecision::from_mode(mode, reason, features)
    }

    fn hold(
        &self,
        reason: &'static str,
        features: &BasketOverlayPickerFeatures,
    ) -> BasketOverlayPickerDecision {
        BasketOverlayPickerDecision::from_mode(self.current_mode, reason, features)
    }
}

impl Default for RuleV1OverlayPicker {
    fn default() -> Self {
        Self::new(RuleV1OverlayPickerConfig::default())
    }
}

impl BasketOverlayPicker for RuleV1OverlayPicker {
    fn id(&self) -> &'static str {
        "rule_v1"
    }

    fn decide(&mut self, features: &BasketOverlayPickerFeatures) -> BasketOverlayPickerDecision {
        if self.dwell_remaining_days > 0 {
            self.dwell_remaining_days -= 1;
            let decision = self.hold("min_dwell", features);
            return if self.current_mode == BasketOverlayMode::AddCappedLongSleeve {
                decision.with_sleeve_scale(self.sleeve_scale(features))
            } else {
                decision
            };
        }

        let leadership_on = Self::leadership_on(features);
        match self.current_mode {
            BasketOverlayMode::BasketOnly => {
                self.off_signal_days = 0;
                if !leadership_on {
                    return BasketOverlayPickerDecision::basket_only("no_leadership");
                }
                let material_short_conflict = features.leadership_short_conflict_ratio
                    >= self.config.suppress_conflict_on_threshold;
                if material_short_conflict && self.strategy_recovered(features) {
                    return self.enter(
                        BasketOverlayMode::SuppressShorts,
                        "leadership_short_conflict",
                        features,
                    );
                }
                if !self.sleeve_allowed(features) {
                    if self.opportunistic_sleeve_allowed(features) {
                        return self
                            .enter(
                                BasketOverlayMode::AddCappedLongSleeve,
                                "leadership_spare_budget",
                                features,
                            )
                            .with_sleeve_scale(self.sleeve_scale(features));
                    }
                    if !self.strategy_weak(features) {
                        return BasketOverlayPickerDecision::basket_only(
                            "leadership_basket_healthy",
                        );
                    }
                    return BasketOverlayPickerDecision::basket_only("sleeve_crowds_basket_only");
                }
                self.enter(
                    BasketOverlayMode::AddCappedLongSleeve,
                    "leadership_weak_basket",
                    features,
                )
                .with_sleeve_scale(self.sleeve_scale(features))
            }
            BasketOverlayMode::SuppressShorts => {
                let off_signal = !leadership_on
                    || features.leadership_short_conflict_ratio
                        <= self.config.suppress_conflict_off_threshold;
                if off_signal {
                    self.off_signal_days += 1;
                } else {
                    self.off_signal_days = 0;
                }
                if self.off_signal_days >= self.config.off_confirmation_days {
                    self.current_mode = BasketOverlayMode::BasketOnly;
                    self.off_signal_days = 0;
                    return BasketOverlayPickerDecision::basket_only("suppress_signal_cleared");
                }
                self.hold("suppress_signal_still_active", features)
            }
            BasketOverlayMode::AddCappedLongSleeve => {
                if !leadership_on {
                    self.off_signal_days += 1;
                } else {
                    self.off_signal_days = 0;
                }
                if self.off_signal_days >= self.config.off_confirmation_days {
                    self.current_mode = BasketOverlayMode::BasketOnly;
                    self.off_signal_days = 0;
                    return BasketOverlayPickerDecision::basket_only("sleeve_signal_cleared");
                }
                self.hold("sleeve_signal_still_active", features)
                    .with_sleeve_scale(self.sleeve_scale(features))
            }
            BasketOverlayMode::ReplaceWithLongOnly => {
                self.current_mode = BasketOverlayMode::BasketOnly;
                BasketOverlayPickerDecision::basket_only("replace_not_allowed")
            }
        }
    }
}

pub enum BasketOverlayPickerHandle {
    BasketOnly(BasketOnlyOverlayPicker),
    Fixed(FixedOverlayPicker),
    RuleV1(RuleV1OverlayPicker),
}

impl BasketOverlayPickerHandle {
    pub fn from_kind(
        kind: BasketOverlayPickerKind,
        configured_mode: Option<BasketOverlayMode>,
        rule_v1_config: Option<RuleV1OverlayPickerConfig>,
    ) -> Self {
        match kind {
            BasketOverlayPickerKind::Fixed => configured_mode
                .map(FixedOverlayPicker::new)
                .map(Self::Fixed)
                .unwrap_or_else(|| Self::BasketOnly(BasketOnlyOverlayPicker)),
            BasketOverlayPickerKind::RuleV1 => {
                Self::RuleV1(RuleV1OverlayPicker::new(rule_v1_config.unwrap_or_default()))
            }
        }
    }

    pub fn load_state(&mut self, path: &std::path::Path) -> Result<bool, String> {
        if !path.exists() {
            return Ok(false);
        }
        let content = std::fs::read_to_string(path)
            .map_err(|e| format!("read overlay picker state {}: {e}", path.display()))?;
        let state: BasketOverlayPickerState = serde_json::from_str(&content)
            .map_err(|e| format!("parse overlay picker state {}: {e}", path.display()))?;
        if state.picker_id != self.id() {
            return Ok(false);
        }
        match (self, state.rule_v1) {
            (Self::RuleV1(picker), Some(rule_state)) => {
                if picker.config != rule_state.config {
                    return Ok(false);
                }
                *picker = rule_state;
                Ok(true)
            }
            (Self::BasketOnly(_), None) | (Self::Fixed(_), None) => Ok(true),
            _ => Ok(false),
        }
    }

    pub fn save_state(&self, path: &std::path::Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("create picker state dir {}: {e}", parent.display()))?;
        }
        let state = BasketOverlayPickerState {
            picker_id: self.id().to_string(),
            rule_v1: match self {
                Self::RuleV1(picker) => Some(picker.clone()),
                Self::BasketOnly(_) | Self::Fixed(_) => None,
            },
        };
        let content = serde_json::to_string_pretty(&state)
            .map_err(|e| format!("serialize overlay picker state: {e}"))?;
        let tmp = path.with_extension("picker.tmp");
        std::fs::write(&tmp, content)
            .map_err(|e| format!("write overlay picker tmp {}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, path)
            .map_err(|e| format!("rename overlay picker state {}: {e}", path.display()))
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct BasketOverlayPickerState {
    picker_id: String,
    rule_v1: Option<RuleV1OverlayPicker>,
}

impl BasketOverlayPicker for BasketOverlayPickerHandle {
    fn id(&self) -> &'static str {
        match self {
            Self::BasketOnly(picker) => picker.id(),
            Self::Fixed(picker) => picker.id(),
            Self::RuleV1(picker) => picker.id(),
        }
    }

    fn decide(&mut self, features: &BasketOverlayPickerFeatures) -> BasketOverlayPickerDecision {
        match self {
            Self::BasketOnly(picker) => picker.decide(features),
            Self::Fixed(picker) => picker.decide(features),
            Self::RuleV1(picker) => picker.decide(features),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basket_only_picker_ignores_feature_payload() {
        let features = BasketOverlayPickerFeatures {
            active_sectors: HashSet::from(["chips".to_string()]),
            long_symbols: vec!["NVDA".to_string()],
            leadership_short_conflict_ratio: 0.0,
            strategy_return_20d: 0.0,
            strategy_drawdown_20d: 0.0,
            basket_only_scale_if_sleeve: 1.0,
        };

        let mut picker = BasketOnlyOverlayPicker;
        let decision = picker.decide(&features);

        assert_eq!(decision.mode, BasketOverlayMode::BasketOnly);
        assert!(decision.active_sectors.is_empty());
        assert!(decision.long_symbols.is_empty());
    }

    #[test]
    fn fixed_picker_freezes_feature_payload() {
        let features = BasketOverlayPickerFeatures {
            active_sectors: HashSet::from(["chips".to_string()]),
            long_symbols: vec!["NVDA".to_string(), "AMD".to_string()],
            leadership_short_conflict_ratio: 0.0,
            strategy_return_20d: 0.0,
            strategy_drawdown_20d: 0.0,
            basket_only_scale_if_sleeve: 1.0,
        };

        let mut picker = FixedOverlayPicker::new(BasketOverlayMode::AddCappedLongSleeve);
        let decision = picker.decide(&features);

        assert_eq!(decision.mode, BasketOverlayMode::AddCappedLongSleeve);
        assert_eq!(decision.active_sectors, features.active_sectors);
        assert_eq!(decision.long_symbols, features.long_symbols);
    }

    #[test]
    fn rule_v1_uses_suppress_for_material_short_conflict() {
        let mut picker = RuleV1OverlayPicker::new(RuleV1OverlayPickerConfig {
            min_dwell_days: 0,
            off_confirmation_days: 2,
            suppress_conflict_on_threshold: 0.15,
            suppress_conflict_off_threshold: 0.05,
            ..RuleV1OverlayPickerConfig::default()
        });
        let features = BasketOverlayPickerFeatures {
            active_sectors: HashSet::from(["chips".to_string()]),
            long_symbols: vec!["NVDA".to_string()],
            leadership_short_conflict_ratio: 0.25,
            strategy_return_20d: 0.10,
            strategy_drawdown_20d: 0.0,
            basket_only_scale_if_sleeve: 1.0,
        };

        let decision = picker.decide(&features);

        assert_eq!(decision.mode, BasketOverlayMode::SuppressShorts);
        assert_eq!(decision.reason, "leadership_short_conflict");
    }

    #[test]
    fn rule_v1_uses_sleeve_when_leadership_has_no_short_conflict_and_strategy_is_weak() {
        let mut picker = RuleV1OverlayPicker::new(RuleV1OverlayPickerConfig {
            min_dwell_days: 0,
            off_confirmation_days: 2,
            suppress_conflict_on_threshold: 0.15,
            suppress_conflict_off_threshold: 0.05,
            ..RuleV1OverlayPickerConfig::default()
        });
        let features = BasketOverlayPickerFeatures {
            active_sectors: HashSet::from(["faang".to_string()]),
            long_symbols: vec!["META".to_string()],
            leadership_short_conflict_ratio: 0.0,
            strategy_return_20d: -0.02,
            strategy_drawdown_20d: 0.06,
            basket_only_scale_if_sleeve: 0.75,
        };

        let decision = picker.decide(&features);

        assert_eq!(decision.mode, BasketOverlayMode::AddCappedLongSleeve);
        assert_eq!(decision.reason, "leadership_weak_basket");
    }

    #[test]
    fn rule_v1_stays_basket_only_when_basket_is_healthy() {
        let mut picker = RuleV1OverlayPicker::new(RuleV1OverlayPickerConfig {
            min_dwell_days: 0,
            off_confirmation_days: 2,
            suppress_conflict_on_threshold: 0.15,
            suppress_conflict_off_threshold: 0.05,
            ..RuleV1OverlayPickerConfig::default()
        });
        let features = BasketOverlayPickerFeatures {
            active_sectors: HashSet::from(["faang".to_string()]),
            long_symbols: vec!["META".to_string()],
            leadership_short_conflict_ratio: 0.0,
            strategy_return_20d: 0.07,
            strategy_drawdown_20d: 0.01,
            basket_only_scale_if_sleeve: 0.75,
        };

        let decision = picker.decide(&features);

        assert_eq!(decision.mode, BasketOverlayMode::BasketOnly);
        assert_eq!(decision.reason, "leadership_basket_healthy");
    }

    #[test]
    fn rule_v1_uses_opportunistic_sleeve_when_it_does_not_crowd_the_basket() {
        let mut picker = RuleV1OverlayPicker::new(RuleV1OverlayPickerConfig {
            min_dwell_days: 0,
            off_confirmation_days: 2,
            ..RuleV1OverlayPickerConfig::default()
        });
        let features = BasketOverlayPickerFeatures {
            active_sectors: HashSet::from(["chips".to_string()]),
            long_symbols: vec!["NVDA".to_string()],
            leadership_short_conflict_ratio: 0.10,
            strategy_return_20d: 0.08,
            strategy_drawdown_20d: 0.01,
            basket_only_scale_if_sleeve: 0.90,
        };

        let decision = picker.decide(&features);

        assert_eq!(decision.mode, BasketOverlayMode::AddCappedLongSleeve);
        assert_eq!(decision.reason, "leadership_spare_budget");
    }

    #[test]
    fn rule_v1_avoids_opportunistic_sleeve_when_basket_is_too_strong() {
        let mut picker = RuleV1OverlayPicker::new(RuleV1OverlayPickerConfig {
            min_dwell_days: 0,
            off_confirmation_days: 2,
            ..RuleV1OverlayPickerConfig::default()
        });
        let features = BasketOverlayPickerFeatures {
            active_sectors: HashSet::from(["chips".to_string()]),
            long_symbols: vec!["NVDA".to_string()],
            leadership_short_conflict_ratio: 0.10,
            strategy_return_20d: 0.12,
            strategy_drawdown_20d: 0.01,
            basket_only_scale_if_sleeve: 0.90,
        };

        let decision = picker.decide(&features);

        assert_eq!(decision.mode, BasketOverlayMode::BasketOnly);
        assert_eq!(decision.reason, "leadership_basket_healthy");
    }

    #[test]
    fn rule_v1_uses_suppress_not_opportunistic_sleeve_when_short_conflict_is_material() {
        let mut picker = RuleV1OverlayPicker::new(RuleV1OverlayPickerConfig {
            min_dwell_days: 0,
            off_confirmation_days: 2,
            ..RuleV1OverlayPickerConfig::default()
        });
        let features = BasketOverlayPickerFeatures {
            active_sectors: HashSet::from(["chips".to_string()]),
            long_symbols: vec!["NVDA".to_string()],
            leadership_short_conflict_ratio: 0.18,
            strategy_return_20d: 0.08,
            strategy_drawdown_20d: 0.01,
            basket_only_scale_if_sleeve: 0.90,
        };

        let decision = picker.decide(&features);

        assert_eq!(decision.mode, BasketOverlayMode::SuppressShorts);
        assert_eq!(decision.reason, "leadership_short_conflict");
    }

    #[test]
    fn rule_v1_holds_sleeve_while_leadership_stays_active_after_recovery() {
        let mut picker = RuleV1OverlayPicker::new(RuleV1OverlayPickerConfig {
            min_dwell_days: 0,
            off_confirmation_days: 2,
            ..RuleV1OverlayPickerConfig::default()
        });
        let weak_features = BasketOverlayPickerFeatures {
            active_sectors: HashSet::from(["faang".to_string()]),
            long_symbols: vec!["META".to_string()],
            leadership_short_conflict_ratio: 0.0,
            strategy_return_20d: -0.02,
            strategy_drawdown_20d: 0.04,
            basket_only_scale_if_sleeve: 0.90,
        };
        let recovered_features = BasketOverlayPickerFeatures {
            active_sectors: HashSet::from(["faang".to_string()]),
            long_symbols: vec!["META".to_string()],
            leadership_short_conflict_ratio: 0.0,
            strategy_return_20d: 0.12,
            strategy_drawdown_20d: 0.0,
            basket_only_scale_if_sleeve: 0.90,
        };

        assert_eq!(
            picker.decide(&weak_features).mode,
            BasketOverlayMode::AddCappedLongSleeve
        );
        let decision = picker.decide(&recovered_features);

        assert_eq!(decision.mode, BasketOverlayMode::AddCappedLongSleeve);
        assert_eq!(decision.reason, "sleeve_signal_still_active");
    }

    #[test]
    fn rule_v1_uses_sleeve_not_suppress_when_conflict_exists_but_basket_is_not_healthy() {
        let mut picker = RuleV1OverlayPicker::new(RuleV1OverlayPickerConfig {
            min_dwell_days: 0,
            off_confirmation_days: 2,
            suppress_conflict_on_threshold: 0.15,
            suppress_conflict_off_threshold: 0.05,
            ..RuleV1OverlayPickerConfig::default()
        });
        let features = BasketOverlayPickerFeatures {
            active_sectors: HashSet::from(["chips".to_string()]),
            long_symbols: vec!["NVDA".to_string()],
            leadership_short_conflict_ratio: 0.25,
            strategy_return_20d: -0.01,
            strategy_drawdown_20d: 0.04,
            basket_only_scale_if_sleeve: 0.90,
        };

        let decision = picker.decide(&features);

        assert_eq!(decision.mode, BasketOverlayMode::AddCappedLongSleeve);
        assert_eq!(decision.reason, "leadership_weak_basket");
    }

    #[test]
    fn rule_v1_scales_sleeve_down_in_drawdown() {
        let mut picker = RuleV1OverlayPicker::new(RuleV1OverlayPickerConfig {
            min_dwell_days: 0,
            off_confirmation_days: 2,
            ..RuleV1OverlayPickerConfig::default()
        });
        let features = BasketOverlayPickerFeatures {
            active_sectors: HashSet::from(["faang".to_string()]),
            long_symbols: vec!["META".to_string()],
            leadership_short_conflict_ratio: 0.0,
            strategy_return_20d: 0.02,
            strategy_drawdown_20d: 0.08,
            basket_only_scale_if_sleeve: 0.90,
        };

        let decision = picker.decide(&features);

        assert_eq!(decision.mode, BasketOverlayMode::AddCappedLongSleeve);
        assert_eq!(decision.sleeve_leverage_scale, 0.5);
    }

    #[test]
    fn rule_v1_keeps_rescue_sleeve_full_until_deeper_drawdown() {
        let mut picker = RuleV1OverlayPicker::new(RuleV1OverlayPickerConfig {
            min_dwell_days: 0,
            off_confirmation_days: 2,
            ..RuleV1OverlayPickerConfig::default()
        });
        let features = BasketOverlayPickerFeatures {
            active_sectors: HashSet::from(["faang".to_string()]),
            long_symbols: vec!["META".to_string()],
            leadership_short_conflict_ratio: 0.0,
            strategy_return_20d: -0.02,
            strategy_drawdown_20d: 0.08,
            basket_only_scale_if_sleeve: 0.90,
        };

        let decision = picker.decide(&features);

        assert_eq!(decision.mode, BasketOverlayMode::AddCappedLongSleeve);
        assert_eq!(decision.sleeve_leverage_scale, 1.0);
    }

    #[test]
    fn rule_v1_state_roundtrips() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("picker.json");
        let features = BasketOverlayPickerFeatures {
            active_sectors: HashSet::from(["chips".to_string()]),
            long_symbols: vec!["NVDA".to_string()],
            leadership_short_conflict_ratio: 0.25,
            strategy_return_20d: 0.08,
            strategy_drawdown_20d: 0.01,
            basket_only_scale_if_sleeve: 0.75,
        };
        let mut picker =
            BasketOverlayPickerHandle::from_kind(BasketOverlayPickerKind::RuleV1, None, None);
        assert_eq!(
            picker.decide(&features).mode,
            BasketOverlayMode::SuppressShorts
        );
        picker.save_state(&path).unwrap();

        let mut loaded =
            BasketOverlayPickerHandle::from_kind(BasketOverlayPickerKind::RuleV1, None, None);
        assert!(loaded.load_state(&path).unwrap());
        assert_eq!(
            loaded.decide(&features).mode,
            BasketOverlayMode::SuppressShorts
        );
    }
}
