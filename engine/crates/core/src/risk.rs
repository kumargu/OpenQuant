/// Risk gates: last line of defense before an order goes out.
/// If any gate rejects, no trade happens.

use crate::signals::{Side, SignalOutput};

/// Risk configuration.
#[derive(Debug, Clone)]
pub struct RiskConfig {
    /// Max notional value per position (in USD). Default: 10_000
    pub max_position_notional: f64,
    /// Max daily loss before kill switch trips (in USD). Default: 500
    pub max_daily_loss: f64,
    /// Min expected move as multiple of estimated cost. Default: 3.0
    pub min_reward_cost_ratio: f64,
    /// Estimated round-trip cost as fraction of price. Default: 0.001 (10 bps)
    pub estimated_cost_bps: f64,
}

impl Default for RiskConfig {
    fn default() -> Self {
        Self {
            max_position_notional: 10_000.0,
            max_daily_loss: 500.0,
            min_reward_cost_ratio: 3.0,
            estimated_cost_bps: 0.001,
        }
    }
}

/// Mutable risk state that tracks daily P&L.
#[derive(Debug, Clone)]
pub struct RiskState {
    pub daily_pnl: f64,
    pub killed: bool,
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
            // Allow fractional quantities (crypto). Cap at 100 units for stocks.
            let desired = if price > 1000.0 {
                max_qty // fractional for expensive assets
            } else {
                max_qty.min(100.0).floor() // whole shares for cheaper ones
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
    use crate::signals::{Side, SignalOutput};

    fn buy_signal(score: f64) -> SignalOutput {
        SignalOutput {
            side: Side::Buy,
            score,
            reason: "test".into(),
        }
    }

    fn sell_signal(score: f64) -> SignalOutput {
        SignalOutput {
            side: Side::Sell,
            score,
            reason: "test".into(),
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
}
