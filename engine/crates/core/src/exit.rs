//! Exit rules — manage open positions independently of entry signals.
//!
//! Exit rules run on every bar for every open position. They protect
//! capital and enforce discipline. The engine checks these BEFORE
//! checking for new entry signals.
//!
//! ```text
//!  on_bar():
//!    for each open position:
//!      ├── stop loss hit?      → emit Sell (cut loss)
//!      ├── max hold exceeded?  → emit Sell (time exit)
//!      └── take profit hit?    → emit Sell (lock gain)
//!    then:
//!      └── check strategy for new entries
//! ```
//!
//! V1 exit rules:
//! - Stop loss: close if price drops X% below entry
//! - Max hold: close after N bars regardless
//! - Take profit: close if price rises X% above entry

use crate::signals::{Side, SignalReason};
use crate::engine::OrderIntent;

/// Exit rule configuration.
#[derive(Debug, Clone)]
pub struct ExitConfig {
    /// Close position if price drops this % below entry. 0.0 = disabled.
    /// Example: 0.02 = 2% stop loss.
    pub stop_loss_pct: f64,

    /// Close position after this many bars. 0 = disabled.
    pub max_hold_bars: usize,

    /// Close position if price rises this % above entry. 0.0 = disabled.
    /// Example: 0.03 = 3% take profit.
    pub take_profit_pct: f64,
}

impl Default for ExitConfig {
    fn default() -> Self {
        Self {
            stop_loss_pct: 0.02,    // 2% stop loss
            max_hold_bars: 100,     // ~8 hours on 5-min bars
            take_profit_pct: 0.0,   // disabled by default
        }
    }
}

/// Metadata for an open position tracked by the engine.
#[derive(Debug, Clone)]
pub struct OpenPosition {
    pub symbol: String,
    pub entry_price: f64,
    pub qty: f64,
    pub entry_bar: usize,
}

