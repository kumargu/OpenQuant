//! Risk gates — last line of defense before an order goes out.
//!
//! Every signal must pass through risk checks before becoming an order.
//! If any gate rejects, no trade happens. Risk management is the main strategy.
//!
//! V1 gates:
//! - Kill switch: halt all trading if daily P&L exceeds max loss
//! - Cost filter: reject if signal isn't strong enough relative to costs
//! - Position sizing: cap notional exposure, allow fractional for expensive assets

use crate::features::MarketRegime;
use crate::signals::{Side, SignalOutput};
use tracing::{error, info, warn};

/// Bet sizing method.
#[derive(Debug, Clone, Copy, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BetSizingMethod {
    /// Fixed sizing: qty = max_notional / price (current behavior).
    #[default]
    Linear,
    /// Bayesian Kelly: posterior-mean Kelly fraction, uncertainty-penalized.
    /// Starts conservative (prior), grows as trade evidence accumulates.
    Kelly,
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
    /// Bet sizing method: "linear" or "kelly". Default: linear
    pub bet_sizing: BetSizingMethod,
    /// Kelly fraction ceiling (half-Kelly = 0.5). Default: 0.5
    pub kelly_fraction: f64,
    /// Minimum Kelly size as fraction of max notional. Default: 0.05
    pub kelly_min_size: f64,
    /// Maximum Kelly size as fraction of max notional. Default: 0.80
    pub kelly_max_size: f64,
    /// Beta prior wins (α). Higher = stronger prior toward 50% win rate. Default: 2.0
    pub kelly_prior_wins: f64,
    /// Beta prior losses (β). Default: 2.0
    pub kelly_prior_losses: f64,
    /// Kelly fraction in LowVol regime (slightly more aggressive). Default: 0.6
    pub kelly_fraction_low_vol: f64,
    /// Kelly fraction in HighVol regime (reduced). Default: 0.25
    pub kelly_fraction_high_vol: f64,
    /// Kelly fraction in Crisis regime (minimal). Default: 0.10
    pub kelly_fraction_crisis: f64,
    /// Enable smooth drawdown-based deleveraging. Default: true
    pub drawdown_deleverage: bool,
    /// Exponent for deleveraging curve. 2 = concave (faster near max). Default: 2.0
    pub drawdown_deleverage_curve: f64,
}

impl Default for RiskConfig {
    fn default() -> Self {
        Self {
            max_position_notional: 10_000.0,
            max_daily_loss: 500.0,
            min_reward_cost_ratio: 3.0,
            estimated_cost_bps: 0.001,
            bet_sizing: BetSizingMethod::Linear,
            kelly_fraction: 0.5,
            kelly_min_size: 0.05,
            kelly_max_size: 0.80,
            // Seeded from backtest results (~60% win rate). Beta(12,8) gives
            // non-zero initial Kelly fraction instead of cold-start zero with
            // Beta(2,2). Live session 2026-03-19 showed qty=1 on $420 GLD
            // because uninformative prior computed zero edge.
            kelly_prior_wins: 12.0,
            kelly_prior_losses: 8.0,
            kelly_fraction_low_vol: 0.6,
            kelly_fraction_high_vol: 0.25,
            kelly_fraction_crisis: 0.10,
            drawdown_deleverage: true,
            drawdown_deleverage_curve: 2.0,
        }
    }
}

/// Bayesian Kelly position sizing state.
///
/// Maintains a Beta posterior for win rate and running stats for payoff ratio.
/// The Kelly fraction is posterior-mean with an uncertainty penalty that
/// automatically makes sizing conservative when few trades have been observed.
#[derive(Debug, Clone)]
pub struct BayesianKellyState {
    win_alpha: f64,    // Beta posterior: α (prior + wins)
    win_beta: f64,     // Beta posterior: β (prior + losses)
    sum_win_pnl: f64,  // sum of winning trade P&L
    sum_loss_pnl: f64, // sum of losing trade |P&L|
    n_wins: usize,
    n_losses: usize,
}

