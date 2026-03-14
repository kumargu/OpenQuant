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

#[pymodule]
fn openquant(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<Engine>()?;
    Ok(())
}
