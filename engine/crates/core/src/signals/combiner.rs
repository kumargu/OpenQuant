//! Strategy combiner — run multiple strategies per symbol via score-weighted voting.
//!
//! Implements the [Composite pattern](https://en.wikipedia.org/wiki/Composite_pattern):
//! `StrategyCombiner` itself implements `Strategy`, so the engine doesn't need
//! to know it's running multiple strategies underneath.
//!
//! # Score-weighted voting
//!
//! ```text
//!  For each strategy i with signal sᵢ, score cᵢ, and weight wᵢ:
//!
//!    vote_buy  += wᵢ × cᵢ     (if sᵢ = Buy)
//!    vote_sell += wᵢ × cᵢ     (if sᵢ = Sell)
//!
//!    net = vote_buy - vote_sell
//!
//!    net >  threshold  →  BUY  (combined_score = net)
//!    net < -threshold  →  SELL (combined_score = |net|)
//!    |net| ≤ threshold →  NO TRADE (strategies cancel out)
//! ```
//!
//! # Conflict resolution
//!
//! ```text
//!  Example: weights = { mean_rev: 0.5, momentum: 0.5 }
//!
//!  Scenario 1 — Strategies agree:
//!    MeanRev: BUY  score=1.2 → vote_buy  += 0.5×1.2 = 0.60
//!    Momentum: BUY score=0.8 → vote_buy  += 0.5×0.8 = 0.40
//!    net = 1.00 → STRONG BUY ✅
//!
//!  Scenario 2 — Strategies disagree:
//!    MeanRev: BUY  score=0.6 → vote_buy  += 0.5×0.6 = 0.30
//!    Momentum: SELL score=1.5 → vote_sell += 0.5×1.5 = 0.75
//!    net = -0.45 → SELL (momentum wins with higher conviction)
//!
//!  Scenario 3 — Weak disagreement:
//!    MeanRev: BUY  score=0.5 → vote_buy  += 0.5×0.5 = 0.25
//!    Momentum: SELL score=0.4 → vote_sell += 0.5×0.4 = 0.20
//!    net = 0.05 → NO TRADE (below threshold, conflicting signals) ⚠️
//! ```

use super::{Side, SignalOutput, Strategy};
use crate::features::FeatureValues;

/// A named, weighted strategy entry in the combiner.
pub struct StrategyEntry {
    pub strategy: Box<dyn Strategy>,
    pub weight: f64,
    pub name: &'static str,
}

/// Configuration for the strategy combiner.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Config {
    /// Enable multi-strategy combiner. Default: true.
    ///
    /// When false, the engine uses only the mean-reversion strategy
    /// (single-strategy mode). Useful for A/B testing or isolating
    /// strategy performance.
    pub enabled: bool,

    /// Minimum |net_vote| to produce a signal. Default: 0.2
    ///
    /// Higher values require stronger consensus — fewer trades, higher quality.
    /// Lower values allow single-strategy signals through more easily.
    /// Set to 0.0 to let any signal fire (no conflict filtering).
    pub min_net_score: f64,

    /// Minimum number of strategies that must vote before the combiner
    /// produces an entry signal. Default: 1.
    pub min_strategies: usize,

    /// Minimum number of strategies that must vote SELL before the combiner
    /// exits an existing position. Default: 2.
    ///
    /// Higher than min_strategies to prevent single-strategy churn: one
    /// strategy buying and another immediately selling on the next bar.
    /// Stop loss / take profit / max hold bypass this (hard exits always fire).
    pub min_exit_strategies: usize,

    /// Weight for mean-reversion strategy. Default: 0.5
    pub weight_mean_reversion: f64,

    /// Weight for momentum strategy. Default: 0.5
    pub weight_momentum: f64,

    /// Weight for VWAP reversion strategy. Default: 0.0 (disabled by default)
    pub weight_vwap_reversion: f64,

    /// Weight for breakout strategy. Default: 0.0 (disabled by default)
    pub weight_breakout: f64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: true,
            min_net_score: 0.2,
            min_strategies: 1,
            min_exit_strategies: 2,
            weight_mean_reversion: 0.5,
            weight_momentum: 0.5,
            weight_vwap_reversion: 0.0,
            weight_breakout: 0.0,
        }
    }
}

