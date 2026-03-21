//! Read `active_pairs.json` and write `pair_trading_history.json`.
//!
//! Bridges the pair-picker binary (daily statistical validation) with the
//! PairsEngine (real-time trading). The pair-picker writes `active_pairs.json`;
//! the engine reads it on startup and after daily refreshes.
//!
//! Trading history written here feeds back into Thompson sampling for
//! adaptive pair selection.

use crate::pairs::PairConfig;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;
use tracing::{info, warn};

/// Parsed active pair from `active_pairs.json`.
/// Mirrors the pair-picker's `ActivePair` output format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivePairEntry {
    pub leg_a: String,
    pub leg_b: String,
    pub alpha: f64,
    pub beta: f64,
    pub half_life_days: f64,
    pub adf_statistic: f64,
    pub adf_pvalue: f64,
    pub beta_cv: f64,
    pub structural_break: bool,
    #[serde(default)]
    pub regime_robustness: f64,
    pub economic_rationale: String,
    pub score: f64,
}

/// The active_pairs.json file format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActivePairsFile {
    pub generated_at: DateTime<Utc>,
    pub pairs: Vec<ActivePairEntry>,
}

/// Staleness threshold: reject file older than this many hours.
const MAX_STALENESS_HOURS: i64 = 48;

/// Load active pairs from JSON file and convert to PairConfigs.
///
/// Returns `None` if file is missing, unparseable, or stale (>48h old).
/// The caller should fall back to last known good pairs in that case.
pub fn load_active_pairs(path: &Path) -> Option<(ActivePairsFile, Vec<PairConfig>)> {
    let contents = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) => {
            warn!(path = %path.display(), error = %e, "active_pairs.json not found — using fallback");
            return None;
        }
    };

    let file: ActivePairsFile = match serde_json::from_str(&contents) {
        Ok(f) => f,
        Err(e) => {
            warn!(path = %path.display(), error = %e, "active_pairs.json parse error — using fallback");
            return None;
        }
    };

    // Check staleness
    let age = Utc::now().signed_duration_since(file.generated_at);
    if age.num_hours() > MAX_STALENESS_HOURS {
        warn!(
            age_hours = age.num_hours(),
            generated_at = %file.generated_at,
            "active_pairs.json is stale (>{}h) — using fallback",
            MAX_STALENESS_HOURS
        );
        return None;
    }

    let configs: Vec<PairConfig> = file
        .pairs
        .iter()
        .map(|p| {
            info!(
                pair = format!("{}/{}", p.leg_a, p.leg_b).as_str(),
                beta = format!("{:.4}", p.beta).as_str(),
                score = format!("{:.3}", p.score).as_str(),
                half_life = format!("{:.1}", p.half_life_days).as_str(),
                "Loaded active pair"
            );
            PairConfig {
                leg_a: p.leg_a.clone(),
                leg_b: p.leg_b.clone(),
                alpha: p.alpha,
                beta: p.beta,
                // Use defaults for trading parameters — these come from openquant.toml
                ..PairConfig::default()
            }
        })
        .collect();

    info!(
        count = configs.len(),
        generated_at = %file.generated_at,
        "Loaded {} active pairs from {}",
        configs.len(),
        path.display()
    );

    Some((file, configs))
}

// ---------------------------------------------------------------------------
// Trading history — written after each trade closes
// ---------------------------------------------------------------------------

/// A closed pair trade, written for Thompson sampling feedback.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClosedPairTrade {
    pub pair: (String, String),
    pub entry_date: String,
    pub exit_date: String,
    pub entry_zscore: f64,
    pub exit_zscore: f64,
    /// Return in basis points.
    pub return_bps: f64,
    pub holding_period_bars: usize,
    pub exit_reason: String,
}

/// Trade history file format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairTradingHistory {
    pub trades: Vec<ClosedPairTrade>,
}

impl PairTradingHistory {
    /// Load existing history, or create empty.
    pub fn load(path: &Path) -> Self {
        match fs::read_to_string(path) {
            Ok(contents) => serde_json::from_str(&contents).unwrap_or(Self { trades: Vec::new() }),
            Err(_) => Self { trades: Vec::new() },
        }
    }

