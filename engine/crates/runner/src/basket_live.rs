//! Live / paper / replay runner for the basket spread engine.
//!
//! Drives `basket_engine::BasketEngine` (continuous streaming state machine)
//! with bars from either the Alpaca WebSocket (live, paper) or per-symbol
//! parquet files via [`crate::parquet_bar_source::ParquetBarSource`] (replay).
//! All three modes flow through `run_basket_live`; the only difference is
//! which `Broker`, `BarSource`, `Clock`, and `SessionTrigger` impls are
//! passed in.
//!
//! Flow per trading day:
//!   1. Startup: load the frozen basket fit artifact and build `BasketEngine`
//!      from those persisted `BasketFit`s. Engine enters with empty state.
//!   2. Bar loop: for each 1-min bar, update per-symbol "last RTH bar".
//!   3. Session close (final RTH minute after close+grace): snapshot the
//!      day's closes, call
//!      `BasketEngine::on_bars()`, get `PositionIntent`s.
//!   4. Portfolio: aggregate intents → admit active baskets → convert target
//!      notionals to target shares → `OrderIntent`s via `diff_to_orders()`.
//!   5. Execute: depending on `BasketExecution`, log only (Noop), or place
//!      orders on paper/live Alpaca.
//!
//! Three execution modes:
//!   - `Noop`: log intents, place no orders. Use this for the first sessions
//!     to verify engine behavior before any capital moves.
//!   - `Paper`: paper-api.alpaca.markets (paper money).
//!   - `Live`: api.alpaca.markets (real money). Gated behind explicit opt-in.

use std::collections::{HashMap, HashSet, VecDeque};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use basket_engine::{
    plan_portfolio, BasketEngine, DailyBar, OrderIntent, PortfolioConfig, PortfolioPlan,
    PositionIntent, Side,
};
use basket_picker::{load_universe, BasketFit};
use chrono::{DateTime, NaiveDate, Utc};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use serde::{Deserialize, Serialize};
use tracing::{debug, error, info, warn};

use crate::alpaca::ExecutionMode;
use crate::bar_source::BarSource;
use crate::basket_journal::{
    serialize_shares_map, serialize_string_vec, BasketJournal, BasketOrderEvent,
    BasketPickerDecisionRecord, BasketRunRecord, BasketSessionCloseRecord,
};
use crate::basket_overlay_picker::{
    BasketOverlayMode, BasketOverlayPicker, BasketOverlayPickerFeatures, BasketOverlayPickerHandle,
    BasketOverlayPickerKind,
};
use crate::broker::Broker;
use crate::clock::Clock;
use crate::market_session;
use crate::session_trigger::SessionTrigger;
use crate::stream;

macro_rules! bug {
    ($kind:literal, $($field:tt)*) => {{
        metrics::counter!("bug", "component" => "basket_live", "kind" => $kind).increment(1);
        error!(bug = true, bug_marker = "BUG", kind = $kind, $($field)*);
    }};
}

/// Execution mode for basket live/paper.
///
/// Distinct from [`ExecutionMode`] because basket adds a `Noop` shadow mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BasketExecution {
    /// Log intents only; no Alpaca order placed.
    Noop,
    /// Paper trading API.
    Paper,
    /// Real-money trading API. Requires explicit `--execution live`.
    Live,
}

