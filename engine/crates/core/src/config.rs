//! TOML configuration loader.
//!
//! Reads `openquant.toml` and produces an `EngineConfig`. Rust owns
//! the config schema — Python just passes a file path.

use std::collections::HashMap;
use std::path::Path;

use crate::engine::{EngineConfig, SymbolOverrides};
use crate::exit::ExitConfig;
use crate::risk::RiskConfig;
use crate::signals::mean_reversion;

/// Top-level TOML file layout.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct ConfigFile {
    pub metrics: MetricsConfig,
    pub signal: mean_reversion::Config,
    pub risk: RiskConfig,
    pub exit: ExitConfig,
    pub data: DataConfig,
    pub symbol_overrides: HashMap<String, SymbolOverrides>,
}

/// Metrics toggle (more fields later when we wire CloudWatch).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct MetricsConfig {
    pub enabled: bool,
}

impl Default for MetricsConfig {
    fn default() -> Self {
        Self { enabled: true }
    }
}

/// Data-level settings.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct DataConfig {
    /// Maximum bar age in seconds (0 = disabled).
    pub max_bar_age_seconds: i64,
}

impl ConfigFile {
    /// Read and parse a TOML config file.
    pub fn load(path: &Path) -> Result<Self, String> {
        let contents = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        toml::from_str(&contents).map_err(|e| format!("invalid TOML in {}: {e}", path.display()))
    }

    /// Convert into an `EngineConfig` ready for the engine.
    pub fn into_engine_config(self) -> EngineConfig {
        EngineConfig {
            signal: self.signal,
            risk: self.risk,
            exit: self.exit,
            symbol_overrides: self.symbol_overrides,
            max_bar_age_ms: self.data.max_bar_age_seconds * 1000,
            metrics_enabled: self.metrics.enabled,
        }
    }

    /// Whether metrics collection is enabled.
    pub fn metrics_enabled(&self) -> bool {
        self.metrics.enabled
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn parse_full_config() {
        let toml = r#"
[metrics]
enabled = false

[signal]
buy_z_threshold = -2.5
sell_z_threshold = 2.5
min_relative_volume = 1.5
min_score = 0.3
trend_filter = false

[risk]
max_position_notional = 5000.0
max_daily_loss = 250.0
min_reward_cost_ratio = 2.0
estimated_cost_bps = 0.002

[exit]
stop_loss_pct = 0.01
stop_loss_atr_mult = 3.0
max_hold_bars = 50
take_profit_pct = 0.05

[data]
max_bar_age_seconds = 120

[symbol_overrides.BTCUSD]
buy_z_threshold = -3.0
stop_loss_atr_mult = 4.0
"#;
        let cfg: ConfigFile = toml::from_str(toml).unwrap();

        assert!(!cfg.metrics_enabled());
        assert_eq!(cfg.signal.buy_z_threshold, -2.5);
        assert_eq!(cfg.signal.sell_z_threshold, 2.5);
        assert_eq!(cfg.signal.min_relative_volume, 1.5);
        assert!(!cfg.signal.trend_filter);
        assert_eq!(cfg.risk.max_position_notional, 5000.0);
        assert_eq!(cfg.risk.max_daily_loss, 250.0);
        assert_eq!(cfg.exit.stop_loss_atr_mult, 3.0);
        assert_eq!(cfg.exit.max_hold_bars, 50);
        assert_eq!(cfg.data.max_bar_age_seconds, 120);

        let ovr = &cfg.symbol_overrides["BTCUSD"];
        assert_eq!(ovr.buy_z_threshold, Some(-3.0));
        assert_eq!(ovr.stop_loss_atr_mult, Some(4.0));
        assert!(ovr.sell_z_threshold.is_none());

        let ec = cfg.into_engine_config();
        assert_eq!(ec.max_bar_age_ms, 120_000);
    }

    #[test]
    fn empty_toml_uses_defaults() {
        let cfg: ConfigFile = toml::from_str("").unwrap();
        assert!(cfg.metrics_enabled());
        assert_eq!(cfg.signal.buy_z_threshold, -2.2);
        assert_eq!(cfg.risk.max_position_notional, 10_000.0);
        assert_eq!(cfg.exit.stop_loss_atr_mult, 2.5);
    }

    #[test]
    fn partial_toml_fills_defaults() {
        let toml = r#"
[signal]
buy_z_threshold = -3.0
"#;
        let cfg: ConfigFile = toml::from_str(toml).unwrap();
        assert_eq!(cfg.signal.buy_z_threshold, -3.0);
        // Everything else should be default
        assert_eq!(cfg.signal.sell_z_threshold, 2.0);
        assert_eq!(cfg.risk.max_daily_loss, 500.0);
        assert_eq!(cfg.exit.max_hold_bars, 100);
    }

    #[test]
    fn load_from_file() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(
            tmp,
            r#"
[signal]
buy_z_threshold = -1.5
"#
        )
        .unwrap();
        let cfg = ConfigFile::load(tmp.path()).unwrap();
        assert_eq!(cfg.signal.buy_z_threshold, -1.5);
    }

    #[test]
    fn load_missing_file_errors() {
        let result = ConfigFile::load(Path::new("/nonexistent/openquant.toml"));
        assert!(result.is_err());
    }

    #[test]
    fn load_invalid_toml_errors() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        write!(tmp, "this is not valid {{ toml").unwrap();
        let result = ConfigFile::load(tmp.path());
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("invalid TOML"));
    }
}
