//! Risk gates — last line of defense before an order goes out.
//!
//! Every signal must pass through risk checks before becoming an order.
//! If any gate rejects, no trade happens. Risk management is the main strategy.
//!
//! V1 gates:
//! - Kill switch: halt all trading if daily P&L exceeds max loss
//! - Cost filter: reject if signal isn't strong enough relative to costs
//! - Position sizing: cap notional exposure, allow fractional for expensive assets

use crate::signals::{Side, SignalOutput};

/// Bet sizing method.
#[derive(Debug, Clone, Copy, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BetSizingMethod {
    /// Fixed sizing: qty = max_notional / price (current behavior).
    #[default]
    Linear,
    /// Sigmoid scaling: qty = max_notional / price × sigmoid(score).
    /// Higher conviction signals get larger positions.
    Sigmoid,
}

/// Risk configuration.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct RiskConfig {
    /// Max notional value per position (in USD). Default: 10_000
    pub max_position_notional: f64,
    /// Max daily loss before kill switch trips (in USD). Default: 500
    pub max_daily_loss: f64,
    /// Min expected move as multiple of estimated cost. Default: 3.0
    pub min_reward_cost_ratio: f64,
    /// Estimated round-trip cost as fraction of price. Default: 0.001 (10 bps)
    pub estimated_cost_bps: f64,
    /// Bet sizing method: "linear" (fixed) or "sigmoid" (score-scaled). Default: linear
    pub bet_sizing: BetSizingMethod,
    /// Sigmoid steepness — higher = sharper transition. Default: 10.0
    pub sigmoid_slope: f64,
    /// Sigmoid center — score at which sizing = 50% of max. Default: 0.5
    pub sigmoid_center: f64,
}

impl Default for RiskConfig {
    fn default() -> Self {
        Self {
            max_position_notional: 10_000.0,
            max_daily_loss: 500.0,
            min_reward_cost_ratio: 3.0,
            estimated_cost_bps: 0.001,
            bet_sizing: BetSizingMethod::Linear,
            sigmoid_slope: 10.0,
            sigmoid_center: 0.5,
        }
    }
}

/// Sigmoid function mapping score to [0, 1] confidence.
/// sigmoid(score) = 1 / (1 + exp(-slope * (score - center)))
fn sigmoid(score: f64, slope: f64, center: f64) -> f64 {
    1.0 / (1.0 + (-slope * (score - center)).exp())
}

/// Mutable risk state that tracks daily P&L.
#[derive(Debug, Clone)]
pub struct RiskState {
    pub daily_pnl: f64,
    pub killed: bool,
}

impl Default for RiskState {
    fn default() -> Self {
        Self::new()
    }
}

impl RiskState {
    pub fn new() -> Self {
        Self {
            daily_pnl: 0.0,
            killed: false,
        }
    }

    /// Reset at start of new trading day.
    pub fn reset_daily(&mut self) {
        self.daily_pnl = 0.0;
        self.killed = false;
    }

    /// Record realized P&L from a closed trade.
    pub fn record_pnl(&mut self, pnl: f64, config: &RiskConfig) {
        self.daily_pnl += pnl;
        if self.daily_pnl < -config.max_daily_loss {
            self.killed = true;
        }
    }
}

/// Reason a trade was rejected.
#[derive(Debug, Clone)]
pub struct Rejection {
    pub reason: String,
}