impl BasketExecution {
    /// Map to the Alpaca adapter's [`ExecutionMode`]. Noop returns None.
    fn alpaca_mode(self) -> Option<ExecutionMode> {
        match self {
            Self::Noop => None,
            Self::Paper => Some(ExecutionMode::Paper),
            Self::Live => Some(ExecutionMode::Live),
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Noop => "NOOP (shadow)",
            Self::Paper => "PAPER",
            Self::Live => "LIVE",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StartupPhase {
    NonTradingDay,
    PreOpen,
    Intraday,
    PostClosePendingCatchup,
    PostCloseProcessed,
}

impl StartupPhase {
    fn as_str(self) -> &'static str {
        match self {
            Self::NonTradingDay => "non_trading_day",
            Self::PreOpen => "pre_open",
            Self::Intraday => "intraday",
            Self::PostClosePendingCatchup => "post_close_pending_catchup",
            Self::PostCloseProcessed => "post_close_processed",
        }
    }
}

#[derive(Debug, Clone)]
pub struct BasketRunOptions {
    pub fit_artifact_path: Option<PathBuf>,
    pub journal_path: Option<PathBuf>,
    pub leadership_overlay: Option<LeadershipOverlayConfig>,
    pub overlay_picker: BasketOverlayPickerKind,
    pub rule_v1_config: Option<crate::basket_overlay_picker::RuleV1OverlayPickerConfig>,
}

impl Default for BasketRunOptions {
    fn default() -> Self {
        Self {
            fit_artifact_path: None,
            journal_path: None,
            leadership_overlay: None,
            overlay_picker: BasketOverlayPickerKind::Fixed,
            rule_v1_config: None,
        }
    }
}

// Grace period after session close before firing the engine. Lets
// late-arriving final-RTH-minute bars land in the buffer.
//
// The `clock` and `session_trigger` parameters MUST agree on this value:
// `IntervalSessionTrigger` is constructed with the same constant in
// `main.rs`. If they diverge, replay/live cadence drifts.
const CLOSE_GRACE_MIN: u32 = 2;
const BROKER_QTY_EPSILON: f64 = 0.5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StartupStateSource {
    Snapshot,
    BrokerReconciled,
    Fresh,
}

#[derive(Debug, Clone)]
pub struct LeadershipOverlayConfig {
    pub sectors: Vec<String>,
    pub on_ret5d_threshold: f64,
    pub on_breadth5d_threshold: f64,
    pub off_ret5d_threshold: f64,
    pub off_breadth5d_threshold: f64,
    pub persistence_days: usize,
    pub min_hold_days: usize,
    pub mode: BasketOverlayMode,
    pub long_only_leverage: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct SectorLeadershipSnapshot {
    active_sectors: HashSet<String>,
}

#[derive(Debug, Clone, Copy)]
struct SectorLeadershipFeatures {
    ret5d: f64,
    breadth5d: f64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct SectorClassifierState {
    enabled: bool,
    pending_on_days: usize,
    hold_days_remaining: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SectorLeadershipTrackerState {
    #[serde(default)]
    config_fingerprint: String,
    prev_closes: Option<HashMap<String, f64>>,
    sector_returns: HashMap<String, VecDeque<f64>>,
    sector_breadths: HashMap<String, VecDeque<f64>>,
    classifier_states: HashMap<String, SectorClassifierState>,
    last_snapshot: SectorLeadershipSnapshot,
}

#[derive(Debug, Clone)]
struct SectorLeadershipTracker {
    config: LeadershipOverlayConfig,
    sector_members: HashMap<String, Vec<String>>,
    prev_closes: Option<HashMap<String, f64>>,
    sector_returns: HashMap<String, VecDeque<f64>>,
    sector_breadths: HashMap<String, VecDeque<f64>>,
    classifier_states: HashMap<String, SectorClassifierState>,
    last_snapshot: SectorLeadershipSnapshot,
}

impl SectorLeadershipTracker {
    fn new(config: LeadershipOverlayConfig, sector_members: HashMap<String, Vec<String>>) -> Self {
        let mut sector_returns = HashMap::new();
        let mut sector_breadths = HashMap::new();
        for sector in &config.sectors {
            sector_returns.insert(sector.clone(), VecDeque::with_capacity(5));
            sector_breadths.insert(sector.clone(), VecDeque::with_capacity(5));
        }
        let classifier_states = config
            .sectors
            .iter()
            .map(|sector| (sector.clone(), SectorClassifierState::default()))
            .collect();
        Self {
            config,
            sector_members,
            prev_closes: None,
            sector_returns,
            sector_breadths,
            classifier_states,
            last_snapshot: SectorLeadershipSnapshot::default(),
        }
    }

    fn active_sectors_for_today(&self) -> &HashSet<String> {
        &self.last_snapshot.active_sectors
    }

    fn active_symbols_for_today(&self) -> Vec<String> {
        let mut symbols: Vec<String> = self
            .last_snapshot
            .active_sectors
            .iter()
            .filter_map(|sector| self.sector_members.get(sector))
            .flat_map(|members| members.iter().cloned())
            .collect();
        symbols.sort();
        symbols.dedup();
        symbols
    }

    fn config_fingerprint(&self) -> String {
        let mut sectors = self.config.sectors.clone();
        sectors.sort();
        let mode = match self.config.mode {
            BasketOverlayMode::Baseline => "baseline",
            BasketOverlayMode::SuppressShorts => "suppress_shorts",
            BasketOverlayMode::ReplaceWithLongOnly => "replace_with_long_only",
            BasketOverlayMode::AddCappedLongSleeve => "add_capped_long_sleeve",
        };
        format!(
            "sectors={}|on_ret={:.8}|on_breadth={:.8}|off_ret={:.8}|off_breadth={:.8}|persistence={}|min_hold={}|mode={}|long_only_lev={:.8}",
            sectors.join(","),
            self.config.on_ret5d_threshold,
            self.config.on_breadth5d_threshold,
            self.config.off_ret5d_threshold,
            self.config.off_breadth5d_threshold,
            self.config.persistence_days,
            self.config.min_hold_days,
            mode,
            self.config.long_only_leverage
        )
    }

    fn observe_close_snapshot(&mut self, closes: &HashMap<String, f64>) {
        let Some(prev) = self.prev_closes.clone() else {
            self.prev_closes = Some(closes.clone());
            return;
        };
        let mut next_active = HashSet::new();
        let sectors = self.config.sectors.clone();
        for sector in &sectors {
            if let Some(features) = self.observe_sector_features(sector, &prev, closes) {
                let state = self.classifier_states.entry(sector.clone()).or_default();
                let on_signal = features.ret5d > self.config.on_ret5d_threshold
                    && features.breadth5d > self.config.on_breadth5d_threshold;
                let off_signal = features.ret5d < self.config.off_ret5d_threshold
                    && features.breadth5d < self.config.off_breadth5d_threshold;

                if state.enabled {
                    if state.hold_days_remaining > 0 {
                        state.hold_days_remaining -= 1;
                    }
                    if state.hold_days_remaining == 0 && off_signal {
                        state.enabled = false;
                        state.pending_on_days = 0;
                        info!(
                            sector = sector.as_str(),
                            ret5d = features.ret5d,
                            breadth5d = features.breadth5d,
                            "leadership overlay classifier switched OFF"
                        );
                    }
                } else if on_signal {
                    state.pending_on_days += 1;
                    if state.pending_on_days >= self.config.persistence_days.max(1) {
                        state.enabled = true;
                        state.pending_on_days = 0;
                        state.hold_days_remaining = self.config.min_hold_days;
                        info!(
                            sector = sector.as_str(),
                            ret5d = features.ret5d,
                            breadth5d = features.breadth5d,
                            min_hold_days = self.config.min_hold_days,
                            "leadership overlay classifier switched ON"
                        );
                    }
                } else {
                    state.pending_on_days = 0;
                }
            }

            if self
                .classifier_states
                .get(sector)
                .map(|state| state.enabled)
                .unwrap_or(false)
            {
                next_active.insert(sector.clone());
            }
        }
        self.last_snapshot = SectorLeadershipSnapshot {
            active_sectors: next_active,
        };
        self.prev_closes = Some(closes.clone());
    }

    fn observe_sector_features(
        &mut self,
        sector: &str,
        prev: &HashMap<String, f64>,
        closes: &HashMap<String, f64>,
    ) -> Option<SectorLeadershipFeatures> {
        let members = self.sector_members.get(sector)?;
        let mut rets = Vec::new();
        let mut up = 0usize;
        let mut total = 0usize;
        for symbol in members {
            let Some(&close) = closes.get(symbol) else {
                continue;
            };
            let Some(&prev_close) = prev.get(symbol) else {
                continue;
            };
            if !close.is_finite() || !prev_close.is_finite() || close <= 0.0 || prev_close <= 0.0 {
                continue;
            }
            let ret = close / prev_close - 1.0;
            rets.push(ret);
            total += 1;
            if ret > 0.0 {
                up += 1;
            }
        }
        if rets.is_empty() || total == 0 {
            return None;
        }
        let ew_ret = rets.iter().sum::<f64>() / rets.len() as f64;
        let breadth = up as f64 / total as f64;
        let hist_rets = self.sector_returns.entry(sector.to_string()).or_default();
        let hist_breadths = self.sector_breadths.entry(sector.to_string()).or_default();
        hist_rets.push_back(ew_ret);
        hist_breadths.push_back(breadth);
        while hist_rets.len() > 5 {
            hist_rets.pop_front();
        }
        while hist_breadths.len() > 5 {
            hist_breadths.pop_front();
        }
        if hist_rets.len() != 5 || hist_breadths.len() != 5 {
            return None;
        }
        Some(SectorLeadershipFeatures {
            ret5d: hist_rets.iter().fold(1.0_f64, |acc, r| acc * (1.0 + r)) - 1.0,
            breadth5d: hist_breadths.iter().sum::<f64>() / hist_breadths.len() as f64,
        })
    }

    fn load_state(&mut self, path: &Path) -> Result<bool, String> {
        if !path.exists() {
            return Ok(false);
        }
        let content = std::fs::read_to_string(path)
            .map_err(|e| format!("read leadership classifier state {}: {e}", path.display()))?;
        let state: SectorLeadershipTrackerState = serde_json::from_str(&content)
            .map_err(|e| format!("parse leadership classifier state {}: {e}", path.display()))?;
        let expected_fingerprint = self.config_fingerprint();
        if state.config_fingerprint != expected_fingerprint {
            warn!(
                state_path = %path.display(),
                "leadership classifier state config mismatch — rebuilding from warmup data"
            );
            return Ok(false);
        }
        self.prev_closes = state.prev_closes;
        self.sector_returns = state.sector_returns;
        self.sector_breadths = state.sector_breadths;
        self.classifier_states = state.classifier_states;
        for sector in &self.config.sectors {
            self.sector_returns.entry(sector.clone()).or_default();
            self.sector_breadths.entry(sector.clone()).or_default();
            self.classifier_states.entry(sector.clone()).or_default();
        }
        self.last_snapshot = state.last_snapshot;
        Ok(true)
    }

    fn save_state(&self, path: &Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("create leadership state dir {}: {e}", parent.display()))?;
        }
        let state = SectorLeadershipTrackerState {
            config_fingerprint: self.config_fingerprint(),
            prev_closes: self.prev_closes.clone(),
            sector_returns: self.sector_returns.clone(),
            sector_breadths: self.sector_breadths.clone(),
            classifier_states: self.classifier_states.clone(),
            last_snapshot: self.last_snapshot.clone(),
        };
        let content = serde_json::to_string_pretty(&state)
            .map_err(|e| format!("serialize leadership classifier state: {e}"))?;
        let tmp = path.with_extension("leadership.tmp");
        std::fs::write(&tmp, content)
            .map_err(|e| format!("write leadership classifier tmp {}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, path)
            .map_err(|e| format!("rename leadership classifier state {}: {e}", path.display()))
    }
}

fn previous_trading_day(mut day: NaiveDate) -> Option<NaiveDate> {
    for _ in 0..10 {
        day = day.pred_opt()?;
        if market_session::is_trading_day(day) {
            return Some(day);
        }
    }
    None
}

fn warm_leadership_tracker(
    tracker: &mut SectorLeadershipTracker,
    bars_dir: &Path,
    symbols: &[String],
    anchor_day: NaiveDate,
) -> Result<(), String> {
    let warm_days = 5 + tracker.config.persistence_days + tracker.config.min_hold_days + 5;
    let closes =
        load_daily_closes_with_timestamps(bars_dir, symbols, warm_days as i64, Some(anchor_day))?;
    let mut by_day: std::collections::BTreeMap<NaiveDate, HashMap<String, f64>> =
        std::collections::BTreeMap::new();
    for (symbol, series) in closes {
        for (day, _ts_us, close) in series {
            if day <= anchor_day && close.is_finite() && close > 0.0 {
                by_day.entry(day).or_default().insert(symbol.clone(), close);
            }
        }
    }
    for closes_for_day in by_day.values() {
        tracker.observe_close_snapshot(closes_for_day);
    }
    info!(
        requested_warm_days = warm_days,
        warm_days = by_day.len(),
        anchor_day = %anchor_day,
        active_sectors = ?tracker.active_sectors_for_today(),
        "leadership tracker warmed from historical close snapshots"
    );
    Ok(())
}

fn leadership_picker_features(
    tracker: Option<&SectorLeadershipTracker>,
    equity_features: StrategyEquityFeatures,
) -> BasketOverlayPickerFeatures {
    BasketOverlayPickerFeatures {
        active_sectors: tracker
            .map(|t| t.active_sectors_for_today().clone())
            .unwrap_or_default(),
        long_symbols: tracker
            .map(|t| t.active_symbols_for_today())
            .unwrap_or_default(),
        leadership_short_conflict_ratio: 0.0,
        strategy_return_20d: equity_features.return_20d,
        strategy_drawdown_20d: equity_features.drawdown_20d,
        baseline_scale_if_sleeve: 1.0,
    }
}

fn add_baseline_plan_features(
    mut features: BasketOverlayPickerFeatures,
    baseline_notionals: &HashMap<String, f64>,
    portfolio_config: &PortfolioConfig,
    leadership_overlay: Option<&LeadershipOverlayConfig>,
) -> BasketOverlayPickerFeatures {
    let gross = baseline_notionals
        .values()
        .map(|notional| notional.abs())
        .sum::<f64>();
    if gross <= 0.0 || features.long_symbols.is_empty() {
        features.leadership_short_conflict_ratio = 0.0;
        return features;
    }
    let leadership_symbols: HashSet<&str> =
        features.long_symbols.iter().map(String::as_str).collect();
    let conflict = baseline_notionals
        .iter()
        .filter(|(symbol, notional)| {
            **notional < 0.0 && leadership_symbols.contains(symbol.as_str())
        })
        .map(|(_symbol, notional)| notional.abs())
        .sum::<f64>();
    features.leadership_short_conflict_ratio = conflict / gross;
    features.baseline_scale_if_sleeve =
        baseline_scale_if_sleeve(baseline_notionals, portfolio_config, leadership_overlay);
    features
}

fn baseline_scale_if_sleeve(
    baseline_notionals: &HashMap<String, f64>,
    portfolio_config: &PortfolioConfig,
    leadership_overlay: Option<&LeadershipOverlayConfig>,
) -> f64 {
    let Some(cfg) = leadership_overlay else {
        return 1.0;
    };
    let baseline_gross = baseline_notionals
        .values()
        .map(|notional| notional.abs())
        .sum::<f64>();
    if baseline_gross <= 0.0 {
        return 1.0;
    }
    let gross_cap = portfolio_config.capital * portfolio_config.leverage;
    let sleeve_budget = (cfg.long_only_leverage * portfolio_config.capital).min(gross_cap);
    let baseline_budget = (gross_cap - sleeve_budget).max(0.0);
    (baseline_budget / baseline_gross).clamp(0.0, 1.0)
}

fn engine_flatten_baskets_for_plan(
    plan: &PortfolioPlan,
    suppressed_baskets: &[String],
    using_long_replacement: bool,
) -> Vec<String> {
    if using_long_replacement {
        return Vec::new();
    }
    let mut basket_ids = suppressed_baskets.to_vec();
    basket_ids.extend(plan.excluded_baskets.iter().cloned());
    basket_ids.sort();
    basket_ids.dedup();
    basket_ids
}

#[derive(Debug, Clone, Copy, Default)]
struct StrategyEquityFeatures {
    return_20d: f64,
    drawdown_20d: f64,
}

fn strategy_equity_features(equity_history: &VecDeque<f64>) -> StrategyEquityFeatures {
    let values: Vec<f64> = equity_history
        .iter()
        .copied()
        .filter(|equity| equity.is_finite() && *equity > 0.0)
        .collect();
    if values.len() < 2 {
        return StrategyEquityFeatures::default();
    }
    let last = *values.last().unwrap();
    let window_start = values.len().saturating_sub(21);
    let window = &values[window_start..];
    let first = window[0];
    let peak = window.iter().copied().fold(first, f64::max);
    StrategyEquityFeatures {
        return_20d: last / first - 1.0,
        drawdown_20d: if peak > 0.0 {
            (peak - last) / peak
        } else {
            0.0
        },
    }
}

fn push_equity_history(equity_history: &mut VecDeque<f64>, equity: f64) {
    if !equity.is_finite() || equity <= 0.0 {
        return;
    }
    equity_history.push_back(equity);
    while equity_history.len() > 21 {
        equity_history.pop_front();
    }
}

pub fn leadership_classifier_state_path(engine_state_path: &Path) -> PathBuf {
    let mut name = engine_state_path
        .file_name()
        .map(|s| s.to_os_string())
        .unwrap_or_else(|| OsString::from("basket.state.json"));
    name.push(".leadership.json");
    engine_state_path.with_file_name(name)
}

pub fn overlay_picker_state_path(engine_state_path: &Path) -> PathBuf {
    let mut name = engine_state_path
        .file_name()
        .map(|s| s.to_os_string())
        .unwrap_or_else(|| OsString::from("basket.state.json"));
    name.push(".picker.json");
    engine_state_path.with_file_name(name)
}

fn basket_sector(basket_id: &str) -> &str {
    basket_id.split(':').next().unwrap_or(basket_id)
}

fn leadership_short_suppression_baskets(
    engine: &basket_engine::BasketEngine,
    active_sectors: &HashSet<String>,
) -> Vec<String> {
    if active_sectors.is_empty() {
        return Vec::new();
    }
    let mut suppressed = Vec::new();
    for (basket_id, _params) in engine.iter_params() {
        if !active_sectors.contains(basket_sector(basket_id)) {
            continue;
        }
        let Some(state) = engine.get_state(basket_id) else {
            continue;
        };
        if state.position < 0 {
            suppressed.push(basket_id.clone());
        }
    }
    suppressed
}

fn leadership_long_only_notionals(
    closes: &HashMap<String, f64>,
    symbols: &[String],
    capital: f64,
    leverage: f64,
) -> HashMap<String, f64> {
    let active_symbols: Vec<&String> = symbols
        .iter()
        .filter(|symbol| matches!(closes.get(*symbol), Some(price) if price.is_finite() && *price > 0.0))
        .collect();
    if active_symbols.is_empty() || !capital.is_finite() || !leverage.is_finite() || leverage <= 0.0
    {
        return HashMap::new();
    }
    let per_symbol = capital * leverage / active_symbols.len() as f64;
    active_symbols
        .into_iter()
        .map(|symbol| (symbol.clone(), per_symbol))
        .collect()
}

fn scale_notionals(targets: &HashMap<String, f64>, scale: f64) -> HashMap<String, f64> {
    if !scale.is_finite() || scale <= 0.0 {
        return HashMap::new();
    }
    targets
        .iter()
        .filter_map(|(symbol, notional)| {
            let scaled = *notional * scale;
            if scaled.is_finite() && scaled.abs() > f64::EPSILON {
                Some((symbol.clone(), scaled))
            } else {
                None
            }
        })
        .collect()
}

fn merge_notionals(lhs: &HashMap<String, f64>, rhs: &HashMap<String, f64>) -> HashMap<String, f64> {
    let mut merged = lhs.clone();
    for (symbol, notional) in rhs {
        *merged.entry(symbol.clone()).or_default() += *notional;
    }
    merged.retain(|_, notional| notional.is_finite() && notional.abs() > f64::EPSILON);
    merged
}

fn parse_equity(account: &crate::alpaca::AlpacaAccount) -> Result<f64, String> {
    let equity = account
        .equity
        .parse::<f64>()
        .map_err(|e| format!("invalid Alpaca equity '{}': {e}", account.equity))?;
    if !equity.is_finite() || equity <= 0.0 {
        return Err(format!("Alpaca equity is not positive: {}", account.equity));
    }
    Ok(equity)
}

fn top_abs_notional_legs(targets: &HashMap<String, f64>, limit: usize) -> Vec<String> {
    let mut legs: Vec<(&str, f64)> = targets
        .iter()
        .filter_map(|(symbol, notional)| {
            if notional.is_finite() && *notional != 0.0 {
                Some((symbol.as_str(), *notional))
            } else {
                None
            }
        })
        .collect();
    legs.sort_by(|(lhs_symbol, lhs_notional), (rhs_symbol, rhs_notional)| {
        rhs_notional
            .abs()
            .partial_cmp(&lhs_notional.abs())
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| lhs_symbol.cmp(rhs_symbol))
    });
    legs.into_iter()
        .take(limit)
        .map(|(symbol, notional)| format!("{symbol}:{notional:.0}"))
        .collect()
}

fn effective_execution_capital(config_capital: f64, account_equity: Option<f64>) -> f64 {
    if !config_capital.is_finite() || config_capital <= 0.0 {
        return config_capital;
    }
    match account_equity {
        Some(equity) if equity.is_finite() && equity > 0.0 => config_capital.min(equity),
        _ => config_capital,
    }
}

async fn preflight_account_check(broker: &impl Broker, mode: ExecutionMode) -> Result<(), String> {
    let account = broker.get_account(mode).await?;
    let buying_power = parse_buying_power(&account)?;
    let equity = parse_equity(&account)?;
    if account.status != "ACTIVE" {
        return Err(format!(
            "Alpaca account not ACTIVE: status={}",
            account.status
        ));
    }
    if account.trading_blocked || account.account_blocked {
        return Err(format!(
            "Alpaca account blocked: trading_blocked={}, account_blocked={}",
            account.trading_blocked, account.account_blocked
        ));
    }
    info!(
        mode = ?mode,
        buying_power = %format!("{:.0}", buying_power),
        equity = %format!("{:.0}", equity),
        status = account.status.as_str(),
        "startup account preflight passed"
    );
    Ok(())
}

fn parse_buying_power(account: &crate::alpaca::AlpacaAccount) -> Result<f64, String> {
    let buying_power = account.buying_power.parse::<f64>().map_err(|e| {
        format!(
            "invalid Alpaca buying_power '{}': {e}",
            account.buying_power
        )
    })?;
    if !buying_power.is_finite() || buying_power <= 0.0 {
        return Err(format!(
            "Alpaca buying power is not positive: {}",
            account.buying_power
        ));
    }
    Ok(buying_power)
}

async fn check_order_set_affordability(
    broker: &impl Broker,
    mode: ExecutionMode,
    date: NaiveDate,
    current_shares: &HashMap<String, f64>,
    target_shares: &HashMap<String, f64>,
    orders: &[OrderIntent],
    closes: &HashMap<String, f64>,
) -> Result<(), String> {
    let account = broker.get_account(mode).await?;
    let buying_power = parse_buying_power(&account)?;
    let equity = parse_equity(&account)?;
    let (current_long_gross, current_short_gross) = gross_by_side(current_shares, closes);
    let (target_long_gross, target_short_gross) = gross_by_side(target_shares, closes);
    let incremental_long = (target_long_gross - current_long_gross).max(0.0);
    let incremental_short = (target_short_gross - current_short_gross).max(0.0);
    let incremental_exposure = incremental_long + incremental_short;
    let target_gross = target_long_gross + target_short_gross;
    let current_gross = current_long_gross + current_short_gross;
    let order_turnover: f64 = orders
        .iter()
        .filter_map(|o| closes.get(&o.symbol).map(|p| p * o.qty as f64))
        .sum();
    if incremental_exposure > buying_power + 1.0 {
        return Err(format!(
            "incremental exposure {:.2} exceeds Alpaca buying power {:.2} on {} (equity {:.2}, current_gross {:.2}, target_gross {:.2})",
            incremental_exposure, buying_power, date, equity, current_gross, target_gross
        ));
    }
    info!(
        date = %date,
        equity = %format!("{:.0}", equity),
        current_long_gross = %format!("{:.0}", current_long_gross),
        current_short_gross = %format!("{:.0}", current_short_gross),
        current_gross = %format!("{:.0}", current_gross),
        target_long_gross = %format!("{:.0}", target_long_gross),
        target_short_gross = %format!("{:.0}", target_short_gross),
        target_gross = %format!("{:.0}", target_gross),
        incremental_long_notional = %format!("{:.0}", incremental_long),
        incremental_short_notional = %format!("{:.0}", incremental_short),
        incremental_exposure_notional = %format!("{:.0}", incremental_exposure),
        order_turnover_notional = %format!("{:.0}", order_turnover),
        buying_power = %format!("{:.0}", buying_power),
        "order-set affordability check passed"
    );
    Ok(())
}

fn gross_by_side(shares: &HashMap<String, f64>, closes: &HashMap<String, f64>) -> (f64, f64) {
    let mut long_gross = 0.0;
    let mut short_gross = 0.0;
    for (symbol, qty) in shares {
        let Some(price) = closes.get(symbol) else {
            continue;
        };
        let notional = qty * price;
        if notional > 0.0 {
            long_gross += notional;
        } else {
            short_gross += notional.abs();
        }
    }
    (long_gross, short_gross)
}

fn summarize_orders_by_side(
    orders: &[OrderIntent],
    closes: &HashMap<String, f64>,
) -> (usize, usize, f64, f64) {
    let mut buy_count = 0usize;
    let mut sell_count = 0usize;
    let mut buy_notional = 0.0_f64;
    let mut sell_notional = 0.0_f64;
    for order in orders {
        let notional = closes
            .get(&order.symbol)
            .map(|price| *price * order.qty as f64)
            .filter(|n| n.is_finite() && *n > 0.0)
            .unwrap_or(0.0);
        match order.side {
            Side::Buy => {
                buy_count += 1;
                buy_notional += notional;
            }
            Side::Sell => {
                sell_count += 1;
                sell_notional += notional;
            }
        }
    }
    (buy_count, sell_count, buy_notional, sell_notional)
}

fn order_reason_fields(reason: &basket_engine::OrderReason) -> (&'static str, Option<&str>) {
    match reason {
        basket_engine::OrderReason::Entry { basket_id } => ("entry", Some(basket_id.as_str())),
        basket_engine::OrderReason::Flip { basket_id } => ("flip", Some(basket_id.as_str())),
        basket_engine::OrderReason::Rebalance => ("rebalance", None),
        basket_engine::OrderReason::Aggregated => ("aggregated", None),
    }
}

fn push_order_if_nonzero(orders: &mut Vec<OrderIntent>, symbol: &str, delta: f64) {
    let qty = delta.abs().round() as u32;
    if qty == 0 {
        return;
    }
    let side = if delta > 0.0 { Side::Buy } else { Side::Sell };
    orders.push(OrderIntent {
        symbol: symbol.to_string(),
        qty,
        side,
        reason: basket_engine::OrderReason::Aggregated,
    });
}

fn staged_diff_to_orders(
    current: &HashMap<String, f64>,
    target: &HashMap<String, f64>,
) -> Vec<OrderIntent> {
    let mut reducing = Vec::new();
    let mut expanding = Vec::new();

    let mut all_symbols: Vec<&String> = current.keys().chain(target.keys()).collect();
    all_symbols.sort();
    all_symbols.dedup();

    for symbol in all_symbols {
        let current_shares = current.get(symbol).copied().unwrap_or(0.0);
        let target_shares = target.get(symbol).copied().unwrap_or(0.0);
        let current_sign = current_shares.signum() as i8;
        let target_sign = target_shares.signum() as i8;

        if (current_shares - target_shares).abs() < 0.5 {
            continue;
        }

        if current_shares == 0.0 || target_shares == 0.0 || current_sign == target_sign {
            let delta = target_shares - current_shares;
            if delta == 0.0 {
                continue;
            }
            let reduce_first = if current_shares == 0.0 {
                false
            } else if target_shares == 0.0 {
                true
            } else {
                target_shares.abs() < current_shares.abs()
            };
            if reduce_first {
                push_order_if_nonzero(&mut reducing, symbol, delta);
            } else {
                push_order_if_nonzero(&mut expanding, symbol, delta);
            }
            continue;
        }

        // Sign flip: close the old side completely, then open the new side.
        push_order_if_nonzero(&mut reducing, symbol, -current_shares);
        push_order_if_nonzero(&mut expanding, symbol, target_shares);
    }

    reducing.extend(expanding);
    reducing
}

async fn wait_for_stream_health(
    bar_rx: &mut tokio::sync::mpsc::Receiver<stream::StreamBar>,
    timeout_secs: u64,
) -> Result<Option<stream::StreamBar>, String> {
    match tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), bar_rx.recv()).await {
        Ok(Some(bar)) => Ok(Some(bar)),
        Ok(None) => Err("stream closed before first startup bar arrived".to_string()),
        Err(_) => Err(format!(
            "stream health gate timed out after {}s without any live bar",
            timeout_secs
        )),
    }
}

fn classify_startup_phase(
    now: DateTime<Utc>,
    last_processed_trading_day: Option<NaiveDate>,
    close_grace_min: u32,
) -> StartupPhase {
    let today = market_session::trading_day_utc(now);
    if !market_session::is_trading_day(today) {
        StartupPhase::NonTradingDay
    } else if market_session::is_after_close_grace_utc(now, close_grace_min) {
        if last_processed_trading_day == Some(today) {
            StartupPhase::PostCloseProcessed
        } else {
            StartupPhase::PostClosePendingCatchup
        }
    } else if market_session::is_rth_utc(now) {
        StartupPhase::Intraday
    } else {
        StartupPhase::PreOpen
    }
}

/// Run the basket live/paper loop.
///
/// Returns on Ctrl+C or fatal error.
#[allow(clippy::too_many_arguments)]
pub async fn run_basket_live(
    broker: &impl Broker,
    bar_source: &impl BarSource,
    clock: &impl Clock,
    session_trigger: &mut impl SessionTrigger,
    universe_path: &Path,
    state_path: &Path,
    bars_dir: &Path,
    execution: BasketExecution,
    portfolio_config: PortfolioConfig,
    fits: &[BasketFit],
    options: BasketRunOptions,
) -> Result<(), String> {
    info!(
        universe = %universe_path.display(),
        state_path = %state_path.display(),
        bars_dir = %bars_dir.display(),
        execution = execution.label(),
        n_fits = fits.len(),
        "========== BASKET LIVE RUNNER =========="
    );
    portfolio_config.validate()?;

    if execution == BasketExecution::Live {
        warn!("LIVE MODE — real-money orders will be placed on every EOD signal");
    }
    if let Some(mode) = execution.alpaca_mode() {
        preflight_account_check(broker, mode).await?;
    }

    // 1. Load universe + frozen fit artifact.
    let universe = load_universe(universe_path)?;
    info!(
        baskets = universe.num_baskets(),
        sectors = universe.sectors.len(),
        "loaded universe"
    );

    let symbols = collect_symbols(&universe);
    let sector_members: HashMap<String, Vec<String>> = universe
        .sectors
        .iter()
        .map(|(name, sector)| (name.clone(), sector.members.clone()))
        .collect();
    info!(
        symbols = symbols.len(),
        fits = fits.len(),
        "loaded frozen basket fit artifact"
    );

    let valid_count = fits.iter().filter(|f| f.valid).count();
    info!(
        total = fits.len(),
        valid = valid_count,
        "loaded basket fits"
    );
    if valid_count == 0 {
        // Tally rejection reasons so the operator can see WHY all
        // baskets failed — vital when replay's auto-fit produces 0
        // valid fits and you don't know whether it's a data window
        // problem, a numerical fit problem, or a config problem.
        let mut reasons: std::collections::BTreeMap<String, usize> =
            std::collections::BTreeMap::new();
        for f in fits {
            let reason = f
                .reject_reason
                .clone()
                .unwrap_or_else(|| "unknown".to_string());
            *reasons.entry(reason).or_insert(0) += 1;
        }
        for (reason, count) in &reasons {
            error!(reason = %reason, count, "fit rejected");
        }
        return Err("no valid baskets in fit artifact".to_string());
    }

    let state_exists = state_path.exists();

    // 2. Seed current_shares from Alpaca positions (startup reconciliation).
    //    Without this, a restart with live open positions would trigger
    //    target-minus-zero share deltas, flooding Alpaca with duplicate orders.
    //    Noop skips this (no Alpaca account needed for shadow mode).
    //    Paper/Live FAIL CLOSED: if reconciliation cannot load open positions,
    //    we refuse to start. Trading from an empty share map would diff
    //    targets against zero and flood Alpaca with duplicate orders against
    //    already-open broker positions, potentially double-sizing every leg.
    let now = clock.now();
    let today = market_session::trading_day_utc(now);
    let mut current_shares = match execution.alpaca_mode() {
        None => {
            info!("noop mode — skipping startup position reconciliation");
            HashMap::new()
        }
        Some(mode) => seed_current_shares_from_alpaca(broker, mode, &symbols).await?,
    };
    let mut equity_history = VecDeque::new();
    push_equity_history(&mut equity_history, portfolio_config.capital);

    let (mut engine, mut last_processed_trading_day, startup_state_source) =
        initialize_engine_state(
            fits,
            state_path,
            &current_shares,
            execution.alpaca_mode().is_some(),
        )?;
    let leadership_state_path = leadership_classifier_state_path(state_path);
    let can_load_sidecar_state = matches!(startup_state_source, StartupStateSource::Snapshot);
    if !can_load_sidecar_state {
        move_sidecar_state_aside_if_present(
            &leadership_state_path,
            startup_state_source,
            "engine_state_not_loaded_from_snapshot",
        )?;
    }
    let mut leadership_tracker = options
        .leadership_overlay
        .clone()
        .map(|cfg| SectorLeadershipTracker::new(cfg, sector_members));
    if let Some(tracker) = leadership_tracker.as_mut() {
        match if can_load_sidecar_state {
            tracker.load_state(&leadership_state_path)
        } else {
            Ok(false)
        } {
            Ok(true) => info!(
                state_path = %leadership_state_path.display(),
                active_sectors = ?tracker.active_sectors_for_today(),
                "loaded persisted leadership overlay classifier state"
            ),
            Ok(false) => {
                if can_load_sidecar_state && leadership_state_path.exists() {
                    move_sidecar_state_aside_if_present(
                        &leadership_state_path,
                        startup_state_source,
                        "leadership_sidecar_state_mismatch",
                    )?;
                }
                let warm_anchor = if last_processed_trading_day == Some(today) {
                    Some(today)
                } else {
                    previous_trading_day(today)
                };
                if let Some(anchor_day) = warm_anchor {
                    warm_leadership_tracker(tracker, bars_dir, &symbols, anchor_day)?;
                }
                tracker.save_state(&leadership_state_path)?;
            }
            Err(e) => return Err(e),
        }
    }
    if let Some(cfg) = options.leadership_overlay.as_ref() {
        info!(
            sectors = ?cfg.sectors,
            on_ret5d_threshold = cfg.on_ret5d_threshold,
            on_breadth5d_threshold = cfg.on_breadth5d_threshold,
            off_ret5d_threshold = cfg.off_ret5d_threshold,
            off_breadth5d_threshold = cfg.off_breadth5d_threshold,
            persistence_days = cfg.persistence_days,
            min_hold_days = cfg.min_hold_days,
            configured_overlay_mode = match cfg.mode {
                BasketOverlayMode::Baseline => "baseline",
                BasketOverlayMode::SuppressShorts => "suppress_shorts",
                BasketOverlayMode::ReplaceWithLongOnly => "replace_with_long_only",
                BasketOverlayMode::AddCappedLongSleeve => "add_capped_long_sleeve",
            },
            configured_picker = match options.overlay_picker {
                BasketOverlayPickerKind::Fixed => "fixed",
                BasketOverlayPickerKind::RuleV1 => "rule_v1",
            },
            long_only_leverage = cfg.long_only_leverage,
            "leadership overlay configured; runtime mode may still be chosen by picker"
        );
    }
    let mut overlay_picker = BasketOverlayPickerHandle::from_kind(
        options.overlay_picker,
        options.leadership_overlay.as_ref().map(|cfg| cfg.mode),
        options.rule_v1_config.clone(),
    );
    let picker_state_path = overlay_picker_state_path(state_path);
    if !can_load_sidecar_state {
        move_sidecar_state_aside_if_present(
            &picker_state_path,
            startup_state_source,
            "engine_state_not_loaded_from_snapshot",
        )?;
    }
    match if can_load_sidecar_state {
        overlay_picker.load_state(&picker_state_path)
    } else {
        Ok(false)
    } {
        Ok(true) => info!(
            state_path = %picker_state_path.display(),
            picker_id = overlay_picker.id(),
            "loaded persisted basket overlay picker state"
        ),
        Ok(false) => {
            if can_load_sidecar_state && picker_state_path.exists() {
                move_sidecar_state_aside_if_present(
                    &picker_state_path,
                    startup_state_source,
                    "overlay_picker_sidecar_state_mismatch",
                )?;
            }
            overlay_picker.save_state(&picker_state_path)?;
        }
        Err(e) => return Err(e),
    }
    info!(
        picker_id = overlay_picker.id(),
        "basket overlay picker initialized"
    );

    let startup_phase = classify_startup_phase(now, last_processed_trading_day, CLOSE_GRACE_MIN);
    let journal = match options.journal_path.as_deref() {
        Some(path) => Some(BasketJournal::open(path)?),
        None => None,
    };
    let run_id = format!(
        "{}-{}",
        execution
            .label()
            .to_ascii_lowercase()
            .replace([' ', '(', ')'], "-"),
        now.timestamp_millis()
    );
    info!(
        now_utc = %now.to_rfc3339(),
        trading_day = %today,
        startup_phase = startup_phase.as_str(),
        startup_state_source = ?startup_state_source,
        state_exists,
        last_processed = ?last_processed_trading_day,
        broker_positions = current_shares.len(),
        "basket startup phase evaluated"
    );
    if let Some(mode) = execution.alpaca_mode() {
        let account = broker.get_account(mode).await?;
        let account_equity = parse_equity(&account)?;
        let effective_capital =
            effective_execution_capital(portfolio_config.capital, Some(account_equity));
        info!(
            configured_capital = %format!("{:.0}", portfolio_config.capital),
            account_equity = %format!("{:.0}", account_equity),
            effective_execution_capital = %format!("{:.0}", effective_capital),
            leverage = portfolio_config.leverage,
            gross_cap = %format!("{:.0}", effective_capital * portfolio_config.leverage),
            n_active_baskets = portfolio_config.n_active_baskets,
            "basket execution capital resolved"
        );
        if effective_capital < portfolio_config.capital {
            warn!(
                configured_capital = %format!("{:.0}", portfolio_config.capital),
                account_equity = %format!("{:.0}", account_equity),
                effective_execution_capital = %format!("{:.0}", effective_capital),
                "account equity is below configured basket capital; sizing will be capped to account equity"
            );
        }
    }
    if let Some(journal) = &journal {
        let universe_path_str = universe_path.display().to_string();
        let state_path_str = state_path.display().to_string();
        let fit_artifact_path_str = options
            .fit_artifact_path
            .as_ref()
            .map(|p| p.display().to_string());
        journal.record_run(&BasketRunRecord {
            run_id: run_id.as_str(),
            started_at_utc: now,
            execution_mode: execution.label(),
            universe_path: universe_path_str.as_str(),
            fit_artifact_path: fit_artifact_path_str.as_deref(),
            state_path: state_path_str.as_str(),
            startup_phase: startup_phase.as_str(),
            symbols: symbols.len(),
            baskets: engine.num_baskets(),
            capital: portfolio_config.capital,
            leverage: portfolio_config.leverage,
            n_active_baskets: portfolio_config.n_active_baskets,
            broker_positions: current_shares.len(),
            last_processed_trading_day,
        })?;
    }
    // 3. Bar loop: buffer per (symbol, date) → last RTH bar.
    //    Engine is triggered by a wall-clock timer (not by one symbol's final RTH bar
    //    arrival) so that no single symbol becoming a data source-of-failure can
    //    silently skip an entire session.
    let mut day_closes: HashMap<NaiveDate, HashMap<String, f64>> = HashMap::new();
    let mut processed_sessions: std::collections::HashSet<NaiveDate> = Default::default();
    if last_processed_trading_day == Some(today) {
        processed_sessions.insert(today);
    }

    if market_session::is_trading_day(today)
        && market_session::is_after_close_grace_utc(now, CLOSE_GRACE_MIN)
        && last_processed_trading_day != Some(today)
    {
        let catchup_closes = load_close_snapshot_for_day(bars_dir, &symbols, today)?;
        info!(
            date = %today,
            symbols = catchup_closes.len(),
            "startup is after close grace on an unprocessed trading day — running one catch-up close cycle"
        );
        process_session_close(
            &mut engine,
            broker,
            today,
            &catchup_closes,
            &portfolio_config,
            &mut current_shares,
            execution,
            journal.as_ref(),
            run_id.as_str(),
            leadership_picker_features(
                leadership_tracker.as_ref(),
                strategy_equity_features(&equity_history),
            ),
            &mut overlay_picker,
            options.leadership_overlay.as_ref(),
        )
        .await?;
        overlay_picker.save_state(&picker_state_path)?;
        if let Some(tracker) = leadership_tracker.as_mut() {
            tracker.observe_close_snapshot(&catchup_closes);
            tracker.save_state(&leadership_state_path)?;
        }
        // Hook for replay's daily-equity time series. Noop on AlpacaClient.
        broker.record_eod(today).await;
        if let Some(mode) = execution.alpaca_mode() {
            let equity = parse_equity(&broker.get_account(mode).await?)?;
            push_equity_history(&mut equity_history, equity);
        }
        last_processed_trading_day = Some(today);
        engine.save_state_with_day(state_path, last_processed_trading_day)?;
        processed_sessions.insert(today);
        info!(
            date = %today,
            state_path = %state_path.display(),
            last_processed = ?last_processed_trading_day,
            "catch-up close cycle completed and startup state persisted"
        );
    }

    // 4. Subscribe to all universe symbols via the bar source.
    let mut bar_rx = bar_source.start(&symbols).await;
    info!("subscribed to bar source for 1-min bars");

    if market_session::is_trading_day(today) && market_session::is_rth_utc(now) {
        match wait_for_stream_health(&mut bar_rx, 90).await {
            Ok(Some(startup_bar)) => {
                let bar_open_ts_ms = startup_bar.timestamp - 60_000;
                if let Some(dt) = DateTime::<Utc>::from_timestamp_millis(bar_open_ts_ms) {
                    if market_session::is_rth_utc(dt)
                        && startup_bar.close.is_finite()
                        && startup_bar.close > 0.0
                    {
                        let date = market_session::trading_day_utc(dt);
                        day_closes
                            .entry(date)
                            .or_default()
                            .insert(startup_bar.symbol.clone(), startup_bar.close);
                        info!(
                            symbol = startup_bar.symbol.as_str(),
                            date = %date,
                            close = startup_bar.close,
                            "startup stream health gate passed"
                        );
                    }
                }
            }
            Ok(None) => {}
            Err(e) => warn!(
                error = e.as_str(),
                "startup stream health gate did not observe a first bar; continuing"
            ),
        }
    }

    // Dedicated 60s heartbeat that summarizes buffer state. The session-close
    // schedule is owned by `session_trigger` (see `SessionTrigger` trait); the
    // heartbeat is observability only.
    let mut heartbeat = tokio::time::interval(std::time::Duration::from_secs(60));
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // Skip first fire so we don't heartbeat with zero data.
    heartbeat.tick().await;
    let ctrl_c = tokio::signal::ctrl_c();
    tokio::pin!(ctrl_c);

    let symbols_expected = symbols.len();
    let mut bars_processed_total: u64 = 0;
    let mut bars_processed_window: u64 = 0;
    let mut last_bar_rx_ts_ms: i64 = 0;

    loop {
        tokio::select! {
            Some(bar) = bar_rx.recv() => {
                // `stream.rs` shifts Alpaca bar timestamps by +60s (open→close time).
                // Undo that here so `minute` reflects bar-OPEN time, matching
                // `market_session::is_rth_utc` semantics. Without this, the last
                // RTH bar (e.g. open=19:59, stream=20:00 in DST) would be
                // excluded by `RTH_START_MIN..SESSION_CLOSE_MIN` and the close
                // would never enter the buffer — missing the daily close.
                let bar_open_ts_ms = bar.timestamp - 60_000;
                let dt = match DateTime::<Utc>::from_timestamp_millis(bar_open_ts_ms) {
                    Some(d) => d,
                    None => continue,
                };
                if !market_session::is_rth_utc(dt) {
                    // Outside RTH — ignore (do not contaminate daily close).
                    debug!(
                        symbol = bar.symbol.as_str(),
                        ts = bar.timestamp,
                        "bar outside RTH — discarded"
                    );
                    continue;
                }
                let date = market_session::trading_day_utc(dt);
                if bar.close.is_finite() && bar.close > 0.0 {
                    let entry = day_closes.entry(date).or_default();
                    entry.insert(bar.symbol.clone(), bar.close);
                    bars_processed_total += 1;
                    bars_processed_window += 1;
                    last_bar_rx_ts_ms = bar.timestamp;
                    debug!(
                        symbol = bar.symbol.as_str(),
                        close = bar.close,
                        date = %date,
                        buffer_size = entry.len(),
                        symbols_expected,
                        "buffered bar"
                    );
                }
            }
            _ = heartbeat.tick() => {
                // BAR_LOOP heartbeat — surfaces whether bars are making it
                // out of `stream.rs`'s channel into the basket buffer. If
                // this goes silent while `stream heartbeat` shows activity,
                // the channel is backed up or the RTH filter is rejecting
                // everything.
                let now = clock.now();
                let today = market_session::trading_day_utc(now);
                let in_rth = market_session::is_rth_utc(now);
                let past_close = market_session::is_after_close_grace_utc(now, CLOSE_GRACE_MIN);
                let buffered_today = day_closes.get(&today).map(|m| m.len()).unwrap_or(0);
                let last_bar_age_s = if last_bar_rx_ts_ms == 0 {
                    -1i64
                } else {
                    (now.timestamp_millis() - last_bar_rx_ts_ms) / 1000
                };
                info!(
                    bars_processed_total,
                    bars_processed_window,
                    buffered_today,
                    symbols_expected,
                    in_rth,
                    past_close,
                    processed_today = processed_sessions.contains(&today),
                    current_positions = current_shares.len(),
                    last_bar_age_s,
                    "BAR_LOOP heartbeat"
                );
                bars_processed_window = 0;
            }
            session_event = session_trigger.next() => {
                // Session-close trigger: the trigger has determined that
                // `today` is past session close + grace. Live uses
                // `IntervalSessionTrigger` (30s wall-clock poll); replay
                // uses `BarDrivenSessionTrigger` so cadence follows bar
                // timestamps. The trigger dedups internally, so `today`
                // is yielded at most once; `processed_sessions` is the
                // persisted-state dedup that catches restarts after a
                // session was already processed.
                //
                // `None` = trigger exhausted (replay drained its
                // parquet bars). Live's `IntervalSessionTrigger` never
                // returns `None`; this branch is the replay exit path.
                let today = match session_event {
                    Some(d) => d,
                    None => {
                        info!(
                            bars_processed_total,
                            sessions_processed = processed_sessions.len(),
                            "========== REPLAY EXHAUSTED — SHUTDOWN =========="
                        );
                        break;
                    }
                };
                // EVERY session_event MUST end with `ack_session_processed`
                // so the bar emitter (blocked on `done_rx.recv()` after
                // `session_tx.send(date)`) can resume. Skipping the ack
                // — even on dedup or non-trading days — hangs replay
                // forever (codex review on PR #322). The labeled block
                // gives every short-circuit path a single fall-through
                // point. The only paths that can skip the ack are the
                // `return Err(...)` failures below: those drop the
                // trigger, the emitter sees `None` from
                // `done_rx.recv()`, and the run unwinds cleanly.
                'session: {
                    if processed_sessions.contains(&today) {
                        // Resume case (state on disk had this date): the
                        // emitter still drives session_tx for D, so we
                        // must still ack.
                        info!(
                            date = %today,
                            "session-close event for already-processed date — acknowledging without rerun"
                        );
                        break 'session;
                    }
                    let closes_for_day = day_closes.remove(&today).unwrap_or_default();
                    if closes_for_day.is_empty() {
                        if !market_session::is_trading_day(today) {
                            info!(
                                date = %today,
                                "session close grace elapsed on non-trading day with zero buffered closes — marking processed"
                            );
                            processed_sessions.insert(today);
                            break 'session;
                        }
                        bug!(
                            "zero_buffered_closes_on_trading_day",
                            date = %today,
                            symbols_expected,
                            buffered_days = day_closes.len(),
                            current_positions = current_shares.len(),
                            "session close grace elapsed on trading day with zero buffered closes"
                        );
                        return Err(format!(
                            "session close grace elapsed on trading day {today} but no RTH closes were buffered"
                        ));
                    }
                    // Log exactly which symbols' closes we have and, crucially,
                    // which expected ones are missing. Yesterday we had no
                    // way to tell mid-incident whether this was a subscribe
                    // problem, a stream-drop problem, or a buffer problem.
                    let missing: Vec<&str> = symbols
                        .iter()
                        .filter(|s| !closes_for_day.contains_key(s.as_str()))
                        .map(|s| s.as_str())
                        .collect();
                    info!(
                        date = %today,
                        closes_in_buffer = closes_for_day.len(),
                        symbols_expected,
                        missing_count = missing.len(),
                        missing_sample = ?missing.iter().take(10).collect::<Vec<_>>(),
                        "session close firing"
                    );
                    if !missing.is_empty() {
                        bug!(
                            "incomplete_close_snapshot",
                            date = %today,
                            closes_in_buffer = closes_for_day.len(),
                            symbols_expected,
                            missing_count = missing.len(),
                            missing_sample = ?missing.iter().take(20).collect::<Vec<_>>(),
                            "incomplete close snapshot at session close"
                        );
                        return Err(format!(
                            "incomplete close snapshot for {today}: missing {} symbols",
                            missing.len()
                        ));
                    }
                    processed_sessions.insert(today);
                    process_session_close(
                        &mut engine,
                        broker,
                        today,
                        &closes_for_day,
                        &portfolio_config,
                        &mut current_shares,
                        execution,
                        journal.as_ref(),
                        run_id.as_str(),
                        leadership_picker_features(
                            leadership_tracker.as_ref(),
                            strategy_equity_features(&equity_history),
                        ),
                        &mut overlay_picker,
                        options.leadership_overlay.as_ref(),
                    )
                    .await?;
                    overlay_picker.save_state(&picker_state_path)?;
                    if let Some(tracker) = leadership_tracker.as_mut() {
                        tracker.observe_close_snapshot(&closes_for_day);
                        tracker.save_state(&leadership_state_path)?;
                    }
                    // Hook for replay's daily-equity time series.
                    // Noop on AlpacaClient.
                    broker.record_eod(today).await;
                    if let Some(mode) = execution.alpaca_mode() {
                        let equity = parse_equity(&broker.get_account(mode).await?)?;
                        push_equity_history(&mut equity_history, equity);
                    }
                    last_processed_trading_day = Some(today);
                    engine.save_state_with_day(state_path, last_processed_trading_day)?;
                    info!(
                        date = %today,
                        state_path = %state_path.display(),
                        last_processed = ?last_processed_trading_day,
                        "persisted basket engine state after session close"
                    );
                }
                // Replay's `BarDrivenSessionTrigger` uses this to
                // release the bar emitter, which has been blocked
                // from overwriting `SharedCloses` with the next day's
                // prices while we ran. Live's
                // `IntervalSessionTrigger` no-ops the ack.
                session_trigger.ack_session_processed().await;
            }
            _ = &mut ctrl_c => {
                info!(
                    bars_processed_total,
                    sessions_processed = processed_sessions.len(),
                    "========== SHUTDOWN =========="
                );
                break;
            }
        }
    }
    Ok(())
}

/// Fetch open positions from Alpaca and express them as signed shares per symbol.
/// Used on startup so `diff_to_orders` computes correct deltas from the engine's target.
///
/// Returns `Err` on any fetch failure; the caller must treat this as fatal for
/// Recover engine state from broker holdings when the state-snapshot file
/// is missing but Alpaca has open positions. For each basket whose target
/// symbol is held by the broker with non-zero qty, set the basket's
/// `state.position` to `+1` (long) or `-1` (short) based on the qty sign.
/// Baskets without held targets stay at the engine's current state
/// (typically flat) and get reconciled at the next session close via
/// normal delta math.
///
/// Returns the number of baskets that got reconciled.
///
/// Quantity threshold: |qty| < 0.5 share is treated as zero (handles
/// floating-point noise from Alpaca's positions endpoint).
fn reconcile_engine_state_from_broker(
    engine: &mut basket_engine::BasketEngine,
    broker_shares: &HashMap<String, f64>,
) -> Result<usize, String> {
    use basket_engine::BasketState;
    let mut new_states: HashMap<String, BasketState> = HashMap::new();
    for (basket_id, params) in engine.iter_params() {
        let target_qty = broker_shares.get(&params.target).copied().unwrap_or(0.0);
        let state = if target_qty.abs() < BROKER_QTY_EPSILON {
            BasketState::default()
        } else {
            BasketState {
                position: if target_qty > 0.0 { 1 } else { -1 },
                ..Default::default()
            }
        };
        new_states.insert(basket_id.clone(), state);
    }
    let count = new_states.values().filter(|s| s.position != 0).count();
    engine.apply_states(new_states)?;
    Ok(count)
}

fn initialize_engine_state(
    fits: &[BasketFit],
    state_path: &Path,
    broker_shares: &HashMap<String, f64>,
    broker_execution_enabled: bool,
) -> Result<(BasketEngine, Option<NaiveDate>, StartupStateSource), String> {
    let expected_ids: std::collections::HashSet<String> = fits
        .iter()
        .filter(|f| f.valid)
        .map(|f| f.candidate.id())
        .collect();
    let mut fresh = BasketEngine::new(fits);

    if !state_path.exists() {
        if broker_execution_enabled && !broker_shares.is_empty() {
            let reconciled = reconcile_engine_state_from_broker(&mut fresh, broker_shares)?;
            warn!(
                state_path = %state_path.display(),
                broker_positions = broker_shares.len(),
                reconciled_baskets = reconciled,
                "state file missing — reconciled engine state from broker positions"
            );
            return Ok((fresh, None, StartupStateSource::BrokerReconciled));
        }
        info!(baskets = fresh.num_baskets(), "basket engine initialized");
        return Ok((fresh, None, StartupStateSource::Fresh));
    }

    match BasketEngine::load_snapshot(state_path) {
        Ok(snapshot) => {
            let loaded_ids: std::collections::HashSet<String> =
                snapshot.states.keys().cloned().collect();
            if loaded_ids != expected_ids {
                return recover_from_unloadable_state(
                    fresh,
                    state_path,
                    broker_shares,
                    broker_execution_enabled,
                    format!(
                        "state snapshot basket set mismatch: snapshot={}, artifact={}",
                        loaded_ids.len(),
                        expected_ids.len()
                    ),
                );
            }
            let last_processed_trading_day = snapshot.last_processed_trading_day;
            fresh.apply_states(snapshot.states)?;
            info!(
                baskets = fresh.num_baskets(),
                state_path = %state_path.display(),
                last_processed = ?last_processed_trading_day,
                "loaded basket runtime state onto current fit artifact params"
            );
            Ok((
                fresh,
                last_processed_trading_day,
                StartupStateSource::Snapshot,
            ))
        }
        Err(e) => recover_from_unloadable_state(
            fresh,
            state_path,
            broker_shares,
            broker_execution_enabled,
            format!("failed to load state snapshot: {e}"),
        ),
    }
}

fn recover_from_unloadable_state(
    mut fresh: BasketEngine,
    state_path: &Path,
    broker_shares: &HashMap<String, f64>,
    broker_execution_enabled: bool,
    reason: String,
) -> Result<(BasketEngine, Option<NaiveDate>, StartupStateSource), String> {
    let backup_path = move_state_file_aside(state_path)?;
    bug!(
        "engine_state_snapshot_unusable",
        state_path = %state_path.display(),
        backup_path = %backup_path.display(),
        reason = %reason,
        "state snapshot unusable — moved aside for recovery"
    );

    if broker_execution_enabled && !broker_shares.is_empty() {
        let reconciled = reconcile_engine_state_from_broker(&mut fresh, broker_shares)?;
        warn!(
            broker_positions = broker_shares.len(),
            reconciled_baskets = reconciled,
            "recovered engine state from broker positions after unloading state snapshot"
        );
        Ok((fresh, None, StartupStateSource::BrokerReconciled))
    } else {
        info!(
            baskets = fresh.num_baskets(),
            "state unavailable but broker flat — starting fresh"
        );
        Ok((fresh, None, StartupStateSource::Fresh))
    }
}

fn move_state_file_aside(path: &Path) -> Result<PathBuf, String> {
    let ts = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
    let mut backup_name: OsString = path
        .file_name()
        .map(|s| s.to_os_string())
        .unwrap_or_else(|| OsString::from("basket.state.json"));
    backup_name.push(format!(".unusable.{ts}"));
    let backup_path = path.with_file_name(backup_name);
    std::fs::rename(path, &backup_path).map_err(|e| {
        format!(
            "failed to move unusable state snapshot {} aside to {}: {e}",
            path.display(),
            backup_path.display()
        )
    })?;
    Ok(backup_path)
}

fn move_sidecar_state_aside_if_present(
    path: &Path,
    startup_source: StartupStateSource,
    reason: &str,
) -> Result<(), String> {
    if !path.exists() {
        return Ok(());
    }
    let ts = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
    let mut backup_name: OsString = path
        .file_name()
        .map(|s| s.to_os_string())
        .unwrap_or_else(|| OsString::from("basket.sidecar.json"));
    backup_name.push(format!(".ignored.{ts}"));
    let backup_path = path.with_file_name(backup_name);
    std::fs::rename(path, &backup_path).map_err(|e| {
        format!(
            "failed to move stale sidecar state {} aside to {}: {e}",
            path.display(),
            backup_path.display()
        )
    })?;
    bug!(
        "stale_sidecar_state_ignored",
        state_path = %path.display(),
        backup_path = %backup_path.display(),
        startup_state_source = ?startup_source,
        reason,
        "sidecar state ignored because engine state did not load from a matching snapshot"
    );
    Ok(())
}

/// paper/live execution (trading from an empty share map would double-size
/// every already-open leg on the first session).
async fn seed_current_shares_from_alpaca(
    broker: &impl Broker,
    mode: ExecutionMode,
    allowed_symbols: &[String],
) -> Result<HashMap<String, f64>, String> {
    let positions = broker.get_positions(mode).await.map_err(|e| {
        format!(
            "startup position reconciliation failed — refusing to trade without a \
             trusted share inventory (fetch error: {e})"
        )
    })?;
    let allowed: std::collections::HashSet<&str> =
        allowed_symbols.iter().map(|s| s.as_str()).collect();
    let mut ignored_symbols = Vec::new();
    let shares: HashMap<String, f64> = positions
        .into_iter()
        .filter_map(|(sym, (qty, _avg_entry))| {
            if allowed.contains(sym.as_str()) {
                Some((sym, qty))
            } else {
                ignored_symbols.push(sym);
                None
            }
        })
        .collect();
    if !ignored_symbols.is_empty() {
        ignored_symbols.sort();
        warn!(
            ignored_positions = ignored_symbols.len(),
            ignored_sample = ?ignored_symbols.iter().take(10).collect::<Vec<_>>(),
            "ignoring non-basket broker positions during startup reconciliation"
        );
    }
    info!(
        n_positions = shares.len(),
        "seeded current_shares from Alpaca open positions"
    );
    Ok(shares)
}

fn load_close_snapshot_for_day(
    bars_dir: &Path,
    symbols: &[String],
    day: NaiveDate,
) -> Result<HashMap<String, f64>, String> {
    let closes = load_daily_closes_with_timestamps(bars_dir, symbols, 10, Some(day))?;
    let mut snapshot = HashMap::new();
    let mut missing = Vec::new();
    let expected_last_bar_ts_us =
        (market_session::close_timestamp_utc_for_day(day) - 60_000) * 1_000;
    for symbol in symbols {
        match closes.get(symbol).and_then(|series| {
            series.iter().find_map(|(d, ts_us, c)| {
                if *d == day && *ts_us == expected_last_bar_ts_us {
                    Some(*c)
                } else {
                    None
                }
            })
        }) {
            Some(close) if close.is_finite() && close > 0.0 => {
                snapshot.insert(symbol.clone(), close);
            }
            _ => missing.push(symbol.clone()),
        }
    }
    if missing.is_empty() {
        info!(
            date = %day,
            symbols = snapshot.len(),
            expected_last_bar_ts_us,
            "loaded finalized close snapshot for trading day"
        );
        Ok(snapshot)
    } else {
        missing.sort();
        Err(format!(
            "close snapshot incomplete for {}: missing {} symbols (sample: {})",
            day,
            missing.len(),
            missing.into_iter().take(10).collect::<Vec<_>>().join(", ")
        ))
    }
}

/// Run the engine for one session close and dispatch orders.
#[allow(clippy::too_many_arguments)]
async fn process_session_close(
    engine: &mut BasketEngine,
    broker: &impl Broker,
    date: NaiveDate,
    closes: &HashMap<String, f64>,
    portfolio_config: &PortfolioConfig,
    current_shares: &mut HashMap<String, f64>,
    execution: BasketExecution,
    journal: Option<&BasketJournal>,
    run_id: &str,
    picker_features: BasketOverlayPickerFeatures,
    overlay_picker: &mut impl BasketOverlayPicker,
    leadership_overlay: Option<&LeadershipOverlayConfig>,
) -> Result<(), String> {
    debug_assert!(
        portfolio_config.validate().is_ok(),
        "process_session_close received invalid PortfolioConfig"
    );
    if closes.is_empty() {
        warn!(date = %date, "no RTH closes buffered for session — skipping engine");
        return Ok(());
    }

    // Build DailyBar slice for BasketEngine.
    let daily_bars: Vec<DailyBar> = closes
        .iter()
        .map(|(symbol, &close)| DailyBar {
            symbol: symbol.clone(),
            date,
            close,
        })
        .collect();

    let intents = engine.on_bars(&daily_bars);
    info!(
        date = %date,
        symbols = closes.len(),
        intents = intents.len(),
        "session close processed"
    );

    for intent in &intents {
        log_intent(intent);
    }

    let allowed_symbols: Vec<String> = closes.keys().cloned().collect();
    if let Some(mode) = execution.alpaca_mode() {
        *current_shares = seed_current_shares_from_alpaca(broker, mode, &allowed_symbols).await?;
        info!(
            date = %date,
            current_positions = current_shares.len(),
            "refreshed broker share inventory before computing basket order deltas"
        );
    }
    let current_shares_before = current_shares.clone();
    let execution_account_equity = match execution.alpaca_mode() {
        Some(mode) => Some(parse_equity(&broker.get_account(mode).await?)?),
        None => None,
    };
    let mut effective_portfolio_config = portfolio_config.clone();
    effective_portfolio_config.capital =
        effective_execution_capital(portfolio_config.capital, execution_account_equity);

    // Portfolio layer: apply active-basket admission first, then convert
    // admitted target notionals to target shares. Suppression is planned on
    // a clone so the core basket engine state remains intact.
    let baseline_plan = plan_portfolio(engine, &effective_portfolio_config);
    let baseline_features = add_baseline_plan_features(
        picker_features,
        &baseline_plan.symbol_notionals,
        &effective_portfolio_config,
        leadership_overlay,
    );
    let picker_decision = overlay_picker.decide(&baseline_features);
    let leadership_active_sectors = &picker_decision.active_sectors;
    let leadership_long_symbols = &picker_decision.long_symbols;
    if leadership_overlay.is_none() && picker_decision.mode != BasketOverlayMode::Baseline {
        bug!(
            "overlay_mode_without_config",
            date = %date,
            picker_id = overlay_picker.id(),
            picker_mode = picker_decision.mode.as_str(),
            picker_reason = picker_decision.reason,
            "overlay picker selected a non-baseline mode without leadership overlay config"
        );
        return Err(format!(
            "overlay picker selected {} without leadership overlay config",
            picker_decision.mode.as_str()
        ));
    }
    info!(
        date = %date,
        picker_id = overlay_picker.id(),
        picker_mode = picker_decision.mode.as_str(),
        picker_reason = picker_decision.reason,
        leadership_active_sectors = ?leadership_active_sectors,
        leadership_symbols_active = leadership_long_symbols.len(),
        leadership_short_conflict_ratio = %format!("{:.4}", baseline_features.leadership_short_conflict_ratio),
        strategy_return_20d = %format!("{:.4}", baseline_features.strategy_return_20d),
        strategy_drawdown_20d = %format!("{:.4}", baseline_features.strategy_drawdown_20d),
        baseline_scale_if_sleeve = %format!("{:.4}", baseline_features.baseline_scale_if_sleeve),
        sleeve_leverage_scale = %format!("{:.4}", picker_decision.sleeve_leverage_scale),
        "basket overlay picker decision"
    );
    if let Some(journal) = journal {
        let mut active_sectors: Vec<String> = leadership_active_sectors.iter().cloned().collect();
        active_sectors.sort();
        journal.record_picker_decision(&BasketPickerDecisionRecord {
            run_id,
            trading_day: date,
            picker_id: overlay_picker.id(),
            mode: picker_decision.mode.as_str(),
            reason: picker_decision.reason,
            active_sectors_json: serialize_string_vec(&active_sectors),
            active_symbols_json: serialize_string_vec(leadership_long_symbols),
            leadership_short_conflict_ratio: baseline_features.leadership_short_conflict_ratio,
            strategy_return_20d: baseline_features.strategy_return_20d,
            strategy_drawdown_20d: baseline_features.strategy_drawdown_20d,
            baseline_scale_if_sleeve: baseline_features.baseline_scale_if_sleeve,
            sleeve_leverage_scale: picker_decision.sleeve_leverage_scale,
        })?;
    }

    let suppressed_baskets = if matches!(picker_decision.mode, BasketOverlayMode::SuppressShorts) {
        leadership_short_suppression_baskets(engine, leadership_active_sectors)
    } else {
        Vec::new()
    };
    let plan = if suppressed_baskets.is_empty() {
        baseline_plan
    } else {
        let mut planning_engine = engine.clone();
        planning_engine.flatten_baskets(&suppressed_baskets);
        info!(
            date = %date,
            leadership_active_sectors = ?leadership_active_sectors,
            suppressed_baskets = ?suppressed_baskets,
            "leadership short suppression removed flagged shorts from target plan"
        );
        plan_portfolio(&planning_engine, &effective_portfolio_config)
    };
    let using_long_replacement =
        matches!(picker_decision.mode, BasketOverlayMode::ReplaceWithLongOnly)
            && !leadership_long_symbols.is_empty();
    let using_capped_long_sleeve =
        matches!(picker_decision.mode, BasketOverlayMode::AddCappedLongSleeve)
            && !leadership_long_symbols.is_empty();
    let baseline_target_notionals = plan.symbol_notionals.clone();
    let target_notionals = if using_long_replacement {
        leadership_long_only_notionals(
            closes,
            leadership_long_symbols,
            effective_portfolio_config.capital,
            leadership_overlay
                .map(|cfg| cfg.long_only_leverage)
                .unwrap_or(1.0),
        )
    } else if using_capped_long_sleeve {
        let sleeve_leverage = leadership_overlay
            .map(|cfg| cfg.long_only_leverage * picker_decision.sleeve_leverage_scale)
            .unwrap_or(0.0);
        let baseline_gross = baseline_target_notionals
            .values()
            .map(|notional| notional.abs())
            .sum::<f64>();
        let sleeve_budget = leadership_overlay
            .map(|_| sleeve_leverage * effective_portfolio_config.capital)
            .unwrap_or(0.0)
            .min(effective_portfolio_config.capital * effective_portfolio_config.leverage);
        let baseline_budget = (effective_portfolio_config.capital
            * effective_portfolio_config.leverage
            - sleeve_budget)
            .max(0.0);
        let baseline_scale = if baseline_gross > 0.0 {
            (baseline_budget / baseline_gross).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let scaled_baseline = scale_notionals(&baseline_target_notionals, baseline_scale);
        let sleeve_notionals = leadership_long_only_notionals(
            closes,
            leadership_long_symbols,
            effective_portfolio_config.capital,
            sleeve_leverage,
        );
        merge_notionals(&scaled_baseline, &sleeve_notionals)
    } else {
        plan.symbol_notionals.clone()
    };
    if using_long_replacement || using_capped_long_sleeve {
        let (baseline_gross_long, baseline_gross_short, baseline_max_abs, _) =
            summarize_notionals(&baseline_target_notionals);
        let baseline_gross_notional = baseline_gross_long + baseline_gross_short.abs();
        info!(
            date = %date,
            leadership_active_sectors = ?leadership_active_sectors,
            overlay_symbol_count = leadership_long_symbols.len(),
            overlay_symbols_sample = ?leadership_long_symbols.iter().take(8).collect::<Vec<_>>(),
            execution_effective_capital = %format!("{:.0}", effective_portfolio_config.capital),
            execution_account_equity = execution_account_equity.map(|equity| format!("{equity:.0}")),
            overlay_mode = if using_long_replacement {
                "replace_with_long_only"
            } else {
                "add_capped_long_sleeve"
            },
            baseline_selected_baskets = plan.selected_baskets.len(),
            baseline_selected_baskets_sample = ?plan.selected_baskets.iter().take(8).collect::<Vec<_>>(),
            baseline_targets = baseline_target_notionals.len(),
            baseline_gross_long = %format!("{:.0}", baseline_gross_long),
            baseline_gross_short = %format!("{:.0}", baseline_gross_short),
            baseline_gross_notional = %format!("{:.0}", baseline_gross_notional),
            baseline_max_abs_leg = %format!("{:.0}", baseline_max_abs),
            baseline_top_abs_legs = ?top_abs_notional_legs(&baseline_target_notionals, 6),
            overlay_top_abs_legs = ?top_abs_notional_legs(&target_notionals, 6),
            "leadership overlay transformed baseline basket portfolio"
        );
    }
    let engine_flatten_baskets =
        engine_flatten_baskets_for_plan(&plan, &suppressed_baskets, using_long_replacement);
    if !engine_flatten_baskets.is_empty() {
        info!(
            date = %date,
            picker_mode = picker_decision.mode.as_str(),
            flattened_baskets = ?engine_flatten_baskets,
            "flattening engine state to match executed basket plan"
        );
        engine.flatten_baskets(&engine_flatten_baskets);
    }
    let target_shares = target_shares_from_notionals(&target_notionals, closes)?;
    let executable_target_notionals = notionals_from_target_shares(&target_shares, closes);

    // Summary of the notional plan before we diff — this is where yesterday's
    // $340K-on-$100K problem was invisible. Emit gross long, gross short,
    // net, absolute max leg, and median leg so we can spot sizing anomalies
    // without shelling into sqlite. `gross_long + gross_short` = gross
    // notional = leverage × equity (should be ≤ equity × leverage_assumed
    // from the universe TOML).
    let (gross_long, gross_short, max_abs, sorted_abs) =
        summarize_notionals(&executable_target_notionals);
    let median_abs = if sorted_abs.is_empty() {
        0.0
    } else {
        sorted_abs[sorted_abs.len() / 2]
    };
    let gross_notional = gross_long + gross_short.abs();
    let gross_cap = effective_portfolio_config.capital * effective_portfolio_config.leverage;
    let selected_baskets_json = serialize_string_vec(&plan.selected_baskets);
    let excluded_baskets_json = serialize_string_vec(&plan.excluded_baskets);
    let target_shares_json = serialize_shares_map(&target_shares);
    let target_positions_len = target_shares.len();
    info!(
        date = %date,
        leadership_mode = match picker_decision.mode {
            BasketOverlayMode::Baseline if leadership_overlay.is_none() => "disabled",
            BasketOverlayMode::Baseline => "baseline",
            BasketOverlayMode::SuppressShorts if !leadership_active_sectors.is_empty() => "suppress_shorts",
            BasketOverlayMode::SuppressShorts => "suppress_shorts_inactive",
            BasketOverlayMode::ReplaceWithLongOnly if using_long_replacement => "replace_with_long_only",
            BasketOverlayMode::ReplaceWithLongOnly => "replace_with_long_only_inactive",
            BasketOverlayMode::AddCappedLongSleeve if using_capped_long_sleeve => "add_capped_long_sleeve",
            BasketOverlayMode::AddCappedLongSleeve => "add_capped_long_sleeve_inactive",
        },
        leadership_sectors_active = ?leadership_active_sectors,
        leadership_symbols_active = leadership_long_symbols.len(),
        targets = executable_target_notionals.len(),
        target_positions = target_shares.len(),
        current_positions = current_shares.len(),
        gross_long = %format!("{:.0}", gross_long),
        gross_short = %format!("{:.0}", gross_short),
        gross_notional = %format!("{:.0}", gross_notional),
        gross_cap = %format!("{:.0}", gross_cap),
        net_notional = %format!("{:.0}", gross_long + gross_short),
        max_abs_leg = %format!("{:.0}", max_abs),
        median_abs_leg = %format!("{:.0}", median_abs),
        top_abs_legs = ?top_abs_notional_legs(&executable_target_notionals, 6),
        "target notionals summary"
    );
    if gross_notional > gross_cap + 1.0 {
        bug!(
            "target_gross_exceeds_cap",
            date = %date,
            gross_notional = %format!("{:.2}", gross_notional),
            gross_cap = %format!("{:.2}", gross_cap),
            "target gross notional exceeds configured cap"
        );
        return Err(format!(
            "target gross notional {:.2} exceeds configured cap {:.2}",
            gross_notional, gross_cap
        ));
    }
    if !plan.excluded_baskets.is_empty() {
        warn!(
            date = %date,
            active_baskets = plan.active_baskets,
            cap = effective_portfolio_config.n_active_baskets,
            admitted = plan.selected_baskets.len(),
            excluded = plan.excluded_baskets.len(),
            excluded_sample = ?plan.excluded_baskets.iter().take(10).collect::<Vec<_>>(),
            "active-basket cap excluded baskets from the baseline target portfolio"
        );
    } else {
        info!(
            date = %date,
            active_baskets = plan.active_baskets,
            cap = effective_portfolio_config.n_active_baskets,
            admitted = plan.selected_baskets.len(),
            "portfolio admission completed without exclusions"
        );
    }

    let orders = staged_diff_to_orders(current_shares, &target_shares);
    if orders.is_empty() {
        info!(date = %date, "no orders to emit — targets already match current");
        if let Some(journal) = journal {
            journal.record_session_close(&BasketSessionCloseRecord {
                run_id,
                trading_day: date,
                status: "aligned",
                closes_received: closes.len(),
                symbols_expected: closes.len(),
                active_baskets: plan.active_baskets,
                admitted_baskets: plan.selected_baskets.len(),
                excluded_baskets: plan.excluded_baskets.len(),
                gross_long,
                gross_short,
                gross_notional,
                gross_cap,
                net_notional: gross_long + gross_short,
                max_abs_leg: max_abs,
                median_abs_leg: median_abs,
                target_positions: target_positions_len,
                current_positions_before: current_shares_before.len(),
                current_positions_after: current_shares.len(),
                buy_orders: 0,
                sell_orders: 0,
                buy_notional: 0.0,
                sell_notional: 0.0,
                order_gross_notional: 0.0,
                order_max_notional: 0.0,
                accepted_orders: 0,
                failed_orders: 0,
                target_gross: Some(gross_notional),
                actual_gross: None,
                divergence_pct: None,
                selected_baskets_json: selected_baskets_json.clone(),
                excluded_baskets_json: excluded_baskets_json.clone(),
                current_shares_before_json: serialize_shares_map(&current_shares_before),
                target_shares_json: target_shares_json.clone(),
                current_shares_after_json: Some(serialize_shares_map(current_shares)),
                error_text: None,
            })?;
        }
        return Ok(());
    }

    // Distribution of order notionals — flags the "one leg $30K, rest $200"
    // case that we saw yesterday. Computed cheaply from prices + qtys.
    let order_notionals: Vec<f64> = orders
        .iter()
        .filter_map(|o| {
            closes
                .get(&o.symbol)
                .map(|p| p * o.qty as f64)
                .filter(|n| n.is_finite() && *n > 0.0)
        })
        .collect();
    let order_gross: f64 = order_notionals.iter().sum();
    let order_max = order_notionals.iter().cloned().fold(0.0_f64, f64::max);
    let (buy_orders, sell_orders, buy_notional, sell_notional) =
        summarize_orders_by_side(&orders, closes);
    info!(
        date = %date,
        n_orders = orders.len(),
        buy_orders,
        sell_orders,
        buy_notional = %format!("{:.0}", buy_notional),
        sell_notional = %format!("{:.0}", sell_notional),
        order_gross_notional = %format!("{:.0}", order_gross),
        order_max_notional = %format!("{:.0}", order_max),
        "emitting orders"
    );

    match execution.alpaca_mode() {
        None => {
            // Noop — log only, then advance the simulated share state directly
            // to the target so shadow mode stays deterministic across sessions.
            for (seq, order) in orders.iter().enumerate() {
                log_order(order, "NOOP");
                if let Some(journal) = journal {
                    let (reason, basket_id) = order_reason_fields(&order.reason);
                    journal.record_order_event(&BasketOrderEvent {
                        run_id,
                        trading_day: date,
                        seq,
                        symbol: order.symbol.as_str(),
                        side: match order.side {
                            Side::Buy => "buy",
                            Side::Sell => "sell",
                        },
                        requested_qty: order.qty as f64,
                        intended_notional: closes
                            .get(&order.symbol)
                            .map(|price| *price * order.qty as f64),
                        reason,
                        basket_id,
                        broker_order_id: None,
                        broker_status: None,
                        submission_status: "noop",
                        error_text: None,
                    })?;
                }
            }
            *current_shares = target_shares.clone();
            if let Some(journal) = journal {
                journal.record_session_close(&BasketSessionCloseRecord {
                    run_id,
                    trading_day: date,
                    status: "noop",
                    closes_received: closes.len(),
                    symbols_expected: closes.len(),
                    active_baskets: plan.active_baskets,
                    admitted_baskets: plan.selected_baskets.len(),
                    excluded_baskets: plan.excluded_baskets.len(),
                    gross_long,
                    gross_short,
                    gross_notional,
                    gross_cap,
                    net_notional: gross_long + gross_short,
                    max_abs_leg: max_abs,
                    median_abs_leg: median_abs,
                    target_positions: target_positions_len,
                    current_positions_before: current_shares_before.len(),
                    current_positions_after: current_shares.len(),
                    buy_orders,
                    sell_orders,
                    buy_notional,
                    sell_notional,
                    order_gross_notional: order_gross,
                    order_max_notional: order_max,
                    accepted_orders: orders.len(),
                    failed_orders: 0,
                    target_gross: Some(gross_notional),
                    actual_gross: Some(gross_notional),
                    divergence_pct: Some(0.0),
                    selected_baskets_json: selected_baskets_json.clone(),
                    excluded_baskets_json: excluded_baskets_json.clone(),
                    current_shares_before_json: serialize_shares_map(&current_shares_before),
                    target_shares_json: target_shares_json.clone(),
                    current_shares_after_json: Some(serialize_shares_map(current_shares)),
                    error_text: None,
                })?;
            }
        }
        Some(mode) => {
            if let Err(e) = check_order_set_affordability(
                broker,
                mode,
                date,
                current_shares,
                &target_shares,
                &orders,
                closes,
            )
            .await
            {
                if let Some(journal) = journal {
                    journal.record_session_close(&BasketSessionCloseRecord {
                        run_id,
                        trading_day: date,
                        status: "affordability_error",
                        closes_received: closes.len(),
                        symbols_expected: closes.len(),
                        active_baskets: plan.active_baskets,
                        admitted_baskets: plan.selected_baskets.len(),
                        excluded_baskets: plan.excluded_baskets.len(),
                        gross_long,
                        gross_short,
                        gross_notional,
                        gross_cap,
                        net_notional: gross_long + gross_short,
                        max_abs_leg: max_abs,
                        median_abs_leg: median_abs,
                        target_positions: target_positions_len,
                        current_positions_before: current_shares_before.len(),
                        current_positions_after: current_shares.len(),
                        buy_orders,
                        sell_orders,
                        buy_notional,
                        sell_notional,
                        order_gross_notional: order_gross,
                        order_max_notional: order_max,
                        accepted_orders: 0,
                        failed_orders: orders.len(),
                        target_gross: Some(gross_notional),
                        actual_gross: None,
                        divergence_pct: None,
                        selected_baskets_json: selected_baskets_json.clone(),
                        excluded_baskets_json: excluded_baskets_json.clone(),
                        current_shares_before_json: serialize_shares_map(&current_shares_before),
                        target_shares_json: target_shares_json.clone(),
                        current_shares_after_json: Some(serialize_shares_map(current_shares)),
                        error_text: Some(e.clone()),
                    })?;
                }
                return Err(e);
            }
            let mut accepted_orders = 0usize;
            let mut failed_orders = 0usize;
            for (seq, order) in orders.iter().enumerate() {
                log_order(order, execution.label());
                let side_str = match order.side {
                    Side::Buy => "buy",
                    Side::Sell => "sell",
                };
                let (reason, basket_id) = order_reason_fields(&order.reason);
                match broker
                    .place_order(&order.symbol, order.qty as f64, side_str, mode)
                    .await
                {
                    Ok(o) => {
                        info!(
                            symbol = order.symbol.as_str(),
                            qty = order.qty,
                            side = side_str,
                            reason,
                            basket_id,
                            order_id = o.id.as_str(),
                            status = o.status.as_str(),
                            "ORDER PLACED"
                        );
                        accepted_orders += 1;
                        if let Some(journal) = journal {
                            journal.record_order_event(&BasketOrderEvent {
                                run_id,
                                trading_day: date,
                                seq,
                                symbol: order.symbol.as_str(),
                                side: side_str,
                                requested_qty: order.qty as f64,
                                intended_notional: closes
                                    .get(&order.symbol)
                                    .map(|price| *price * order.qty as f64),
                                reason,
                                basket_id,
                                broker_order_id: Some(o.id.as_str()),
                                broker_status: Some(o.status.as_str()),
                                submission_status: "accepted",
                                error_text: None,
                            })?;
                        }
                    }
                    Err(e) => {
                        failed_orders += 1;
                        bug!(
                            "broker_order_failed",
                            symbol = order.symbol.as_str(),
                            qty = order.qty,
                            side = side_str,
                            reason,
                            basket_id,
                            error = e.as_str(),
                            "ORDER FAILED"
                        );
                        if let Some(journal) = journal {
                            journal.record_order_event(&BasketOrderEvent {
                                run_id,
                                trading_day: date,
                                seq,
                                symbol: order.symbol.as_str(),
                                side: side_str,
                                requested_qty: order.qty as f64,
                                intended_notional: closes
                                    .get(&order.symbol)
                                    .map(|price| *price * order.qty as f64),
                                reason,
                                basket_id,
                                broker_order_id: None,
                                broker_status: None,
                                submission_status: "failed",
                                error_text: Some(e.as_str()),
                            })?;
                        }
                    }
                }
            }
            info!(
                date = %date,
                accepted_orders,
                failed_orders,
                "submitted basket orders without mutating in-memory share inventory; next session refresh will reconcile actual fills"
            );

            // Post-submission broker reconciliation: after letting fills settle,
            // refetch positions and compare actual gross to target. Catches silent
            // portfolio drift from partial fills / rejections (the failure mode
            // that turned yesterday's $100K config into a $341K lopsided book).
            if accepted_orders + failed_orders > 0 {
                let reconciliation_delay_secs = broker.reconciliation_delay_secs();
                if reconciliation_delay_secs > 0 {
                    tokio::time::sleep(std::time::Duration::from_secs(reconciliation_delay_secs))
                        .await;
                }
                let mut current_shares_after_json = None;
                let mut current_positions_after = current_shares.len();
                let mut actual_gross = None;
                let mut divergence_pct = None;
                match seed_current_shares_from_alpaca(broker, mode, &allowed_symbols).await {
                    Ok(actual_shares) => {
                        let actual_gross_value: f64 = actual_shares
                            .iter()
                            .filter_map(|(sym, qty)| closes.get(sym).map(|p| (qty * p).abs()))
                            .sum();
                        let target_gross = gross_notional;
                        let divergence_pct_value = if target_gross > 0.0 {
                            ((actual_gross_value - target_gross).abs() / target_gross) * 100.0
                        } else {
                            0.0
                        };
                        actual_gross = Some(actual_gross_value);
                        divergence_pct = Some(divergence_pct_value);
                        current_positions_after = actual_shares.len();
                        current_shares_after_json = Some(serialize_shares_map(&actual_shares));
                        if divergence_pct_value > 10.0 {
                            bug!(
                                "post_submit_gross_divergence",
                                date = %date,
                                target_gross = %format!("{:.0}", target_gross),
                                actual_gross = %format!("{:.0}", actual_gross_value),
                                divergence_pct = %format!("{:.1}", divergence_pct_value),
                                accepted_orders,
                                failed_orders,
                                broker_positions = actual_shares.len(),
                                "BROKER DIVERGENCE: actual gross differs from target by >10%"
                            );
                        } else {
                            info!(
                                date = %date,
                                target_gross = %format!("{:.0}", target_gross),
                                actual_gross = %format!("{:.0}", actual_gross_value),
                                divergence_pct = %format!("{:.1}", divergence_pct_value),
                                broker_positions = actual_shares.len(),
                                "post-submission reconciliation OK"
                            );
                        }
                    }
                    Err(e) => {
                        bug!(
                            "post_submit_reconciliation_failed",
                            date = %date,
                            error = e.as_str(),
                            "post-submission reconciliation failed — could not refetch broker positions"
                        );
                    }
                }
                if let Some(journal) = journal {
                    journal.record_session_close(&BasketSessionCloseRecord {
                        run_id,
                        trading_day: date,
                        status: if failed_orders == 0 {
                            "submitted"
                        } else {
                            "partial_failure"
                        },
                        closes_received: closes.len(),
                        symbols_expected: closes.len(),
                        active_baskets: plan.active_baskets,
                        admitted_baskets: plan.selected_baskets.len(),
                        excluded_baskets: plan.excluded_baskets.len(),
                        gross_long,
                        gross_short,
                        gross_notional,
                        gross_cap,
                        net_notional: gross_long + gross_short,
                        max_abs_leg: max_abs,
                        median_abs_leg: median_abs,
                        target_positions: target_positions_len,
                        current_positions_before: current_shares_before.len(),
                        current_positions_after,
                        buy_orders,
                        sell_orders,
                        buy_notional,
                        sell_notional,
                        order_gross_notional: order_gross,
                        order_max_notional: order_max,
                        accepted_orders,
                        failed_orders,
                        target_gross: Some(gross_notional),
                        actual_gross,
                        divergence_pct,
                        selected_baskets_json: selected_baskets_json.clone(),
                        excluded_baskets_json: excluded_baskets_json.clone(),
                        current_shares_before_json: serialize_shares_map(&current_shares_before),
                        target_shares_json: target_shares_json.clone(),
                        current_shares_after_json,
                        error_text: None,
                    })?;
                }
            }
        }
    }
    Ok(())
}

fn target_shares_from_notionals(
    target_notionals: &HashMap<String, f64>,
    closes: &HashMap<String, f64>,
) -> Result<HashMap<String, f64>, String> {
    let mut target_shares = HashMap::new();
    let mut missing_prices = Vec::new();
    for (symbol, notional) in target_notionals {
        let price = match closes.get(symbol) {
            Some(price) if price.is_finite() && *price > 0.0 => *price,
            _ => {
                missing_prices.push(symbol.clone());
                continue;
            }
        };
        let raw_shares = notional / price;
        let shares = if raw_shares > 0.0 {
            raw_shares.floor()
        } else {
            raw_shares.ceil()
        };
        if shares.abs() >= 1.0 {
            target_shares.insert(symbol.clone(), shares);
        }
    }
    if missing_prices.is_empty() {
        Ok(target_shares)
    } else {
        missing_prices.sort();
        Err(format!(
            "missing close prices for target-share conversion: {}",
            missing_prices.join(", ")
        ))
    }
}

fn notionals_from_target_shares(
    target_shares: &HashMap<String, f64>,
    closes: &HashMap<String, f64>,
) -> HashMap<String, f64> {
    target_shares
        .iter()
        .filter_map(|(symbol, shares)| {
            closes.get(symbol).and_then(|price| {
                let notional = shares * price;
                if notional.is_finite() && notional.abs() > f64::EPSILON {
                    Some((symbol.clone(), notional))
                } else {
                    None
                }
            })
        })
        .collect()
}

/// Summarize a `target_notionals` map into (gross_long, gross_short, max_abs,
/// sorted_abs). `gross_short` is returned as a negative number.
fn summarize_notionals(targets: &HashMap<String, f64>) -> (f64, f64, f64, Vec<f64>) {
    let mut gross_long = 0.0_f64;
    let mut gross_short = 0.0_f64;
    let mut max_abs = 0.0_f64;
    let mut abs: Vec<f64> = Vec::with_capacity(targets.len());
    for &n in targets.values() {
        if !n.is_finite() {
            continue;
        }
        if n > 0.0 {
            gross_long += n;
        } else {
            gross_short += n;
        }
        let a = n.abs();
        abs.push(a);
        if a > max_abs {
            max_abs = a;
        }
    }
    abs.sort_by(|x, y| x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal));
    (gross_long, gross_short, max_abs, abs)
}

fn log_intent(intent: &PositionIntent) {
    info!(
        basket_id = %intent.basket_id,
        target_position = intent.target_position,
        z = %format!("{:.4}", intent.z_score),
        spread = %format!("{:.6}", intent.spread),
        reason = intent.reason.as_str(),
        date = %intent.date,
        "BASKET_INTENT"
    );
}

fn log_order(order: &OrderIntent, label: &str) {
    let side_str = match order.side {
        Side::Buy => "buy",
        Side::Sell => "sell",
    };
    let (reason, basket_id) = order_reason_fields(&order.reason);
    info!(
        mode = label,
        symbol = order.symbol.as_str(),
        qty = order.qty,
        side = side_str,
        reason,
        basket_id,
        "BASKET_ORDER"
    );
}

pub(crate) fn collect_symbols(universe: &basket_picker::Universe) -> Vec<String> {
    let mut symbols: Vec<String> = universe
        .sectors
        .values()
        .flat_map(|s| s.members.iter().cloned())
        .collect();
    symbols.sort();
    symbols.dedup();
    symbols
}

/// Read the last `window_days` trading days of daily closes for each symbol.
/// Aggregates 1-min parquets to RTH-last-bar closes (same rule as replay).
pub(crate) fn load_warmup_closes(
    bars_dir: &Path,
    symbols: &[String],
    window_days: i64,
) -> Result<HashMap<String, Vec<(NaiveDate, f64)>>, String> {
    let today = Utc::now().date_naive();
    load_daily_closes(bars_dir, symbols, window_days, today.pred_opt())
}

/// Same as [`load_warmup_closes`] but with an explicit "as-of" cutoff.
///
/// Used by the replay path to build a fit using data **strictly before**
/// the replay window, so the fit can't peek at the data it's about to
/// trade against.
pub(crate) fn load_warmup_closes_as_of(
    bars_dir: &Path,
    symbols: &[String],
    window_days: i64,
    as_of: NaiveDate,
) -> Result<HashMap<String, Vec<(NaiveDate, f64)>>, String> {
    load_daily_closes(bars_dir, symbols, window_days, as_of.pred_opt())
}

fn load_daily_closes(
    bars_dir: &Path,
    symbols: &[String],
    window_days: i64,
    max_day_inclusive: Option<NaiveDate>,
) -> Result<HashMap<String, Vec<(NaiveDate, f64)>>, String> {
    let closes =
        load_daily_closes_with_timestamps(bars_dir, symbols, window_days, max_day_inclusive)?;
    Ok(closes
        .into_iter()
        .map(|(symbol, series)| {
            (
                symbol,
                series
                    .into_iter()
                    .map(|(date, _ts_us, close)| (date, close))
                    .collect(),
            )
        })
        .collect())
}

#[allow(clippy::type_complexity)]
fn load_daily_closes_with_timestamps(
    bars_dir: &Path,
    symbols: &[String],
    window_days: i64,
    max_day_inclusive: Option<NaiveDate>,
) -> Result<HashMap<String, Vec<(NaiveDate, i64, f64)>>, String> {
    use arrow::array::{Array, Float64Array, TimestampMicrosecondArray};
    use std::collections::BTreeMap;
    // The window anchor is the most recent date the caller wants to
    // include — `max_day_inclusive` if provided (replay's "as-of"
    // cutoff), or "today" otherwise (live warm-up). The lower bound
    // is `anchor - window_days`. Anchoring on `Utc::now()` here would
    // make `as_of`-based callers fail silently when their requested
    // window doesn't overlap "now − window_days" (#306 finding).
    let anchor = max_day_inclusive.unwrap_or_else(|| Utc::now().date_naive());
    let cutoff = anchor - chrono::Duration::days(window_days);

    let mut out = HashMap::new();
    for symbol in symbols {
        let path = bars_dir.join(format!("{symbol}.parquet"));
        let file = match std::fs::File::open(&path) {
            Ok(f) => f,
            Err(e) => {
                warn!(symbol = %symbol, error = %e, "skip symbol — parquet missing");
                continue;
            }
        };
        let builder = ParquetRecordBatchReaderBuilder::try_new(file)
            .map_err(|e| format!("reader {symbol}: {e}"))?;
        let reader = builder
            .build()
            .map_err(|e| format!("build {symbol}: {e}"))?;

        let mut daily: BTreeMap<NaiveDate, (i64, f64)> = BTreeMap::new();
        for batch in reader {
            let batch = batch.map_err(|e| format!("batch {symbol}: {e}"))?;
            let ts = batch
                .column(0)
                .as_any()
                .downcast_ref::<TimestampMicrosecondArray>()
                .ok_or_else(|| format!("ts cast {symbol}"))?;
            let close = batch
                .column(4)
                .as_any()
                .downcast_ref::<Float64Array>()
                .ok_or_else(|| format!("close cast {symbol}"))?;

            for i in 0..batch.num_rows() {
                let ts_us = ts.value(i);
                let secs = ts_us / 1_000_000;
                let dt = match DateTime::<Utc>::from_timestamp(secs, 0) {
                    Some(d) => d.naive_utc(),
                    None => continue,
                };
                let dt_utc = dt.and_utc();
                if !market_session::is_rth_utc(dt_utc) {
                    continue;
                }
                let px = close.value(i);
                if !px.is_finite() || px <= 0.0 {
                    continue;
                }
                let date = market_session::trading_day_utc(dt_utc);
                if date < cutoff {
                    continue;
                }
                if let Some(max_day) = max_day_inclusive {
                    if date > max_day {
                        continue;
                    }
                }
                daily
                    .entry(date)
                    .and_modify(|(prev_ts, prev_close)| {
                        if ts_us > *prev_ts {
                            *prev_ts = ts_us;
                            *prev_close = px;
                        }
                    })
                    .or_insert((ts_us, px));
            }
        }
        let series: Vec<(NaiveDate, i64, f64)> = daily
            .into_iter()
            .map(|(d, (ts_us, c))| (d, ts_us, c))
            .collect();
        if !series.is_empty() {
            out.insert(symbol.clone(), series);
        }
    }
    Ok(out)
}

/// Align the date index for ONE basket (`target` + its peer `members`).
///
/// Produces the `HashMap<symbol, Vec<f64>>` shape that `basket_picker::validate`
/// requires, intersecting dates across ONLY this basket's symbols. Missing
/// symbols are passed through unaligned (length 0 after intersection with
/// nothing), so the validator emits a precise "missing symbol" rejection.
pub(crate) fn align_basket_history(
    closes: &HashMap<String, Vec<(NaiveDate, f64)>>,
    symbols: &[&str],
) -> HashMap<String, Vec<f64>> {
    let mut series_by_symbol: Vec<(&str, &Vec<(NaiveDate, f64)>)> =
        Vec::with_capacity(symbols.len());
    for s in symbols {
        if let Some(v) = closes.get(*s) {
            series_by_symbol.push((*s, v));
        }
    }
    if series_by_symbol.is_empty() {
        return HashMap::new();
    }

    // Intersection of dates across ONLY this basket's symbols.
    let mut common: std::collections::BTreeSet<NaiveDate> =
        series_by_symbol[0].1.iter().map(|(d, _)| *d).collect();
    for (_, series) in &series_by_symbol[1..] {
        let s: std::collections::BTreeSet<NaiveDate> = series.iter().map(|(d, _)| *d).collect();
        common = common.intersection(&s).cloned().collect();
    }

    let mut out = HashMap::new();
    for (symbol, series) in &series_by_symbol {
        let map: HashMap<NaiveDate, f64> = series.iter().copied().collect();
        let aligned: Vec<f64> = common.iter().filter_map(|d| map.get(d).copied()).collect();
        out.insert((*symbol).to_string(), aligned);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::alpaca::AlpacaAccount;
    use basket_picker::{BasketCandidate, BasketFit, BertramResult, OuFit};
    use chrono::{TimeZone, Timelike};
    use tempfile::tempdir;

    fn make_test_fit(target: &str, peers: &[&str], sector: &str) -> BasketFit {
        let candidate = BasketCandidate {
            target: target.to_string(),
            members: peers.iter().map(|s| s.to_string()).collect(),
            sector: sector.to_string(),
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
            adf_statistic: None,
            adf_pvalue: None,
            dominance_score: None,
            valid: true,
            reject_reason: None,
        }
    }

    fn make_test_fits() -> Vec<BasketFit> {
        vec![
            make_test_fit("AMD", &["NVDA", "INTC"], "chips"),
            make_test_fit("AAPL", &["AMZN", "GOOGL"], "faang"),
        ]
    }

    #[test]
    fn test_basket_execution_alpaca_mode_mapping() {
        assert!(BasketExecution::Noop.alpaca_mode().is_none());
        assert_eq!(
            BasketExecution::Paper.alpaca_mode(),
            Some(ExecutionMode::Paper)
        );
        assert_eq!(
            BasketExecution::Live.alpaca_mode(),
            Some(ExecutionMode::Live)
        );
    }

    #[test]
    fn test_sidecar_state_is_moved_aside_when_engine_state_is_not_snapshot() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("basket.state.json.picker.json");
        std::fs::write(&path, "{}").unwrap();

        move_sidecar_state_aside_if_present(&path, StartupStateSource::Fresh, "test").unwrap();

        assert!(!path.exists());
        let backups: Vec<_> = std::fs::read_dir(tmp.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(backups.len(), 1);
        assert!(backups[0].starts_with("basket.state.json.picker.json.ignored."));
    }

    /// Verifies the bar-timestamp unshift needed in the live bar loop.
    /// stream.rs adds +60s (open→close); we subtract it to get bar-open
    /// time for RTH filtering, so the 19:59-open / 20:00-close bar is
    /// correctly classified as RTH rather than being filtered out.
    #[test]
    fn test_bar_timestamp_unshift_keeps_last_rth_bar() {
        // Alpaca bar open-time 19:59 UTC = 71940 minutes from epoch day start.
        // stream.rs adds 60_000 ms → stream timestamp = 20:00 UTC.
        let base = DateTime::<Utc>::from_timestamp(0, 0).unwrap();
        let _ = base; // sanity: construction works
                      // Build a millis value for some 2026-02-06 19:59 UTC, shift +60s.
        let open = chrono::NaiveDate::from_ymd_opt(2026, 2, 6)
            .unwrap()
            .and_hms_opt(19, 59, 0)
            .unwrap()
            .and_utc();
        let stream_ts_ms = open.timestamp_millis() + 60_000;
        // Replicate the unshift used in the bar loop.
        let bar_open_ts_ms = stream_ts_ms - 60_000;
        let dt = DateTime::<Utc>::from_timestamp_millis(bar_open_ts_ms).unwrap();
        let minute = dt.hour() * 60 + dt.minute();
        assert_eq!(minute, 19 * 60 + 59, "unshift must recover bar-open minute");
        assert!(
            market_session::is_rth_utc(dt),
            "last RTH bar (19:59 open) must pass RTH filter after unshift"
        );
    }

    #[test]
    fn test_align_basket_history_intersects_only_basket_symbols() {
        let mut closes = HashMap::new();
        closes.insert(
            "A".to_string(),
            vec![
                (NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(), 10.0),
                (NaiveDate::from_ymd_opt(2026, 1, 2).unwrap(), 11.0),
                (NaiveDate::from_ymd_opt(2026, 1, 3).unwrap(), 12.0),
            ],
        );
        closes.insert(
            "B".to_string(),
            vec![
                // Missing 2026-01-01
                (NaiveDate::from_ymd_opt(2026, 1, 2).unwrap(), 20.0),
                (NaiveDate::from_ymd_opt(2026, 1, 3).unwrap(), 21.0),
            ],
        );
        let aligned = align_basket_history(&closes, &["A", "B"]);
        // Intersection is [2026-01-02, 2026-01-03] — each series has 2 entries.
        assert_eq!(aligned.get("A").unwrap().len(), 2);
        assert_eq!(aligned.get("B").unwrap().len(), 2);
        assert_eq!(aligned.get("A").unwrap()[0], 11.0);
        assert_eq!(aligned.get("B").unwrap()[0], 20.0);
    }

    #[test]
    fn test_align_basket_history_ignores_unrelated_sparse_symbols() {
        // Basket X/Y both have full 3-day history; unrelated sparse C has
        // only 1 day. A universe-wide intersection would shrink X/Y to that
        // 1 day. Per-basket alignment must keep X/Y at 3.
        let mut closes = HashMap::new();
        closes.insert(
            "X".to_string(),
            vec![
                (NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(), 10.0),
                (NaiveDate::from_ymd_opt(2026, 1, 2).unwrap(), 11.0),
                (NaiveDate::from_ymd_opt(2026, 1, 3).unwrap(), 12.0),
            ],
        );
        closes.insert(
            "Y".to_string(),
            vec![
                (NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(), 20.0),
                (NaiveDate::from_ymd_opt(2026, 1, 2).unwrap(), 21.0),
                (NaiveDate::from_ymd_opt(2026, 1, 3).unwrap(), 22.0),
            ],
        );
        closes.insert(
            "C_SPARSE".to_string(),
            vec![(NaiveDate::from_ymd_opt(2026, 1, 3).unwrap(), 99.0)],
        );
        let aligned = align_basket_history(&closes, &["X", "Y"]);
        assert_eq!(aligned.get("X").unwrap().len(), 3);
        assert_eq!(aligned.get("Y").unwrap().len(), 3);
        assert!(
            !aligned.contains_key("C_SPARSE"),
            "symbols outside the basket must not appear in the aligned map"
        );
    }

    #[test]
    fn test_target_shares_from_notionals_truncates_toward_zero() {
        let mut notionals = HashMap::new();
        notionals.insert("AMD".to_string(), 5050.0);
        notionals.insert("NVDA".to_string(), -2501.0);

        let mut closes = HashMap::new();
        closes.insert("AMD".to_string(), 101.0);
        closes.insert("NVDA".to_string(), 200.0);

        let shares = target_shares_from_notionals(&notionals, &closes).unwrap();
        assert_eq!(shares.get("AMD").copied(), Some(50.0));
        assert_eq!(shares.get("NVDA").copied(), Some(-12.0));
    }

    #[test]
    fn test_notionals_from_target_shares_uses_executable_share_book() {
        let shares = HashMap::from([("AAA".to_string(), 2.0), ("BBB".to_string(), -3.0)]);
        let closes = HashMap::from([("AAA".to_string(), 100.0), ("BBB".to_string(), 50.0)]);

        let notionals = notionals_from_target_shares(&shares, &closes);

        assert_eq!(notionals.get("AAA").copied(), Some(200.0));
        assert_eq!(notionals.get("BBB").copied(), Some(-150.0));
    }

    #[test]
    fn test_leadership_short_suppression_selects_only_flagged_shorts_without_mutation() {
        let fits = make_test_fits();
        let mut engine = BasketEngine::new(&fits);
        let states = HashMap::from([
            (
                fits[0].candidate.id(),
                basket_engine::BasketState {
                    position: -1,
                    ..Default::default()
                },
            ),
            (
                fits[1].candidate.id(),
                basket_engine::BasketState {
                    position: 1,
                    ..Default::default()
                },
            ),
        ]);
        engine.apply_states(states).unwrap();
        let suppressed =
            leadership_short_suppression_baskets(&engine, &HashSet::from(["chips".to_string()]));
        assert_eq!(suppressed.len(), 1);
        assert_eq!(basket_sector(&suppressed[0]), "chips");
        assert_eq!(
            engine.get_state(&fits[0].candidate.id()).unwrap().position,
            -1
        );
        assert_eq!(
            engine.get_state(&fits[1].candidate.id()).unwrap().position,
            1
        );
    }

    #[test]
    fn test_baseline_scale_if_sleeve_respects_gross_budget() {
        let portfolio_config = PortfolioConfig {
            capital: 10_000.0,
            leverage: 4.0,
            n_active_baskets: 5,
        };
        let overlay = LeadershipOverlayConfig {
            sectors: vec!["chips".to_string()],
            on_ret5d_threshold: 0.02,
            on_breadth5d_threshold: 0.56,
            off_ret5d_threshold: 0.0,
            off_breadth5d_threshold: 0.5,
            persistence_days: 2,
            min_hold_days: 3,
            mode: BasketOverlayMode::Baseline,
            long_only_leverage: 1.0,
        };
        let baseline_notionals = HashMap::from([
            ("NVDA".to_string(), -10_000.0),
            ("AAPL".to_string(), 10_000.0),
            ("UNH".to_string(), 10_000.0),
            ("CNC".to_string(), -10_000.0),
        ]);

        let scale =
            baseline_scale_if_sleeve(&baseline_notionals, &portfolio_config, Some(&overlay));

        assert!((scale - 0.75).abs() < 1e-9);
    }

    #[test]
    fn test_engine_flatten_baskets_for_plan_includes_suppressed_and_replanned_exclusions() {
        let plan = PortfolioPlan {
            symbol_notionals: HashMap::new(),
            selected_baskets: vec!["admitted".to_string()],
            excluded_baskets: vec!["cap_excluded".to_string(), "suppressed".to_string()],
            active_baskets: 3,
        };

        let flattened = engine_flatten_baskets_for_plan(
            &plan,
            &["suppressed".to_string(), "other_suppressed".to_string()],
            false,
        );

        assert_eq!(
            flattened,
            vec![
                "cap_excluded".to_string(),
                "other_suppressed".to_string(),
                "suppressed".to_string(),
            ]
        );
    }

    #[test]
    fn test_engine_flatten_baskets_for_plan_skips_flattening_for_long_replacement() {
        let plan = PortfolioPlan {
            symbol_notionals: HashMap::new(),
            selected_baskets: vec!["admitted".to_string()],
            excluded_baskets: vec!["cap_excluded".to_string()],
            active_baskets: 2,
        };

        let flattened = engine_flatten_baskets_for_plan(&plan, &["suppressed".to_string()], true);

        assert!(flattened.is_empty());
    }

    #[test]
    fn test_add_baseline_plan_features_measures_leadership_short_conflict() {
        let portfolio_config = PortfolioConfig {
            capital: 10_000.0,
            leverage: 4.0,
            n_active_baskets: 5,
        };
        let overlay = LeadershipOverlayConfig {
            sectors: vec!["chips".to_string()],
            on_ret5d_threshold: 0.02,
            on_breadth5d_threshold: 0.56,
            off_ret5d_threshold: 0.0,
            off_breadth5d_threshold: 0.5,
            persistence_days: 2,
            min_hold_days: 3,
            mode: BasketOverlayMode::Baseline,
            long_only_leverage: 1.0,
        };
        let features = BasketOverlayPickerFeatures {
            active_sectors: HashSet::from(["chips".to_string()]),
            long_symbols: vec!["NVDA".to_string(), "AAPL".to_string()],
            strategy_return_20d: 0.04,
            strategy_drawdown_20d: 0.01,
            ..Default::default()
        };
        let baseline_notionals = HashMap::from([
            ("NVDA".to_string(), -10_000.0),
            ("AAPL".to_string(), 10_000.0),
            ("UNH".to_string(), 10_000.0),
            ("CNC".to_string(), -10_000.0),
        ]);

        let features = add_baseline_plan_features(
            features,
            &baseline_notionals,
            &portfolio_config,
            Some(&overlay),
        );

        assert!((features.leadership_short_conflict_ratio - 0.25).abs() < 1e-9);
        assert!((features.baseline_scale_if_sleeve - 0.75).abs() < 1e-9);
    }

    #[test]
    fn test_sector_leadership_tracker_activates_lagged_sector_flag() {
        let config = LeadershipOverlayConfig {
            sectors: vec!["chips".to_string()],
            on_ret5d_threshold: 0.02,
            on_breadth5d_threshold: 0.55,
            off_ret5d_threshold: 0.0,
            off_breadth5d_threshold: 0.5,
            persistence_days: 1,
            min_hold_days: 2,
            mode: BasketOverlayMode::SuppressShorts,
            long_only_leverage: 1.0,
        };
        let mut tracker = SectorLeadershipTracker::new(
            config,
            HashMap::from([(
                "chips".to_string(),
                vec!["AMD".to_string(), "NVDA".to_string(), "INTC".to_string()],
            )]),
        );
        let days = [
            [100.0, 100.0, 100.0],
            [101.0, 101.0, 100.0],
            [102.0, 102.0, 101.0],
            [103.0, 103.0, 102.0],
            [104.0, 104.0, 103.0],
            [105.0, 105.0, 104.0],
        ];
        for day in days {
            tracker.observe_close_snapshot(&HashMap::from([
                ("AMD".to_string(), day[0]),
                ("NVDA".to_string(), day[1]),
                ("INTC".to_string(), day[2]),
            ]));
        }
        assert!(tracker.active_sectors_for_today().contains("chips"));
    }

    #[test]
    fn test_sector_leadership_tracker_hysteresis_and_min_hold() {
        let config = LeadershipOverlayConfig {
            sectors: vec!["chips".to_string()],
            on_ret5d_threshold: 0.02,
            on_breadth5d_threshold: 0.55,
            off_ret5d_threshold: 0.0,
            off_breadth5d_threshold: 0.5,
            persistence_days: 1,
            min_hold_days: 2,
            mode: BasketOverlayMode::SuppressShorts,
            long_only_leverage: 1.0,
        };
        let mut tracker = SectorLeadershipTracker::new(
            config,
            HashMap::from([(
                "chips".to_string(),
                vec!["AMD".to_string(), "NVDA".to_string()],
            )]),
        );

        for price in [100.0, 101.0, 102.0, 103.0, 104.0, 105.0] {
            tracker.observe_close_snapshot(&HashMap::from([
                ("AMD".to_string(), price),
                ("NVDA".to_string(), price),
            ]));
        }
        assert!(tracker.active_sectors_for_today().contains("chips"));

        for price in [99.0, 98.0] {
            tracker.observe_close_snapshot(&HashMap::from([
                ("AMD".to_string(), price),
                ("NVDA".to_string(), price),
            ]));
            assert!(
                tracker.active_sectors_for_today().contains("chips"),
                "minimum hold should prevent immediate off switch"
            );
        }

        tracker.observe_close_snapshot(&HashMap::from([
            ("AMD".to_string(), 97.0),
            ("NVDA".to_string(), 97.0),
        ]));
        assert!(!tracker.active_sectors_for_today().contains("chips"));
    }

    #[test]
    fn test_sector_leadership_tracker_state_roundtrip_preserves_decision() {
        let config = LeadershipOverlayConfig {
            sectors: vec!["chips".to_string()],
            on_ret5d_threshold: 0.02,
            on_breadth5d_threshold: 0.55,
            off_ret5d_threshold: 0.0,
            off_breadth5d_threshold: 0.5,
            persistence_days: 1,
            min_hold_days: 3,
            mode: BasketOverlayMode::SuppressShorts,
            long_only_leverage: 1.0,
        };
        let sector_members = HashMap::from([(
            "chips".to_string(),
            vec!["AMD".to_string(), "NVDA".to_string()],
        )]);
        let mut tracker = SectorLeadershipTracker::new(config.clone(), sector_members.clone());
        for price in [100.0, 101.0, 102.0, 103.0, 104.0, 105.0] {
            tracker.observe_close_snapshot(&HashMap::from([
                ("AMD".to_string(), price),
                ("NVDA".to_string(), price),
            ]));
        }
        assert!(tracker.active_sectors_for_today().contains("chips"));

        let tmp = tempdir().unwrap();
        let path = tmp.path().join("classifier.json");
        tracker.save_state(&path).unwrap();

        let mut loaded = SectorLeadershipTracker::new(config, sector_members);
        assert!(loaded.load_state(&path).unwrap());
        assert_eq!(
            loaded.active_sectors_for_today(),
            tracker.active_sectors_for_today()
        );
    }

    #[test]
    fn test_sector_leadership_tracker_rejects_mismatched_state_config() {
        let config = LeadershipOverlayConfig {
            sectors: vec!["chips".to_string()],
            on_ret5d_threshold: 0.02,
            on_breadth5d_threshold: 0.55,
            off_ret5d_threshold: 0.0,
            off_breadth5d_threshold: 0.5,
            persistence_days: 1,
            min_hold_days: 3,
            mode: BasketOverlayMode::SuppressShorts,
            long_only_leverage: 1.0,
        };
        let sector_members = HashMap::from([(
            "chips".to_string(),
            vec!["AMD".to_string(), "NVDA".to_string()],
        )]);
        let mut tracker = SectorLeadershipTracker::new(config.clone(), sector_members.clone());
        for price in [100.0, 101.0, 102.0, 103.0, 104.0, 105.0] {
            tracker.observe_close_snapshot(&HashMap::from([
                ("AMD".to_string(), price),
                ("NVDA".to_string(), price),
            ]));
        }

        let tmp = tempdir().unwrap();
        let path = tmp.path().join("classifier.json");
        tracker.save_state(&path).unwrap();

        let changed_config = LeadershipOverlayConfig {
            on_ret5d_threshold: 0.03,
            ..config
        };
        let mut loaded = SectorLeadershipTracker::new(changed_config, sector_members);
        assert!(!loaded.load_state(&path).unwrap());
        assert!(
            loaded.active_sectors_for_today().is_empty(),
            "mismatched classifier state must not carry old active sectors"
        );
    }

    #[test]
    fn test_leadership_long_only_notionals_equal_weight_selected_symbols() {
        let closes = HashMap::from([
            ("AAPL".to_string(), 200.0),
            ("NVDA".to_string(), 100.0),
            ("XOM".to_string(), 50.0),
        ]);
        let symbols = vec!["AAPL".to_string(), "NVDA".to_string()];
        let notionals = leadership_long_only_notionals(&closes, &symbols, 10_000.0, 2.0);
        assert_eq!(notionals.len(), 2);
        assert_eq!(notionals.get("AAPL").copied(), Some(10_000.0));
        assert_eq!(notionals.get("NVDA").copied(), Some(10_000.0));
        assert!(!notionals.contains_key("XOM"));
    }

    #[test]
    fn test_target_shares_from_notionals_fails_closed_on_missing_price() {
        let mut notionals = HashMap::new();
        notionals.insert("AMD".to_string(), 5000.0);
        notionals.insert("NVDA".to_string(), 2500.0);

        let mut closes = HashMap::new();
        closes.insert("AMD".to_string(), 100.0);

        let err = target_shares_from_notionals(&notionals, &closes).unwrap_err();
        assert!(err.contains("NVDA"));
    }

    #[test]
    fn test_classify_startup_phase_distinguishes_post_close_catchup() {
        let dt = Utc.with_ymd_and_hms(2026, 4, 22, 20, 5, 0).unwrap();
        let today = market_session::trading_day_utc(dt);

        assert_eq!(
            classify_startup_phase(dt, None, 2),
            StartupPhase::PostClosePendingCatchup
        );
        assert_eq!(
            classify_startup_phase(dt, Some(today), 2),
            StartupPhase::PostCloseProcessed
        );
    }

    #[test]
    fn test_reconcile_engine_state_from_broker_builds_complete_state_map() {
        let fits = make_test_fits();
        let mut engine = BasketEngine::new(&fits);
        let broker_shares = HashMap::from([
            ("AMD".to_string(), 15.0),
            ("NVDA".to_string(), -7.0),
            ("AMZN".to_string(), 3.0),
        ]);

        let reconciled = reconcile_engine_state_from_broker(&mut engine, &broker_shares).unwrap();
        assert_eq!(reconciled, 1);

        let amd_id = fits[0].candidate.id();
        let aapl_id = fits[1].candidate.id();
        assert_eq!(engine.get_state(&amd_id).unwrap().position, 1);
        assert_eq!(engine.get_state(&aapl_id).unwrap().position, 0);
    }

    #[test]
    fn test_initialize_engine_state_recovers_from_corrupt_snapshot_using_broker() {
        let fits = make_test_fits();
        let tmp = tempdir().unwrap();
        let state_path = tmp.path().join("basket.state.json");
        std::fs::write(&state_path, "{ definitely not json").unwrap();

        let broker_shares = HashMap::from([("AMD".to_string(), -12.0)]);
        let (engine, last_processed, source) =
            initialize_engine_state(&fits, &state_path, &broker_shares, true).unwrap();

        assert_eq!(source, StartupStateSource::BrokerReconciled);
        assert_eq!(last_processed, None);
        assert_eq!(
            engine.get_state(&fits[0].candidate.id()).unwrap().position,
            -1
        );
        assert_eq!(
            engine.get_state(&fits[1].candidate.id()).unwrap().position,
            0
        );
        assert!(!state_path.exists(), "corrupt state should be moved aside");
        let backups: Vec<_> = std::fs::read_dir(tmp.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .filter(|name| name.contains(".unusable."))
            .collect();
        assert_eq!(backups.len(), 1);
    }

    #[test]
    fn test_initialize_engine_state_recovers_from_mismatched_snapshot_when_broker_flat() {
        let fits = make_test_fits();
        let tmp = tempdir().unwrap();
        let state_path = tmp.path().join("basket.state.json");

        let wrong_fit = make_test_fit("MSFT", &["ORCL", "CRM"], "entsw");
        let mut wrong_engine = BasketEngine::new(&[wrong_fit]);
        let wrong_basket_id = wrong_engine.iter_params().next().unwrap().0.clone();
        let mut states = HashMap::new();
        states.insert(
            wrong_basket_id,
            basket_engine::BasketState {
                position: 1,
                ..Default::default()
            },
        );
        wrong_engine.apply_states(states).unwrap();
        wrong_engine.save_state(&state_path).unwrap();

        let (engine, last_processed, source) =
            initialize_engine_state(&fits, &state_path, &HashMap::new(), true).unwrap();

        assert_eq!(source, StartupStateSource::Fresh);
        assert_eq!(last_processed, None);
        assert_eq!(
            engine.get_state(&fits[0].candidate.id()).unwrap().position,
            0
        );
        assert_eq!(
            engine.get_state(&fits[1].candidate.id()).unwrap().position,
            0
        );
        assert!(
            !state_path.exists(),
            "mismatched state should be moved aside"
        );
    }

    #[test]
    fn test_summarize_orders_by_side_reports_counts_and_notionals() {
        let orders = vec![
            OrderIntent {
                symbol: "AMD".to_string(),
                qty: 10,
                side: Side::Buy,
                reason: basket_engine::OrderReason::Entry {
                    basket_id: "test".to_string(),
                },
            },
            OrderIntent {
                symbol: "NVDA".to_string(),
                qty: 5,
                side: Side::Sell,
                reason: basket_engine::OrderReason::Flip {
                    basket_id: "test".to_string(),
                },
            },
            OrderIntent {
                symbol: "AAPL".to_string(),
                qty: 4,
                side: Side::Buy,
                reason: basket_engine::OrderReason::Aggregated,
            },
        ];
        let closes = HashMap::from([
            ("AMD".to_string(), 100.0),
            ("NVDA".to_string(), 200.0),
            ("AAPL".to_string(), 50.0),
        ]);

        let (buy_count, sell_count, buy_notional, sell_notional) =
            summarize_orders_by_side(&orders, &closes);

        assert_eq!(buy_count, 2);
        assert_eq!(sell_count, 1);
        assert_eq!(buy_notional, 1_200.0);
        assert_eq!(sell_notional, 1_000.0);
    }

    #[test]
    fn test_parse_buying_power_rejects_nonpositive_values() {
        let account = AlpacaAccount {
            status: "ACTIVE".to_string(),
            buying_power: "0".to_string(),
            equity: "100000".to_string(),
            trading_blocked: false,
            account_blocked: false,
        };
        let err = parse_buying_power(&account).unwrap_err();
        assert!(err.contains("not positive"));
    }

    #[test]
    fn test_parse_equity_rejects_nonpositive_values() {
        let account = AlpacaAccount {
            status: "ACTIVE".to_string(),
            buying_power: "100000".to_string(),
            equity: "0".to_string(),
            trading_blocked: false,
            account_blocked: false,
        };
        let err = parse_equity(&account).unwrap_err();
        assert!(err.contains("not positive"));
    }

    #[test]
    fn test_top_abs_notional_legs_sorts_by_magnitude() {
        let targets = HashMap::from([
            ("AMD".to_string(), 1000.0),
            ("NVDA".to_string(), -2500.0),
            ("AAPL".to_string(), 1500.0),
        ]);
        assert_eq!(
            top_abs_notional_legs(&targets, 2),
            vec!["NVDA:-2500".to_string(), "AAPL:1500".to_string()]
        );
    }

    #[test]
    fn test_effective_execution_capital_never_exceeds_config_capital() {
        assert_eq!(
            effective_execution_capital(10_000.0, Some(8_500.0)),
            8_500.0
        );
        assert_eq!(
            effective_execution_capital(10_000.0, Some(12_500.0)),
            10_000.0
        );
        assert_eq!(effective_execution_capital(10_000.0, None), 10_000.0);
    }

    #[test]
    fn test_scale_notionals_scales_and_drops_zeroes() {
        let targets = HashMap::from([("AMD".to_string(), 1000.0), ("NVDA".to_string(), -500.0)]);
        let scaled = scale_notionals(&targets, 0.5);
        assert_eq!(scaled.get("AMD"), Some(&500.0));
        assert_eq!(scaled.get("NVDA"), Some(&-250.0));
        assert!(scale_notionals(&targets, 0.0).is_empty());
    }

    #[test]
    fn test_merge_notionals_adds_overlapping_symbols() {
        let lhs = HashMap::from([("AMD".to_string(), 1000.0), ("NVDA".to_string(), -500.0)]);
        let rhs = HashMap::from([("AMD".to_string(), 250.0), ("AAPL".to_string(), 700.0)]);
        let merged = merge_notionals(&lhs, &rhs);
        assert_eq!(merged.get("AMD"), Some(&1250.0));
        assert_eq!(merged.get("NVDA"), Some(&-500.0));
        assert_eq!(merged.get("AAPL"), Some(&700.0));
    }

    #[test]
    fn test_incremental_gross_logic_allows_self_financing_rotation_shape() {
        let mut current: HashMap<String, f64> = HashMap::new();
        current.insert("AMD".to_string(), 100.0);
        let mut target: HashMap<String, f64> = HashMap::new();
        target.insert("NVDA".to_string(), 50.0);
        let mut closes: HashMap<String, f64> = HashMap::new();
        closes.insert("AMD".to_string(), 100.0);
        closes.insert("NVDA".to_string(), 200.0);

        let (current_long, current_short) = gross_by_side(&current, &closes);
        let (target_long, target_short) = gross_by_side(&target, &closes);
        let incremental_exposure =
            (target_long - current_long).max(0.0) + (target_short - current_short).max(0.0);

        assert_eq!(current_long, 10_000.0);
        assert_eq!(current_short, 0.0);
        assert_eq!(target_long, 10_000.0);
        assert_eq!(target_short, 0.0);
        assert_eq!(incremental_exposure, 0.0);
    }

    #[test]
    fn test_incremental_exposure_counts_long_to_short_reversal() {
        let mut current: HashMap<String, f64> = HashMap::new();
        current.insert("AMD".to_string(), 100.0);
        let mut target: HashMap<String, f64> = HashMap::new();
        target.insert("AMD".to_string(), -100.0);
        let mut closes: HashMap<String, f64> = HashMap::new();
        closes.insert("AMD".to_string(), 100.0);

        let (current_long, current_short) = gross_by_side(&current, &closes);
        let (target_long, target_short) = gross_by_side(&target, &closes);
        let incremental_exposure =
            (target_long - current_long).max(0.0) + (target_short - current_short).max(0.0);

        assert_eq!(current_long, 10_000.0);
        assert_eq!(current_short, 0.0);
        assert_eq!(target_long, 0.0);
        assert_eq!(target_short, 10_000.0);
        assert_eq!(incremental_exposure, 10_000.0);
    }

    #[test]
    fn test_staged_diff_to_orders_splits_sign_flip_into_close_then_open() {
        let current = HashMap::from([("AMD".to_string(), 100.0)]);
        let target = HashMap::from([("AMD".to_string(), -80.0)]);

        let orders = staged_diff_to_orders(&current, &target);
        assert_eq!(orders.len(), 2);
        assert_eq!(orders[0].symbol, "AMD");
        assert_eq!(orders[0].side, Side::Sell);
        assert_eq!(orders[0].qty, 100);
        assert_eq!(orders[1].symbol, "AMD");
        assert_eq!(orders[1].side, Side::Sell);
        assert_eq!(orders[1].qty, 80);
    }

    #[test]
    fn test_staged_diff_to_orders_reduces_before_expanding_same_sign() {
        let current = HashMap::from([("AMD".to_string(), 100.0)]);
        let target = HashMap::from([("AMD".to_string(), 40.0), ("NVDA".to_string(), 25.0)]);

        let orders = staged_diff_to_orders(&current, &target);
        assert_eq!(orders.len(), 2);
        assert_eq!(orders[0].symbol, "AMD");
        assert_eq!(orders[0].side, Side::Sell);
        assert_eq!(orders[0].qty, 60);
        assert_eq!(orders[1].symbol, "NVDA");
        assert_eq!(orders[1].side, Side::Buy);
        assert_eq!(orders[1].qty, 25);
    }
}