    /// Append a trade and write back to disk.
    pub fn append_and_save(&mut self, trade: ClosedPairTrade, path: &Path) -> std::io::Result<()> {
        info!(
            pair = format!("{}/{}", trade.pair.0, trade.pair.1).as_str(),
            return_bps = format!("{:.1}", trade.return_bps).as_str(),
            holding_bars = trade.holding_period_bars,
            exit = trade.exit_reason.as_str(),
            "Recording closed pair trade"
        );
        self.trades.push(trade);
        let json = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, json)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn sample_active_pairs_json() -> String {
        format!(
            r#"{{
  "generated_at": "{}",
  "pairs": [
    {{
      "leg_a": "GS",
      "leg_b": "MS",
      "alpha": 0.5,
      "beta": 1.23,
      "half_life_days": 8.5,
      "adf_statistic": -3.5,
      "adf_pvalue": 0.003,
      "beta_cv": 0.08,
      "structural_break": false,
      "regime_robustness": 0.85,
      "economic_rationale": "Investment banks",
      "score": 0.85
    }},
    {{
      "leg_a": "GLD",
      "leg_b": "SLV",
      "alpha": 0.2,
      "beta": 0.37,
      "half_life_days": 10.0,
      "adf_statistic": -4.1,
      "adf_pvalue": 0.001,
      "beta_cv": 0.05,
      "structural_break": false,
      "regime_robustness": 0.95,
      "economic_rationale": "Precious metals",
      "score": 0.92
    }}
  ]
}}"#,
            Utc::now().to_rfc3339()
        )
    }

    #[test]
    fn test_load_active_pairs() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("active_pairs.json");
        fs::write(&path, sample_active_pairs_json()).unwrap();

        let (file, configs) = load_active_pairs(&path).unwrap();
        assert_eq!(file.pairs.len(), 2);
        assert_eq!(configs.len(), 2);
        assert_eq!(configs[0].leg_a, "GS");
        assert_eq!(configs[0].leg_b, "MS");
        assert!((configs[0].beta - 1.23).abs() < 0.01);
        assert_eq!(configs[1].leg_a, "GLD");
    }

    #[test]
    fn test_load_missing_file() {
        let result = load_active_pairs(Path::new("/nonexistent/active_pairs.json"));
        assert!(result.is_none());
    }

    #[test]
    fn test_load_stale_file() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("active_pairs.json");
        // Write file with old timestamp
        let json = r#"{
  "generated_at": "2020-01-01T00:00:00Z",
  "pairs": []
}"#;
        fs::write(&path, json).unwrap();

        let result = load_active_pairs(&path);
        assert!(result.is_none(), "Stale file should be rejected");
    }

    #[test]
    fn test_load_invalid_json() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("active_pairs.json");
        fs::write(&path, "not json").unwrap();

        let result = load_active_pairs(&path);
        assert!(result.is_none());
    }

    #[test]
    fn test_trading_history_append() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("pair_trading_history.json");

        let mut history = PairTradingHistory::load(&path);
        assert_eq!(history.trades.len(), 0);

        let trade = ClosedPairTrade {
            pair: ("GS".into(), "MS".into()),
            entry_date: "2026-03-10".into(),
            exit_date: "2026-03-14".into(),
            entry_zscore: 2.1,
            exit_zscore: 0.3,
            return_bps: 42.0,
            holding_period_bars: 4,
            exit_reason: "reversion".into(),
        };

        history.append_and_save(trade, &path).unwrap();
        assert_eq!(history.trades.len(), 1);

        // Reload and verify
        let reloaded = PairTradingHistory::load(&path);
        assert_eq!(reloaded.trades.len(), 1);
        assert!((reloaded.trades[0].return_bps - 42.0).abs() < 0.01);
    }

    #[test]
    fn test_trading_history_accumulates() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("history.json");

        let mut history = PairTradingHistory::load(&path);

        for i in 0..3 {
            let trade = ClosedPairTrade {
                pair: ("A".into(), "B".into()),
                entry_date: format!("2026-03-{:02}", 10 + i),
                exit_date: format!("2026-03-{:02}", 12 + i),
                entry_zscore: 2.0,
                exit_zscore: 0.5,
                return_bps: (i as f64) * 10.0,
                holding_period_bars: 2,
                exit_reason: "reversion".into(),
            };
            history.append_and_save(trade, &path).unwrap();
        }

        let reloaded = PairTradingHistory::load(&path);
        assert_eq!(reloaded.trades.len(), 3);
    }
}
