//! Portfolio aggregation and order generation.

use std::collections::{HashMap, HashSet};

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
    /// Adverse-move stop-loss in z-units. `None` disables (pure
    /// Bertram symmetric).
    ///
    /// Default is `None`. Lab swept `[1.5..4.0]` over 49 baskets
    /// / 9 sectors and found 2.0 best — but Q4 2025 A/B with the
    /// OQ replay setup (cap=5, single-basket-per-target) showed
    /// stop@2.0 makes things WORSE: cum_return=-9.7% vs no-stop
    /// -2.5%, Sharpe -1.67 vs -0.54. Crystallizing the loss + re-
    /// entering at adverse z added losses on top of the unrealized
    /// drawdown that would have eventually mean-reverted. The cap
    /// concentrates positions enough that the lab's diversification
    /// argument doesn't carry over. Operators can opt in via
    /// `--stop-loss-z N` once the cap/diversification mismatch is
    /// understood.
    #[serde(default = "default_stop_loss_z")]
    pub stop_loss_z: Option<f64>,
}

fn default_stop_loss_z() -> Option<f64> {
    None
}

impl Default for PortfolioConfig {
    fn default() -> Self {
        Self {
            capital: 100_000.0,
            leverage: 4.0,
            n_active_baskets: 10,
            stop_loss_z: default_stop_loss_z(),
        }
    }
}

impl PortfolioConfig {
    /// Validate portfolio sizing inputs before live use.
    pub fn validate(&self) -> Result<(), String> {
        if !self.capital.is_finite() || self.capital <= 0.0 {
            return Err(format!(
                "portfolio capital must be positive, got {}",
                self.capital
            ));
        }
        if !self.leverage.is_finite() || self.leverage <= 0.0 {
            return Err(format!(
                "portfolio leverage must be positive, got {}",
                self.leverage
            ));
        }
        if self.n_active_baskets == 0 {
            return Err("portfolio n_active_baskets must be at least 1".to_string());
        }
        Ok(())
    }

    /// Fraction of buying power the strategy is willing to deploy.
    /// 0.90 = leave a 10% safety buffer so per-symbol rounding,
    /// price drift between plan and submit, and the per-basket
    /// allocation skew don't push the actual submitted gross over
    /// the broker's `equity × leverage` cap. Without this buffer,
    /// borderline orders get rejected and the hedge book ends up
    /// lopsided — a feedback-loop bug we observed in Q4 2024.
    pub const BUYING_POWER_UTILIZATION: f64 = 0.90;

    /// Notional per basket sized off the static `capital` field.
    /// **Live use should prefer [`Self::notional_per_basket_for_equity`]**
    /// — initial capital ignores wins (under-sizes after rallies)
    /// and ignores losses (over-sizes after drawdowns, causing the
    /// rejection feedback loop). This method is kept for tests and
    /// as a fallback when live equity isn't yet available.
    pub fn notional_per_basket(&self) -> f64 {
        self.notional_per_basket_for_equity(self.capital)
    }

