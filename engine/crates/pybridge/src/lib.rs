/// Thin PyO3 bridge — just type conversion at the boundary.
/// All logic lives in openquant-core. This is ~100 lines of glue.

use pyo3::prelude::*;
use pyo3::types::PyDict;

use openquant_core::engine::{Engine as CoreEngine, EngineConfig};
use openquant_core::market_data::Bar;
use openquant_core::signals::Side;

#[pyclass]
struct Engine {
    inner: CoreEngine,
}

#[pymethods]
impl Engine {
    #[new]
    #[pyo3(signature = (
        max_position_notional = 10_000.0,
        max_daily_loss = 500.0,
        buy_z_threshold = -2.0,
        sell_z_threshold = 2.0,
        min_relative_volume = 1.2,
    ))]
    fn new(
        max_position_notional: f64,
        max_daily_loss: f64,
        buy_z_threshold: f64,
        sell_z_threshold: f64,
        min_relative_volume: f64,
    ) -> Self {
        let config = EngineConfig {
            signal: openquant_core::signals::SignalConfig {
                buy_z_threshold,
                sell_z_threshold,
                min_relative_volume,
                ..Default::default()
            },
            risk: openquant_core::risk::RiskConfig {
                max_position_notional,
                max_daily_loss,
                ..Default::default()
            },
        };
        Self {
            inner: CoreEngine::new(config),
        }
    }

    /// Feed a bar, get back list of order intent dicts.
    /// Returns: [{"symbol", "side", "qty", "reason", "score"}, ...]
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

        let intents = self.inner.on_bar(&bar);
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
            dict.set_item("reason", &intent.reason)?;
            dict.set_item("score", intent.signal_score)?;
            results.push(dict);
        }

        Ok(results)
    }

    /// Notify engine of a fill (updates portfolio and risk state).
    #[pyo3(signature = (symbol, side, qty, fill_price))]
    fn on_fill(&mut self, symbol: &str, side: &str, qty: f64, fill_price: f64) -> PyResult<()> {
        let side = match side {
            "buy" => Side::Buy,
            "sell" => Side::Sell,
            _ => return Err(pyo3::exceptions::PyValueError::new_err("side must be 'buy' or 'sell'")),
        };
        self.inner.on_fill(symbol, side, qty, fill_price);
        Ok(())
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
                dict.set_item("bar_range", f.bar_range)?;
                dict.set_item("close_location", f.close_location)?;
                dict.set_item("warmed_up", f.warmed_up)?;
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
}

/// Run a backtest over historical bars. All computation in Rust.
///
/// bars: list of (symbol, timestamp, open, high, low, close, volume) tuples
/// Returns dict with full stats + trade list + equity curve.
#[pyfunction]
#[pyo3(signature = (
    bars,
    max_position_notional = 10_000.0,
    max_daily_loss = 500.0,
    buy_z_threshold = -2.0,
    sell_z_threshold = 2.0,
    min_relative_volume = 1.2,
))]
fn backtest<'py>(
    py: Python<'py>,
    bars: Vec<(String, i64, f64, f64, f64, f64, f64)>,
    max_position_notional: f64,
    max_daily_loss: f64,
    buy_z_threshold: f64,
    sell_z_threshold: f64,
    min_relative_volume: f64,
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
        signal: openquant_core::signals::SignalConfig {
            buy_z_threshold,
            sell_z_threshold,
            min_relative_volume,
            ..Default::default()
        },
        risk: openquant_core::risk::RiskConfig {
            max_position_notional,
            max_daily_loss,
            ..Default::default()
        },
    };

    let result = openquant_core::backtest::run(&core_bars, config);

    // Convert to Python dict
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
    dict.set_item("signals_generated", result.signals_generated)?;
    dict.set_item("equity_curve", result.equity_curve)?;

    // Trade records as list of dicts
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
            td.set_item("entry_reason", &t.entry_reason).unwrap();
            td.set_item("exit_reason", &t.exit_reason).unwrap();
            td.set_item("bars_held", t.bars_held).unwrap();
            td
        })
        .collect();
    dict.set_item("trades", trades_list)?;

    Ok(dict)
}

#[pymodule]
fn openquant(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<Engine>()?;
    m.add_function(wrap_pyfunction!(backtest, m)?)?;
    Ok(())
}
