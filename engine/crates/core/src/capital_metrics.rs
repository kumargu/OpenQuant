//! Capital utilization and return decomposition — Gatev et al. (2006) framework.
//!
//! Separates trade quality (RoEC) from capital efficiency (Utilization), giving
//! the complete picture via RoCC = RoEC × Utilization.
//!
//! ```text
//! Return_committed = Return_employed × Utilization_rate
//!                                                        (Gatev et al. 2006)
//! ```
//!
//! # Metrics
//!
//! - **RoEC** (Return on Employed Capital): P&L per dollar actually deployed.
//!   Measures raw trade quality independent of how often capital is deployed.
//!   Formula: `total_pnl / total_dollar_days_employed`
//!
//! - **Utilization**: Fraction of committed capital deployed on average.
//!   Formula: `mean(deployed_capital / total_capital)` over the period.
//!
//! - **RoCC** (Return on Committed Capital): The headline performance number.
//!   Formula: `RoEC × Utilization` = `total_pnl / (total_capital × n_days)`.
//!   This is the apples-to-apples comparison metric for any strategy.
//!
//! - **Return per trade**: P&L as a fraction of capital deployed per trade.
//!   Formula: `trade_pnl / (2 × capital_per_leg)`.
//!
//! - **Return per dollar-day**: Common unit for scoring active trades against
//!   queued signals. Formula: `trade_pnl / (capital × hold_days)`.
//!
//! - **Opportunity cost**: Estimated return lost to idle capital.
//!   Formula: `total_pnl × (1 - avg_utilization) / avg_utilization`
//!   (what we'd have earned if idle capital generated the same RoCC).
//!
//! # Reference
//!
//! Gatev, Goetzmann & Rouwenhorst (2006), "Pairs Trading: Performance of a
//! Relative-Value Arbitrage Rule", *Review of Financial Studies* 19(3).
//! doi:10.1093/rfs/hhj020

use tracing::info;

/// A single completed trade, required for capital metrics computation.
#[derive(Debug, Clone)]
pub struct TradeInput {
    /// Dollar P&L for this trade (positive = profit, negative = loss).
    pub pnl: f64,
    /// Capital deployed per leg (total deployed = `2 × capital_per_leg`).
    pub capital_per_leg: f64,
    /// Number of days the capital was deployed (holding period).
    pub hold_days: f64,
}

/// Per-day utilization snapshot, required for utilization computation.
#[derive(Debug, Clone)]
pub struct DailyUtilInput {
    /// Total committed capital (constant across days for a fixed portfolio).
    pub total_capital: f64,
    /// Capital actually deployed in open positions that day.
    pub deployed_capital: f64,
}

/// Configuration for capital metrics computation.
#[derive(Debug, Clone)]
pub struct CapitalMetricsConfig {
    /// Total committed capital (e.g., $10,000). Must be > 0.
    pub total_capital: f64,
    /// Number of trading days in the period.
    pub n_days: usize,
}

impl Default for CapitalMetricsConfig {
    fn default() -> Self {
        Self {
            total_capital: 10_000.0,
            n_days: 1,
        }
    }
}

/// The computed capital utilization metrics.
///
/// All rates are expressed as fractions (not percent). Multiply by 100 for %.
/// All per-day rates are per calendar trading day.
#[derive(Debug, Clone)]
pub struct CapitalMetrics {
    /// Return on Employed Capital: P&L per dollar-day actually deployed.
    /// `total_pnl / total_dollar_days_employed`. Zero if no trades.
    pub roec: f64,

    /// Average utilization rate: fraction of capital deployed.
    /// Computed from daily snapshots if provided; falls back to trade-weighted
    /// estimate if daily snapshots are empty.
    pub avg_utilization: f64,

    /// Return on Committed Capital: `roec × avg_utilization`.
    /// Equivalent to `total_pnl / (total_capital × n_days)`.
    /// This is the headline number for cross-strategy comparison.
    pub rocc: f64,

    /// Average return per trade: `mean(trade_pnl / total_deployed_per_trade)`.
    /// Zero if no trades.
    pub avg_return_per_trade: f64,

    /// Average return per dollar-day: `mean(trade_pnl / (capital × hold_days))`.
    /// Common unit for scoring signals against active trades.
    pub avg_return_per_dollar_day: f64,

    /// Estimated opportunity cost from idle capital.
    /// If `avg_utilization > 0`: `total_pnl × (1 - u) / u`.
    /// Answers: "how much more would we earn if utilization reached 100%?"
    pub opportunity_cost: f64,

