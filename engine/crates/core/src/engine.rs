//! Core engine: the single entry point that ties everything together.
//!
//! Feed a bar via `on_bar()`, get back order intents. Internally runs:
//!
//! ```text
//!  on_bar(bar):
//!    1. Update features
//!    2. Check exit rules on open positions  ← NEW
//!       (stop loss, take profit, max hold)
//!    3. If no exit, check strategy for new entry
//!    4. Risk gates on any signal
//!    5. Return order intents
//! ```
//!
//! The engine is strategy-agnostic — it holds a boxed `Strategy` trait object.

use std::collections::HashMap;

use crate::exit::{self, ExitConfig, OpenPosition};
use crate::features::{FeatureState, FeatureValues};
use crate::market_data::Bar;
use crate::portfolio::Portfolio;
use crate::risk::{self, RiskConfig, RiskState};
use crate::signals::{Side, SignalReason, Strategy, mean_reversion};

/// An order the engine wants placed.
#[derive(Debug, Clone)]
pub struct OrderIntent {
    pub symbol: String,
    pub side: Side,
    pub qty: f64,
    pub reason: SignalReason,
    pub signal_score: f64,
    pub z_score: f64,
    pub relative_volume: f64,
}

/// Full outcome of processing a bar — for journaling.
/// Captures features, signal decision, and risk gate result.
#[derive(Debug, Clone)]
pub struct BarOutcome {
    pub features: FeatureValues,
    pub intents: Vec<OrderIntent>,
    pub signal_fired: bool,
    pub signal_side: Option<Side>,
    pub signal_score: Option<f64>,
    pub signal_reason: Option<SignalReason>,
    pub risk_passed: Option<bool>,
    pub risk_rejection: Option<String>,
    pub qty_approved: Option<f64>,
}