impl BayesianKellyState {
    pub fn new(prior_wins: f64, prior_losses: f64) -> Self {
        Self {
            win_alpha: prior_wins,
            win_beta: prior_losses,
            sum_win_pnl: 0.0,
            sum_loss_pnl: 0.0,
            n_wins: 0,
            n_losses: 0,
        }
    }

    /// Observe a completed trade. Positive pnl = win, negative = loss.
    pub fn observe_trade(&mut self, pnl: f64) {
        if pnl > 0.0 {
            self.win_alpha += 1.0;
            self.sum_win_pnl += pnl;
            self.n_wins += 1;
        } else {
            self.win_beta += 1.0;
            self.sum_loss_pnl += pnl.abs();
            self.n_losses += 1;
        }
    }

    /// Compute the uncertainty-penalized Kelly fraction (0.0 - 1.0).
    ///
    /// Uses posterior mean of win rate and observed payoff ratio.
    /// Penalizes by posterior standard deviation to be conservative
    /// when estimates are noisy (few trades).
    pub fn kelly_fraction(&self) -> f64 {
        let total = self.win_alpha + self.win_beta;
        let p = self.win_alpha / total; // posterior mean win rate
        let q = 1.0 - p;

        // Payoff ratio: average win / average loss
        let avg_win = if self.n_wins > 0 {
            self.sum_win_pnl / self.n_wins as f64
        } else {
            1.0 // prior: 1:1 payoff
        };
        let avg_loss = if self.n_losses > 0 {
            self.sum_loss_pnl / self.n_losses as f64
        } else {
            1.0
        };
        let b = avg_win / avg_loss.max(1e-10);

        // Kelly: f* = (p*b - q) / b
        let kelly = (p * b - q) / b;
        if kelly <= 0.0 {
            return 0.0; // negative edge → don't bet
        }

        // Uncertainty penalty: posterior std of Beta(α, β) = √(αβ / (α+β)²(α+β+1))
        let posterior_var = (self.win_alpha * self.win_beta) / (total * total * (total + 1.0));
        let posterior_std = posterior_var.sqrt();
        // Scale penalty: 4× std (so at prior Beta(2,2), penalty ≈ 0.45 → conservative)
        let uncertainty_penalty = (1.0 - posterior_std * 4.0).max(0.2);

        (kelly * uncertainty_penalty).clamp(0.0, 1.0)
    }

