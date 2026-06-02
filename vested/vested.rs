use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use basket_engine::{basket_to_legs, BasketEngine};
use chrono::NaiveDate;

pub const DEFAULT_UNIVERSE: &str = "config/basket_universe_buildout.toml";
pub const DEFAULT_CAPITAL: f64 = 10_000.0;
pub const DEFAULT_N_ACTIVE_BASKETS: usize = 5;
pub const DEFAULT_LEADERSHIP_LONG_ONLY_LEVERAGE: f64 = 1.25;
pub const DEFAULT_REGIME_MIN_OBSERVATIONS: usize = 21;
pub const DEFAULT_REGIME_MIN_RETURN_20D: f64 = 0.0;
pub const DEFAULT_REGIME_MAX_DRAWDOWN_20D: f64 = 0.05;
pub const DEFAULT_REGIME_RISK_OFF_SCALE: f64 = 0.0;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RegimeGateConfig {
    pub min_observations: usize,
    pub min_return_20d: f64,
    pub max_drawdown_20d: f64,
    pub risk_off_scale: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RegimeGateDecision {
    pub risk_on: bool,
    pub exposure_scale: f64,
    pub reason: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProjectionKind {
    DropShorts,
    PeerMirror,
    ShortPenalty,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProjectionConfig {
    pub kind: ProjectionKind,
    pub short_penalty: f64,
    pub min_long_purity: f64,
    pub min_basket_signal: f64,
}

impl Default for ProjectionConfig {
    fn default() -> Self {
        Self {
            kind: ProjectionKind::DropShorts,
            short_penalty: 1.0,
            min_long_purity: 0.0,
            min_basket_signal: 0.0,
        }
    }
}

#[derive(Debug, Default)]
pub struct ProjectionResult {
    pub target_notionals: HashMap<String, f64>,
    pub suppressed_short_targets: usize,
    pub suppressed_short_gross: f64,
    pub pre_scale_long_gross: f64,
    pub long_scale: f64,
    pub kept_long_targets: usize,
    pub skipped_low_signal_baskets: usize,
    pub skipped_low_purity_symbols: usize,
    pub peer_mirror: Option<PeerMirrorProjection>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StrategyFeatures {
    pub return_20d: f64,
    pub drawdown_20d: f64,
    pub observations: usize,
}

pub struct ProjectionInput<'a> {
    pub engine: &'a BasketEngine,
    pub selected_baskets: &'a [String],
    pub target_notionals: &'a HashMap<String, f64>,
    pub notional_per_basket: f64,
    pub cash_account_cap: f64,
    pub strategy: StrategyFeatures,
    pub selected_basket_projection_allowed: bool,
}

#[derive(Debug)]
pub struct ProjectionDecision {
    pub target_notionals: HashMap<String, f64>,
    pub projection_config: ProjectionConfig,
    pub projection: ProjectionResult,
    pub regime_decision: Option<RegimeGateDecision>,
}

pub trait VestedTargetAdapter {
    fn project_targets(&self, input: ProjectionInput<'_>) -> ProjectionDecision;
    fn picks_tsv(&self) -> Option<&Path>;
}

#[derive(Debug, Clone, PartialEq)]
pub struct ModeConfig {
    pub projection: ProjectionConfig,
    pub regime_gate: Option<RegimeGateConfig>,
    pub picks_tsv: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct VestedMode {
    config: ModeConfig,
}

impl VestedMode {
    pub fn new(config: ModeConfig) -> Self {
        Self { config }
    }
}

fn retain_positive_notionals(targets: &mut HashMap<String, f64>) {
    targets.retain(|_, notional| notional.is_finite() && *notional > 0.0);
}

impl VestedTargetAdapter for VestedMode {
    fn project_targets(&self, input: ProjectionInput<'_>) -> ProjectionDecision {
        let mut projection_config = self.config.projection;
        if !input.selected_basket_projection_allowed
            && matches!(
                projection_config.kind,
                ProjectionKind::PeerMirror | ProjectionKind::ShortPenalty
            )
        {
            projection_config.kind = ProjectionKind::DropShorts;
        }
        let mut projection = project_long_only_notionals(
            input.engine,
            input.selected_baskets,
            input.target_notionals,
            input.notional_per_basket,
            input.cash_account_cap,
            projection_config,
        );
        let regime_decision = self.config.regime_gate.map(|gate| {
            decide_regime_gate(
                gate,
                input.strategy.observations,
                input.strategy.return_20d,
                input.strategy.drawdown_20d,
            )
        });
        if let Some(decision) = regime_decision {
            if decision.exposure_scale < 1.0 {
                for notional in projection.target_notionals.values_mut() {
                    *notional *= decision.exposure_scale;
                }
                retain_positive_notionals(&mut projection.target_notionals);
            }
        }
        ProjectionDecision {
            target_notionals: projection.target_notionals.clone(),
            projection_config,
            projection,
            regime_decision,
        }
    }

    fn picks_tsv(&self) -> Option<&Path> {
        self.config.picks_tsv.as_deref()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresetKind {
    Paper,
    Live,
    Replay,
}

impl PresetKind {
    fn output_root(self) -> &'static str {
        match self {
            Self::Paper => "data/paper/vested_model",
            Self::Live => "data/live/vested_model",
            Self::Replay => "data/replay/vested_model",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Preset {
    pub kind: PresetKind,
    pub universe: PathBuf,
    pub capital: f64,
    pub n_active_baskets: usize,
    pub leadership_long_only_leverage: f64,
    pub vested_long_only: bool,
    pub vested_projection: ProjectionConfig,
    pub vested_regime_gate: Option<RegimeGateConfig>,
    pub output_root: PathBuf,
    pub state_path: PathBuf,
    pub picks_tsv: PathBuf,
    pub report_tsv: Option<PathBuf>,
    pub journal_path: Option<PathBuf>,
}

pub fn preset(kind: PresetKind) -> Preset {
    let output_root = PathBuf::from(kind.output_root());
    Preset {
        kind,
        universe: PathBuf::from(DEFAULT_UNIVERSE),
        capital: DEFAULT_CAPITAL,
        n_active_baskets: DEFAULT_N_ACTIVE_BASKETS,
        leadership_long_only_leverage: DEFAULT_LEADERSHIP_LONG_ONLY_LEVERAGE,
        vested_long_only: true,
        vested_projection: ProjectionConfig::default(),
        vested_regime_gate: Some(default_regime_gate_config()),
        state_path: output_root.join("state.json"),
        picks_tsv: output_root.join("picks.tsv"),
        report_tsv: matches!(kind, PresetKind::Replay).then(|| output_root.join("report.tsv")),
        journal_path: (!matches!(kind, PresetKind::Replay))
            .then(|| output_root.join("journal.sqlite3")),
        output_root,
    }
}

pub fn default_regime_gate_config() -> RegimeGateConfig {
    RegimeGateConfig {
        min_observations: DEFAULT_REGIME_MIN_OBSERVATIONS,
        min_return_20d: DEFAULT_REGIME_MIN_RETURN_20D,
        max_drawdown_20d: DEFAULT_REGIME_MAX_DRAWDOWN_20D,
        risk_off_scale: DEFAULT_REGIME_RISK_OFF_SCALE,
    }
}

pub fn decide_regime_gate(
    config: RegimeGateConfig,
    observations: usize,
    return_20d: f64,
    drawdown_20d: f64,
) -> RegimeGateDecision {
    if observations < config.min_observations {
        return RegimeGateDecision {
            risk_on: true,
            exposure_scale: 1.0,
            reason: "warming_up",
        };
    }
    if return_20d < config.min_return_20d {
        return RegimeGateDecision {
            risk_on: false,
            exposure_scale: config.risk_off_scale.clamp(0.0, 1.0),
            reason: "return_below_threshold",
        };
    }
    if drawdown_20d > config.max_drawdown_20d {
        return RegimeGateDecision {
            risk_on: false,
            exposure_scale: config.risk_off_scale.clamp(0.0, 1.0),
            reason: "drawdown_above_threshold",
        };
    }
    RegimeGateDecision {
        risk_on: true,
        exposure_scale: 1.0,
        reason: "regime_healthy",
    }
}

pub fn record_picks_tsv(
    path: &Path,
    date: NaiveDate,
    vested_long_only: bool,
    target_shares: &HashMap<String, f64>,
    target_notionals: &HashMap<String, f64>,
    closes: &HashMap<String, f64>,
) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("create picks TSV parent {}: {e}", parent.display()))?;
    }
    let needs_header = match std::fs::metadata(path) {
        Ok(meta) => meta.len() == 0,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
        Err(e) => return Err(format!("stat picks TSV {}: {e}", path.display())),
    };
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| format!("open picks TSV {}: {e}", path.display()))?;
    if needs_header {
        writeln!(
            f,
            "date\trow_type\tsymbol\tshares\tclose\tnotional\tweight\tvested_long_only\ttarget_count\tgross_long"
        )
        .map_err(|e| e.to_string())?;
    }

    let gross_long: f64 = target_notionals
        .values()
        .filter(|notional| notional.is_finite() && **notional > 0.0)
        .sum();
    writeln!(
        f,
        "{}\tsummary\t\t\t\t\t\t{}\t{}\t{:.4}",
        date,
        vested_long_only,
        target_notionals.len(),
        gross_long
    )
    .map_err(|e| e.to_string())?;

    let mut symbols: Vec<&String> = target_notionals.keys().collect();
    symbols.sort();
    for symbol in symbols {
        let shares = target_shares.get(symbol).copied().unwrap_or(0.0);
        let close = closes.get(symbol).copied().unwrap_or(0.0);
        let notional = target_notionals.get(symbol).copied().unwrap_or(0.0);
        let weight = if gross_long > 0.0 {
            notional / gross_long
        } else {
            0.0
        };
        writeln!(
            f,
            "{}\tposition\t{}\t{:.4}\t{:.4}\t{:.4}\t{:.6}\t{}\t{}\t{:.4}",
            date,
            symbol,
            shares,
            close,
            notional,
            weight,
            vested_long_only,
            target_notionals.len(),
            gross_long
        )
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

pub fn project_long_only_notionals(
    engine: &BasketEngine,
    selected_baskets: &[String],
    target_notionals: &HashMap<String, f64>,
    notional_per_basket: f64,
    cash_account_cap: f64,
    config: ProjectionConfig,
) -> ProjectionResult {
    let suppressed_short_targets = target_notionals
        .values()
        .filter(|notional| notional.is_finite() && **notional < 0.0)
        .count();
    let suppressed_short_gross: f64 = target_notionals
        .values()
        .filter(|notional| notional.is_finite() && **notional < 0.0)
        .map(|notional| notional.abs())
        .sum();

    let mut result = ProjectionResult {
        suppressed_short_targets,
        suppressed_short_gross,
        ..ProjectionResult::default()
    };

    result.target_notionals = match config.kind {
        ProjectionKind::DropShorts => {
            let mut projected = target_notionals.clone();
            projected.retain(|_, notional| notional.is_finite() && *notional > 0.0);
            projected
        }
        ProjectionKind::PeerMirror => {
            let projection = peer_mirror_notionals(engine, selected_baskets, notional_per_basket);
            let projected = projection.target_notionals.clone();
            result.peer_mirror = Some(projection);
            projected
        }
        ProjectionKind::ShortPenalty => short_penalty_notionals(
            engine,
            selected_baskets,
            notional_per_basket,
            config,
            &mut result,
        ),
    };

    retain_positive_notionals(&mut result.target_notionals);
    result.pre_scale_long_gross = result.target_notionals.values().sum();
    result.long_scale =
        if result.pre_scale_long_gross > cash_account_cap && result.pre_scale_long_gross > 0.0 {
            cash_account_cap / result.pre_scale_long_gross
        } else {
            1.0
        };
    if result.long_scale < 1.0 {
        for notional in result.target_notionals.values_mut() {
            *notional *= result.long_scale;
        }
        retain_positive_notionals(&mut result.target_notionals);
    }
    result.kept_long_targets = result.target_notionals.len();
    result
}

fn short_penalty_notionals(
    engine: &BasketEngine,
    selected_baskets: &[String],
    notional_per_basket: f64,
    config: ProjectionConfig,
    result: &mut ProjectionResult,
) -> HashMap<String, f64> {
    let short_penalty = config.short_penalty.max(0.0);
    let min_long_purity = config.min_long_purity.clamp(0.0, 1.0);
    let min_basket_signal = config.min_basket_signal.max(0.0);
    let mut long_support: HashMap<String, f64> = HashMap::new();
    let mut short_pressure: HashMap<String, f64> = HashMap::new();

    for basket_id in selected_baskets {
        let Some(params) = engine.get_params(basket_id) else {
            continue;
        };
        let Some(state) = engine.get_state(basket_id) else {
            continue;
        };
        let signal = state
            .last_signal_score
            .or(state.last_z)
            .unwrap_or(0.0)
            .abs();
        if signal < min_basket_signal {
            result.skipped_low_signal_baskets += 1;
            continue;
        }

        for leg in basket_to_legs(params, state.position, notional_per_basket) {
            if !leg.notional.is_finite() {
                continue;
            }
            if leg.notional > 0.0 {
                *long_support.entry(leg.symbol).or_default() += leg.notional;
            } else if leg.notional < 0.0 {
                *short_pressure.entry(leg.symbol).or_default() += leg.notional.abs();
            }
        }
    }

    let mut projected = HashMap::new();
    for (symbol, long_notional) in long_support {
        if long_notional <= 0.0 {
            continue;
        }
        let pressure = short_pressure.get(&symbol).copied().unwrap_or(0.0);
        let denominator = long_notional + pressure * short_penalty;
        let purity = if denominator > 0.0 {
            long_notional / denominator
        } else {
            0.0
        };
        if purity < min_long_purity {
            result.skipped_low_purity_symbols += 1;
            continue;
        }
        let notional = long_notional - pressure * short_penalty;
        if notional > 0.0 && notional.is_finite() {
            projected.insert(symbol, notional);
        }
    }
    projected
}

#[derive(Debug, Default)]
pub struct PeerMirrorProjection {
    pub target_notionals: HashMap<String, f64>,
    pub mirrored_baskets: usize,
    pub skipped_baskets: usize,
    pub retained_positive_gross: f64,
    pub mirrored_short_gross: f64,
}

pub fn peer_mirror_notionals(
    engine: &BasketEngine,
    selected_baskets: &[String],
    notional_per_basket: f64,
) -> PeerMirrorProjection {
    let mut projection = PeerMirrorProjection::default();
    for basket_id in selected_baskets {
        let Some(params) = engine.get_params(basket_id) else {
            projection.skipped_baskets += 1;
            continue;
        };
        let Some(state) = engine.get_state(basket_id) else {
            projection.skipped_baskets += 1;
            continue;
        };
        let legs = basket_to_legs(params, state.position, notional_per_basket);
        let positives: Vec<_> = legs
            .iter()
            .filter(|leg| leg.notional.is_finite() && leg.notional > 0.0)
            .collect();
        let positive_gross: f64 = positives.iter().map(|leg| leg.notional).sum();
        if positive_gross <= 0.0 {
            projection.skipped_baskets += 1;
            continue;
        }

        let short_gross: f64 = legs
            .iter()
            .filter(|leg| leg.notional.is_finite() && leg.notional < 0.0)
            .map(|leg| leg.notional.abs())
            .sum();
        projection.retained_positive_gross += positive_gross;
        projection.mirrored_short_gross += short_gross;
        if short_gross > 0.0 {
            projection.mirrored_baskets += 1;
        }

        for leg in positives {
            let mirror_weight = leg.notional / positive_gross;
            let mirrored_notional = leg.notional + short_gross * mirror_weight;
            *projection
                .target_notionals
                .entry(leg.symbol.clone())
                .or_default() += mirrored_notional;
        }
    }
    projection
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drop_shorts_scales_longs_to_cash_cap() {
        let engine = BasketEngine::new(&[]);
        let targets = HashMap::from([
            ("AAPL".to_string(), 6_000.0),
            ("MSFT".to_string(), 4_000.0),
            ("NVDA".to_string(), -5_000.0),
        ]);

        let result = project_long_only_notionals(
            &engine,
            &[],
            &targets,
            1_000.0,
            5_000.0,
            ProjectionConfig::default(),
        );

        assert_eq!(result.suppressed_short_targets, 1);
        assert_eq!(result.kept_long_targets, 2);
        assert_eq!(result.long_scale, 0.5);
        assert_eq!(result.target_notionals.get("AAPL").copied(), Some(3_000.0));
        assert_eq!(result.target_notionals.get("MSFT").copied(), Some(2_000.0));
        assert!(!result.target_notionals.contains_key("NVDA"));
    }

    #[test]
    fn regime_gate_can_scale_vested_book_to_cash() {
        let engine = BasketEngine::new(&[]);
        let targets = HashMap::from([("AAPL".to_string(), 2_000.0)]);
        let mode = VestedMode::new(ModeConfig {
            projection: ProjectionConfig::default(),
            regime_gate: Some(RegimeGateConfig {
                min_observations: 2,
                min_return_20d: 0.0,
                max_drawdown_20d: 0.05,
                risk_off_scale: 0.0,
            }),
            picks_tsv: None,
        });

        let decision = mode.project_targets(ProjectionInput {
            engine: &engine,
            selected_baskets: &[],
            target_notionals: &targets,
            notional_per_basket: 1_000.0,
            cash_account_cap: 10_000.0,
            strategy: StrategyFeatures {
                return_20d: -0.01,
                drawdown_20d: 0.0,
                observations: 21,
            },
            selected_basket_projection_allowed: true,
        });

        assert!(decision.target_notionals.is_empty());
        assert_eq!(
            decision.regime_decision.map(|decision| decision.reason),
            Some("return_below_threshold")
        );
    }

    #[test]
    fn selected_basket_projection_falls_back_after_overlay_transform() {
        let engine = BasketEngine::new(&[]);
        let targets = HashMap::from([
            ("AAPL".to_string(), 2_000.0),
            ("MSFT".to_string(), -1_000.0),
        ]);
        let mode = VestedMode::new(ModeConfig {
            projection: ProjectionConfig {
                kind: ProjectionKind::PeerMirror,
                ..ProjectionConfig::default()
            },
            regime_gate: None,
            picks_tsv: None,
        });

        let decision = mode.project_targets(ProjectionInput {
            engine: &engine,
            selected_baskets: &[],
            target_notionals: &targets,
            notional_per_basket: 1_000.0,
            cash_account_cap: 10_000.0,
            strategy: StrategyFeatures {
                return_20d: 0.0,
                drawdown_20d: 0.0,
                observations: 21,
            },
            selected_basket_projection_allowed: false,
        });

        assert_eq!(decision.projection_config.kind, ProjectionKind::DropShorts);
        assert_eq!(
            decision.target_notionals.get("AAPL").copied(),
            Some(2_000.0)
        );
        assert!(!decision.target_notionals.contains_key("MSFT"));
    }
}
