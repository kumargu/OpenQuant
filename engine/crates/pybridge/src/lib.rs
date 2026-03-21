//! Thin PyO3 bridge — just type conversion at the boundary.
//! All logic lives in openquant-core. Python never does math.

use pyo3::prelude::*;
use pyo3::types::{PyAnyMethods, PyDict};

use openquant_core::engine::{Engine as CoreEngine, EngineConfig, SymbolOverrides};
use openquant_core::market_data::Bar;
use openquant_core::signals::Side;
use openquant_core::signals::mean_reversion;
use std::collections::HashMap;

use openquant_journal::DataRuntime;
use openquant_journal::writer::{BarRecord, FillRecord};

#[pyclass]
struct Engine {
    inner: CoreEngine,
    journal: Option<DataRuntime>,
    engine_version: String,
}

#[pymethods]
impl Engine {
    #[new]
    #[pyo3(signature = (
        max_position_notional = 10_000.0,
        max_daily_loss = 500.0,
        buy_z_threshold = -2.2,
        sell_z_threshold = 2.0,
        min_relative_volume = 1.2,
        stop_loss_pct = 0.02,
        max_hold_bars = 100,
        take_profit_pct = 0.0,
        trend_filter = true,
        stop_loss_atr_mult = 0.0,
        max_bar_age_seconds = 0,
        symbol_overrides = None,
        journal_path = None,
    ))]
    #[allow(clippy::too_many_arguments)]
    fn new(
        max_position_notional: f64,
        max_daily_loss: f64,
        buy_z_threshold: f64,
        sell_z_threshold: f64,
        min_relative_volume: f64,
        stop_loss_pct: f64,
        max_hold_bars: usize,
        take_profit_pct: f64,
        trend_filter: bool,
        stop_loss_atr_mult: f64,
        max_bar_age_seconds: i64,
        symbol_overrides: Option<&Bound<'_, PyDict>>,
        journal_path: Option<String>,
    ) -> PyResult<Self> {
        // Parse per-symbol overrides from Python dict
        let overrides = match symbol_overrides {
            Some(py_dict) => {
                let mut map = HashMap::new();
                for (key, val) in py_dict.iter() {
                    let symbol: String = key.extract()?;
                    let params: &Bound<'_, PyDict> = val.downcast()?;
                    let ovr = SymbolOverrides {
                        asset_class: None,
                        buy_z_threshold: params
                            .get_item("buy_z_threshold")?
                            .map(|v| v.extract())
                            .transpose()?,
                        sell_z_threshold: params
                            .get_item("sell_z_threshold")?
                            .map(|v| v.extract())
                            .transpose()?,
                        min_relative_volume: params
                            .get_item("min_relative_volume")?
                            .map(|v| v.extract())
                            .transpose()?,
                        trend_filter: params
                            .get_item("trend_filter")?
                            .map(|v| v.extract())
                            .transpose()?,
                        stop_loss_pct: params
                            .get_item("stop_loss_pct")?
                            .map(|v| v.extract())
                            .transpose()?,
                        stop_loss_atr_mult: params
                            .get_item("stop_loss_atr_mult")?
                            .map(|v| v.extract())
                            .transpose()?,
                        max_hold_bars: params
                            .get_item("max_hold_bars")?
                            .map(|v| v.extract())
                            .transpose()?,
                        take_profit_pct: params
                            .get_item("take_profit_pct")?
                            .map(|v| v.extract())
                            .transpose()?,
                        min_hold_bars: params
                            .get_item("min_hold_bars")?
                            .map(|v| v.extract())
                            .transpose()?,
                        weight_mean_reversion: params
                            .get_item("weight_mean_reversion")?
                            .map(|v| v.extract())
                            .transpose()?,
                        weight_momentum: params
                            .get_item("weight_momentum")?
                            .map(|v| v.extract())
                            .transpose()?,
                        weight_vwap_reversion: params
                            .get_item("weight_vwap_reversion")?
                            .map(|v| v.extract())
                            .transpose()?,
                        weight_breakout: params
                            .get_item("weight_breakout")?
                            .map(|v| v.extract())
                            .transpose()?,
                    };
                    map.insert(symbol, ovr);
                }
                map
            }
            None => HashMap::new(),
        };

        let config = EngineConfig {
            signal: mean_reversion::Config {
                buy_z_threshold,
                sell_z_threshold,
                min_relative_volume,
                trend_filter,
                ..Default::default()
            },
            risk: openquant_core::risk::RiskConfig {
                max_position_notional,
                max_daily_loss,
                ..Default::default()
            },
            exit: openquant_core::exit::ExitConfig {
                stop_loss_pct,
                max_hold_bars,
                take_profit_pct,
                stop_loss_atr_mult,
                min_hold_bars: 0,
            },
            symbol_overrides: overrides,
            max_bar_age_ms: max_bar_age_seconds * 1000,
            metrics_enabled: false,
            ..Default::default()
        };

        // Get engine version from git
        let engine_version = option_env!("CARGO_PKG_VERSION")
            .unwrap_or("dev")
            .to_string();

        // Start journal if path provided — fail fast on bad path
        let journal = match journal_path {
            Some(path) => {
                let p = std::path::PathBuf::from(&path);
                if let Some(parent) = p.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| {
                        pyo3::exceptions::PyIOError::new_err(format!(
                            "cannot create journal directory '{}': {e}",
                            parent.display()
                        ))
                    })?;
                }
                Some(DataRuntime::new(&p, 4096))
            }
            None => None,
        };

        Ok(Self {
            inner: CoreEngine::new(config),
            journal,
            engine_version,
        })
    }

    /// Feed a bar, get back list of order intent dicts.
    /// If journal is enabled, logs the full bar record (features + decision).
    #[allow(clippy::too_many_arguments)]
    #[pyo3(signature = (symbol, timestamp, open, high, low, close, volume))]
    fn on_bar<'py>(
        &mut self,
        py: Python<'py>,
        symbol: &str,
        timestamp: i64,
        open: f64,
        high: f64,
        low: f64,
        close: f64,
        volume: f64,
    ) -> PyResult<Vec<Bound<'py, PyDict>>> {
        let bar = Bar {
            symbol: symbol.to_string(),
            timestamp,
            open,
            high,
            low,
            close,
            volume,
        };

        // Use journaled path if journal is active, otherwise fast path
        let (intents, journal_record) = if self.journal.is_some() {
            let outcome = self.inner.on_bar_journaled(&bar);
            let record = BarRecord {
                symbol: bar.symbol.clone(),
                timestamp: bar.timestamp,
                open: bar.open,
                high: bar.high,
                low: bar.low,
                close: bar.close,
                volume: bar.volume,
                features: outcome.features,
                signal_fired: outcome.signal_fired,
                signal_side: outcome.signal_side.map(|s| match s {
                    Side::Buy => "buy".to_string(),
                    Side::Sell => "sell".to_string(),
                }),
                signal_score: outcome.signal_score,
                signal_reason: outcome.signal_reason,
                risk_passed: outcome.risk_passed,
                risk_rejection: outcome.risk_rejection,
                qty_approved: outcome.qty_approved,
                engine_version: self.engine_version.clone(),
            };
            (outcome.intents, Some(record))
        } else {
            (self.inner.on_bar(&bar), None)
        };

        // Send to journal (non-blocking)
        if let (Some(rt), Some(record)) = (&self.journal, journal_record) {
            rt.journal().log_bar(record);
        }

        let mut results = Vec::with_capacity(intents.len());
        for intent in &intents {
            let dict = PyDict::new(py);
            dict.set_item("symbol", &intent.symbol)?;
            dict.set_item(
                "side",
                match intent.side {
                    Side::Buy => "buy",
                    Side::Sell => "sell",
                },
            )?;
            dict.set_item("qty", intent.qty)?;
            dict.set_item(
                "reason",
                format!(
                    "{}: z={:.2}, vol={:.2}",
                    intent.reason.describe(),
                    intent.z_score,
                    intent.relative_volume
                ),
            )?;
            dict.set_item("score", intent.signal_score)?;
            if !intent.votes.is_empty() {
                dict.set_item("votes", &intent.votes)?;
            }
            results.push(dict);
        }

        Ok(results)
    }

    /// Notify engine of a fill (updates portfolio and risk state).
    #[pyo3(signature = (symbol, side, qty, fill_price))]
    fn on_fill(&mut self, symbol: &str, side: &str, qty: f64, fill_price: f64) -> PyResult<()> {
        let side_enum = match side {
            "buy" => Side::Buy,
            "sell" => Side::Sell,
            _ => {
                return Err(pyo3::exceptions::PyValueError::new_err(
                    "side must be 'buy' or 'sell'",
                ));
            }
        };
        self.inner.on_fill(symbol, side_enum, qty, fill_price);

        // Journal the fill
        if let Some(rt) = &self.journal {
            rt.journal().log_fill(FillRecord {
                symbol: symbol.to_string(),
                side: side.to_string(),
                qty,
                fill_price,
                slippage: 0.0,
                engine_version: self.engine_version.clone(),
            });
        }

        Ok(())
    }

    /// Enable or disable warmup mode (bypasses stale-data gate).
    /// Call with `true` before feeding historical warmup bars,
    /// then `false` once switching to live data.
    fn set_warmup_mode(&mut self, enabled: bool) {
        self.inner.set_warmup_mode(enabled);
    }

    /// Reset daily risk state.
    fn reset_daily(&mut self) {
        self.inner.reset_daily();
    }

    /// Get current feature values for a symbol (for debugging).
    fn features<'py>(&self, py: Python<'py>, symbol: &str) -> PyResult<Option<Bound<'py, PyDict>>> {
        match self.inner.current_features(symbol) {
            Some(f) => {
                let dict = PyDict::new(py);
                dict.set_item("return_1", f.return_1)?;
                dict.set_item("return_5", f.return_5)?;
                dict.set_item("return_20", f.return_20)?;
                dict.set_item("sma_20", f.sma_20)?;
                dict.set_item("return_std_20", f.return_std_20)?;
                dict.set_item("return_z_score", f.return_z_score)?;
                dict.set_item("relative_volume", f.relative_volume)?;
                dict.set_item("sma_50", f.sma_50)?;
                dict.set_item("atr", f.atr)?;
                dict.set_item("bar_range", f.bar_range)?;
                dict.set_item("close_location", f.close_location)?;
                dict.set_item("trend_up", f.trend_up)?;
                dict.set_item("warmed_up", f.warmed_up)?;
                // V2: momentum features
                dict.set_item("ema_fast", f.ema_fast)?;
                dict.set_item("ema_slow", f.ema_slow)?;
                dict.set_item("ema_fast_above_slow", f.ema_fast_above_slow)?;
                dict.set_item("adx", f.adx)?;
                dict.set_item("plus_di", f.plus_di)?;
                dict.set_item("minus_di", f.minus_di)?;
                // V2: Bollinger features
                dict.set_item("bollinger_upper", f.bollinger_upper)?;
                dict.set_item("bollinger_lower", f.bollinger_lower)?;
                dict.set_item("bollinger_pct_b", f.bollinger_pct_b)?;
                dict.set_item("bollinger_bandwidth", f.bollinger_bandwidth)?;
                // V5: GARCH
                dict.set_item("garch_vol", f.garch_vol)?;
                // V6: Regime
                dict.set_item("market_regime", f.market_regime.to_string())?;
                dict.set_item("regime_change_prob", f.regime_change_prob)?;
                dict.set_item("garch_vol_percentile", f.garch_vol_percentile)?;
                Ok(Some(dict))
            }
            None => Ok(None),
        }
    }

    /// Get current positions as list of dicts.
    fn positions<'py>(&self, py: Python<'py>) -> PyResult<Vec<Bound<'py, PyDict>>> {
        let positions = self.inner.positions().positions();
        let mut results = Vec::new();
        for pos in positions {
            let dict = PyDict::new(py);
            dict.set_item("symbol", &pos.symbol)?;
            dict.set_item("qty", pos.qty)?;
            dict.set_item("avg_entry_price", pos.avg_entry_price)?;
            dict.set_item("unrealized_pnl", pos.unrealized_pnl)?;
            results.push(dict);
        }
        Ok(results)
    }

    /// Number of journal records dropped due to backpressure.
    fn journal_dropped(&self) -> u64 {
        self.journal
            .as_ref()
            .map(|rt| rt.journal().dropped_count())
            .unwrap_or(0)
    }

    /// Per-symbol count of bars skipped due to stale data.
    fn stale_bars_skipped<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyDict>> {
        let dict = PyDict::new(py);
        for (symbol, count) in self.inner.stale_bars_skipped() {
            dict.set_item(symbol, *count)?;
        }
        Ok(dict)
    }

    /// Gracefully shut down the journal (flushes all pending writes).
    fn shutdown_journal(&mut self) {
        if let Some(rt) = self.journal.take() {
            rt.shutdown();
        }
    }

    /// Create engine from a TOML config file.
    ///
    /// Reads the file, parses all sections, and builds the engine.
    /// Optional `journal_path` enables journaling (not in TOML — runtime concern).
    #[staticmethod]
    #[pyo3(signature = (config_path, journal_path = None, warmup_bars = 64))]
    fn from_toml(
        config_path: &str,
        journal_path: Option<String>,
        warmup_bars: usize,
    ) -> PyResult<Self> {
        let cfg_file = openquant_core::config::ConfigFile::load(std::path::Path::new(config_path))
            .map_err(pyo3::exceptions::PyValueError::new_err)?;

        let engine_version = option_env!("CARGO_PKG_VERSION")
            .unwrap_or("dev")
            .to_string();

        let mut config = cfg_file.into_engine_config();
        config.warmup_bars = warmup_bars;

        let journal = match journal_path {
            Some(path) => {
                let p = std::path::PathBuf::from(&path);
                if let Some(parent) = p.parent() {
                    std::fs::create_dir_all(parent).map_err(|e| {
                        pyo3::exceptions::PyIOError::new_err(format!(
                            "cannot create journal directory '{}': {e}",
                            parent.display()
                        ))
                    })?;
                }
                Some(DataRuntime::new(&p, 4096))
            }
            None => None,
        };

        Ok(Self {
            inner: CoreEngine::new(config),
            journal,
            engine_version,
        })
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        if let Some(rt) = self.journal.take() {
            rt.shutdown();
        }
    }
}

