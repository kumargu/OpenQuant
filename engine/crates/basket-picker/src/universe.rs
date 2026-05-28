//! Universe loading from TOML configuration.

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use crate::schema::BasketCandidate;

/// Version metadata from the universe file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionInfo {
    pub schema: String,
    pub version: String,
    pub frozen_at: String,
}

/// Strategy configuration from the universe file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyConfig {
    pub method: String,
    pub spread_formula: String,
    pub threshold_method: String,
    pub threshold_clip_min: f64,
    pub threshold_clip_max: f64,
    pub residual_window_days: usize,
    pub forward_window_days: usize,
    pub refit_cadence: String,
    pub cost_bps_assumed: f64,
    pub leverage_assumed: f64,
    pub sizing: String,
    #[serde(default)]
    pub adf_gate_enabled: bool,
    #[serde(default = "default_adf_pvalue_max")]
    pub adf_pvalue_max: f64,
    #[serde(default)]
    pub dominance_gate_enabled: bool,
    #[serde(default = "default_dominance_max")]
    pub dominance_max: f64,
}

fn default_adf_pvalue_max() -> f64 {
    0.05
}

fn default_dominance_max() -> f64 {
    0.60
}

fn default_runner_capital() -> f64 {
    10_000.0
}

fn default_runner_n_active_baskets() -> usize {
    5
}

fn default_runner_min_share_preservation_threshold() -> f64 {
    0.85
}

fn default_runner_supported_reallocation_band_max_notional() -> f64 {
    1_000.0
}

fn default_runner_supported_reallocation_band_max_shares() -> f64 {
    1.0
}

fn default_runner_picker() -> RunnerLeadershipPickerConfig {
    RunnerLeadershipPickerConfig::Fixed
}

fn default_runner_overlay_mode() -> RunnerLeadershipOverlayModeConfig {
    RunnerLeadershipOverlayModeConfig::BasketOnly
}

fn default_runner_long_only_leverage() -> f64 {
    1.0
}

fn default_runner_off_ret5d_threshold() -> f64 {
    0.0
}

fn default_runner_off_breadth5d_threshold() -> f64 {
    0.5
}

fn default_runner_persistence_days() -> usize {
    2
}

fn default_runner_min_hold_days() -> usize {
    3
}

fn default_rule_v1_min_dwell_days() -> usize {
    5
}

fn default_rule_v1_off_confirmation_days() -> usize {
    2
}

fn default_rule_v1_suppress_conflict_on_threshold() -> f64 {
    0.15
}

fn default_rule_v1_suppress_conflict_off_threshold() -> f64 {
    0.05
}

fn default_rule_v1_weak_return_threshold() -> f64 {
    0.0
}

fn default_rule_v1_drawdown_on_threshold() -> f64 {
    0.05
}

fn default_rule_v1_recovered_return_threshold() -> f64 {
    0.03
}

fn default_rule_v1_recovered_drawdown_threshold() -> f64 {
    0.03
}

fn default_rule_v1_sleeve_return_ceiling() -> f64 {
    0.03
}

fn default_rule_v1_min_basket_only_scale_for_sleeve() -> f64 {
    0.70
}

fn default_rule_v1_opportunistic_sleeve_min_basket_only_scale() -> f64 {
    0.85
}

fn default_rule_v1_opportunistic_sleeve_return_ceiling() -> f64 {
    0.10
}

fn default_rule_v1_halve_sleeve_drawdown_threshold() -> f64 {
    0.05
}

fn default_rule_v1_quarter_sleeve_drawdown_threshold() -> f64 {
    0.10
}

/// Sector configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SectorConfig {
    pub members: Vec<String>,
    pub traded_targets: Vec<String>,
}

/// Constraints on what NOT to do.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DontDoConfig {
    #[serde(default)]
    pub hl_trade_gate: bool,
    #[serde(default)]
    pub time_based_exit: bool,
    #[serde(default)]
    pub stop_loss: bool,
    #[serde(default)]
    pub regime_derisk: bool,
    #[serde(default)]
    pub continuous_sizing: bool,
    #[serde(default)]
    pub score_based_ranking: bool,
    #[serde(default)]
    pub hl_derived_max_hold: bool,
    #[serde(default)]
    pub pair_engine_reuse: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunnerPortfolioConfig {
    #[serde(default = "default_runner_capital")]
    pub capital: f64,
    #[serde(default = "default_runner_n_active_baskets")]
    pub n_active_baskets: usize,
    #[serde(default)]
    pub preserve_near_unit_shares: bool,
    #[serde(default = "default_runner_min_share_preservation_threshold")]
    pub min_share_preservation_threshold: f64,
    #[serde(default)]
    pub supported_reallocation_band_enabled: bool,
    #[serde(default = "default_runner_supported_reallocation_band_max_notional")]
    pub supported_reallocation_band_max_notional: f64,
    #[serde(default = "default_runner_supported_reallocation_band_max_shares")]
    pub supported_reallocation_band_max_shares: f64,
}

