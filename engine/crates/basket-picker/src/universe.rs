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

/// Raw TOML structure for deserialization.
#[derive(Debug, Deserialize)]
struct RawUniverse {
    version: VersionInfo,
    strategy: StrategyConfig,
    sectors: HashMap<String, SectorConfig>,
    #[serde(default)]
    dont_do: DontDoConfig,
}

/// The complete basket universe.
#[derive(Debug, Clone)]
pub struct Universe {
    pub version: VersionInfo,
    pub strategy: StrategyConfig,
    pub sectors: HashMap<String, SectorConfig>,
    pub dont_do: DontDoConfig,
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

    // Build candidates from sectors
    let mut candidates = Vec::new();
    for (sector_name, sector) in &raw.sectors {
        for target in &sector.traded_targets {
            // Members are all symbols in the sector except the target
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
        assert_eq!(amd_basket.id(), "chips:AMD");
    }
}