/// Run a backtest over historical bars. All computation in Rust.
#[pyfunction]
#[pyo3(signature = (
    bars,
    max_position_notional = 10_000.0,
    max_daily_loss = 500.0,
    buy_z_threshold = -2.2,
    sell_z_threshold = 2.0,
    min_relative_volume = 1.2,
    stop_loss_pct = 0.02,
    max_hold_bars = 100,
    take_profit_pct = 0.0,
    trend_filter = true,
    stop_loss_atr_mult = 0.0,
))]
#[allow(clippy::too_many_arguments)]
fn backtest<'py>(
    py: Python<'py>,
    bars: Vec<(String, i64, f64, f64, f64, f64, f64)>,
    max_position_notional: f64,
    max_daily_loss: f64,
    buy_z_threshold: f64,
    sell_z_threshold: f64,
    min_relative_volume: f64,
    stop_loss_pct: f64,
    max_hold_bars: usize,
    take_profit_pct: f64,
    trend_filter: bool,
    stop_loss_atr_mult: f64,
) -> PyResult<Bound<'py, PyDict>> {
    let core_bars: Vec<Bar> = bars
        .into_iter()
        .map(|(symbol, ts, o, h, l, c, v)| Bar {
            symbol,
            timestamp: ts,
            open: o,
            high: h,
            low: l,
            close: c,
            volume: v,
        })
        .collect();

    let config = EngineConfig {
        signal: mean_reversion::Config {
            buy_z_threshold,
            sell_z_threshold,
            min_relative_volume,
            trend_filter,
            ..Default::default()
        },
        risk: openquant_core::risk::RiskConfig {
            max_position_notional,
            max_daily_loss,
            ..Default::default()
        },
        exit: openquant_core::exit::ExitConfig {
            stop_loss_pct,
            max_hold_bars,
            take_profit_pct,
            stop_loss_atr_mult,
            min_hold_bars: 0,
        },
        symbol_overrides: HashMap::new(),
        max_bar_age_ms: 0, // disabled for backtesting — historical data is always "old"
        metrics_enabled: false,
        ..Default::default()
    };

    let result = openquant_core::backtest::run(&core_bars, config);

    let dict = PyDict::new(py);
    dict.set_item("total_bars", result.total_bars)?;
    dict.set_item("total_trades", result.total_trades)?;
    dict.set_item("winning_trades", result.winning_trades)?;
    dict.set_item("losing_trades", result.losing_trades)?;
    dict.set_item("win_rate", result.win_rate)?;
    dict.set_item("total_pnl", result.total_pnl)?;
    dict.set_item("avg_win", result.avg_win)?;
    dict.set_item("avg_loss", result.avg_loss)?;
    dict.set_item("profit_factor", result.profit_factor)?;
    dict.set_item("max_drawdown", result.max_drawdown)?;
    dict.set_item("max_drawdown_pct", result.max_drawdown_pct)?;
    dict.set_item("expectancy", result.expectancy)?;
    dict.set_item("sharpe_approx", result.sharpe_approx)?;
    dict.set_item("psr", result.psr)?;
    dict.set_item("dsr", result.dsr)?;
    dict.set_item("signals_generated", result.signals_generated)?;
    dict.set_item("equity_curve", result.equity_curve)?;

    let trades_list: Vec<Bound<'py, PyDict>> = result
        .trades
        .iter()
        .map(|t| {
            let td = PyDict::new(py);
            td.set_item("symbol", &t.symbol).unwrap();
            td.set_item("entry_price", t.entry_price).unwrap();
            td.set_item("exit_price", t.exit_price).unwrap();
            td.set_item("qty", t.qty).unwrap();
            td.set_item("entry_time", t.entry_time).unwrap();
            td.set_item("exit_time", t.exit_time).unwrap();
            td.set_item("pnl", t.pnl).unwrap();
            td.set_item("return_pct", t.return_pct).unwrap();
            td.set_item("entry_reason", t.entry_reason.describe())
                .unwrap();
            td.set_item("exit_reason", t.exit_reason.describe())
                .unwrap();
            td.set_item("bars_held", t.bars_held).unwrap();
            td
        })
        .collect();
    dict.set_item("trades", trades_list)?;

    Ok(dict)
}