    /// Total P&L across all trades.
    pub total_pnl: f64,

    /// Total number of trades.
    pub n_trades: usize,

    /// Total dollar-days of capital employed (`sum(capital × hold_days)`).
    pub total_dollar_days_employed: f64,
}

/// Compute capital utilization metrics from completed trades and daily snapshots.
///
/// # Arguments
///
/// - `trades`: completed trade records. May be empty (returns zero metrics).
/// - `daily_util`: per-day deployed/total snapshots. If empty, utilization is
///   estimated from trade-weighted calculation using `config.total_capital` and
///   `config.n_days`.
/// - `config`: total committed capital and period length.
///
/// # Panics
///
/// Never panics — guards all NaN/infinity at the boundary.
pub fn compute_capital_metrics(
    trades: &[TradeInput],
    daily_util: &[DailyUtilInput],
    config: &CapitalMetricsConfig,
) -> CapitalMetrics {
    // Guard config
    let total_capital = if config.total_capital.is_finite() && config.total_capital > 0.0 {
        config.total_capital
    } else {
        1.0 // fallback to avoid div-by-zero; metrics will be nonsensical but won't panic
    };
    let n_days = config.n_days.max(1) as f64;

    // Aggregate trade-level quantities
    let mut total_pnl = 0.0_f64;
    let mut total_dollar_days = 0.0_f64;
    let mut sum_return_per_trade = 0.0_f64;
    let mut sum_return_per_dollar_day = 0.0_f64;
    let mut valid_trades = 0usize;

    for t in trades {
        // Guard inputs
        if !t.pnl.is_finite() || !t.capital_per_leg.is_finite() || !t.hold_days.is_finite() {
            continue;
        }
        if t.capital_per_leg <= 0.0 || t.hold_days <= 0.0 {
            continue;
        }

        let total_deployed = 2.0 * t.capital_per_leg;
        let dollar_days = total_deployed * t.hold_days;

        total_pnl += t.pnl;
        total_dollar_days += dollar_days;

        let ret_per_trade = t.pnl / total_deployed;
        let ret_per_dollar_day = t.pnl / dollar_days;

        sum_return_per_trade += ret_per_trade;
        sum_return_per_dollar_day += ret_per_dollar_day;
        valid_trades += 1;
    }

    // RoEC: total P&L per dollar-day employed
    let roec = if total_dollar_days > 0.0 {
        total_pnl / total_dollar_days
    } else {
        0.0
    };

    // Utilization: from daily snapshots (preferred) or trade-weighted estimate
    let avg_utilization = if !daily_util.is_empty() {
        let mut sum_util = 0.0_f64;
        let mut valid_days = 0usize;
        for d in daily_util {
            if d.total_capital.is_finite()
                && d.total_capital > 0.0
                && d.deployed_capital.is_finite()
                && d.deployed_capital >= 0.0
            {
                // Clamp to [0, 1] — deployed can never exceed total
                let util = (d.deployed_capital / d.total_capital).clamp(0.0, 1.0);
                sum_util += util;
                valid_days += 1;
            }
        }
        if valid_days > 0 {
            sum_util / valid_days as f64
        } else {
            0.0
        }
    } else if total_dollar_days > 0.0 {
        // Fallback: estimate from trade dollar-days over committed capital × period
        let committed_dollar_days = total_capital * n_days;
        (total_dollar_days / committed_dollar_days).clamp(0.0, 1.0)
    } else {
        0.0
    };

    // RoCC = RoEC × Utilization = total_pnl / (total_capital × n_days)
    let rocc = roec * avg_utilization;

    // Per-trade and per-dollar-day averages
    let avg_return_per_trade = if valid_trades > 0 {
        sum_return_per_trade / valid_trades as f64
    } else {
        0.0
    };
    let avg_return_per_dollar_day = if valid_trades > 0 {
        sum_return_per_dollar_day / valid_trades as f64
    } else {
        0.0
    };

    // Opportunity cost: returns lost to idle capital at current RoCC rate
    // If utilization = u, then idle fraction = (1 - u).
    // Opportunity cost = total_pnl × (1 - u) / u  (how much more at 100% util)
    let opportunity_cost = if avg_utilization > 0.0 && total_pnl > 0.0 {
        total_pnl * (1.0 - avg_utilization) / avg_utilization
    } else {
        0.0
    };

    let metrics = CapitalMetrics {
        roec,
        avg_utilization,
        rocc,
        avg_return_per_trade,
        avg_return_per_dollar_day,
        opportunity_cost,
        total_pnl,
        n_trades: valid_trades,
        total_dollar_days_employed: total_dollar_days,
    };

    info!(
        n_trades = valid_trades,
        total_pnl = format!("{:.2}", total_pnl).as_str(),
        roec_pct = format!("{:.4}%", roec * 100.0).as_str(),
        utilization_pct = format!("{:.1}%", avg_utilization * 100.0).as_str(),
        rocc_pct = format!("{:.4}%", rocc * 100.0).as_str(),
        avg_return_per_trade_pct = format!("{:.4}%", avg_return_per_trade * 100.0).as_str(),
        opportunity_cost = format!("{:.2}", opportunity_cost).as_str(),
        "Capital metrics computed"
    );

    metrics
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config_10k(n_days: usize) -> CapitalMetricsConfig {
        CapitalMetricsConfig {
            total_capital: 10_000.0,
            n_days,
        }
    }

    // -----------------------------------------------------------------------
    // Empty / zero input
    // -----------------------------------------------------------------------

    #[test]
    fn empty_trades_returns_zero_metrics() {
        let metrics = compute_capital_metrics(&[], &[], &config_10k(20));
        assert_eq!(metrics.n_trades, 0);
        assert_eq!(metrics.total_pnl, 0.0);
        assert_eq!(metrics.roec, 0.0);
        assert_eq!(metrics.avg_utilization, 0.0);
        assert_eq!(metrics.rocc, 0.0);
        assert_eq!(metrics.avg_return_per_trade, 0.0);
        assert_eq!(metrics.avg_return_per_dollar_day, 0.0);
        assert_eq!(metrics.opportunity_cost, 0.0);
    }

    #[test]
    fn empty_trades_with_daily_util_still_computes_utilization() {
        let daily = vec![
            DailyUtilInput {
                total_capital: 10_000.0,
                deployed_capital: 6_000.0,
            },
            DailyUtilInput {
                total_capital: 10_000.0,
                deployed_capital: 4_000.0,
            },
        ];
        let metrics = compute_capital_metrics(&[], &daily, &config_10k(2));
        assert_eq!(metrics.n_trades, 0);
        assert!((metrics.avg_utilization - 0.5).abs() < 1e-10); // (0.6 + 0.4) / 2
        assert_eq!(metrics.roec, 0.0);
        assert_eq!(metrics.rocc, 0.0);
    }

    // -----------------------------------------------------------------------
    // Single trade
    // -----------------------------------------------------------------------

    #[test]
    fn single_profitable_trade() {
        // One trade: $100 profit on $1000 per leg for 5 days
        let trades = vec![TradeInput {
            pnl: 100.0,
            capital_per_leg: 1_000.0,
            hold_days: 5.0,
        }];
        let metrics = compute_capital_metrics(&trades, &[], &config_10k(20));

        // total_dollar_days = 2 * 1000 * 5 = 10_000
        assert!((metrics.total_dollar_days_employed - 10_000.0).abs() < 1e-10);

        // RoEC = 100 / 10_000 = 0.01 (1% per day employed)
        assert!((metrics.roec - 0.01).abs() < 1e-10);

        // avg_return_per_trade = 100 / 2000 = 0.05 (5%)
        assert!((metrics.avg_return_per_trade - 0.05).abs() < 1e-10);

        // avg_return_per_dollar_day = 100 / 10_000 = 0.01
        assert!((metrics.avg_return_per_dollar_day - 0.01).abs() < 1e-10);

        // utilization fallback: 10_000 / (10_000 * 20) = 0.05
        assert!((metrics.avg_utilization - 0.05).abs() < 1e-10);

        // RoCC = 0.01 * 0.05 = 0.0005
        assert!((metrics.rocc - 0.0005).abs() < 1e-10);
    }

    // -----------------------------------------------------------------------
    // Gatev decomposition: RoCC = RoEC × Utilization
    // -----------------------------------------------------------------------

    #[test]
    fn gatev_decomposition_holds() {
        // Verify: RoCC = RoEC × Utilization exactly
        let trades = vec![
            TradeInput {
                pnl: 50.0,
                capital_per_leg: 2_000.0,
                hold_days: 3.0,
            },
            TradeInput {
                pnl: -20.0,
                capital_per_leg: 1_500.0,
                hold_days: 2.0,
            },
            TradeInput {
                pnl: 80.0,
                capital_per_leg: 2_500.0,
                hold_days: 4.0,
            },
        ];
        let daily = vec![
            DailyUtilInput {
                total_capital: 10_000.0,
                deployed_capital: 7_000.0,
            },
            DailyUtilInput {
                total_capital: 10_000.0,
                deployed_capital: 5_000.0,
            },
            DailyUtilInput {
                total_capital: 10_000.0,
                deployed_capital: 8_000.0,
            },
        ];
        let metrics = compute_capital_metrics(&trades, &daily, &config_10k(3));

        // RoCC must equal RoEC × Utilization
        let expected_rocc = metrics.roec * metrics.avg_utilization;
        assert!(
            (metrics.rocc - expected_rocc).abs() < 1e-12,
            "RoCC={} ≠ RoEC × Util={} × {} = {}",
            metrics.rocc,
            metrics.roec,
            metrics.avg_utilization,
            expected_rocc
        );
    }

    // -----------------------------------------------------------------------
    // Utilization from daily snapshots
    // -----------------------------------------------------------------------

    #[test]
    fn daily_util_takes_priority_over_fallback() {
        let trades = vec![TradeInput {
            pnl: 10.0,
            capital_per_leg: 1_000.0,
            hold_days: 1.0,
        }];
        // Snapshots say 70% utilization
        let daily = vec![DailyUtilInput {
            total_capital: 10_000.0,
            deployed_capital: 7_000.0,
        }];
        let metrics = compute_capital_metrics(&trades, &daily, &config_10k(1));
        assert!(
            (metrics.avg_utilization - 0.7).abs() < 1e-10,
            "Expected 0.7, got {}",
            metrics.avg_utilization
        );
    }

    #[test]
    fn utilization_clamped_at_one() {
        // Edge case: deployed > total (data error — should clamp)
        let daily = vec![DailyUtilInput {
            total_capital: 1_000.0,
            deployed_capital: 2_000.0,
        }];
        let metrics = compute_capital_metrics(&[], &daily, &config_10k(1));
        assert!((metrics.avg_utilization - 1.0).abs() < 1e-10);
    }

    #[test]
    fn utilization_zero_deployed() {
        let daily = vec![
            DailyUtilInput {
                total_capital: 10_000.0,
                deployed_capital: 0.0,
            },
            DailyUtilInput {
                total_capital: 10_000.0,
                deployed_capital: 0.0,
            },
        ];
        let metrics = compute_capital_metrics(&[], &daily, &config_10k(2));
        assert_eq!(metrics.avg_utilization, 0.0);
    }

    // -----------------------------------------------------------------------
    // NaN / infinity guard
    // -----------------------------------------------------------------------

    #[test]
    fn nan_trade_inputs_are_skipped() {
        let trades = vec![
            TradeInput {
                pnl: f64::NAN,
                capital_per_leg: 1_000.0,
                hold_days: 5.0,
            },
            TradeInput {
                pnl: 100.0,
                capital_per_leg: f64::INFINITY,
                hold_days: 5.0,
            },
            TradeInput {
                pnl: 100.0,
                capital_per_leg: 1_000.0,
                hold_days: f64::NAN,
            },
            TradeInput {
                pnl: 50.0,
                capital_per_leg: 1_000.0,
                hold_days: 3.0,
            }, // valid
        ];
        let metrics = compute_capital_metrics(&trades, &[], &config_10k(20));
        assert_eq!(metrics.n_trades, 1, "Only one valid trade");
        assert!((metrics.total_pnl - 50.0).abs() < 1e-10);
    }

    #[test]
    fn zero_capital_trades_are_skipped() {
        let trades = vec![
            TradeInput {
                pnl: 100.0,
                capital_per_leg: 0.0,
                hold_days: 5.0,
            },
            TradeInput {
                pnl: 100.0,
                capital_per_leg: -100.0,
                hold_days: 5.0,
            },
            TradeInput {
                pnl: 100.0,
                capital_per_leg: 1_000.0,
                hold_days: 0.0,
            },
            TradeInput {
                pnl: 50.0,
                capital_per_leg: 1_000.0,
                hold_days: 2.0,
            }, // valid
        ];
        let metrics = compute_capital_metrics(&trades, &[], &config_10k(20));
        assert_eq!(metrics.n_trades, 1);
    }

    #[test]
    fn nan_daily_util_entries_are_skipped() {
        let daily = vec![
            DailyUtilInput {
                total_capital: f64::NAN,
                deployed_capital: 500.0,
            },
            DailyUtilInput {
                total_capital: 10_000.0,
                deployed_capital: f64::NAN,
            },
            DailyUtilInput {
                total_capital: 10_000.0,
                deployed_capital: 8_000.0,
            }, // valid
        ];
        let metrics = compute_capital_metrics(&[], &daily, &config_10k(3));
        assert!((metrics.avg_utilization - 0.8).abs() < 1e-10);
    }

    // -----------------------------------------------------------------------
    // Opportunity cost
    // -----------------------------------------------------------------------

    #[test]
    fn opportunity_cost_at_100pct_utilization() {
        // At 100% utilization, opportunity cost = 0 (no idle capital)
        let trades = vec![TradeInput {
            pnl: 100.0,
            capital_per_leg: 5_000.0,
            hold_days: 1.0,
        }];
        let daily = vec![DailyUtilInput {
            total_capital: 10_000.0,
            deployed_capital: 10_000.0,
        }];
        let metrics = compute_capital_metrics(&trades, &daily, &config_10k(1));
        assert!((metrics.avg_utilization - 1.0).abs() < 1e-10);
        assert_eq!(metrics.opportunity_cost, 0.0);
    }

    #[test]
    fn opportunity_cost_at_50pct_utilization() {
        // At 50% utilization with $100 total P&L:
        // opportunity_cost = 100 * (1 - 0.5) / 0.5 = 100
        let trades = vec![TradeInput {
            pnl: 100.0,
            capital_per_leg: 2_500.0,
            hold_days: 2.0,
        }];
        let daily = vec![
            DailyUtilInput {
                total_capital: 10_000.0,
                deployed_capital: 5_000.0,
            },
            DailyUtilInput {
                total_capital: 10_000.0,
                deployed_capital: 5_000.0,
            },
        ];
        let metrics = compute_capital_metrics(&trades, &daily, &config_10k(2));
        assert!((metrics.avg_utilization - 0.5).abs() < 1e-10);
        assert!((metrics.opportunity_cost - 100.0).abs() < 1e-10);
    }

    #[test]
    fn opportunity_cost_zero_when_negative_pnl() {
        // Negative P&L → opportunity cost = 0 (no "foregone gains")
        let trades = vec![TradeInput {
            pnl: -50.0,
            capital_per_leg: 2_000.0,
            hold_days: 3.0,
        }];
        let daily = vec![DailyUtilInput {
            total_capital: 10_000.0,
            deployed_capital: 4_000.0,
        }];
        let metrics = compute_capital_metrics(&trades, &daily, &config_10k(1));
        assert_eq!(metrics.opportunity_cost, 0.0);
    }

    // -----------------------------------------------------------------------
    // Multi-trade aggregate correctness
    // -----------------------------------------------------------------------

    #[test]
    fn multi_trade_roec_correctness() {
        // 2 trades:
        // Trade A: $60 pnl, $1000/leg, 3 days → dollar-days = 2*1000*3 = 6000
        // Trade B: $40 pnl, $2000/leg, 2 days → dollar-days = 2*2000*2 = 8000
        // Total: $100 pnl, 14000 dollar-days → RoEC = 100/14000
        let trades = vec![
            TradeInput {
                pnl: 60.0,
                capital_per_leg: 1_000.0,
                hold_days: 3.0,
            },
            TradeInput {
                pnl: 40.0,
                capital_per_leg: 2_000.0,
                hold_days: 2.0,
            },
        ];
        let metrics = compute_capital_metrics(&trades, &[], &config_10k(10));
        assert!((metrics.total_dollar_days_employed - 14_000.0).abs() < 1e-10);
        assert!((metrics.total_pnl - 100.0).abs() < 1e-10);
        assert!((metrics.roec - 100.0 / 14_000.0).abs() < 1e-12);
    }

    #[test]
    fn invalid_config_does_not_panic() {
        // Zero or NaN total_capital should not panic
        let cfg_zero = CapitalMetricsConfig {
            total_capital: 0.0,
            n_days: 10,
        };
        let cfg_nan = CapitalMetricsConfig {
            total_capital: f64::NAN,
            n_days: 10,
        };
        let cfg_neg = CapitalMetricsConfig {
            total_capital: -1000.0,
            n_days: 0,
        };
        let t = vec![TradeInput {
            pnl: 50.0,
            capital_per_leg: 1_000.0,
            hold_days: 2.0,
        }];
        // These must not panic
        let _ = compute_capital_metrics(&t, &[], &cfg_zero);
        let _ = compute_capital_metrics(&t, &[], &cfg_nan);
        let _ = compute_capital_metrics(&t, &[], &cfg_neg);
    }
}
