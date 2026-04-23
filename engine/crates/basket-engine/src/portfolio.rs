//! Portfolio aggregation and order generation.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::engine::{BasketEngine, BasketParams};

/// Portfolio configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortfolioConfig {
    /// Total capital available (USD).
    pub capital: f64,
    /// Leverage multiplier (e.g., 4.0 for 4x).
    pub leverage: f64,
    /// Maximum number of active baskets.
    pub n_active_baskets: usize,
}

impl Default for PortfolioConfig {
    fn default() -> Self {
        Self {
            capital: 100_000.0,
            leverage: 4.0,
            n_active_baskets: 10,
        }
    }
}

impl PortfolioConfig {
    /// Notional per basket = (capital * leverage) / n_active_baskets.
    pub fn notional_per_basket(&self) -> f64 {
        (self.capital * self.leverage) / self.n_active_baskets as f64
    }
}

/// A leg's notional exposure.
#[derive(Debug, Clone)]
pub struct LegNotional {
    /// Symbol.
    pub symbol: String,
    /// Notional exposure (positive = long, negative = short).
    pub notional: f64,
}

/// Compute leg notionals for a basket position.
///
/// For a basket with target and N peers:
/// - Long basket (position = 1): long target, short each peer
/// - Short basket (position = -1): short target, long each peer
///
/// Notional is split: target gets 50%, peers split the other 50%.
pub fn basket_to_legs(params: &BasketParams, position: i8, notional: f64) -> Vec<LegNotional> {
    if position == 0 {
        return vec![];
    }

    let sign = position as f64;
    let n_peers = params.peers.len() as f64;

    // Target gets 50% of notional
    let target_notional = sign * notional * 0.5;
    // Each peer gets (50% / n_peers), opposite direction from target
    let peer_notional = -sign * notional * 0.5 / n_peers;

    let mut legs = Vec::with_capacity(1 + params.peers.len());

    legs.push(LegNotional {
        symbol: params.target.clone(),
        notional: target_notional,
    });

    for peer in &params.peers {
        legs.push(LegNotional {
            symbol: peer.clone(),
            notional: peer_notional,
        });
    }

    legs
}

/// Aggregate all basket positions into symbol-level notionals.
pub fn aggregate_positions(
    engine: &BasketEngine,
    config: &PortfolioConfig,
) -> Result<HashMap<String, f64>, String> {
    let active_count = engine
        .iter_params()
        .filter_map(|(basket_id, _)| engine.get_state(basket_id))
        .filter(|s| s.position != 0)
        .count();
    if active_count > config.n_active_baskets {
        return Err(format!(
            "active basket cap exceeded: active={}, configured_cap={}",
            active_count, config.n_active_baskets
        ));
    }

    let notional_per_basket = config.notional_per_basket();
    let mut symbol_notionals: HashMap<String, f64> = HashMap::new();

    for (basket_id, params) in engine.iter_params() {
        let state = match engine.get_state(basket_id) {
            Some(s) => s,
            None => continue,
        };

        if state.position == 0 {
            continue;
        }

        let legs = basket_to_legs(params, state.position, notional_per_basket);
        for leg in legs {
            *symbol_notionals.entry(leg.symbol).or_default() += leg.notional;
        }
    }

    Ok(symbol_notionals)
}

/// Order side.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Side {
    Buy,
    Sell,
}

/// Order reason for logging.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderReason {
    /// New basket entry.
    Entry { basket_id: String },
    /// Basket flip (reversal).
    Flip { basket_id: String },
    /// Rebalance due to price changes.
    Rebalance,
    /// Multiple basket changes aggregated.
    Aggregated,
}

/// An order intent to execute.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderIntent {
    /// Symbol to trade.
    pub symbol: String,
    /// Buy or sell.
    pub side: Side,
    /// Quantity (shares).
    pub qty: u32,
    /// Reason for the order.
    pub reason: OrderReason,
}