/// Run a backtest using the full TOML config (including per-symbol overrides).
#[pyfunction]
#[pyo3(signature = (bars, config_path))]
fn backtest_toml<'py>(
    py: Python<'py>,
    bars: Vec<(String, i64, f64, f64, f64, f64, f64)>,
    config_path: &str,
) -> PyResult<Bound<'py, PyDict>> {
    let core_bars: Vec<Bar> = bars
        .into_iter()
        .map(|(symbol, ts, o, h, l, c, v)| Bar {
            symbol,
            timestamp: ts,
            open: o,
            high: h,
            low: l,
            close: c,
            volume: v,
        })
        .collect();

    let cfg_file = openquant_core::config::ConfigFile::load(std::path::Path::new(config_path))
        .map_err(pyo3::exceptions::PyValueError::new_err)?;
    let mut config = cfg_file.into_engine_config();
    config.max_bar_age_ms = 0; // disabled for backtesting
    config.metrics_enabled = false;

    let result = openquant_core::backtest::run(&core_bars, config);

    let dict = PyDict::new(py);
    dict.set_item("total_bars", result.total_bars)?;
    dict.set_item("total_trades", result.total_trades)?;
    dict.set_item("winning_trades", result.winning_trades)?;
    dict.set_item("losing_trades", result.losing_trades)?;
    dict.set_item("win_rate", result.win_rate)?;
    dict.set_item("total_pnl", result.total_pnl)?;
    dict.set_item("avg_win", result.avg_win)?;
    dict.set_item("avg_loss", result.avg_loss)?;
    dict.set_item("profit_factor", result.profit_factor)?;
    dict.set_item("max_drawdown", result.max_drawdown)?;
    dict.set_item("max_drawdown_pct", result.max_drawdown_pct)?;
    dict.set_item("expectancy", result.expectancy)?;
    dict.set_item("sharpe_approx", result.sharpe_approx)?;
    dict.set_item("psr", result.psr)?;
    dict.set_item("dsr", result.dsr)?;
    dict.set_item("signals_generated", result.signals_generated)?;
    dict.set_item("equity_curve", result.equity_curve)?;

    let trades_list: Vec<Bound<'py, PyDict>> = result
        .trades
        .iter()
        .map(|t| {
            let td = PyDict::new(py);
            td.set_item("symbol", &t.symbol).unwrap();
            td.set_item("entry_price", t.entry_price).unwrap();
            td.set_item("exit_price", t.exit_price).unwrap();
            td.set_item("qty", t.qty).unwrap();
            td.set_item("entry_time", t.entry_time).unwrap();
            td.set_item("exit_time", t.exit_time).unwrap();
            td.set_item("pnl", t.pnl).unwrap();
            td.set_item("return_pct", t.return_pct).unwrap();
            td.set_item("entry_reason", t.entry_reason.describe())
                .unwrap();
            td.set_item("exit_reason", t.exit_reason.describe())
                .unwrap();
            td.set_item("bars_held", t.bars_held).unwrap();
            td
        })
        .collect();
    dict.set_item("trades", trades_list)?;

    Ok(dict)
}

