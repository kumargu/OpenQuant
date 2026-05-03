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
    #[serde(default)]
    pub baseline_sharpe_point: Option<f64>,
    #[serde(default)]
    pub baseline_ci_95_lo: Option<f64>,
    #[serde(default)]
    pub baseline_ci_95_hi: Option<f64>,
    #[serde(default)]
    pub baseline_ci_method: Option<String>,
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

/// Sector configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SectorConfig {
    pub members: Vec<String>,
    pub traded_targets: Vec<String>,
}

/// Runner-side knobs that affect HOW the runner samples and acts at
/// session close, not WHAT the strategy is. Kept separate from
/// [`StrategyConfig`] so the strategy fingerprint stays clean across
/// research-only timing/snapshot experiments.
///
/// Intentionally permissive on missing values — every field has a
/// `serde(default)` so the existing frozen v1 universe TOML (which
/// has no `[runner]` section) parses unchanged and produces
/// byte-identical behavior to the pre-#321 runtime.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RunnerConfig {
    /// Snapshot cutoff for the once-per-day basket decision: keep the
    /// most-recent intraday bar whose OPEN minute (NY local) is at or
    /// before `(RTH_CLOSE_MINUTE − decision_offset_minutes_before_close)`.
    ///
    /// `0` = last RTH bar (current production behavior).
    /// `5` = "t-5m" snapshot — last bar opening at or before 15:55 ET.
    /// `15` = "t-15m" — at or before 15:45 ET.
    /// `30` = "t-30m" — at or before 15:30 ET.
    ///
    /// In replay this is the only meaningful timing knob — bars are
    /// drained chronologically before the session-close trigger fires,
    /// so the live distinction between "close" and "close + grace" (when
    /// the runner reacts) collapses onto "which bar's close did you
    /// keep" (what this knob controls).
    ///
    /// Live's `CLOSE_GRACE_MIN` is independent and unaffected.
    #[serde(default)]
    pub decision_offset_minutes_before_close: u32,
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
/// The file must follow the basket_universe_v1 schema.
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

    // Validate runner config bounds. RTH is 6.5h = 390 minutes, so an
    // offset >= 390 would push the cutoff before the open and reject
    // every bar — fail fast instead of producing an empty snapshot.
    const RTH_MINUTES: u32 = 390;
    if raw.runner.decision_offset_minutes_before_close >= RTH_MINUTES {
        return Err(format!(
            "[runner].decision_offset_minutes_before_close must be < {RTH_MINUTES} \
             (got {}); the cutoff would push before RTH open",
            raw.runner.decision_offset_minutes_before_close
        ));
    }

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
"#;

    #[test]
    fn test_load_universe_minimal() {
        let universe = load_universe_from_str(MINIMAL_TOML).unwrap();
        assert_eq!(universe.version.version, "v1");
        assert_eq!(universe.strategy.threshold_clip_min, 0.15);
        assert_eq!(universe.num_baskets(), 2);
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
}