/// Convert target notionals into whole-share targets using current prices.
pub fn target_shares_from_notionals(
    target: &HashMap<String, f64>,
    prices: &HashMap<String, f64>,
) -> Result<HashMap<String, i64>, String> {
    let mut shares = HashMap::new();
    let mut invalid_symbols = Vec::new();
    for (symbol, target_notional) in target {
        let price = match prices.get(symbol) {
            Some(&p) if p.is_finite() && p > 0.0 => p,
            _ => {
                invalid_symbols.push(symbol.clone());
                continue;
            }
        };
        let qty = (target_notional / price).round() as i64;
        if qty != 0 {
            shares.insert(symbol.clone(), qty);
        }
    }
    if invalid_symbols.is_empty() {
        Ok(shares)
    } else {
        invalid_symbols.sort();
        Err(format!(
            "missing or invalid close for target share conversion: {}",
            invalid_symbols.join(", ")
        ))
    }
}

/// Compute orders needed to move from current shares to target shares.
pub fn diff_to_orders(
    current: &HashMap<String, i64>,
    target: &HashMap<String, i64>,
) -> Vec<OrderIntent> {
    let mut orders = Vec::new();

    // All symbols in either current or target
    let mut all_symbols: Vec<&String> = current.keys().chain(target.keys()).collect();
    all_symbols.sort();
    all_symbols.dedup();

    for symbol in all_symbols {
        let current_qty = current.get(symbol).copied().unwrap_or(0);
        let target_qty = target.get(symbol).copied().unwrap_or(0);
        let delta = target_qty - current_qty;
        if delta == 0 {
            continue;
        }

        let side = if delta > 0 { Side::Buy } else { Side::Sell };

        orders.push(OrderIntent {
            symbol: symbol.clone(),
            side,
            qty: delta.unsigned_abs() as u32,
            reason: OrderReason::Aggregated,
        });
    }

    // Sort for deterministic ordering
    orders.sort_by(|a, b| a.symbol.cmp(&b.symbol));
    orders
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DailyBar;
    use basket_picker::{BasketCandidate, BasketFit, BertramResult};
    use chrono::NaiveDate;

    fn make_test_params() -> BasketParams {
        BasketParams {
            basket_id: "test:AMD:2026-04-20:abc12345".to_string(),
            target: "AMD".to_string(),
            peers: vec!["NVDA".to_string(), "INTC".to_string()],
            ou: basket_picker::OuFit {
                a: 0.0,
                b: 0.95,
                kappa: 12.92,
                mu: 0.0,
                sigma: 0.01,
                sigma_eq: 0.032,
                half_life_days: 13.51,
            },
            threshold_k: 1.25,
        }
    }

    fn make_test_fit(target: &str, peers: &[&str]) -> BasketFit {
        BasketFit {
            candidate: BasketCandidate {
                target: target.to_string(),
                members: peers.iter().map(|s| s.to_string()).collect(),
                sector: "test".to_string(),
                fit_date: NaiveDate::from_ymd_opt(2026, 4, 20).unwrap(),
            },
            ou: Some(basket_picker::OuFit {
                a: 0.0,
                b: 0.95,
                kappa: 12.92,
                mu: 0.0,
                sigma: 0.01,
                sigma_eq: 0.032,
                half_life_days: 13.51,
            }),
            bertram: Some(BertramResult {
                a: -0.04,
                m: 0.04,
                k: 1.25,
                expected_return_rate: 0.1,
                expected_trade_length_days: 10.0,
                sigma_cont: 0.05,
            }),
            threshold_k: 1.25,
            valid: true,
            reject_reason: None,
        }
    }

    #[test]
    fn test_notional_per_basket() {
        let config = PortfolioConfig {
            capital: 100_000.0,
            leverage: 4.0,
            n_active_baskets: 10,
        };
        assert_eq!(config.notional_per_basket(), 40_000.0);
    }

    #[test]
    fn test_basket_to_legs_long() {
        let params = make_test_params();
        let legs = basket_to_legs(&params, 1, 10_000.0);

        assert_eq!(legs.len(), 3);
        // Target long 50%
        assert_eq!(legs[0].symbol, "AMD");
        assert!((legs[0].notional - 5000.0).abs() < 1e-6);
        // Each peer short 25%
        assert_eq!(legs[1].symbol, "NVDA");
        assert!((legs[1].notional - (-2500.0)).abs() < 1e-6);
        assert_eq!(legs[2].symbol, "INTC");
        assert!((legs[2].notional - (-2500.0)).abs() < 1e-6);
    }

    #[test]
    fn test_basket_to_legs_short() {
        let params = make_test_params();
        let legs = basket_to_legs(&params, -1, 10_000.0);

        assert_eq!(legs.len(), 3);
        // Target short 50%
        assert_eq!(legs[0].symbol, "AMD");
        assert!((legs[0].notional - (-5000.0)).abs() < 1e-6);
        // Each peer long 25%
        assert_eq!(legs[1].symbol, "NVDA");
        assert!((legs[1].notional - 2500.0).abs() < 1e-6);
    }

    #[test]
    fn test_basket_to_legs_flat() {
        let params = make_test_params();
        let legs = basket_to_legs(&params, 0, 10_000.0);
        assert!(legs.is_empty());
    }

    #[test]
    fn test_diff_to_orders() {
        let mut current: HashMap<String, i64> = HashMap::new();
        current.insert("AMD".to_string(), 50);

        let mut target: HashMap<String, i64> = HashMap::new();
        target.insert("AMD".to_string(), 30);
        target.insert("NVDA".to_string(), 10);

        let orders = diff_to_orders(&current, &target);

        assert_eq!(orders.len(), 2);
        // AMD: 3000 - 5000 = -2000, sell 20 shares
        let amd_order = orders.iter().find(|o| o.symbol == "AMD").unwrap();
        assert_eq!(amd_order.side, Side::Sell);
        assert_eq!(amd_order.qty, 20);
        // NVDA: 2000 - 0 = 2000, buy 10 shares
        let nvda_order = orders.iter().find(|o| o.symbol == "NVDA").unwrap();
        assert_eq!(nvda_order.side, Side::Buy);
        assert_eq!(nvda_order.qty, 10);
    }

    #[test]
    fn test_target_shares_from_notionals() {
        let mut target: HashMap<String, f64> = HashMap::new();
        target.insert("AMD".to_string(), 3000.0);
        target.insert("NVDA".to_string(), -2000.0);

        let mut prices: HashMap<String, f64> = HashMap::new();
        prices.insert("AMD".to_string(), 100.0);
        prices.insert("NVDA".to_string(), 200.0);

        let shares = target_shares_from_notionals(&target, &prices).unwrap();
        assert_eq!(shares.get("AMD"), Some(&30));
        assert_eq!(shares.get("NVDA"), Some(&-10));
    }

    #[test]
    fn test_target_shares_from_notionals_rejects_invalid_price() {
        let mut target: HashMap<String, f64> = HashMap::new();
        target.insert("AMD".to_string(), 3000.0);

        let prices: HashMap<String, f64> = HashMap::new();
        let err = target_shares_from_notionals(&target, &prices).unwrap_err();
        assert!(err.contains("AMD"));
    }

    #[test]
    fn test_aggregate_positions_rejects_cap_breach() {
        let fit_a = make_test_fit("AAA", &["BBB", "CCC"]);
        let fit_b = make_test_fit("DDD", &["EEE", "FFF"]);
        let mut engine = BasketEngine::new(&[fit_a.clone(), fit_b.clone()]);

        let bars = vec![
            DailyBar { symbol: "AAA".to_string(), date: NaiveDate::from_ymd_opt(2026, 4, 21).unwrap(), close: 90.0 },
            DailyBar { symbol: "BBB".to_string(), date: NaiveDate::from_ymd_opt(2026, 4, 21).unwrap(), close: 100.0 },
            DailyBar { symbol: "CCC".to_string(), date: NaiveDate::from_ymd_opt(2026, 4, 21).unwrap(), close: 100.0 },
            DailyBar { symbol: "DDD".to_string(), date: NaiveDate::from_ymd_opt(2026, 4, 21).unwrap(), close: 90.0 },
            DailyBar { symbol: "EEE".to_string(), date: NaiveDate::from_ymd_opt(2026, 4, 21).unwrap(), close: 100.0 },
            DailyBar { symbol: "FFF".to_string(), date: NaiveDate::from_ymd_opt(2026, 4, 21).unwrap(), close: 100.0 },
        ];
        engine.on_bars(&bars);

        let config = PortfolioConfig {
            capital: 100_000.0,
            leverage: 4.0,
            n_active_baskets: 1,
        };
        assert!(aggregate_positions(&engine, &config).is_err());
    }
}