/// Validate bar data quality. Returns a dict with quality metrics.
#[pyfunction]
#[pyo3(signature = (bars, gap_threshold_minutes = 5))]
fn validate_bars<'py>(
    py: Python<'py>,
    bars: Vec<(String, i64, f64, f64, f64, f64, f64)>,
    gap_threshold_minutes: i64,
) -> PyResult<Bound<'py, PyDict>> {
    let core_bars: Vec<Bar> = bars
        .into_iter()
        .map(|(symbol, ts, o, h, l, c, v)| Bar {
            symbol,
            timestamp: ts,
            open: o,
            high: h,
            low: l,
            close: c,
            volume: v,
        })
        .collect();

    let gap_threshold_ms = gap_threshold_minutes * 60 * 1000;
    let report = openquant_core::market_data::validate_bars(&core_bars, gap_threshold_ms);

    let dict = PyDict::new(py);
    dict.set_item("total_bars", report.total_bars)?;
    dict.set_item("ohlc_violations", report.ohlc_violations)?;
    dict.set_item("non_positive_prices", report.non_positive_prices)?;
    dict.set_item("zero_volume_bars", report.zero_volume_bars)?;
    dict.set_item("zero_volume_pct", report.zero_volume_pct())?;
    dict.set_item("timestamp_backwards", report.timestamp_backwards)?;
    dict.set_item("duplicate_timestamps", report.duplicate_timestamps)?;
    dict.set_item("has_critical_issues", report.has_critical_issues())?;

    let gaps: Vec<(usize, i64)> = report.gaps;
    dict.set_item("gap_count", gaps.len())?;
    dict.set_item("gaps", gaps)?;

    Ok(dict)
}

