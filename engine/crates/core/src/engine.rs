//! Core engine: the single entry point that ties everything together.
//!
//! Feed a bar via `on_bar()`, get back order intents. Internally runs:
//! features → strategy → risk gates → order intents.
//!
//! The engine is strategy-agnostic — it holds a boxed `Strategy` trait object.
//! Swap strategies by passing a different one at construction time.

use std::collections::HashMap;

use crate::features::{FeatureState, FeatureValues};
use crate::market_data::Bar;
use crate::portfolio::Portfolio;
use crate::risk::{self, RiskConfig, RiskState};
use crate::signals::{Side, Strategy, mean_reversion};

/// An order the engine wants placed.
#[derive(Debug, Clone)]
pub struct OrderIntent {
    pub symbol: String,
    pub side: Side,
    pub qty: f64,
    pub reason: String,
    pub signal_score: f64,
}

/// Engine configuration.
#[derive(Debug, Clone)]
pub struct EngineConfig {
    pub signal: mean_reversion::Config,
    pub risk: RiskConfig,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            signal: mean_reversion::Config::default(),
            risk: RiskConfig::default(),
        }
    }
}

/// The core engine. Maintains all state, processes bars, emits order intents.
pub struct Engine {
    strategy: Box<dyn Strategy>,
    features: HashMap<String, FeatureState>,
    last_features: HashMap<String, FeatureValues>,
    portfolio: Portfolio,
    risk_state: RiskState,
    risk_config: RiskConfig,
}

impl Engine {
    /// Create engine with default mean-reversion strategy.
    pub fn new(config: EngineConfig) -> Self {
        let strategy = mean_reversion::MeanReversion::new(config.signal);
        Self {
            strategy: Box::new(strategy),
            features: HashMap::new(),
            last_features: HashMap::new(),
            portfolio: Portfolio::new(),
            risk_state: RiskState::new(),
            risk_config: config.risk,
        }
    }

    /// Create engine with a custom strategy.
    pub fn with_strategy(strategy: Box<dyn Strategy>, risk_config: RiskConfig) -> Self {
        Self {
            strategy,
            features: HashMap::new(),
            last_features: HashMap::new(),
            portfolio: Portfolio::new(),
            risk_state: RiskState::new(),
            risk_config,
        }
    }

    /// Process a new bar. Returns order intents (may be empty).
    pub fn on_bar(&mut self, bar: &Bar) -> Vec<OrderIntent> {
        // 1. Update features
        let feature_state = self
            .features
            .entry(bar.symbol.clone())
            .or_insert_with(FeatureState::new);

        let features = feature_state.update(bar.close, bar.high, bar.low, bar.volume);
        self.last_features.insert(bar.symbol.clone(), features.clone());

        // 2. Score via strategy
        let has_position = self.portfolio.has_position(&bar.symbol);
        let signal = match self.strategy.score(&features, has_position) {
            Some(s) => s,
            None => return vec![],
        };

        // 3. Risk gates
        let position_qty = self.portfolio.position_qty(&bar.symbol);
        let qty = match risk::check(
            &signal,
            bar.close,
            position_qty,
            &self.risk_state,
            &self.risk_config,
        ) {
            Ok(qty) => qty,
            Err(_rejection) => return vec![],
        };

        vec![OrderIntent {
            symbol: bar.symbol.clone(),
            side: signal.side,
            qty,
            reason: signal.reason,
            signal_score: signal.score,
        }]
    }

    /// Notify engine that an order was filled (updates portfolio and risk).
    pub fn on_fill(&mut self, symbol: &str, side: Side, qty: f64, fill_price: f64) {
        let realized_pnl = self.portfolio.on_fill(symbol, side, qty, fill_price);
        if realized_pnl != 0.0 {
            self.risk_state.record_pnl(realized_pnl, &self.risk_config);
        }
    }

    /// Reset daily risk state.
    pub fn reset_daily(&mut self) {
        self.risk_state.reset_daily();
    }

    /// Current feature values for a symbol.
    pub fn current_features(&self, symbol: &str) -> Option<&FeatureValues> {
        self.last_features.get(symbol)
    }

    /// Current portfolio positions.
    pub fn positions(&self) -> &Portfolio {
        &self.portfolio
    }

    /// Current risk state.
    pub fn risk_state(&self) -> &RiskState {
        &self.risk_state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn steady_bar(symbol: &str, close: f64, volume: f64) -> Bar {
        Bar {
            symbol: symbol.into(),
            timestamp: 0,
            open: close,
            high: close + 0.5,
            low: close - 0.5,
            close,
            volume,
        }
    }

    #[test]
    fn warmup_produces_no_signals() {
        let mut engine = Engine::new(EngineConfig::default());
        for i in 0..19 {
            let bar = steady_bar("AAPL", 100.0 + (i as f64 * 0.01), 1000.0);
            assert!(engine.on_bar(&bar).is_empty(), "no signal during warmup, bar {i}");
        }
    }

    #[test]
    fn big_drop_triggers_buy() {
        let config = EngineConfig {
            risk: RiskConfig {
                min_reward_cost_ratio: 0.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut engine = Engine::new(config);

        for _ in 0..25 {
            engine.on_bar(&steady_bar("AAPL", 100.0, 1000.0));
        }

        let crash = Bar {
            symbol: "AAPL".into(), timestamp: 0,
            open: 100.0, high: 100.0, low: 93.0, close: 94.0, volume: 2000.0,
        };
        let intents = engine.on_bar(&crash);
        assert!(!intents.is_empty(), "expected buy signal on big drop");
        assert_eq!(intents[0].side, Side::Buy);
    }

    #[test]
    fn kill_switch_blocks_after_loss() {
        let config = EngineConfig {
            risk: RiskConfig { max_daily_loss: 100.0, ..Default::default() },
            ..Default::default()
        };
        let mut engine = Engine::new(config);

        engine.on_fill("AAPL", Side::Buy, 10.0, 100.0);
        engine.on_fill("AAPL", Side::Sell, 10.0, 85.0); // -150 loss
        assert!(engine.risk_state().killed);

        for _ in 0..25 {
            engine.on_bar(&steady_bar("TSLA", 100.0, 1000.0));
        }
        let crash = Bar {
            symbol: "TSLA".into(), timestamp: 0,
            open: 100.0, high: 100.0, low: 90.0, close: 91.0, volume: 3000.0,
        };
        assert!(engine.on_bar(&crash).is_empty(), "kill switch should block");
    }

    #[test]
    fn deterministic_replay() {
        let bars: Vec<Bar> = (0..30)
            .map(|i| Bar {
                symbol: "TEST".into(),
                timestamp: i * 60000,
                open: 100.0 + (i as f64 * 0.1),
                high: 101.0 + (i as f64 * 0.1),
                low: 99.0 + (i as f64 * 0.1),
                close: 100.5 + (i as f64 * 0.1),
                volume: 1000.0,
            })
            .collect();

        let run = |bars: &[Bar]| -> Vec<Vec<OrderIntent>> {
            let mut engine = Engine::new(EngineConfig::default());
            bars.iter().map(|b| engine.on_bar(b)).collect()
        };

        let r1 = run(&bars);
        let r2 = run(&bars);

        for (a, b) in r1.iter().zip(r2.iter()) {
            assert_eq!(a.len(), b.len());
            for (o1, o2) in a.iter().zip(b.iter()) {
                assert_eq!(o1.symbol, o2.symbol);
                assert_eq!(o1.side, o2.side);
                assert_eq!(o1.qty, o2.qty);
                assert_eq!(o1.signal_score, o2.signal_score);
            }
        }
    }
}