impl Default for RunnerPortfolioConfig {
    fn default() -> Self {
        Self {
            capital: default_runner_capital(),
            n_active_baskets: default_runner_n_active_baskets(),
            preserve_near_unit_shares: false,
            min_share_preservation_threshold: default_runner_min_share_preservation_threshold(),
            supported_reallocation_band_enabled: false,
            supported_reallocation_band_max_notional:
                default_runner_supported_reallocation_band_max_notional(),
            supported_reallocation_band_max_shares:
                default_runner_supported_reallocation_band_max_shares(),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum RunnerLeadershipPickerConfig {
    #[default]
    #[serde(rename = "fixed")]
    Fixed,
    #[serde(rename = "rule_v1", alias = "rule-v1")]
    RuleV1,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum RunnerLeadershipOverlayModeConfig {
    #[default]
    #[serde(rename = "basket_only", alias = "baseline")]
    BasketOnly,
    #[serde(rename = "suppress_shorts", alias = "suppress-shorts")]
    SuppressShorts,
    #[serde(rename = "replace_with_long_only", alias = "replace-with-long-only")]
    ReplaceWithLongOnly,
    #[serde(rename = "add_capped_long_sleeve", alias = "add-capped-long-sleeve")]
    AddCappedLongSleeve,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunnerRuleV1OverlayConfig {
    #[serde(default = "default_rule_v1_min_dwell_days")]
    pub min_dwell_days: usize,
    #[serde(default = "default_rule_v1_off_confirmation_days")]
    pub off_confirmation_days: usize,
    #[serde(default = "default_rule_v1_suppress_conflict_on_threshold")]
    pub suppress_conflict_on_threshold: f64,
    #[serde(default = "default_rule_v1_suppress_conflict_off_threshold")]
    pub suppress_conflict_off_threshold: f64,
    #[serde(default = "default_rule_v1_weak_return_threshold")]
    pub weak_return_threshold: f64,
    #[serde(default = "default_rule_v1_drawdown_on_threshold")]
    pub drawdown_on_threshold: f64,
    #[serde(default = "default_rule_v1_recovered_return_threshold")]
    pub recovered_return_threshold: f64,
    #[serde(default = "default_rule_v1_recovered_drawdown_threshold")]
    pub recovered_drawdown_threshold: f64,
    #[serde(default = "default_rule_v1_sleeve_return_ceiling")]
    pub sleeve_return_ceiling: f64,
    #[serde(
        default = "default_rule_v1_min_basket_only_scale_for_sleeve",
        alias = "min_baseline_scale_for_sleeve"
    )]
    pub min_basket_only_scale_for_sleeve: f64,
    #[serde(
        default = "default_rule_v1_opportunistic_sleeve_min_basket_only_scale",
        alias = "opportunistic_sleeve_min_baseline_scale"
    )]
    pub opportunistic_sleeve_min_basket_only_scale: f64,
    #[serde(default = "default_rule_v1_opportunistic_sleeve_return_ceiling")]
    pub opportunistic_sleeve_return_ceiling: f64,
    #[serde(default = "default_rule_v1_halve_sleeve_drawdown_threshold")]
    pub halve_sleeve_drawdown_threshold: f64,
    #[serde(default = "default_rule_v1_quarter_sleeve_drawdown_threshold")]
    pub quarter_sleeve_drawdown_threshold: f64,
}

impl Default for RunnerRuleV1OverlayConfig {
    fn default() -> Self {
        Self {
            min_dwell_days: default_rule_v1_min_dwell_days(),
            off_confirmation_days: default_rule_v1_off_confirmation_days(),
            suppress_conflict_on_threshold: default_rule_v1_suppress_conflict_on_threshold(),
            suppress_conflict_off_threshold: default_rule_v1_suppress_conflict_off_threshold(),
            weak_return_threshold: default_rule_v1_weak_return_threshold(),
            drawdown_on_threshold: default_rule_v1_drawdown_on_threshold(),
            recovered_return_threshold: default_rule_v1_recovered_return_threshold(),
            recovered_drawdown_threshold: default_rule_v1_recovered_drawdown_threshold(),
            sleeve_return_ceiling: default_rule_v1_sleeve_return_ceiling(),
            min_basket_only_scale_for_sleeve: default_rule_v1_min_basket_only_scale_for_sleeve(),
            opportunistic_sleeve_min_basket_only_scale:
                default_rule_v1_opportunistic_sleeve_min_basket_only_scale(),
            opportunistic_sleeve_return_ceiling:
                default_rule_v1_opportunistic_sleeve_return_ceiling(),
            halve_sleeve_drawdown_threshold: default_rule_v1_halve_sleeve_drawdown_threshold(),
            quarter_sleeve_drawdown_threshold: default_rule_v1_quarter_sleeve_drawdown_threshold(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunnerLeadershipOverlayConfig {
    pub sectors: Vec<String>,
    pub on_ret5d_threshold: f64,
    pub on_breadth5d_threshold: f64,
    #[serde(default = "default_runner_off_ret5d_threshold")]
    pub off_ret5d_threshold: f64,
    #[serde(default = "default_runner_off_breadth5d_threshold")]
    pub off_breadth5d_threshold: f64,
    #[serde(default = "default_runner_persistence_days")]
    pub persistence_days: usize,
    #[serde(default = "default_runner_min_hold_days")]
    pub min_hold_days: usize,
    #[serde(default = "default_runner_overlay_mode")]
    pub mode: RunnerLeadershipOverlayModeConfig,
    #[serde(default = "default_runner_picker")]
    pub picker: RunnerLeadershipPickerConfig,
    #[serde(default = "default_runner_long_only_leverage")]
    pub long_only_leverage: f64,
    #[serde(default)]
    pub rule_v1: RunnerRuleV1OverlayConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RunnerConfig {
    #[serde(default)]
    pub decision_offset_minutes_before_close: u32,
    #[serde(default)]
    pub portfolio: RunnerPortfolioConfig,
    #[serde(default)]
    pub leadership_overlay: Option<RunnerLeadershipOverlayConfig>,
}

/// Raw TOML structure for deserialization.
#[derive(Debug, Deserialize)]
struct RawUniverse {
    version: VersionInfo,
    strategy: StrategyConfig,
    sectors: HashMap<String, SectorConfig>,
    #[serde(default)]
    dont_do: DontDoConfig,
    #[serde(default)]
    runner: RunnerConfig,
}

/// The complete basket universe.
#[derive(Debug, Clone)]
pub struct Universe {
    pub version: VersionInfo,
    pub strategy: StrategyConfig,
    pub sectors: HashMap<String, SectorConfig>,
    pub dont_do: DontDoConfig,
    pub runner: RunnerConfig,
    /// All basket candidates derived from sectors.
    pub candidates: Vec<BasketCandidate>,
}

impl Universe {
    /// Get candidates for a specific sector.
    pub fn candidates_for_sector(&self, sector: &str) -> Vec<&BasketCandidate> {
        self.candidates
            .iter()
            .filter(|c| c.sector == sector)
            .collect()
    }

    /// Total number of tradeable baskets.
    pub fn num_baskets(&self) -> usize {
        self.candidates.len()
    }
}

/// Load universe from a TOML file.
///
/// The file must follow the basket_universe schema.
/// Returns an error string if parsing fails.
pub fn load_universe(path: &Path) -> Result<Universe, String> {
    let content = fs::read_to_string(path).map_err(|e| format!("failed to read file: {}", e))?;

    load_universe_from_str(&content)
}

/// Load universe from a TOML string.
pub fn load_universe_from_str(content: &str) -> Result<Universe, String> {
    let raw: RawUniverse =
        toml::from_str(content).map_err(|e| format!("failed to parse TOML: {}", e))?;

    // Validate schema and version
    if raw.version.schema != "basket_universe" {
        return Err(format!(
            "unsupported schema: expected 'basket_universe', got '{}'",
            raw.version.schema
        ));
    }
    if raw.version.version != "v1" {
        return Err(format!(
            "unsupported version: expected 'v1', got '{}'",
            raw.version.version
        ));
    }

    // Parse frozen_at date for candidates
    let fit_date = NaiveDate::parse_from_str(&raw.version.frozen_at, "%Y-%m-%d")
        .map_err(|e| format!("invalid frozen_at date: {}", e))?;

    // Semantic validation and candidate building
    let mut candidates = Vec::new();
    for (sector_name, sector) in &raw.sectors {
        // Check for duplicate members
        let mut seen_members = std::collections::HashSet::new();
        for m in &sector.members {
            if !seen_members.insert(m) {
                return Err(format!(
                    "sector '{}': duplicate member '{}'",
                    sector_name, m
                ));
            }
        }

        // Check for duplicate traded_targets
        let mut seen_targets = std::collections::HashSet::new();
        for t in &sector.traded_targets {
            if !seen_targets.insert(t) {
                return Err(format!(
                    "sector '{}': duplicate traded_target '{}'",
                    sector_name, t
                ));
            }
        }

        // Check traded_targets ⊆ members
        for target in &sector.traded_targets {
            if !sector.members.contains(target) {
                return Err(format!(
                    "sector '{}': traded_target '{}' not in members",
                    sector_name, target
                ));
            }

            // Peers = members excluding target; need at least 2
            let peer_count = sector.members.iter().filter(|m| *m != target).count();
            if peer_count < 2 {
                return Err(format!(
                    "sector '{}': target '{}' has only {} peer(s), need >= 2",
                    sector_name, target, peer_count
                ));
            }
        }

        // Build candidates
        for target in &sector.traded_targets {
            let members: Vec<String> = sector
                .members
                .iter()
                .filter(|m| *m != target)
                .cloned()
                .collect();

            candidates.push(BasketCandidate {
                target: target.clone(),
                members,
                sector: sector_name.clone(),
                fit_date,
            });
        }
    }

    // Sort for deterministic ordering
    candidates.sort_by_key(|a| a.id());

    Ok(Universe {
        version: raw.version,
        strategy: raw.strategy,
        sectors: raw.sectors,
        dont_do: raw.dont_do,
        runner: raw.runner,
        candidates,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL_TOML: &str = r#"
[version]
schema = "basket_universe"
version = "v1"
frozen_at = "2026-04-20"

[strategy]
method = "basket_spread_ou_bertram"
spread_formula = "log(target) - mean(log(peers))"
threshold_method = "bertram_symmetric"
threshold_clip_min = 0.15
threshold_clip_max = 2.5
residual_window_days = 60
forward_window_days = 60
refit_cadence = "quarterly"
cost_bps_assumed = 5.0
leverage_assumed = 4.0
sizing = "equal_weight_across_baskets"

[sectors.chips]
members = ["NVDA", "AMD", "INTC"]
traded_targets = ["AMD", "INTC"]

[dont_do]
hl_trade_gate = false

[runner.portfolio]
capital = 10000
n_active_baskets = 5
preserve_near_unit_shares = true
min_share_preservation_threshold = 0.9
supported_reallocation_band_enabled = true
supported_reallocation_band_max_notional = 750
supported_reallocation_band_max_shares = 2

[runner.leadership_overlay]
sectors = ["chips"]
on_ret5d_threshold = 0.02
on_breadth5d_threshold = 0.56
picker = "rule_v1"
long_only_leverage = 1.0
"#;

    #[test]
    fn test_load_universe_minimal() {
        let universe = load_universe_from_str(MINIMAL_TOML).unwrap();
        assert_eq!(universe.version.version, "v1");
        assert_eq!(universe.strategy.threshold_clip_min, 0.15);
        assert_eq!(universe.num_baskets(), 2);
        assert_eq!(universe.runner.portfolio.capital, 10_000.0);
        assert_eq!(universe.runner.portfolio.n_active_baskets, 5);
        assert!(universe.runner.portfolio.preserve_near_unit_shares);
        assert!((universe.runner.portfolio.min_share_preservation_threshold - 0.9).abs() < 1e-9);
        assert!(
            universe
                .runner
                .portfolio
                .supported_reallocation_band_enabled
        );
        assert!(
            (universe
                .runner
                .portfolio
                .supported_reallocation_band_max_notional
                - 750.0)
                .abs()
                < 1e-9
        );
        assert!(
            (universe
                .runner
                .portfolio
                .supported_reallocation_band_max_shares
                - 2.0)
                .abs()
                < 1e-9
        );
        assert_eq!(
            universe.runner.leadership_overlay.as_ref().unwrap().picker,
            RunnerLeadershipPickerConfig::RuleV1
        );
    }

    #[test]
    fn test_candidates_have_correct_members() {
        let universe = load_universe_from_str(MINIMAL_TOML).unwrap();
        let amd_basket = universe
            .candidates
            .iter()
            .find(|c| c.target == "AMD")
            .unwrap();
        assert!(amd_basket.members.contains(&"NVDA".to_string()));
        assert!(amd_basket.members.contains(&"INTC".to_string()));
        assert!(!amd_basket.members.contains(&"AMD".to_string()));
    }

    #[test]
    fn test_candidate_id_format() {
        let universe = load_universe_from_str(MINIMAL_TOML).unwrap();
        let amd_basket = universe
            .candidates
            .iter()
            .find(|c| c.target == "AMD")
            .unwrap();
        // ID format: {sector}:{target}:{fit_date}:{members_hash}
        let id = amd_basket.id();
        assert!(id.starts_with("chips:AMD:2026-04-20:"));
        assert_eq!(id.len(), "chips:AMD:2026-04-20:".len() + 8); // 8-char hex hash
    }

    #[test]
    fn test_reject_traded_target_not_in_members() {
        let toml = r#"
[version]
schema = "basket_universe"
version = "v1"
frozen_at = "2026-04-20"

[strategy]
method = "basket_spread_ou_bertram"
spread_formula = "log(target) - mean(log(peers))"
threshold_method = "bertram_symmetric"
threshold_clip_min = 0.15
threshold_clip_max = 2.5
residual_window_days = 60
forward_window_days = 60
refit_cadence = "quarterly"
cost_bps_assumed = 5.0
leverage_assumed = 4.0
sizing = "equal_weight_across_baskets"

[sectors.chips]
members = ["NVDA", "AMD"]
traded_targets = ["INTC"]
"#;
        let err = load_universe_from_str(toml).unwrap_err();
        assert!(err.contains("traded_target 'INTC' not in members"));
    }

    #[test]
    fn test_reject_duplicate_members() {
        let toml = r#"
[version]
schema = "basket_universe"
version = "v1"
frozen_at = "2026-04-20"

[strategy]
method = "basket_spread_ou_bertram"
spread_formula = "log(target) - mean(log(peers))"
threshold_method = "bertram_symmetric"
threshold_clip_min = 0.15
threshold_clip_max = 2.5
residual_window_days = 60
forward_window_days = 60
refit_cadence = "quarterly"
cost_bps_assumed = 5.0
leverage_assumed = 4.0
sizing = "equal_weight_across_baskets"

[sectors.chips]
members = ["NVDA", "AMD", "NVDA"]
traded_targets = ["AMD"]
"#;
        let err = load_universe_from_str(toml).unwrap_err();
        assert!(err.contains("duplicate member 'NVDA'"));
    }

    #[test]
    fn test_reject_insufficient_peers() {
        let toml = r#"
[version]
schema = "basket_universe"
version = "v1"
frozen_at = "2026-04-20"

[strategy]
method = "basket_spread_ou_bertram"
spread_formula = "log(target) - mean(log(peers))"
threshold_method = "bertram_symmetric"
threshold_clip_min = 0.15
threshold_clip_max = 2.5
residual_window_days = 60
forward_window_days = 60
refit_cadence = "quarterly"
cost_bps_assumed = 5.0
leverage_assumed = 4.0
sizing = "equal_weight_across_baskets"

[sectors.chips]
members = ["NVDA", "AMD"]
traded_targets = ["AMD"]
"#;
        let err = load_universe_from_str(toml).unwrap_err();
        assert!(err.contains("has only 1 peer(s), need >= 2"));
    }

    #[test]
    fn test_legacy_overlay_keys_still_load() {
        let toml = r#"
[version]
schema = "basket_universe"
version = "v1"
frozen_at = "2026-04-20"

[strategy]
method = "basket_spread_ou_bertram"
spread_formula = "log(target) - mean(log(peers))"
threshold_method = "bertram_symmetric"
threshold_clip_min = 0.15
threshold_clip_max = 2.5
residual_window_days = 60
forward_window_days = 60
refit_cadence = "quarterly"
cost_bps_assumed = 5.0
leverage_assumed = 4.0
sizing = "equal_weight_across_baskets"

[sectors.chips]
members = ["NVDA", "AMD", "AVGO"]
traded_targets = ["AMD"]

[runner]
decision_offset_minutes_before_close = 0

[runner.portfolio]
capital = 10000
n_active_baskets = 5

[runner.leadership_overlay]
mode = "baseline"
sectors = ["chips"]
on_ret5d_threshold = 0.02
on_breadth5d_threshold = 0.56
off_ret5d_threshold = 0.0
off_breadth5d_threshold = 0.5
persistence_days = 2
min_hold_days = 3
picker = "rule_v1"

[runner.leadership_overlay.rule_v1]
min_baseline_scale_for_sleeve = 0.65
opportunistic_sleeve_min_baseline_scale = 0.80
"#;
        let universe = load_universe_from_str(toml).unwrap();
        let overlay = universe.runner.leadership_overlay.unwrap();
        assert_eq!(overlay.mode, RunnerLeadershipOverlayModeConfig::BasketOnly);
        assert!((overlay.rule_v1.min_basket_only_scale_for_sleeve - 0.65).abs() < 1e-9);
        assert!((overlay.rule_v1.opportunistic_sleeve_min_basket_only_scale - 0.80).abs() < 1e-9);
    }
}