/// Engine configuration.
#[derive(Debug, Clone)]
pub struct EngineConfig {
    pub signal: mean_reversion::Config,
    pub risk: RiskConfig,
    pub exit: ExitConfig,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            signal: mean_reversion::Config::default(),
            risk: RiskConfig::default(),
            exit: ExitConfig::default(),
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
    exit_config: ExitConfig,
    open_positions: HashMap<String, OpenPosition>,
    bar_counter: usize,
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
            exit_config: config.exit,
            open_positions: HashMap::new(),
            bar_counter: 0,
        }
    }

    /// Create engine with a custom strategy.
    pub fn with_strategy(
        strategy: Box<dyn Strategy>,
        risk_config: RiskConfig,
        exit_config: ExitConfig,
    ) -> Self {
        Self {
            strategy,
            features: HashMap::new(),
            last_features: HashMap::new(),
            portfolio: Portfolio::new(),
            risk_state: RiskState::new(),
            risk_config,
            exit_config,
            open_positions: HashMap::new(),
            bar_counter: 0,
        }
    }

    /// Process a new bar. Returns order intents (may be empty).
    pub fn on_bar(&mut self, bar: &Bar) -> Vec<OrderIntent> {
        self.bar_counter += 1;

        // 1. Update features
        let feature_state = self
            .features
            .entry(bar.symbol.clone())
            .or_insert_with(FeatureState::new);

        let features = feature_state.update(bar.close, bar.high, bar.low, bar.volume);
        self.last_features.insert(bar.symbol.clone(), features.clone());

        // 2. Check exit rules on open positions
        if let Some(pos) = self.open_positions.get(&bar.symbol) {
            if let Some(exit_intent) = exit::check(pos, bar.close, self.bar_counter, features.atr, &self.exit_config) {
                return vec![exit_intent];
            }
        }

        // 3. Score via strategy (only if no exit and no position)
        let has_position = self.open_positions.contains_key(&bar.symbol);
        let signal = match self.strategy.score(&features, has_position) {
            Some(s) => s,
            None => return vec![],
        };

        // 4. Risk gates
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
            z_score: signal.z_score,
            relative_volume: signal.relative_volume,
        }]
    }

    /// Process a bar and return full decision details for journaling.
    /// Same logic as `on_bar()` but captures feature state, signal, and risk gate results.
    pub fn on_bar_journaled(&mut self, bar: &Bar) -> BarOutcome {
        self.bar_counter += 1;

        // 1. Update features
        let feature_state = self
            .features
            .entry(bar.symbol.clone())
            .or_insert_with(FeatureState::new);

        let features = feature_state.update(bar.close, bar.high, bar.low, bar.volume);
        self.last_features.insert(bar.symbol.clone(), features.clone());

        // 2. Check exit rules on open positions
        if let Some(pos) = self.open_positions.get(&bar.symbol) {
            if let Some(exit_intent) = exit::check(pos, bar.close, self.bar_counter, features.atr, &self.exit_config) {
                return BarOutcome {
                    features,
                    signal_fired: true,
                    signal_side: Some(exit_intent.side),
                    signal_score: Some(exit_intent.signal_score),
                    signal_reason: Some(exit_intent.reason),
                    risk_passed: Some(true),
                    risk_rejection: None,
                    qty_approved: Some(exit_intent.qty),
                    intents: vec![exit_intent],
                };
            }
        }

        // 3. Score via strategy
        let has_position = self.open_positions.contains_key(&bar.symbol);
        let signal = match self.strategy.score(&features, has_position) {
            Some(s) => s,
            None => {
                return BarOutcome {
                    features,
                    intents: vec![],
                    signal_fired: false,
                    signal_side: None,
                    signal_score: None,
                    signal_reason: None,
                    risk_passed: None,
                    risk_rejection: None,
                    qty_approved: None,
                };
            }
        };

        // 4. Risk gates
        let position_qty = self.portfolio.position_qty(&bar.symbol);
        match risk::check(
            &signal,
            bar.close,
            position_qty,
            &self.risk_state,
            &self.risk_config,
        ) {
            Ok(qty) => {
                let intent = OrderIntent {
                    symbol: bar.symbol.clone(),
                    side: signal.side,
                    qty,
                    reason: signal.reason,
                    signal_score: signal.score,
                    z_score: signal.z_score,
                    relative_volume: signal.relative_volume,
                };
                BarOutcome {
                    features,
                    signal_fired: true,
                    signal_side: Some(signal.side),
                    signal_score: Some(signal.score),
                    signal_reason: Some(signal.reason),
                    risk_passed: Some(true),
                    risk_rejection: None,
                    qty_approved: Some(qty),
                    intents: vec![intent],
                }
            }
            Err(rejection) => BarOutcome {
                features,
                intents: vec![],
                signal_fired: true,
                signal_side: Some(signal.side),
                signal_score: Some(signal.score),
                signal_reason: Some(signal.reason),
                risk_passed: Some(false),
                risk_rejection: Some(rejection.reason),
                qty_approved: None,
            },
        }
    }

    /// Notify engine that an order was filled (updates portfolio, risk, position tracking).
    pub fn on_fill(&mut self, symbol: &str, side: Side, qty: f64, fill_price: f64) {
        let realized_pnl = self.portfolio.on_fill(symbol, side, qty, fill_price);
        if realized_pnl != 0.0 {
            self.risk_state.record_pnl(realized_pnl, &self.risk_config);
        }

        match side {
            Side::Buy => {
                self.open_positions.insert(
                    symbol.to_string(),
                    OpenPosition {
                        symbol: symbol.to_string(),
                        entry_price: fill_price,
                        qty,
                        entry_bar: self.bar_counter,
                    },
                );
            }
            Side::Sell => {
                self.open_positions.remove(symbol);
            }
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

    fn no_exit_config() -> ExitConfig {
        ExitConfig {
            stop_loss_pct: 0.0,
            stop_loss_atr_mult: 0.0,
            max_hold_bars: 0,
            take_profit_pct: 0.0,
        }
    }

    #[test]
    fn warmup_produces_no_signals() {
        let mut engine = Engine::new(EngineConfig::default());
        for i in 0..49 {
            let bar = steady_bar("AAPL", 100.0 + (i as f64 * 0.01), 1000.0);
            assert!(engine.on_bar(&bar).is_empty(), "no signal during warmup, bar {i}");
        }
    }

    #[test]
    fn big_drop_triggers_buy() {
        let config = EngineConfig {
            risk: RiskConfig { min_reward_cost_ratio: 0.0, ..Default::default() },
            exit: no_exit_config(),
            signal: mean_reversion::Config { trend_filter: false, ..Default::default() },
        };
        let mut engine = Engine::new(config);

        for _ in 0..55 {
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
            exit: no_exit_config(),
            ..Default::default()
        };
        let mut engine = Engine::new(config);

        engine.on_fill("AAPL", Side::Buy, 10.0, 100.0);
        engine.on_fill("AAPL", Side::Sell, 10.0, 85.0);
        assert!(engine.risk_state().killed);

        for _ in 0..55 {
            engine.on_bar(&steady_bar("TSLA", 100.0, 1000.0));
        }
        let crash = Bar {
            symbol: "TSLA".into(), timestamp: 0,
            open: 100.0, high: 100.0, low: 90.0, close: 91.0, volume: 3000.0,
        };
        assert!(engine.on_bar(&crash).is_empty(), "kill switch should block");
    }

    #[test]
    fn stop_loss_exits_position() {
        let config = EngineConfig {
            risk: RiskConfig { min_reward_cost_ratio: 0.0, ..Default::default() },
            exit: ExitConfig {
                stop_loss_pct: 0.02, // 2% stop
                stop_loss_atr_mult: 0.0,
                max_hold_bars: 0,
                take_profit_pct: 0.0,
            },
            signal: mean_reversion::Config { trend_filter: false, ..Default::default() },
        };
        let mut engine = Engine::new(config);

        // Warm up and trigger buy
        for _ in 0..55 {
            engine.on_bar(&steady_bar("AAPL", 100.0, 1000.0));
        }
        let crash = Bar {
            symbol: "AAPL".into(), timestamp: 0,
            open: 100.0, high: 100.0, low: 93.0, close: 94.0, volume: 2000.0,
        };
        let intents = engine.on_bar(&crash);
        assert_eq!(intents[0].side, Side::Buy);
        engine.on_fill("AAPL", Side::Buy, intents[0].qty, 94.0);

        // Price drops further — should trigger stop loss
        let drop = Bar {
            symbol: "AAPL".into(), timestamp: 0,
            open: 92.0, high: 92.0, low: 91.0, close: 91.0, volume: 1000.0,
        };
        let intents = engine.on_bar(&drop);
        assert!(!intents.is_empty(), "stop loss should fire");
        assert_eq!(intents[0].side, Side::Sell);
        assert_eq!(intents[0].reason, SignalReason::StopLoss);
    }

    #[test]
    fn max_hold_exits_position() {
        let config = EngineConfig {
            risk: RiskConfig { min_reward_cost_ratio: 0.0, ..Default::default() },
            exit: ExitConfig {
                stop_loss_pct: 0.0,
                stop_loss_atr_mult: 0.0,
                max_hold_bars: 5,
                take_profit_pct: 0.0,
            },
            ..Default::default()
        };
        let mut engine = Engine::new(config);

        // Simulate a fill
        engine.on_fill("AAPL", Side::Buy, 10.0, 100.0);

        // Feed bars until max hold
        for _ in 0..4 {
            assert!(engine.on_bar(&steady_bar("AAPL", 100.0, 1000.0)).is_empty());
        }
        let intents = engine.on_bar(&steady_bar("AAPL", 100.0, 1000.0));
        assert!(!intents.is_empty(), "max hold should fire");
        assert_eq!(intents[0].reason, SignalReason::MaxHoldTime);
    }

    #[test]
    fn take_profit_exits_position() {
        let config = EngineConfig {
            risk: RiskConfig { min_reward_cost_ratio: 0.0, ..Default::default() },
            exit: ExitConfig {
                stop_loss_pct: 0.0,
                stop_loss_atr_mult: 0.0,
                max_hold_bars: 0,
                take_profit_pct: 0.03, // 3%
            },
            ..Default::default()
        };
        let mut engine = Engine::new(config);

        engine.on_fill("AAPL", Side::Buy, 10.0, 100.0);

        // Price rises 4% — should take profit
        let rise = Bar {
            symbol: "AAPL".into(), timestamp: 0,
            open: 104.0, high: 104.5, low: 103.5, close: 104.0, volume: 1000.0,
        };
        let intents = engine.on_bar(&rise);
        assert!(!intents.is_empty(), "take profit should fire");
        assert_eq!(intents[0].reason, SignalReason::TakeProfit);
    }

    #[test]
    fn deterministic_replay() {
        let bars: Vec<Bar> = (0..60)
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
