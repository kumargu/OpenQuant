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

use crate::engine::OrderIntent;
use crate::signals::{Side, SignalReason};

/// Exit rule configuration.
#[derive(Debug, Clone)]
pub struct ExitConfig {
    /// Close position if price drops this % below entry. 0.0 = disabled.
    /// Example: 0.02 = 2% stop loss.
    /// Ignored when stop_loss_atr_mult > 0 (ATR-based stop takes precedence).
    pub stop_loss_pct: f64,

    /// ATR multiplier for dynamic stop loss. 0.0 = disabled (use fixed %).
    /// Example: 2.5 means stop = entry_price - 2.5 * ATR.
    /// When enabled, the stop adapts to volatility — wider in volatile markets.
    pub stop_loss_atr_mult: f64,

    /// Close position after this many bars. 0 = disabled.
    pub max_hold_bars: usize,

    /// Close position if price rises this % above entry. 0.0 = disabled.
    /// Example: 0.03 = 3% take profit.
    pub take_profit_pct: f64,
}

impl Default for ExitConfig {
    fn default() -> Self {
        Self {
            stop_loss_pct: 0.0,      // disabled — ATR stop is preferred
            stop_loss_atr_mult: 2.5, // 2.5x ATR dynamic stop
            max_hold_bars: 100,      // ~8 hours on 5-min bars
            take_profit_pct: 0.0,    // disabled by default
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
/// `atr` is the current Average True Range — used when stop_loss_atr_mult > 0.
pub fn check(
    pos: &OpenPosition,
    current_price: f64,
    current_bar: usize,
    atr: f64,
    config: &ExitConfig,
) -> Option<OrderIntent> {
    let bars_held = current_bar.saturating_sub(pos.entry_bar);
    let return_pct = (current_price - pos.entry_price) / pos.entry_price;

    // Stop loss — ATR-based dynamic stop takes precedence over fixed %
    if config.stop_loss_atr_mult > 0.0 && atr > 0.0 {
        let stop_price = pos.entry_price - (atr * config.stop_loss_atr_mult);
        if current_price < stop_price {
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
    } else if config.stop_loss_pct > 0.0 && return_pct < -config.stop_loss_pct {
        // Fallback: fixed percentage stop
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

    fn fixed_stop_config() -> ExitConfig {
        ExitConfig {
            stop_loss_pct: 0.02,
            stop_loss_atr_mult: 0.0, // disable ATR stop for fixed % tests
            max_hold_bars: 100,
            take_profit_pct: 0.03,
        }
    }

    // --- Fixed % stop loss ---

    #[test]
    fn stop_loss_fires_when_price_drops_below_threshold() {
        let p = pos(100.0, 0);
        // Price dropped 3% (below 2% stop)
        let exit = check(&p, 97.0, 5, 0.0, &fixed_stop_config());
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
        assert!(check(&p, 99.0, 5, 0.0, &fixed_stop_config()).is_none());
    }

    #[test]
    fn stop_loss_does_not_fire_when_disabled() {
        let p = pos(100.0, 0);
        let config = ExitConfig {
            stop_loss_pct: 0.0,
            stop_loss_atr_mult: 0.0,
            ..fixed_stop_config()
        };
        assert!(check(&p, 90.0, 5, 0.0, &config).is_none());
    }

    #[test]
    fn stop_loss_at_exact_threshold_does_not_fire() {
        let p = pos(100.0, 0);
        // Price dropped exactly 2% — not less than -2%, so should not fire
        assert!(check(&p, 98.0, 5, 0.0, &fixed_stop_config()).is_none());
    }

    // --- ATR-based stop loss ---

    #[test]
    fn atr_stop_fires_when_price_below_atr_band() {
        let p = pos(100.0, 0);
        let config = ExitConfig {
            stop_loss_pct: 0.0,
            stop_loss_atr_mult: 2.0,
            max_hold_bars: 0,
            take_profit_pct: 0.0,
        };
        // ATR = 1.5, mult = 2.0 → stop at 100 - 3.0 = 97.0
        // Price 96.0 < 97.0 → should fire
        let exit = check(&p, 96.0, 5, 1.5, &config);
        assert!(exit.is_some());
        assert_eq!(exit.unwrap().reason, SignalReason::StopLoss);
    }

    #[test]
    fn atr_stop_does_not_fire_within_band() {
        let p = pos(100.0, 0);
        let config = ExitConfig {
            stop_loss_pct: 0.0,
            stop_loss_atr_mult: 2.0,
            max_hold_bars: 0,
            take_profit_pct: 0.0,
        };
        // ATR = 1.5, mult = 2.0 → stop at 97.0, price 98.0 is safe
        assert!(check(&p, 98.0, 5, 1.5, &config).is_none());
    }

    #[test]
    fn atr_stop_overrides_fixed_pct() {
        let p = pos(100.0, 0);
        let config = ExitConfig {
            stop_loss_pct: 0.02,     // 2% → stop at 98.0
            stop_loss_atr_mult: 3.0, // ATR=1.5, 3x → stop at 95.5
            max_hold_bars: 0,
            take_profit_pct: 0.0,
        };
        // Price 97.0 would trigger fixed 2% but ATR stop is at 95.5 → no exit
        assert!(check(&p, 97.0, 5, 1.5, &config).is_none());
        // Price 95.0 < 95.5 → should fire
        assert!(check(&p, 95.0, 5, 1.5, &config).is_some());
    }

    // --- Take profit ---

    #[test]
    fn take_profit_fires_when_price_rises_above_threshold() {
        let p = pos(100.0, 0);
        // Price rose 4% (above 3% take profit)
        let exit = check(&p, 104.0, 5, 0.0, &fixed_stop_config());
        assert!(exit.is_some());
        assert_eq!(exit.unwrap().reason, SignalReason::TakeProfit);
    }

    #[test]
    fn take_profit_does_not_fire_within_threshold() {
        let p = pos(100.0, 0);
        assert!(check(&p, 102.0, 5, 0.0, &fixed_stop_config()).is_none());
    }

    #[test]
    fn take_profit_does_not_fire_when_disabled() {
        let p = pos(100.0, 0);
        let config = ExitConfig {
            take_profit_pct: 0.0,
            ..fixed_stop_config()
        };
        assert!(check(&p, 200.0, 5, 0.0, &config).is_none());
    }

    // --- Max hold ---

    #[test]
    fn max_hold_fires_after_n_bars() {
        let p = pos(100.0, 0);
        let exit = check(&p, 100.0, 100, 0.0, &fixed_stop_config());
        assert!(exit.is_some());
        assert_eq!(exit.unwrap().reason, SignalReason::MaxHoldTime);
    }

    #[test]
    fn max_hold_does_not_fire_before_n_bars() {
        let p = pos(100.0, 0);
        assert!(check(&p, 100.0, 99, 0.0, &fixed_stop_config()).is_none());
    }

    #[test]
    fn max_hold_does_not_fire_when_disabled() {
        let p = pos(100.0, 0);
        let config = ExitConfig {
            max_hold_bars: 0,
            ..fixed_stop_config()
        };
        assert!(check(&p, 100.0, 9999, 0.0, &config).is_none());
    }

    // --- Priority ---

    #[test]
    fn stop_loss_takes_priority_over_max_hold() {
        let p = pos(100.0, 0);
        // Both stop loss and max hold would fire
        let exit = check(&p, 95.0, 200, 0.0, &fixed_stop_config());
        assert_eq!(exit.unwrap().reason, SignalReason::StopLoss);
    }

    #[test]
    fn take_profit_takes_priority_over_max_hold() {
        let p = pos(100.0, 0);
        let exit = check(&p, 105.0, 200, 0.0, &fixed_stop_config());
        assert_eq!(exit.unwrap().reason, SignalReason::TakeProfit);
    }

    // --- No exit ---

    #[test]
    fn no_exit_when_price_flat_and_within_hold() {
        let p = pos(100.0, 10);
        assert!(check(&p, 100.5, 50, 0.0, &fixed_stop_config()).is_none());
    }

    #[test]
    fn all_rules_disabled() {
        let p = pos(100.0, 0);
        let config = ExitConfig {
            stop_loss_pct: 0.0,
            stop_loss_atr_mult: 0.0,
            max_hold_bars: 0,
            take_profit_pct: 0.0,
        };
        assert!(check(&p, 50.0, 9999, 0.0, &config).is_none());
    }
}