/// Check all risk gates. Returns Ok(qty) if approved, Err(Rejection) if blocked.
pub fn check(
    signal: &SignalOutput,
    price: f64,
    current_position_qty: f64,
    risk_state: &RiskState,
    config: &RiskConfig,
) -> Result<f64, Rejection> {
    // Gate 1: Kill switch
    if risk_state.killed {
        return Err(Rejection {
            reason: format!(
                "kill switch: daily P&L {:.2} exceeded max loss {:.2}",
                risk_state.daily_pnl, config.max_daily_loss
            ),
        });
    }

    // Gate 2: Cost filter — signal must be strong enough relative to cost
    if config.min_reward_cost_ratio > 0.0 {
        let round_trip_cost_pct = config.estimated_cost_bps * 2.0;
        // Signal score roughly proxies expected move magnitude
        // Reject if score isn't meaningfully above the cost drag
        if signal.score < round_trip_cost_pct * config.min_reward_cost_ratio {
            return Err(Rejection {
                reason: format!(
                    "cost filter: score {:.4} < min required {:.4}",
                    signal.score,
                    round_trip_cost_pct * config.min_reward_cost_ratio,
                ),
            });
        }
    }

    // Gate 3: Position sizing — compute qty within notional limit
    let qty = match signal.side {
        Side::Buy => {
            let max_qty = config.max_position_notional / price;

            // Apply bet sizing method
            let scale = match config.bet_sizing {
                BetSizingMethod::Linear => 1.0,
                BetSizingMethod::Sigmoid => {
                    sigmoid(signal.score, config.sigmoid_slope, config.sigmoid_center)
                }
            };
            let scaled_qty = max_qty * scale;

            // Allow fractional quantities (crypto). Cap at 100 units for stocks.
            let desired = if price > 1000.0 {
                scaled_qty // fractional for expensive assets
            } else {
                scaled_qty.min(100.0).floor() // whole shares for cheaper ones
            };
            if desired <= 0.0 {
                return Err(Rejection {
                    reason: format!("position size: price {price:.2} exceeds max notional"),
                });
            }
            desired
        }
        Side::Sell => {
            // Sell what we hold
            if current_position_qty <= 0.0 {
                return Err(Rejection {
                    reason: "sell rejected: no position to sell".into(),
                });
            }
            current_position_qty
        }
    };

    Ok(qty)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signals::{Side, SignalOutput, SignalReason};

    fn buy_signal(score: f64) -> SignalOutput {
        SignalOutput {
            side: Side::Buy,
            score,
            reason: SignalReason::MeanReversionBuy,
            z_score: -3.0,
            relative_volume: 1.5,
            votes: String::new(),
        }
    }

    fn sell_signal(score: f64) -> SignalOutput {
        SignalOutput {
            side: Side::Sell,
            score,
            reason: SignalReason::MeanReversionSell,
            z_score: 3.0,
            relative_volume: 1.5,
            votes: String::new(),
        }
    }

    fn no_cost_config() -> RiskConfig {
        RiskConfig {
            min_reward_cost_ratio: 0.0, // disable cost filter for unit tests
            ..Default::default()
        }
    }

    #[test]
    fn test_kill_switch_blocks() {
        let mut state = RiskState::new();
        let config = no_cost_config();
        state.record_pnl(-600.0, &config);
        assert!(state.killed);

        let result = check(&buy_signal(1.0), 100.0, 0.0, &state, &config);
        assert!(result.is_err());
        assert!(result.unwrap_err().reason.contains("kill switch"));
    }

    #[test]
    fn test_position_sizing() {
        let state = RiskState::new();
        let config = RiskConfig {
            max_position_notional: 5_000.0,
            min_reward_cost_ratio: 0.0,
            ..Default::default()
        };
        let result = check(&buy_signal(2.0), 100.0, 0.0, &state, &config);
        assert!(result.is_ok());
        let qty = result.unwrap();
        assert!(qty <= 50.0); // 5000 / 100 = 50
        assert!(qty > 0.0);
    }

    #[test]
    fn test_sell_uses_position_qty() {
        let state = RiskState::new();
        let config = no_cost_config();
        let result = check(&sell_signal(1.0), 100.0, 25.0, &state, &config);
        assert_eq!(result.unwrap(), 25.0);
    }

    #[test]
    fn test_sell_rejected_no_position() {
        let state = RiskState::new();
        let config = RiskConfig::default();
        let result = check(&sell_signal(1.0), 100.0, 0.0, &state, &config);
        assert!(result.is_err());
    }

    #[test]
    fn test_daily_reset() {
        let mut state = RiskState::new();
        let config = RiskConfig::default();
        state.record_pnl(-600.0, &config);
        assert!(state.killed);
        state.reset_daily();
        assert!(!state.killed);
        assert_eq!(state.daily_pnl, 0.0);
    }

    #[test]
    fn test_sigmoid_function() {
        // At center, sigmoid = 0.5
        assert!((sigmoid(0.5, 10.0, 0.5) - 0.5).abs() < 1e-10);
        // Well above center → close to 1.0
        assert!(sigmoid(1.0, 10.0, 0.5) > 0.99);
        // Well below center → close to 0.0
        assert!(sigmoid(0.0, 10.0, 0.5) < 0.01);
        // Higher slope = sharper transition
        assert!(sigmoid(0.6, 20.0, 0.5) > sigmoid(0.6, 5.0, 0.5));
    }

    #[test]
    fn test_sigmoid_bet_sizing_scales_qty() {
        let state = RiskState::new();
        let config = RiskConfig {
            max_position_notional: 10_000.0,
            min_reward_cost_ratio: 0.0,
            bet_sizing: BetSizingMethod::Sigmoid,
            sigmoid_slope: 10.0,
            sigmoid_center: 0.5,
            ..Default::default()
        };

        // High score → large position
        let high = check(&buy_signal(1.0), 100.0, 0.0, &state, &config).unwrap();
        // Low score → small position (but above cost filter since ratio=0)
        let low = check(&buy_signal(0.1), 100.0, 0.0, &state, &config).unwrap();

        assert!(
            high > low,
            "high score ({high}) should get larger position than low score ({low})"
        );
        // Full linear qty would be 100 (10_000/100). Sigmoid at 1.0 is ~0.993 → ~99
        assert!(
            high > 90.0,
            "high conviction should be near max, got {high}"
        );
    }

    #[test]
    fn test_linear_bet_sizing_ignores_score() {
        let state = RiskState::new();
        let config = RiskConfig {
            max_position_notional: 10_000.0,
            min_reward_cost_ratio: 0.0,
            bet_sizing: BetSizingMethod::Linear,
            ..Default::default()
        };

        let high = check(&buy_signal(1.0), 100.0, 0.0, &state, &config).unwrap();
        let low = check(&buy_signal(0.1), 100.0, 0.0, &state, &config).unwrap();
        assert_eq!(
            high, low,
            "linear sizing should give same qty regardless of score"
        );
    }
}