/// Check if an open position should be exited.
/// Returns Some(OrderIntent) if an exit rule fires, None otherwise.
pub fn check(
    pos: &OpenPosition,
    current_price: f64,
    current_bar: usize,
    config: &ExitConfig,
) -> Option<OrderIntent> {
    let bars_held = current_bar.saturating_sub(pos.entry_bar);
    let return_pct = (current_price - pos.entry_price) / pos.entry_price;

    // Stop loss — cut the loss
    if config.stop_loss_pct > 0.0 && return_pct < -config.stop_loss_pct {
        return Some(OrderIntent {
            symbol: pos.symbol.clone(),
            side: Side::Sell,
            qty: pos.qty,
            reason: SignalReason::StopLoss,
            signal_score: 0.0,
            z_score: 0.0,
            relative_volume: 0.0,
        });
    }

    // Take profit — lock the gain
    if config.take_profit_pct > 0.0 && return_pct > config.take_profit_pct {
        return Some(OrderIntent {
            symbol: pos.symbol.clone(),
            side: Side::Sell,
            qty: pos.qty,
            reason: SignalReason::TakeProfit,
            signal_score: 0.0,
            z_score: 0.0,
            relative_volume: 0.0,
        });
    }

    // Max hold — time exit
    if config.max_hold_bars > 0 && bars_held >= config.max_hold_bars {
        return Some(OrderIntent {
            symbol: pos.symbol.clone(),
            side: Side::Sell,
            qty: pos.qty,
            reason: SignalReason::MaxHoldTime,
            signal_score: 0.0,
            z_score: 0.0,
            relative_volume: 0.0,
        });
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pos(entry_price: f64, entry_bar: usize) -> OpenPosition {
        OpenPosition {
            symbol: "TEST".into(),
            entry_price,
            qty: 10.0,
            entry_bar,
        }
    }

    fn default_config() -> ExitConfig {
        ExitConfig {
            stop_loss_pct: 0.02,
            max_hold_bars: 100,
            take_profit_pct: 0.03,
        }
    }

    // --- Stop loss ---

    #[test]
    fn stop_loss_fires_when_price_drops_below_threshold() {
        let p = pos(100.0, 0);
        // Price dropped 3% (below 2% stop)
        let exit = check(&p, 97.0, 5, &default_config());
        assert!(exit.is_some());
        let exit = exit.unwrap();
        assert_eq!(exit.side, Side::Sell);
        assert_eq!(exit.reason, SignalReason::StopLoss);
        assert_eq!(exit.qty, 10.0);
    }

    #[test]
    fn stop_loss_does_not_fire_within_threshold() {
        let p = pos(100.0, 0);
        // Price dropped 1% (within 2% stop)
        assert!(check(&p, 99.0, 5, &default_config()).is_none());
    }

    #[test]
    fn stop_loss_does_not_fire_when_disabled() {
        let p = pos(100.0, 0);
        let config = ExitConfig { stop_loss_pct: 0.0, ..default_config() };
        assert!(check(&p, 90.0, 5, &config).is_none());
    }

    #[test]
    fn stop_loss_at_exact_threshold_does_not_fire() {
        let p = pos(100.0, 0);
        // Price dropped exactly 2% — not less than -2%, so should not fire
        assert!(check(&p, 98.0, 5, &default_config()).is_none());
    }

    // --- Take profit ---

    #[test]
    fn take_profit_fires_when_price_rises_above_threshold() {
        let p = pos(100.0, 0);
        // Price rose 4% (above 3% take profit)
        let exit = check(&p, 104.0, 5, &default_config());
        assert!(exit.is_some());
        assert_eq!(exit.unwrap().reason, SignalReason::TakeProfit);
    }

    #[test]
    fn take_profit_does_not_fire_within_threshold() {
        let p = pos(100.0, 0);
        assert!(check(&p, 102.0, 5, &default_config()).is_none());
    }

    #[test]
    fn take_profit_does_not_fire_when_disabled() {
        let p = pos(100.0, 0);
        let config = ExitConfig { take_profit_pct: 0.0, ..default_config() };
        assert!(check(&p, 200.0, 5, &config).is_none());
    }

    // --- Max hold ---

    #[test]
    fn max_hold_fires_after_n_bars() {
        let p = pos(100.0, 0);
        let exit = check(&p, 100.0, 100, &default_config());
        assert!(exit.is_some());
        assert_eq!(exit.unwrap().reason, SignalReason::MaxHoldTime);
    }

    #[test]
    fn max_hold_does_not_fire_before_n_bars() {
        let p = pos(100.0, 0);
        assert!(check(&p, 100.0, 99, &default_config()).is_none());
    }

    #[test]
    fn max_hold_does_not_fire_when_disabled() {
        let p = pos(100.0, 0);
        let config = ExitConfig { max_hold_bars: 0, ..default_config() };
        assert!(check(&p, 100.0, 9999, &config).is_none());
    }

    // --- Priority ---

    #[test]
    fn stop_loss_takes_priority_over_max_hold() {
        let p = pos(100.0, 0);
        // Both stop loss and max hold would fire
        let exit = check(&p, 95.0, 200, &default_config());
        assert_eq!(exit.unwrap().reason, SignalReason::StopLoss);
    }

    #[test]
    fn take_profit_takes_priority_over_max_hold() {
        let p = pos(100.0, 0);
        let exit = check(&p, 105.0, 200, &default_config());
        assert_eq!(exit.unwrap().reason, SignalReason::TakeProfit);
    }

    // --- No exit ---

    #[test]
    fn no_exit_when_price_flat_and_within_hold() {
        let p = pos(100.0, 10);
        assert!(check(&p, 100.5, 50, &default_config()).is_none());
    }

    #[test]
    fn all_rules_disabled() {
        let p = pos(100.0, 0);
        let config = ExitConfig {
            stop_loss_pct: 0.0,
            max_hold_bars: 0,
            take_profit_pct: 0.0,
        };
        assert!(check(&p, 50.0, 9999, &config).is_none());
    }
}