    pub fn total_trades(&self) -> usize {
        self.n_wins + self.n_losses
    }
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

/// Compute smooth deleveraging multiplier based on current drawdown.
///
/// Returns 1.0 at no drawdown, approaching 0.0 at max drawdown.
/// Uses `(1 - dd_ratio)^exponent` — concave curve so deleveraging
/// accelerates as drawdown deepens.
pub fn drawdown_multiplier(daily_pnl: f64, max_daily_loss: f64, exponent: f64) -> f64 {
    if max_daily_loss <= 0.0 || daily_pnl >= 0.0 {
        return 1.0;
    }
    let dd_ratio = (daily_pnl.abs() / max_daily_loss).clamp(0.0, 1.0);
    (1.0 - dd_ratio).powf(exponent).max(0.0)
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
    kelly_state: &BayesianKellyState,
    regime: MarketRegime,
    config: &RiskConfig,
) -> Result<f64, Rejection> {
    // Gate 1: Kill switch
    if risk_state.killed {
        error!(
            daily_pnl = format!("{:.2}", risk_state.daily_pnl).as_str(),
            max_loss = format!("{:.2}", config.max_daily_loss).as_str(),
            "risk: KILLED — daily loss limit breached"
        );
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
        let min_required = round_trip_cost_pct * config.min_reward_cost_ratio;
        // Signal score roughly proxies expected move magnitude
        // Reject if score isn't meaningfully above the cost drag
        if signal.score < min_required {
            warn!(
                score = format!("{:.4}", signal.score).as_str(),
                min_required = format!("{:.4}", min_required).as_str(),
                cost_bps = format!("{:.4}", config.estimated_cost_bps).as_str(),
                "risk: REJECTED — cost filter (signal too weak for costs)"
            );
            return Err(Rejection {
                reason: format!(
                    "cost filter: score {:.4} < min required {:.4}",
                    signal.score, min_required,
                ),
            });
        }
    }

    // Gate 3: Position sizing — compute qty within notional limit
    let qty = match signal.side {
        Side::Buy => {
            let max_qty = config.max_position_notional / price;

            // Drawdown-based deleveraging: smooth reduction as losses accumulate
            let dd_mult = if config.drawdown_deleverage {
                drawdown_multiplier(
                    risk_state.daily_pnl,
                    config.max_daily_loss,
                    config.drawdown_deleverage_curve,
                )
            } else {
                1.0
            };

            // Apply bet sizing method
            let scale = match config.bet_sizing {
                BetSizingMethod::Linear => dd_mult,
                BetSizingMethod::Kelly => {
                    // Regime-conditional Kelly ceiling
                    let regime_kelly = match regime {
                        MarketRegime::LowVol => config.kelly_fraction_low_vol,
                        MarketRegime::Normal => config.kelly_fraction,
                        MarketRegime::HighVol => config.kelly_fraction_high_vol,
                        MarketRegime::Crisis => config.kelly_fraction_crisis,
                        MarketRegime::Unknown => config.kelly_fraction,
                    };
                    let raw = kelly_state.kelly_fraction();
                    let fractional = raw * regime_kelly * dd_mult;
                    fractional.clamp(config.kelly_min_size, config.kelly_max_size)
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

    info!(
        side = format!("{:?}", signal.side).as_str(),
        qty = format!("{:.2}", qty).as_str(),
        price = format!("{:.2}", price).as_str(),
        notional = format!("{:.0}", qty * price).as_str(),
        score = format!("{:.3}", signal.score).as_str(),
        "risk: APPROVED"
    );

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

    fn default_kelly() -> BayesianKellyState {
        // Matches updated default: Beta(12,8) seeded from backtest ~60% win rate
        BayesianKellyState::new(12.0, 8.0)
    }

    #[test]
    fn test_kill_switch_blocks() {
        let mut state = RiskState::new();
        let config = no_cost_config();
        state.record_pnl(-600.0, &config);
        assert!(state.killed);

        let result = check(
            &buy_signal(1.0),
            100.0,
            0.0,
            &state,
            &default_kelly(),
            MarketRegime::Normal,
            &config,
        );
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
        let result = check(
            &buy_signal(2.0),
            100.0,
            0.0,
            &state,
            &default_kelly(),
            MarketRegime::Normal,
            &config,
        );
        assert!(result.is_ok());
        let qty = result.unwrap();
        assert!(qty <= 50.0); // 5000 / 100 = 50
        assert!(qty > 0.0);
    }

    #[test]
    fn test_sell_uses_position_qty() {
        let state = RiskState::new();
        let config = no_cost_config();
        let result = check(
            &sell_signal(1.0),
            100.0,
            25.0,
            &state,
            &default_kelly(),
            MarketRegime::Normal,
            &config,
        );
        assert_eq!(result.unwrap(), 25.0);
    }

    #[test]
    fn test_sell_rejected_no_position() {
        let state = RiskState::new();
        let config = RiskConfig::default();
        let result = check(
            &sell_signal(1.0),
            100.0,
            0.0,
            &state,
            &default_kelly(),
            MarketRegime::Normal,
            &config,
        );
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
    fn test_linear_bet_sizing_ignores_score() {
        let state = RiskState::new();
        let config = RiskConfig {
            max_position_notional: 10_000.0,
            min_reward_cost_ratio: 0.0,
            bet_sizing: BetSizingMethod::Linear,
            ..Default::default()
        };

        let high = check(
            &buy_signal(1.0),
            100.0,
            0.0,
            &state,
            &default_kelly(),
            MarketRegime::Normal,
            &config,
        )
        .unwrap();
        let low = check(
            &buy_signal(0.1),
            100.0,
            0.0,
            &state,
            &default_kelly(),
            MarketRegime::Normal,
            &config,
        )
        .unwrap();
        assert_eq!(
            high, low,
            "linear sizing should give same qty regardless of score"
        );
    }

    // --- Bayesian Kelly tests ---

    #[test]
    fn kelly_fraction_increases_with_more_wins() {
        let mut k = BayesianKellyState::new(2.0, 2.0);
        let f_before = k.kelly_fraction();

        // Observe 20 wins, 5 losses (75% win rate, 2:1 payoff)
        for _ in 0..20 {
            k.observe_trade(100.0);
        }
        for _ in 0..5 {
            k.observe_trade(-50.0);
        }
        let f_after = k.kelly_fraction();

        assert!(
            f_after > f_before,
            "kelly fraction should increase with wins: {f_before:.4} → {f_after:.4}"
        );
    }

    #[test]
    fn kelly_uncertainty_penalty_shrinks_with_more_trades() {
        // More trades → tighter posterior → less penalty → higher fraction
        let mut k_few = BayesianKellyState::new(2.0, 2.0);
        let mut k_many = BayesianKellyState::new(2.0, 2.0);

        // Same win rate (60%), different sample sizes
        for _ in 0..6 {
            k_few.observe_trade(100.0);
        }
        for _ in 0..4 {
            k_few.observe_trade(-80.0);
        }

        for _ in 0..60 {
            k_many.observe_trade(100.0);
        }
        for _ in 0..40 {
            k_many.observe_trade(-80.0);
        }

        assert!(
            k_many.kelly_fraction() > k_few.kelly_fraction(),
            "more trades should give higher kelly (less uncertainty): few={:.4} many={:.4}",
            k_few.kelly_fraction(),
            k_many.kelly_fraction()
        );
    }

    #[test]
    fn kelly_negative_edge_returns_zero() {
        let mut k = BayesianKellyState::new(2.0, 2.0);
        // Observe mostly losses
        for _ in 0..3 {
            k.observe_trade(50.0);
        }
        for _ in 0..20 {
            k.observe_trade(-100.0);
        }

        assert_eq!(
            k.kelly_fraction(),
            0.0,
            "negative edge should return zero kelly"
        );
    }

    #[test]
    fn kelly_cold_start_is_conservative() {
        // Beta(12,8) seeded from backtest ~60% win rate. Gives small but
        // non-zero initial fraction (≈0.11) instead of zero with Beta(2,2).
        // This is intentional — live session 2026-03-19 showed qty=1 on
        // $420 GLD because uninformative prior computed zero edge.
        let k = BayesianKellyState::new(12.0, 8.0);
        let f = k.kelly_fraction();
        assert!(
            f > 0.0,
            "backtest-seeded prior should give non-zero fraction: {f:.4}"
        );
        assert!(f < 0.15, "cold start should still be conservative: {f:.4}");
    }

    #[test]
    fn kelly_sizing_scales_position() {
        let state = RiskState::new();
        let config = RiskConfig {
            max_position_notional: 10_000.0,
            min_reward_cost_ratio: 0.0,
            bet_sizing: BetSizingMethod::Kelly,
            kelly_fraction: 0.5,
            kelly_min_size: 0.05,
            kelly_max_size: 0.80,
            ..Default::default()
        };

        // Kelly with backtest-seeded prior but no live data → small but non-zero
        let qty_cold = check(
            &buy_signal(1.0),
            100.0,
            0.0,
            &state,
            &default_kelly(),
            MarketRegime::Normal,
            &config,
        )
        .unwrap();
        let max_qty = 10_000.0 / 100.0; // 100

        // Should be at minimum (kelly_min_size × max)
        assert!(
            qty_cold <= max_qty * 0.10,
            "cold start should give small position: {qty_cold}"
        );

        // Kelly with strong track record
        let mut k_strong = BayesianKellyState::new(2.0, 2.0);
        for _ in 0..50 {
            k_strong.observe_trade(200.0);
        }
        for _ in 0..15 {
            k_strong.observe_trade(-100.0);
        }

        let qty_strong = check(
            &buy_signal(1.0),
            100.0,
            0.0,
            &state,
            &k_strong,
            MarketRegime::Normal,
            &config,
        )
        .unwrap();
        assert!(
            qty_strong > qty_cold,
            "strong track record should give larger position: cold={qty_cold} strong={qty_strong}"
        );
    }

    // --- Drawdown deleveraging tests ---

    #[test]
    fn drawdown_multiplier_no_loss() {
        assert_eq!(drawdown_multiplier(0.0, 500.0, 2.0), 1.0);
        assert_eq!(drawdown_multiplier(100.0, 500.0, 2.0), 1.0); // positive P&L
    }

    #[test]
    fn drawdown_multiplier_partial_loss() {
        // At -250 (50% of 500 max): (1 - 0.5)^2 = 0.25
        let m = drawdown_multiplier(-250.0, 500.0, 2.0);
        assert!((m - 0.25).abs() < 1e-10, "50% DD should give 0.25, got {m}");
    }

    #[test]
    fn drawdown_multiplier_at_max() {
        let m = drawdown_multiplier(-500.0, 500.0, 2.0);
        assert_eq!(m, 0.0, "at max drawdown multiplier should be 0");
    }

    #[test]
    fn drawdown_multiplier_beyond_max() {
        let m = drawdown_multiplier(-600.0, 500.0, 2.0);
        assert_eq!(m, 0.0, "beyond max drawdown should clamp to 0");
    }

    #[test]
    fn drawdown_deleverage_reduces_position() {
        let mut state = RiskState::new();
        let config = RiskConfig {
            max_position_notional: 10_000.0,
            min_reward_cost_ratio: 0.0,
            drawdown_deleverage: true,
            drawdown_deleverage_curve: 2.0,
            ..Default::default()
        };

        // Full size at 0 loss
        let qty_full = check(
            &buy_signal(1.0),
            100.0,
            0.0,
            &state,
            &default_kelly(),
            MarketRegime::Normal,
            &config,
        )
        .unwrap();

        // Reduced size at -250 loss (50% of max)
        state.record_pnl(-250.0, &config);
        let qty_dd = check(
            &buy_signal(1.0),
            100.0,
            0.0,
            &state,
            &default_kelly(),
            MarketRegime::Normal,
            &config,
        )
        .unwrap();

        assert!(
            qty_dd < qty_full,
            "drawdown should reduce position: full={qty_full} dd={qty_dd}"
        );
    }

    // --- Regime-conditional Kelly tests ---

    #[test]
    fn crisis_regime_uses_minimal_kelly() {
        let state = RiskState::new();
        let mut k = BayesianKellyState::new(2.0, 2.0);
        for _ in 0..50 {
            k.observe_trade(200.0);
        }
        for _ in 0..15 {
            k.observe_trade(-100.0);
        }

        let config = RiskConfig {
            max_position_notional: 10_000.0,
            min_reward_cost_ratio: 0.0,
            bet_sizing: BetSizingMethod::Kelly,
            ..Default::default()
        };

        let qty_normal = check(
            &buy_signal(1.0),
            100.0,
            0.0,
            &state,
            &k,
            MarketRegime::Normal,
            &config,
        )
        .unwrap();

        let qty_crisis = check(
            &buy_signal(1.0),
            100.0,
            0.0,
            &state,
            &k,
            MarketRegime::Crisis,
            &config,
        )
        .unwrap();

        assert!(
            qty_crisis < qty_normal,
            "crisis should give smaller position: normal={qty_normal} crisis={qty_crisis}"
        );
    }
}
