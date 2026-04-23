use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use basket_picker::{load_universe, validate, BasketFit, Universe, ValidatorConfig};
use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::basket_live::{align_basket_history, collect_symbols, load_warmup_closes};

const FIT_ARTIFACT_SCHEMA: &str = "basket_fit_artifact";
const FIT_ARTIFACT_VERSION: &str = "v1";
const WARMUP_DAYS: i64 = 90;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BasketFitArtifact {
    pub schema: String,
    pub version: String,
    pub universe_version: String,
    pub universe_frozen_at: String,
    pub generated_at: String,
    pub fits: Vec<BasketFit>,
}

pub fn default_fit_artifact_path(universe_path: &Path) -> PathBuf {
    let stem = universe_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("basket_universe");
    universe_path.with_file_name(format!("{stem}.fits.json"))
}

pub fn build_live_fit_artifact(
    universe_path: &Path,
    bars_dir: &Path,
) -> Result<BasketFitArtifact, String> {
    let universe = load_universe(universe_path)?;
    let symbols = collect_symbols(&universe);
    let closes = load_warmup_closes(bars_dir, &symbols, WARMUP_DAYS)?;
    build_live_fit_artifact_from_inputs(&universe, &closes)
}

pub fn build_live_fit_artifact_from_inputs(
    universe: &Universe,
    closes: &std::collections::HashMap<String, Vec<(chrono::NaiveDate, f64)>>,
) -> Result<BasketFitArtifact, String> {
    let validator_config = ValidatorConfig {
        residual_window: universe.strategy.residual_window_days,
        k_clip_min: universe.strategy.threshold_clip_min,
        k_clip_max: universe.strategy.threshold_clip_max,
        cost: universe.strategy.cost_bps_assumed / 10_000.0,
    };

    let fits: Vec<BasketFit> = universe
        .candidates
        .iter()
        .map(|c| {
            let mut basket_symbols: Vec<&str> = Vec::with_capacity(c.members.len() + 1);
            basket_symbols.push(c.target.as_str());
            basket_symbols.extend(c.members.iter().map(String::as_str));
            let aligned = align_basket_history(closes, &basket_symbols);
            validate(c, &aligned, &validator_config)
        })
        .collect();
    ensure_unique_fit_ids(&fits)?;

    Ok(BasketFitArtifact {
        schema: FIT_ARTIFACT_SCHEMA.to_string(),
        version: FIT_ARTIFACT_VERSION.to_string(),
        universe_version: universe.version.version.clone(),
        universe_frozen_at: universe.version.frozen_at.clone(),
        generated_at: Utc::now().to_rfc3339(),
        fits,
    })
}

pub fn save_fit_artifact(path: &Path, artifact: &BasketFitArtifact) -> Result<(), String> {
    let content = serde_json::to_string_pretty(artifact)
        .map_err(|e| format!("serialize fit artifact: {e}"))?;
    fs::write(path, content).map_err(|e| format!("write {}: {e}", path.display()))
}

pub fn load_fit_artifact(path: &Path, universe: &Universe) -> Result<BasketFitArtifact, String> {
    let content = fs::read_to_string(path)
        .map_err(|e| format!("read fit artifact {}: {e}", path.display()))?;
    let artifact: BasketFitArtifact =
        serde_json::from_str(&content).map_err(|e| format!("parse fit artifact: {e}"))?;

    if artifact.schema != FIT_ARTIFACT_SCHEMA {
        return Err(format!(
            "unsupported fit artifact schema '{}'",
            artifact.schema
        ));
    }
    if artifact.version != FIT_ARTIFACT_VERSION {
        return Err(format!(
            "unsupported fit artifact version '{}'",
            artifact.version
        ));
    }
    if artifact.universe_version != universe.version.version {
        return Err(format!(
            "fit artifact universe version mismatch: artifact={}, universe={}",
            artifact.universe_version, universe.version.version
        ));
    }
    if artifact.universe_frozen_at != universe.version.frozen_at {
        return Err(format!(
            "fit artifact frozen_at mismatch: artifact={}, universe={}",
            artifact.universe_frozen_at, universe.version.frozen_at
        ));
    }
    ensure_unique_fit_ids(&artifact.fits)?;

    let expected_ids: HashSet<String> = universe.candidates.iter().map(|c| c.id()).collect();
    let artifact_ids: HashSet<String> = artifact.fits.iter().map(|f| f.candidate.id()).collect();
    if expected_ids != artifact_ids {
        return Err(format!(
            "fit artifact candidate set mismatch: artifact={}, universe={}",
            artifact_ids.len(),
            expected_ids.len()
        ));
    }

    Ok(artifact)
}

fn ensure_unique_fit_ids(fits: &[BasketFit]) -> Result<(), String> {
    let mut seen = HashSet::new();
    for fit in fits {
        let id = fit.candidate.id();
        if !seen.insert(id.clone()) {
            return Err(format!("duplicate basket fit id '{id}' in artifact"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use basket_picker::load_universe_from_str;
    use std::collections::HashMap;

    const TEST_UNIVERSE: &str = r#"
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

[sectors.test]
members = ["AAA", "BBB", "CCC"]
traded_targets = ["AAA"]
"#;

    fn mk_series(base: f64) -> Vec<(chrono::NaiveDate, f64)> {
        let mut out = Vec::new();
        for i in 0..90 {
            let d = chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap() + chrono::Days::new(i);
            out.push((d, base + (i as f64 * 0.01)));
        }
        out
    }

    #[test]
    fn test_artifact_roundtrip_and_validation() {
        let universe = load_universe_from_str(TEST_UNIVERSE).unwrap();
        let mut closes: HashMap<String, Vec<(chrono::NaiveDate, f64)>> = HashMap::new();
        closes.insert("AAA".to_string(), mk_series(100.0));
        closes.insert("BBB".to_string(), mk_series(101.0));
        closes.insert("CCC".to_string(), mk_series(99.0));

        let artifact = build_live_fit_artifact_from_inputs(&universe, &closes).unwrap();
        let tmp = tempfile::NamedTempFile::new().unwrap();
        save_fit_artifact(tmp.path(), &artifact).unwrap();
        let loaded = load_fit_artifact(tmp.path(), &universe).unwrap();

        assert_eq!(loaded.schema, FIT_ARTIFACT_SCHEMA);
        assert_eq!(loaded.fits.len(), 1);
        assert_eq!(loaded.fits[0].candidate.id(), universe.candidates[0].id());
    }

    #[test]
    fn test_default_fit_artifact_path() {
        let path = Path::new("config/basket_universe_v1.toml");
        assert_eq!(
            default_fit_artifact_path(path),
            PathBuf::from("config/basket_universe_v1.fits.json")
        );
    }

    #[test]
    fn test_duplicate_fit_ids_are_rejected() {
        let universe = load_universe_from_str(TEST_UNIVERSE).unwrap();
        let mut closes: HashMap<String, Vec<(chrono::NaiveDate, f64)>> = HashMap::new();
        closes.insert("AAA".to_string(), mk_series(100.0));
        closes.insert("BBB".to_string(), mk_series(101.0));
        closes.insert("CCC".to_string(), mk_series(99.0));

        let mut artifact = build_live_fit_artifact_from_inputs(&universe, &closes).unwrap();
        artifact.fits.push(artifact.fits[0].clone());
        let tmp = tempfile::NamedTempFile::new().unwrap();
        save_fit_artifact(tmp.path(), &artifact).unwrap();

        let err = load_fit_artifact(tmp.path(), &universe).unwrap_err();
        assert!(err.contains("duplicate basket fit id"));
    }
}