/// Compute Deflated Sharpe Ratio for multiple-testing correction.
///
/// Arguments:
///   observed_sr: the observed Sharpe ratio
///   n_trades: number of trades in the backtest
///   skewness: skewness of trade returns
///   kurtosis: kurtosis of trade returns (raw, not excess)
///   n_experiments: total number of experiments/configs tried
///
/// Returns DSR as a probability (0-1). DSR > 0.95 means the Sharpe is
/// statistically significant even after correcting for multiple testing.
#[pyfunction]
#[pyo3(signature = (observed_sr, n_trades, skewness, kurtosis, n_experiments))]
fn deflated_sharpe(
    observed_sr: f64,
    n_trades: usize,
    skewness: f64,
    kurtosis: f64,
    n_experiments: usize,
) -> f64 {
    openquant_core::backtest::deflated_sharpe(
        observed_sr,
        n_trades,
        skewness,
        kurtosis,
        n_experiments,
    )
}

/// Load and return the parsed TOML config as a JSON string (for Python inspection).
#[pyfunction]
#[pyo3(signature = (config_path))]
fn load_config(config_path: &str) -> PyResult<String> {
    let cfg = openquant_core::config::ConfigFile::load(std::path::Path::new(config_path))
        .map_err(pyo3::exceptions::PyValueError::new_err)?;
    serde_json::to_string_pretty(&cfg)
        .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(format!("serialize error: {e}")))
}

