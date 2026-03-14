/// The engine: ties features → signals → risk → order intents.
///
/// `on_bar()` is the single entry point. Feed a bar, get back order intents.
/// This is the entire pipeline in one call.

use std::collections::HashMap;

use crate::features::{FeatureState, FeatureValues};
use crate::market_data::Bar;
use crate::portfolio::Portfolio;
use crate::risk::{self, RiskConfig, RiskState};
use crate::signals::{self, SignalConfig, Side};

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
    pub signal: SignalConfig,
    pub risk: RiskConfig,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            signal: SignalConfig::default(),
            risk: RiskConfig::default(),
        }
    }
}

/// The core engine. Maintains all state, processes bars, emits order intents.
pub struct Engine {
    config: EngineConfig,
    features: HashMap<String, FeatureState>,
    portfolio: Portfolio,
    risk_state: RiskState,
}

impl Engine {
    pub fn new(config: EngineConfig) -> Self {
        Self {
            config,
            features: HashMap::new(),
            portfolio: Portfolio::new(),
            risk_state: RiskState::new(),
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

        // 2. Score signals
        let has_position = self.portfolio.has_position(&bar.symbol);
        let signal = signals::score(&features, has_position, &self.config.signal);

        let signal = match signal {
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
            &self.config.risk,
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

    /// Notify engine that an order was filled (so it updates portfolio/risk).
    pub fn on_fill(&mut self, symbol: &str, side: Side, qty: f64, fill_price: f64) {
        let realized_pnl = self.portfolio.on_fill(symbol, side, qty, fill_price);
        if realized_pnl != 0.0 {
            self.risk_state.record_pnl(realized_pnl, &self.config.risk);
        }
    }

    /// Reset daily risk state (call at start of each trading day).
    pub fn reset_daily(&mut self) {
        self.risk_state.reset_daily();
    }

    /// Current feature values for a symbol (for debugging/display).
    pub fn current_features(&mut self, symbol: &str) -> Option<FeatureValues> {
        // Return the last computed features by feeding a dummy? No — we just
        // expose the state. But FeatureState doesn't cache the last output.
        // For now, return None. We'll add caching if needed.
        let _ = symbol;
        None
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
    fn test_warmup_no_signals() {
        let mut engine = Engine::new(EngineConfig::default());
        // First 19 bars should produce no signals (warmup)
        for i in 0..19 {
            let bar = steady_bar("AAPL", 100.0 + (i as f64 * 0.01), 1000.0);
            let intents = engine.on_bar(&bar);
            assert!(intents.is_empty(), "expected no signal during warmup, bar {i}");
        }
    }

    #[test]
    fn test_big_drop_triggers_buy() {
        let config = EngineConfig {
            risk: crate::risk::RiskConfig {
                min_reward_cost_ratio: 0.0, // disable cost filter for this test
                ..Default::default()
            },
            ..Default::default()
        };
        let mut engine = Engine::new(config);

        // Warm up with steady prices
        for _ in 0..25 {
            engine.on_bar(&steady_bar("AAPL", 100.0, 1000.0));
        }

        // Big drop with volume spike
        let crash_bar = Bar {
            symbol: "AAPL".into(),
            timestamp: 0,
            open: 100.0,
            high: 100.0,
            low: 93.0,
            close: 94.0,
            volume: 2000.0,
        };
        let intents = engine.on_bar(&crash_bar);
        assert!(!intents.is_empty(), "expected buy signal on big drop");
        assert_eq!(intents[0].side, Side::Buy);
    }

    #[test]
    fn test_kill_switch_blocks_after_loss() {
        let config = EngineConfig {
            risk: RiskConfig {
                max_daily_loss: 100.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut engine = Engine::new(config);

        // Simulate a big loss
        engine.on_fill("AAPL", Side::Buy, 10.0, 100.0);
        engine.on_fill("AAPL", Side::Sell, 10.0, 85.0); // -150 loss

        assert!(engine.risk_state().killed);

        // Now even a strong signal should be blocked
        for _ in 0..25 {
            engine.on_bar(&steady_bar("TSLA", 100.0, 1000.0));
        }
        let crash = Bar {
            symbol: "TSLA".into(),
            timestamp: 0,
            open: 100.0,
            high: 100.0,
            low: 90.0,
            close: 91.0,
            volume: 3000.0,
        };
        let intents = engine.on_bar(&crash);
        assert!(intents.is_empty(), "kill switch should block all trades");
    }

    #[test]
    fn test_deterministic() {
        // Same inputs must produce same outputs
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

        let run1 = run(&bars);
        let run2 = run(&bars);

        assert_eq!(run1.len(), run2.len());
        for (r1, r2) in run1.iter().zip(run2.iter()) {
            assert_eq!(r1.len(), r2.len());
            for (o1, o2) in r1.iter().zip(r2.iter()) {
                assert_eq!(o1.symbol, o2.symbol);
                assert_eq!(o1.side, o2.side);
                assert_eq!(o1.qty, o2.qty);
                assert_eq!(o1.signal_score, o2.signal_score);
            }
        }
    }
}