/// Combines multiple strategies via score-weighted voting.
///
/// Implements `Strategy` so the engine treats it as a single strategy
/// (Composite pattern). Zero heap allocation per `score()` call —
/// votes and best-signal tracking happen in a single pass.
pub struct StrategyCombiner {
    strategies: Vec<StrategyEntry>,
    min_net_score: f64,
    min_strategies: usize,
    min_exit_strategies: usize,
}

impl StrategyCombiner {
    pub fn new(strategies: Vec<StrategyEntry>, min_net_score: f64) -> Self {
        Self {
            strategies,
            min_net_score,
            min_strategies: 1,
            min_exit_strategies: 2,
        }
    }

    pub fn with_min_strategies(mut self, min_strategies: usize) -> Self {
        self.min_strategies = min_strategies;
        self
    }

    pub fn with_min_exit_strategies(mut self, min_exit_strategies: usize) -> Self {
        self.min_exit_strategies = min_exit_strategies;
        self
    }
}

impl Strategy for StrategyCombiner {
    fn score(&self, features: &FeatureValues, has_position: bool) -> Option<SignalOutput> {
        let mut vote_buy = 0.0_f64;
        let mut vote_sell = 0.0_f64;
        let mut num_voters = 0_usize;
        let mut vote_parts: Vec<String> = Vec::new();
        // Track the highest-conviction signal per side (owned, no Vec needed).
        let mut best_buy: Option<(SignalOutput, f64)> = None; // (signal, weighted_score)
        let mut best_sell: Option<(SignalOutput, f64)> = None;

        // Single pass: score each strategy, tally votes, track best signals.
        for entry in &self.strategies {
            if let Some(signal) = entry.strategy.score(features, has_position) {
                let weighted = entry.weight * signal.score;
                num_voters += 1;
                let side_label = match signal.side {
                    Side::Buy => "BUY",
                    Side::Sell => "SELL",
                };
                vote_parts.push(format!("{}:{}({:.2})", entry.name, side_label, weighted));
                match signal.side {
                    Side::Buy => {
                        vote_buy += weighted;
                        if best_buy.as_ref().is_none_or(|(_, w)| weighted > *w) {
                            best_buy = Some((signal, weighted));
                        }
                    }
                    Side::Sell => {
                        vote_sell += weighted;
                        if best_sell.as_ref().is_none_or(|(_, w)| weighted > *w) {
                            best_sell = Some((signal, weighted));
                        }
                    }
                }
            }
        }

        // Gate: require minimum number of strategies to vote
        if num_voters < self.min_strategies {
            return None;
        }

        let net = vote_buy - vote_sell;

        if net.abs() < self.min_net_score {
            return None;
        }

        // Exit gate: when holding a position and the net vote is SELL,
        // require more strategy agreement than for entries. This prevents
        // one strategy from immediately unwinding what another opened.
        let num_sell_voters = vote_parts.iter().filter(|v| v.contains("SELL")).count();
        if has_position && net < 0.0 && num_sell_voters < self.min_exit_strategies {
            return None;
        }

        let votes = vote_parts.join("+");

        if net > 0.0 {
            // Net buy — use the strongest buy signal's reason and snapshot
            let (best, _) = best_buy?;
            Some(SignalOutput {
                side: Side::Buy,
                score: net,
                reason: best.reason,
                z_score: best.z_score,
                relative_volume: best.relative_volume,
                votes,
            })
        } else {
            // Net sell — use the strongest sell signal's reason and snapshot
            let (best, _) = best_sell?;
            Some(SignalOutput {
                side: Side::Sell,
                score: net.abs(),
                reason: best.reason,
                z_score: best.z_score,
                relative_volume: best.relative_volume,
                votes,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::signals::SignalReason;

    // --- Test helpers ---

    /// A mock strategy that always returns a fixed signal.
    struct FixedStrategy {
        signal: Option<SignalOutput>,
    }

    impl FixedStrategy {
        fn buy(score: f64, reason: SignalReason) -> Self {
            Self {
                signal: Some(SignalOutput {
                    side: Side::Buy,
                    score,
                    reason,
                    z_score: -2.5,
                    relative_volume: 1.5,
                    votes: String::new(),
                }),
            }
        }

        fn sell(score: f64, reason: SignalReason) -> Self {
            Self {
                signal: Some(SignalOutput {
                    side: Side::Sell,
                    score,
                    reason,
                    z_score: 2.0,
                    relative_volume: 1.3,
                    votes: String::new(),
                }),
            }
        }

        fn none() -> Self {
            Self { signal: None }
        }
    }

    impl Strategy for FixedStrategy {
        fn score(&self, _features: &FeatureValues, _has_position: bool) -> Option<SignalOutput> {
            self.signal.clone()
        }
    }

    fn entry(strategy: FixedStrategy, weight: f64, name: &'static str) -> StrategyEntry {
        StrategyEntry {
            strategy: Box::new(strategy),
            weight,
            name,
        }
    }

    fn warmed_features() -> FeatureValues {
        FeatureValues {
            warmed_up: true,
            ..Default::default()
        }
    }

    // --- Agreement tests ---

    #[test]
    fn both_buy_produces_strong_buy() {
        let combiner = StrategyCombiner::new(
            vec![
                entry(
                    FixedStrategy::buy(1.2, SignalReason::MeanReversionBuy),
                    0.5,
                    "mr",
                ),
                entry(
                    FixedStrategy::buy(0.8, SignalReason::MomentumBuy),
                    0.5,
                    "mom",
                ),
            ],
            0.2,
        );
        let sig = combiner.score(&warmed_features(), false).unwrap();
        assert_eq!(sig.side, Side::Buy);
        // net = 0.5*1.2 + 0.5*0.8 = 1.0
        assert!((sig.score - 1.0).abs() < 1e-10);
        // Best reason is MeanReversionBuy (0.5*1.2=0.6 > 0.5*0.8=0.4)
        assert_eq!(sig.reason, SignalReason::MeanReversionBuy);
    }

    #[test]
    fn both_sell_produces_strong_sell() {
        let combiner = StrategyCombiner::new(
            vec![
                entry(
                    FixedStrategy::sell(1.0, SignalReason::MeanReversionSell),
                    0.5,
                    "mr",
                ),
                entry(
                    FixedStrategy::sell(1.5, SignalReason::MomentumSell),
                    0.5,
                    "mom",
                ),
            ],
            0.2,
        );
        let sig = combiner.score(&warmed_features(), true).unwrap();
        assert_eq!(sig.side, Side::Sell);
        // net = -(0.5*1.0 + 0.5*1.5) = -1.25, score = 1.25
        assert!((sig.score - 1.25).abs() < 1e-10);
        // Best reason is MomentumSell (0.5*1.5=0.75 > 0.5*1.0=0.5)
        assert_eq!(sig.reason, SignalReason::MomentumSell);
    }

    // --- Disagreement tests ---

    #[test]
    fn conflicting_signals_cancel_out() {
        let combiner = StrategyCombiner::new(
            vec![
                entry(
                    FixedStrategy::buy(0.5, SignalReason::MeanReversionBuy),
                    0.5,
                    "mr",
                ),
                entry(
                    FixedStrategy::sell(0.4, SignalReason::MomentumSell),
                    0.5,
                    "mom",
                ),
            ],
            0.2,
        );
        // net = 0.5*0.5 - 0.5*0.4 = 0.05, below threshold 0.2
        assert!(combiner.score(&warmed_features(), false).is_none());
    }

    #[test]
    fn stronger_sell_wins_over_weaker_buy() {
        let combiner = StrategyCombiner::new(
            vec![
                entry(
                    FixedStrategy::buy(0.6, SignalReason::MeanReversionBuy),
                    0.5,
                    "mr",
                ),
                entry(
                    FixedStrategy::sell(1.5, SignalReason::MomentumSell),
                    0.5,
                    "mom",
                ),
            ],
            0.2,
        )
        .with_min_exit_strategies(1); // test combiner math, not exit consensus
        let sig = combiner.score(&warmed_features(), true).unwrap();
        assert_eq!(sig.side, Side::Sell);
        // net = 0.5*0.6 - 0.5*1.5 = -0.45
        assert!((sig.score - 0.45).abs() < 1e-10);
    }

    // --- Single strategy tests ---

    #[test]
    fn single_strategy_buy_passes_through() {
        let combiner = StrategyCombiner::new(
            vec![
                entry(FixedStrategy::none(), 0.5, "mr"),
                entry(
                    FixedStrategy::buy(2.0, SignalReason::MomentumBuy),
                    0.5,
                    "mom",
                ),
            ],
            0.2,
        );
        let sig = combiner.score(&warmed_features(), false).unwrap();
        assert_eq!(sig.side, Side::Buy);
        // net = 0.5*2.0 = 1.0
        assert!((sig.score - 1.0).abs() < 1e-10);
    }

    #[test]
    fn single_strategy_sell_passes_through() {
        let combiner = StrategyCombiner::new(
            vec![
                entry(
                    FixedStrategy::sell(1.0, SignalReason::MeanReversionSell),
                    0.5,
                    "mr",
                ),
                entry(FixedStrategy::none(), 0.5, "mom"),
            ],
            0.2,
        )
        .with_min_exit_strategies(1); // test combiner math, not exit consensus
        let sig = combiner.score(&warmed_features(), true).unwrap();
        assert_eq!(sig.side, Side::Sell);
        assert!((sig.score - 0.5).abs() < 1e-10);
    }

    // --- Threshold tests ---

    #[test]
    fn below_threshold_produces_no_signal() {
        let combiner = StrategyCombiner::new(
            vec![entry(
                FixedStrategy::buy(0.3, SignalReason::MeanReversionBuy),
                0.5,
                "mr",
            )],
            0.2,
        );
        // net = 0.5*0.3 = 0.15, below threshold 0.2
        assert!(combiner.score(&warmed_features(), false).is_none());
    }

    #[test]
    fn at_exact_threshold_fires() {
        let combiner = StrategyCombiner::new(
            vec![entry(
                FixedStrategy::buy(0.4, SignalReason::MeanReversionBuy),
                0.5,
                "mr",
            )],
            0.2,
        );
        // net = 0.5*0.4 = 0.2 = threshold (< check, so exact value passes)
        assert!(combiner.score(&warmed_features(), false).is_some());
    }

    #[test]
    fn just_above_threshold_fires() {
        let combiner = StrategyCombiner::new(
            vec![entry(
                FixedStrategy::buy(0.5, SignalReason::MeanReversionBuy),
                0.5,
                "mr",
            )],
            0.2,
        );
        // net = 0.5*0.5 = 0.25 > 0.2
        assert!(combiner.score(&warmed_features(), false).is_some());
    }

    #[test]
    fn zero_threshold_lets_everything_through() {
        let combiner = StrategyCombiner::new(
            vec![entry(
                FixedStrategy::buy(0.01, SignalReason::MeanReversionBuy),
                0.5,
                "mr",
            )],
            0.0,
        );
        // net = 0.005 > 0.0
        assert!(combiner.score(&warmed_features(), false).is_some());
    }

    // --- Weight tests ---

    #[test]
    fn higher_weight_amplifies_signal() {
        let low_weight = StrategyCombiner::new(
            vec![entry(
                FixedStrategy::buy(1.0, SignalReason::MomentumBuy),
                0.3,
                "mom",
            )],
            0.0,
        );
        let high_weight = StrategyCombiner::new(
            vec![entry(
                FixedStrategy::buy(1.0, SignalReason::MomentumBuy),
                0.7,
                "mom",
            )],
            0.0,
        );
        let low = low_weight.score(&warmed_features(), false).unwrap();
        let high = high_weight.score(&warmed_features(), false).unwrap();
        assert!(high.score > low.score);
    }

    #[test]
    fn asymmetric_weights_bias_toward_heavier_strategy() {
        // MeanRev has weight=0.7, Momentum has weight=0.3
        // Both have equal scores, but MeanRev should dominate
        let combiner = StrategyCombiner::new(
            vec![
                entry(
                    FixedStrategy::buy(1.0, SignalReason::MeanReversionBuy),
                    0.7,
                    "mr",
                ),
                entry(
                    FixedStrategy::sell(1.0, SignalReason::MomentumSell),
                    0.3,
                    "mom",
                ),
            ],
            0.2,
        );
        let sig = combiner.score(&warmed_features(), false).unwrap();
        // net = 0.7*1.0 - 0.3*1.0 = 0.4 → BUY (mean-rev wins due to weight)
        assert_eq!(sig.side, Side::Buy);
        assert!((sig.score - 0.4).abs() < 1e-10);
    }

    // --- Edge cases ---

    #[test]
    fn no_strategies_produces_no_signal() {
        let combiner = StrategyCombiner::new(vec![], 0.2);
        assert!(combiner.score(&warmed_features(), false).is_none());
    }

    #[test]
    fn all_strategies_silent_produces_no_signal() {
        let combiner = StrategyCombiner::new(
            vec![
                entry(FixedStrategy::none(), 0.5, "mr"),
                entry(FixedStrategy::none(), 0.5, "mom"),
            ],
            0.0,
        );
        assert!(combiner.score(&warmed_features(), false).is_none());
    }

    #[test]
    fn best_reason_tracks_highest_weighted_score() {
        let combiner = StrategyCombiner::new(
            vec![
                entry(
                    FixedStrategy::buy(0.5, SignalReason::MeanReversionBuy),
                    0.3,
                    "mr",
                ),
                entry(
                    FixedStrategy::buy(1.0, SignalReason::MomentumBuy),
                    0.7,
                    "mom",
                ),
            ],
            0.0,
        );
        let sig = combiner.score(&warmed_features(), false).unwrap();
        // Momentum: 0.7*1.0=0.7 > MeanRev: 0.3*0.5=0.15
        assert_eq!(sig.reason, SignalReason::MomentumBuy);
    }

    #[test]
    fn feature_snapshot_from_strongest_signal() {
        let combiner = StrategyCombiner::new(
            vec![
                entry(
                    FixedStrategy::buy(0.5, SignalReason::MeanReversionBuy),
                    0.3,
                    "mr",
                ),
                entry(
                    FixedStrategy::buy(1.0, SignalReason::MomentumBuy),
                    0.7,
                    "mom",
                ),
            ],
            0.0,
        );
        let sig = combiner.score(&warmed_features(), false).unwrap();
        // Momentum signal has z_score=-2.5, rel_vol=1.5 (from FixedStrategy::buy)
        assert!((sig.z_score - (-2.5)).abs() < 1e-10);
        assert!((sig.relative_volume - 1.5).abs() < 1e-10);
    }

    // --- min_strategies tests ---

    #[test]
    fn min_strategies_blocks_single_voter() {
        let combiner = StrategyCombiner::new(
            vec![
                entry(
                    FixedStrategy::buy(2.0, SignalReason::MomentumBuy),
                    0.5,
                    "mom",
                ),
                entry(FixedStrategy::none(), 0.5, "mr"),
            ],
            0.0, // no net score gate
        )
        .with_min_strategies(2);
        // Only 1 voter, need 2
        assert!(combiner.score(&warmed_features(), false).is_none());
    }

    #[test]
    fn min_strategies_allows_two_voters() {
        let combiner = StrategyCombiner::new(
            vec![
                entry(
                    FixedStrategy::buy(1.0, SignalReason::MomentumBuy),
                    0.5,
                    "mom",
                ),
                entry(
                    FixedStrategy::buy(0.8, SignalReason::MeanReversionBuy),
                    0.5,
                    "mr",
                ),
            ],
            0.0,
        )
        .with_min_strategies(2);
        // 2 voters, need 2 — passes
        let sig = combiner.score(&warmed_features(), false).unwrap();
        assert_eq!(sig.side, Side::Buy);
    }

    #[test]
    fn min_strategies_counts_opposing_voters() {
        let combiner = StrategyCombiner::new(
            vec![
                entry(
                    FixedStrategy::buy(1.0, SignalReason::MomentumBuy),
                    0.5,
                    "mom",
                ),
                entry(
                    FixedStrategy::sell(0.5, SignalReason::MeanReversionSell),
                    0.5,
                    "mr",
                ),
            ],
            0.0,
        )
        .with_min_strategies(2);
        // 2 voters (opposing sides) — still counts as 2 voters
        let sig = combiner.score(&warmed_features(), false).unwrap();
        assert_eq!(sig.side, Side::Buy); // net = 0.5 - 0.25 = 0.25
    }

    // --- Vote breakdown tests ---

    #[test]
    fn vote_breakdown_populated() {
        let combiner = StrategyCombiner::new(
            vec![
                entry(
                    FixedStrategy::buy(1.0, SignalReason::MeanReversionBuy),
                    0.5,
                    "mr",
                ),
                entry(
                    FixedStrategy::buy(0.8, SignalReason::MomentumBuy),
                    0.5,
                    "mom",
                ),
            ],
            0.0,
        );
        let sig = combiner.score(&warmed_features(), false).unwrap();
        assert!(sig.votes.contains("mr:BUY"));
        assert!(sig.votes.contains("mom:BUY"));
    }

    // --- Exit consensus tests ---

    #[test]
    fn single_sell_blocked_when_holding_with_exit_consensus() {
        // Momentum bought, VWAP wants to sell, mean-rev silent.
        // With min_exit_strategies=2, single VWAP sell should be blocked.
        let combiner = StrategyCombiner::new(
            vec![
                entry(FixedStrategy::none(), 0.4, "mr"),   // silent
                entry(FixedStrategy::none(), 0.35, "mom"), // silent
                entry(
                    FixedStrategy::sell(1.0, SignalReason::VwapReversionSell),
                    0.25,
                    "vwap",
                ),
            ],
            0.0, // no net score gate
        )
        .with_min_exit_strategies(2);

        // has_position=true, only 1 sell voter → blocked
        assert!(combiner.score(&warmed_features(), true).is_none());
    }

    #[test]
    fn two_sells_allowed_when_holding_with_exit_consensus() {
        // Both momentum and VWAP agree to sell.
        let combiner = StrategyCombiner::new(
            vec![
                entry(FixedStrategy::none(), 0.4, "mr"),
                entry(
                    FixedStrategy::sell(1.0, SignalReason::MomentumSell),
                    0.35,
                    "mom",
                ),
                entry(
                    FixedStrategy::sell(0.8, SignalReason::VwapReversionSell),
                    0.25,
                    "vwap",
                ),
            ],
            0.0,
        )
        .with_min_exit_strategies(2);

        // has_position=true, 2 sell voters → allowed
        let sig = combiner.score(&warmed_features(), true).unwrap();
        assert_eq!(sig.side, Side::Sell);
    }

    #[test]
    fn single_sell_still_works_when_not_holding() {
        // Entry sell (short signal) should not be gated by min_exit_strategies
        let combiner = StrategyCombiner::new(
            vec![
                entry(FixedStrategy::none(), 0.4, "mr"),
                entry(
                    FixedStrategy::sell(1.0, SignalReason::MomentumSell),
                    0.35,
                    "mom",
                ),
            ],
            0.0,
        )
        .with_min_exit_strategies(2);

        // has_position=false → min_exit_strategies doesn't apply
        let sig = combiner.score(&warmed_features(), false).unwrap();
        assert_eq!(sig.side, Side::Sell);
    }

    #[test]
    fn exit_consensus_does_not_block_buys_when_holding() {
        // Already holding, but combiner wants to buy more (shouldn't happen
        // in practice since engine blocks buys when holding, but test the gate)
        let combiner = StrategyCombiner::new(
            vec![entry(
                FixedStrategy::buy(1.0, SignalReason::MomentumBuy),
                0.5,
                "mom",
            )],
            0.0,
        )
        .with_min_exit_strategies(2);

        // has_position=true but signal is BUY → exit gate doesn't apply
        let sig = combiner.score(&warmed_features(), true).unwrap();
        assert_eq!(sig.side, Side::Buy);
    }
}