/// Python wrapper for the pairs trading engine.
///
/// Usage:
///   pairs = openquant.PairsEngine.from_active_pairs("data/active_pairs.json", "data/pair_trading_history.json")
///   intents = pairs.on_bar("GLD", timestamp, 420.0)
#[pyclass(name = "PairsEngine")]
struct PairsEngineWrapper {
    inner: openquant_core::pairs::engine::PairsEngine,
}

#[pymethods]
impl PairsEngineWrapper {
    /// Create from active_pairs.json (produced by pair-picker binary).
    ///
    /// Loads trade history from history_path for Thompson sampling feedback.
    #[staticmethod]
    #[pyo3(signature = (active_pairs_path, history_path))]
    fn from_active_pairs(active_pairs_path: &str, history_path: &str) -> PyResult<Self> {
        Ok(Self {
            inner: openquant_core::pairs::engine::PairsEngine::from_active_pairs(
                std::path::Path::new(active_pairs_path),
                std::path::Path::new(history_path),
                Vec::new(),
            ),
        })
    }

    /// Reload pairs from active_pairs.json without restart.
    /// Returns true if reload succeeded.
    fn reload(&mut self) -> bool {
        self.inner.reload()
    }

    /// Record a closed trade for Thompson sampling feedback.
    #[allow(clippy::too_many_arguments)]
    fn record_trade(
        &mut self,
        leg_a: &str,
        leg_b: &str,
        entry_date: &str,
        exit_date: &str,
        entry_z: f64,
        exit_z: f64,
        return_bps: f64,
        holding_bars: usize,
        exit_reason: &str,
    ) -> PyResult<()> {
        use openquant_core::pairs::active_pairs::ClosedPairTrade;
        self.inner.record_trade(ClosedPairTrade {
            pair: (leg_a.to_string(), leg_b.to_string()),
            entry_date: entry_date.to_string(),
            exit_date: exit_date.to_string(),
            entry_zscore: entry_z,
            exit_zscore: exit_z,
            return_bps,
            holding_period_bars: holding_bars,
            exit_reason: exit_reason.to_string(),
        });
        Ok(())
    }