    /// Notional per basket sized off **current equity** (queried from
    /// the broker each session) instead of initial capital. Applies
    /// the [`BUYING_POWER_UTILIZATION`] buffer so the per-symbol sum
    /// of gross exposures stays under the broker's cap even after
    /// rounding to whole shares.
    ///
    /// `equity` should be the broker's reported account equity at
    /// session-close time — the same value the broker uses internally
    /// to compute `buying_power = equity × leverage`.
    pub fn notional_per_basket_for_equity(&self, equity: f64) -> f64 {
        // Floor at zero — a drawdown to negative equity (impossible
        // with the current broker but worth defending) shouldn't
        // produce a negative notional.
        let safe_equity = equity.max(0.0);
        (safe_equity * self.leverage * Self::BUYING_POWER_UTILIZATION)
            / self.n_active_baskets as f64
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

/// Portfolio admission result after applying active-basket caps.
#[derive(Debug, Clone)]
pub struct PortfolioPlan {
    /// Symbol-level target notionals for admitted baskets only.
    pub symbol_notionals: HashMap<String, f64>,
    /// Basket ids admitted into the portfolio target set.
    pub selected_baskets: Vec<String>,
    /// Active basket ids excluded by the cap.
    pub excluded_baskets: Vec<String>,
    /// Number of non-flat baskets seen before applying the cap.
    pub active_baskets: usize,
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

/// Aggregate all basket positions into symbol-level notionals after applying
/// the configured active-basket cap.
///
/// Sized off `config.capital` (initial). Use [`plan_portfolio_for_equity`]
/// in production to size off current account equity instead — that's the
/// path that avoids the buying-power-rejection feedback loop.
pub fn plan_portfolio(engine: &BasketEngine, config: &PortfolioConfig) -> PortfolioPlan {
    plan_portfolio_for_equity(engine, config, config.capital)
}

/// Same as [`plan_portfolio`] but sizes per-basket notional off the
/// supplied `equity` instead of `config.capital`. Live/paper/replay
/// callers should pass the broker's current equity at session-close
/// time; tests and one-off experiments may pass `config.capital`.
pub fn plan_portfolio_for_equity(
    engine: &BasketEngine,
    config: &PortfolioConfig,
    equity: f64,
) -> PortfolioPlan {
    let notional_per_basket = config.notional_per_basket_for_equity(equity);
    let mut symbol_notionals: HashMap<String, f64> = HashMap::new();
    let mut active: Vec<(String, f64)> = Vec::new();

    for (basket_id, _params) in engine.iter_params() {
        let state = match engine.get_state(basket_id) {
            Some(s) => s,
            None => continue,
        };

        if state.position == 0 {
            continue;
        }
        active.push((basket_id.clone(), state.last_z.unwrap_or(0.0).abs()));
    }

    active.sort_by(|(a_id, a_z), (b_id, b_z)| {
        b_z.partial_cmp(a_z)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a_id.cmp(b_id))
    });

    let active_baskets = active.len();
    let selected_baskets: Vec<String> = active
        .iter()
        .take(config.n_active_baskets)
        .map(|(basket_id, _)| basket_id.clone())
        .collect();
    let excluded_baskets: Vec<String> = active
        .iter()
        .skip(config.n_active_baskets)
        .map(|(basket_id, _)| basket_id.clone())
        .collect();
    debug_assert!(
        selected_baskets.len() <= config.n_active_baskets,
        "admission cap violated: selected {} > cap {}",
        selected_baskets.len(),
        config.n_active_baskets
    );
    debug_assert_eq!(
        selected_baskets.len() + excluded_baskets.len(),
        active_baskets,
        "selected + excluded must equal active baskets"
    );
    let selected: HashSet<&str> = selected_baskets.iter().map(|s| s.as_str()).collect();

    for (basket_id, params) in engine.iter_params() {
        if !selected.contains(basket_id.as_str()) {
            continue;
        }
        let state = match engine.get_state(basket_id) {
            Some(s) => s,
            None => continue,
        };
        let legs = basket_to_legs(params, state.position, notional_per_basket);
        for leg in legs {
            *symbol_notionals.entry(leg.symbol).or_default() += leg.notional;
        }
    }

    PortfolioPlan {
        symbol_notionals,
        selected_baskets,
        excluded_baskets,
        active_baskets,
    }
}

/// Aggregate all basket positions into symbol-level notionals after applying
/// active-basket admission.
pub fn aggregate_positions(
    engine: &BasketEngine,
    config: &PortfolioConfig,
) -> HashMap<String, f64> {
    plan_portfolio(engine, config).symbol_notionals
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

/// Compute orders needed to move from current to target positions.
///
/// Takes current shares and target shares.
/// Returns the orders needed to reach target.
pub fn diff_to_orders(
    current: &HashMap<String, f64>,
    target: &HashMap<String, f64>,
) -> Vec<OrderIntent> {
    let mut orders = Vec::new();

    // All symbols in either current or target
    let mut all_symbols: Vec<&String> = current.keys().chain(target.keys()).collect();
    all_symbols.sort();
    all_symbols.dedup();

    for symbol in all_symbols {
        let current_shares = current.get(symbol).copied().unwrap_or(0.0);
        let target_shares = target.get(symbol).copied().unwrap_or(0.0);
        debug_assert!(
            current_shares.is_finite() && target_shares.is_finite(),
            "non-finite shares for {symbol}: current={current_shares} target={target_shares}"
        );
        let delta = target_shares - current_shares;
        debug_assert!(
            delta.is_finite(),
            "non-finite share delta for {symbol}: {delta}"
        );
        let qty = delta.abs().round() as u32;
        if qty == 0 {
            continue;
        }
        debug_assert!(qty > 0, "zero qty slipped past the filter for {symbol}");

        let side = if delta > 0.0 { Side::Buy } else { Side::Sell };

        orders.push(OrderIntent {
            symbol: symbol.clone(),
            side,
            qty,
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
    use crate::engine::BasketEngine;
    use crate::DailyBar;
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

    fn make_test_engine() -> BasketEngine {
        let fit = basket_picker::BasketFit {
            candidate: basket_picker::BasketCandidate {
                sector: "semi".to_string(),
                target: "AMD".to_string(),
                members: vec!["NVDA".to_string(), "INTC".to_string()],
                fit_date: NaiveDate::from_ymd_opt(2026, 4, 20).unwrap(),
            },
            valid: true,
            ou: Some(basket_picker::OuFit {
                a: 0.0,
                b: 0.95,
                kappa: 12.92,
                mu: 0.0,
                sigma: 0.01,
                sigma_eq: 0.032,
                half_life_days: 13.51,
            }),
            bertram: Some(basket_picker::BertramResult {
                a: -0.04,
                m: 0.04,
                k: 1.25,
                expected_return_rate: 0.1,
                expected_trade_length_days: 5.0,
                sigma_cont: 0.1,
            }),
            threshold_k: 1.25,
            reject_reason: None,
        };
        let mut fit2 = fit.clone();
        fit2.candidate.target = "MU".to_string();
        fit2.candidate.members = vec!["QCOM".to_string(), "TXN".to_string()];
        let mut engine = BasketEngine::new(&[fit, fit2]);
        let date = NaiveDate::from_ymd_opt(2026, 4, 21).unwrap();
        let bars = vec![
            DailyBar {
                symbol: "AMD".to_string(),
                date,
                close: 90.0,
            },
            DailyBar {
                symbol: "NVDA".to_string(),
                date,
                close: 100.0,
            },
            DailyBar {
                symbol: "INTC".to_string(),
                date,
                close: 100.0,
            },
            DailyBar {
                symbol: "MU".to_string(),
                date,
                close: 110.0,
            },
            DailyBar {
                symbol: "QCOM".to_string(),
                date,
                close: 100.0,
            },
            DailyBar {
                symbol: "TXN".to_string(),
                date,
                close: 100.0,
            },
        ];
        let _ = engine.on_bars(&bars);
        engine
    }

    #[test]
    fn test_notional_per_basket() {
        let config = PortfolioConfig {
            capital: 100_000.0,
            leverage: 4.0,
            n_active_baskets: 10,
            stop_loss_z: None,
        };
        config.validate().unwrap();
        // 100K * 4x leverage * 90% utilization buffer / 10 baskets = 36K each.
        // The 90% factor leaves headroom for per-symbol rounding +
        // plan/submit price drift; without it, borderline orders get
        // rejected by the broker's `equity * leverage` cap and the
        // hedge book ends up lopsided (Q4 2024 feedback-loop bug).
        assert_eq!(config.notional_per_basket(), 36_000.0);
    }

    #[test]
    fn test_notional_scales_with_dynamic_equity() {
        let config = PortfolioConfig {
            capital: 100_000.0,
            leverage: 4.0,
            n_active_baskets: 10,
            stop_loss_z: None,
        };
        // Drawdown to 90K equity → notional shrinks proportionally,
        // keeping the actual gross under buying_power = 90K * 4 = 360K.
        // 90K * 4 * 0.90 / 10 = 32_400.
        assert_eq!(config.notional_per_basket_for_equity(90_000.0), 32_400.0);
        // At full initial equity, parity with the static method.
        assert_eq!(
            config.notional_per_basket_for_equity(100_000.0),
            config.notional_per_basket()
        );
        // Negative or zero equity floors at 0 — defensive.
        assert_eq!(config.notional_per_basket_for_equity(-1.0), 0.0);
        assert_eq!(config.notional_per_basket_for_equity(0.0), 0.0);
    }

    #[test]
    fn test_portfolio_config_rejects_zero_active_baskets() {
        let config = PortfolioConfig {
            capital: 100_000.0,
            leverage: 4.0,
            n_active_baskets: 0,
            stop_loss_z: None,
        };
        let err = config.validate().unwrap_err();
        assert!(err.contains("n_active_baskets"));
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
        let mut current: HashMap<String, f64> = HashMap::new();
        current.insert("AMD".to_string(), 5000.0);

        let mut target: HashMap<String, f64> = HashMap::new();
        target.insert("AMD".to_string(), 3000.0);
        target.insert("NVDA".to_string(), 2000.0);

        let orders = diff_to_orders(&current, &target);

        assert_eq!(orders.len(), 2);
        // AMD: 3000 - 5000 = -2000 shares, sell 2000 shares
        let amd_order = orders.iter().find(|o| o.symbol == "AMD").unwrap();
        assert_eq!(amd_order.side, Side::Sell);
        assert_eq!(amd_order.qty, 2000);
        // NVDA: 2000 - 0 = 2000 shares, buy 2000 shares
        let nvda_order = orders.iter().find(|o| o.symbol == "NVDA").unwrap();
        assert_eq!(nvda_order.side, Side::Buy);
        assert_eq!(nvda_order.qty, 2000);
    }

    #[test]
    fn test_plan_portfolio_enforces_active_basket_cap() {
        let engine = make_test_engine();
        let config = PortfolioConfig {
            capital: 100_000.0,
            leverage: 4.0,
            n_active_baskets: 1,
            stop_loss_z: None,
        };
        let plan = plan_portfolio(&engine, &config);
        assert_eq!(plan.active_baskets, 2);
        assert_eq!(plan.selected_baskets.len(), 1);
        assert_eq!(plan.excluded_baskets.len(), 1);
        let gross: f64 = plan.symbol_notionals.values().map(|n| n.abs()).sum();
        assert!((gross - config.notional_per_basket()).abs() < 1e-6);
    }
}