    /// Number of closed trades recorded.
    fn trade_count(&self) -> usize {
        self.inner.trade_count()
    }

    /// Process a bar. Returns list of order intent dicts (0 or 2 per signal).
    fn on_bar(
        &mut self,
        py: Python<'_>,
        symbol: &str,
        timestamp: i64,
        close: f64,
    ) -> PyResult<Vec<PyObject>> {
        let intents = self.inner.on_bar(symbol, timestamp, close);

        intents
            .into_iter()
            .map(|intent| {
                let dict = PyDict::new(py);
                dict.set_item("symbol", &intent.symbol)?;
                dict.set_item(
                    "side",
                    match intent.side {
                        Side::Buy => "buy",
                        Side::Sell => "sell",
                    },
                )?;
                dict.set_item("qty", intent.qty)?;
                dict.set_item("reason", intent.reason.describe())?;
                dict.set_item("pair_id", &intent.pair_id)?;
                dict.set_item("z_score", intent.z_score)?;
                dict.set_item("spread", intent.spread)?;
                Ok(dict.into())
            })
            .collect()
    }

    /// Number of configured pairs.
    fn pair_count(&self) -> usize {
        self.inner.pair_count()
    }

    /// Reset daily state.
    fn reset_daily(&mut self) {
        self.inner.reset_daily();
    }
}

#[pymodule]
fn openquant(m: &Bound<'_, PyModule>) -> PyResult<()> {
    // Initialize tracing — logs to both stderr and data/journal/engine.log.
    // Controlled by RUST_LOG env var (e.g. RUST_LOG=warn or RUST_LOG=info).
    use tracing_subscriber::{EnvFilter, fmt, prelude::*};
    let filter = EnvFilter::from_default_env();
    // Use cwd-relative path; the runner always runs from the repo root.
    let journal_dir = std::path::PathBuf::from("data/journal");
    std::fs::create_dir_all(&journal_dir).ok();
    let file_appender = tracing_appender::rolling::daily(journal_dir, "engine.log");
    let (file_writer, _guard) = tracing_appender::non_blocking(file_appender);
    // Leak the guard so it lives for the process lifetime
    std::mem::forget(_guard);
    let _ = tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().with_target(true).with_writer(std::io::stderr))
        .with(
            fmt::layer()
                .with_target(true)
                .with_ansi(false)
                .with_writer(file_writer),
        )
        .try_init();

    m.add_class::<Engine>()?;
    m.add_class::<PairsEngineWrapper>()?;
    m.add_function(wrap_pyfunction!(backtest, m)?)?;
    m.add_function(wrap_pyfunction!(backtest_toml, m)?)?;
    m.add_function(wrap_pyfunction!(validate_bars, m)?)?;
    m.add_function(wrap_pyfunction!(deflated_sharpe, m)?)?;
    m.add_function(wrap_pyfunction!(load_config, m)?)?;
    Ok(())
}
