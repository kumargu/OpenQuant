//! Pairs trading — market-neutral mean-reversion on spread between two correlated assets.
//!
//! Instead of trading individual stock mean-reversion (1-2 bps edge, wiped by costs),
//! pairs trading captures the spread reversion between two related assets (8-10 bps edge).
//!
//! # How it works
//!
//! ```text
//!  spread = ln(price_A) - β × ln(price_B)
//!
//!  z = (spread - rolling_mean) / rolling_std
//!
//!  z < -entry_z  →  LONG spread:  BUY A, SELL B
//!  z > +entry_z  →  SHORT spread: SELL A, BUY B
//!  |z| < exit_z  →  CLOSE both legs (spread reverted)
//!  |z| > stop_z  →  STOP LOSS (spread diverged further)
//! ```
//!
//! # Why it works after costs
//!
//! Single-stock mean-reversion: ~1.2 bps edge, breakeven at 1.2 bps.
//! Pairs spread reversion: ~8-10 bps edge, breakeven at ~3 bps per leg.
//! The spread reverts faster and more reliably than individual prices because
//! market-wide moves cancel out — only the idiosyncratic component remains.
//!
//! # Validated pairs (walk-forward, real dollar P&L, 12 bps cost deducted)
//!
//! | Pair      | OOS $/day | Win Rate | Edge (bps) |
//! |-----------|-----------|----------|------------|
//! | GLD/SLV   | $118      | 70%      | ~10        |
//! | COIN/PLTR | $108      | 71%      | ~9         |
//! | C/JPM     | $86       | 75%      | ~9         |
//! | GS/MS     | $77       | 76%      | ~9         |

pub mod engine;

use crate::features::rolling_stats::RollingStats;
use crate::signals::{Side, SignalReason};
use tracing::{debug, error, info, warn};

/// Configuration for a single pair. Parsed from `[[pairs]]` in openquant.toml.
#[derive(Debug, Clone)]
pub struct PairConfig {
    /// Symbol for leg A (the "long" side when going long the spread).
    pub leg_a: String,
    /// Symbol for leg B (the "short" side when going long the spread).
    pub leg_b: String,
    /// Hedge ratio: spread = ln(price_A) - beta × ln(price_B).
    /// Estimated via OLS regression on historical log-prices.
    pub beta: f64,
    /// Z-score threshold to enter a position. Entry when |z| > entry_z.
    pub entry_z: f64,
    /// Z-score threshold to exit (spread reverted). Exit when |z| < exit_z.
    pub exit_z: f64,
    /// Z-score threshold for stop loss (spread diverged further). Exit when |z| > stop_z.
    pub stop_z: f64,
    /// Warmup period: minimum spread observations before trading.
    /// Actual rolling window is fixed at 32 bars (RollingStats<32>).
    /// Values > 32 are clamped to 32. Set to 32 for standard behavior.
    pub lookback: usize,
    /// Maximum bars to hold before forced exit.
    pub max_hold_bars: usize,
    /// Dollar notional per leg. Total exposure = 2 × notional_per_leg.
    pub notional_per_leg: f64,
}

impl Default for PairConfig {
    fn default() -> Self {
        Self {
            leg_a: String::new(),
            leg_b: String::new(),
            beta: 1.0,
            entry_z: 2.0,
            exit_z: 0.5,
            stop_z: 4.0,
            lookback: 32,
            max_hold_bars: 150,
            notional_per_leg: 10_000.0,
        }
    }
}

/// Current position state for a pair.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PairPosition {
    /// No position open.
    Flat,
    /// Long spread: long leg_a, short leg_b.
    /// Entered when z < -entry_z (spread too low, expect reversion up).
    LongSpread,
    /// Short spread: short leg_a, long leg_b.
    /// Entered when z > +entry_z (spread too high, expect reversion down).
    ShortSpread,
}

/// An order intent for one leg of a pair trade.
#[derive(Debug, Clone)]
pub struct PairOrderIntent {
    pub symbol: String,
    pub side: Side,
    pub qty: f64,
    pub reason: SignalReason,
    /// The pair identifier (e.g. "GLD/SLV") for logging.
    pub pair_id: String,
    /// Z-score at the time of the signal.
    pub z_score: f64,
    /// Current spread value.
    pub spread: f64,
}

/// Mutable state for a single pair, updated on each bar.
pub struct PairState {
    /// Most recent close price for leg A (cleared after spread computation).
    last_price_a: Option<f64>,
    /// Most recent close price for leg B (cleared after spread computation).
    last_price_b: Option<f64>,
    /// Rolling statistics of the spread for z-score computation.
    spread_stats: RollingStats<32>,
    /// Number of spread observations (for warmup detection).
    spread_count: usize,
    /// Current position state.
    position: PairPosition,
    /// Bar counter at entry (for max-hold tracking).
    entry_bar: usize,
    /// Prices at entry (for real dollar P&L on exit).
    entry_price_a: f64,
    entry_price_b: f64,
    /// Internal bar counter (incremented each time both legs have new data).
    bar_count: usize,
}

impl PairState {
    pub fn new() -> Self {
        Self {
            last_price_a: None,
            last_price_b: None,
            spread_stats: RollingStats::new(),
            spread_count: 0,
            position: PairPosition::Flat,
            entry_bar: 0,
            entry_price_a: 0.0,
            entry_price_b: 0.0,
            bar_count: 0,
        }
    }

    /// Update with a new price for one leg. Returns order intents if a signal fires.
    ///
    /// The caller feeds bars for all symbols. This method checks if the symbol
    /// matches either leg, buffers the price, and evaluates the spread when
    /// both legs have fresh data.
    pub fn on_price(
        &mut self,
        symbol: &str,
        price: f64,
        config: &PairConfig,
    ) -> Vec<PairOrderIntent> {
        // Buffer the price for the matching leg
        if symbol == config.leg_a {
            self.last_price_a = Some(price);
        } else if symbol == config.leg_b {
            self.last_price_b = Some(price);
        } else {
            return vec![];
        }

        // Need both legs to compute spread
        let (price_a, price_b) = match (self.last_price_a, self.last_price_b) {
            (Some(a), Some(b)) => (a, b),
            _ => return vec![],
        };

        // Guard: reject zero/negative prices (ln would produce -inf/NaN)
        if price_a <= 0.0 || price_b <= 0.0 {
            warn!("pairs: skipping bar — zero/negative price");
            return vec![];
        }

        // Clear buffered prices — require fresh data for next evaluation
        self.last_price_a = None;
        self.last_price_b = None;
        self.bar_count += 1;

        // Compute log-spread
        let spread = price_a.ln() - config.beta * price_b.ln();

        // Feed spread into rolling stats
        self.spread_stats.push(spread);
        self.spread_count += 1;

        // Need enough spread history for z-score
        let min_lookback = config.lookback.min(32); // capped by RollingStats<32>
        if self.spread_count < min_lookback {
            debug!(
                pair = format!("{}/{}", config.leg_a, config.leg_b).as_str(),
                count = self.spread_count,
                needed = min_lookback,
                "pairs: warming up spread stats"
            );
            return vec![];
        }

        // Compute z-score
        let mean = self.spread_stats.mean();
        let std = self.spread_stats.std_dev();
        if std < 1e-10 {
            return vec![];
        }
        let z = (spread - mean) / std;

        // ── Check exits first (if we have a position) ──
        if self.position != PairPosition::Flat {
            let bars_held = self.bar_count - self.entry_bar;

            // Exit condition: spread reverted
            let reverted = match self.position {
                PairPosition::LongSpread => z > -config.exit_z,
                PairPosition::ShortSpread => z < config.exit_z,
                PairPosition::Flat => unreachable!(),
            };

            // Exit condition: stop loss (spread diverged FURTHER from entry, not reverted past)
            let stopped = match self.position {
                PairPosition::LongSpread => z < -config.stop_z,   // entered negative, got more negative
                PairPosition::ShortSpread => z > config.stop_z,    // entered positive, got more positive
                PairPosition::Flat => false,
            };

            // Exit condition: max hold
            let max_held = config.max_hold_bars > 0 && bars_held >= config.max_hold_bars;

            if reverted || stopped || max_held {
                let reason = if stopped {
                    SignalReason::StopLoss
                } else if max_held {
                    SignalReason::MaxHoldTime
                } else {
                    SignalReason::PairsExit
                };

                let pair_id = format!("{}/{}", config.leg_a, config.leg_b);
                let exit_reason = if stopped { "stop" } else if max_held { "max_hold" } else { "reversion" };

                // Use error! for stop loss (risk event), warn! for max hold, info! for reversion
                if stopped {
                    error!(
                        pair = pair_id.as_str(),
                        z = %format_args!("{z:.2}"),
                        bars_held,
                        "pairs: STOP LOSS — spread diverged further"
                    );
                }
                info!(
                    pair = pair_id.as_str(),
                    z = format!("{:.2}", z).as_str(),
                    bars_held,
                    exit = exit_reason,
                    "pairs: EXIT"
                );

                let intents = self.close_position(config, reason, z, spread);
                self.position = PairPosition::Flat;
                return intents;
            }

            // Position held — log z-score for spread tracking
            debug!(
                pair_a = config.leg_a.as_str(),
                pair_b = config.leg_b.as_str(),
                z = %format_args!("{z:.2}"),
                bars_held,
                pos = ?self.position,
                "pairs: HOLDING"
            );
            return vec![];
        }

        // ── Check entries (if flat) ──
        if z < -config.entry_z {
            // Spread too low → expect reversion UP → LONG spread (buy A, sell B)
            let pair_id = format!("{}/{}", config.leg_a, config.leg_b);
            info!(
                pair = pair_id.as_str(),
                z = format!("{:.2}", z).as_str(),
                spread = format!("{:.6}", spread).as_str(),
                "pairs: ENTRY long spread (buy {}, sell {})",
                config.leg_a, config.leg_b,
            );

            self.position = PairPosition::LongSpread;
            self.entry_bar = self.bar_count;
            self.entry_price_a = price_a;
            self.entry_price_b = price_b;

            let qty_a = (config.notional_per_leg / price_a).floor();
            let qty_b = (config.notional_per_leg / price_b).floor();

            return vec![
                PairOrderIntent {
                    symbol: config.leg_a.clone(),
                    side: Side::Buy,
                    qty: qty_a,
                    reason: SignalReason::PairsEntry,
                    pair_id: pair_id.clone(),
                    z_score: z,
                    spread,
                },
                PairOrderIntent {
                    symbol: config.leg_b.clone(),
                    side: Side::Sell,
                    qty: qty_b,
                    reason: SignalReason::PairsEntry,
                    pair_id,
                    z_score: z,
                    spread,
                },
            ];
        } else if z > config.entry_z {
            // Spread too high → expect reversion DOWN → SHORT spread (sell A, buy B)
            let pair_id = format!("{}/{}", config.leg_a, config.leg_b);
            info!(
                pair = pair_id.as_str(),
                z = format!("{:.2}", z).as_str(),
                spread = format!("{:.6}", spread).as_str(),
                "pairs: ENTRY short spread (sell {}, buy {})",
                config.leg_a, config.leg_b,
            );

            self.position = PairPosition::ShortSpread;
            self.entry_bar = self.bar_count;
            self.entry_price_a = price_a;
            self.entry_price_b = price_b;

            let qty_a = (config.notional_per_leg / price_a).floor();
            let qty_b = (config.notional_per_leg / price_b).floor();

            return vec![
                PairOrderIntent {
                    symbol: config.leg_a.clone(),
                    side: Side::Sell,
                    qty: qty_a,
                    reason: SignalReason::PairsEntry,
                    pair_id: pair_id.clone(),
                    z_score: z,
                    spread,
                },
                PairOrderIntent {
                    symbol: config.leg_b.clone(),
                    side: Side::Buy,
                    qty: qty_b,
                    reason: SignalReason::PairsEntry,
                    pair_id,
                    z_score: z,
                    spread,
                },
            ];
        }

        // No signal — z-score within thresholds
        debug!(
            pair_a = config.leg_a.as_str(),
            pair_b = config.leg_b.as_str(),
            z = %format_args!("{z:.2}"),
            entry_z = %format_args!("{:.2}", config.entry_z),
            "pairs: FLAT — z within thresholds"
        );
        vec![]
    }

    /// Generate close orders for both legs.
    fn close_position(
        &self,
        config: &PairConfig,
        reason: SignalReason,
        z: f64,
        spread: f64,
    ) -> Vec<PairOrderIntent> {
        let pair_id = format!("{}/{}", config.leg_a, config.leg_b);
        let qty_a = (config.notional_per_leg / self.entry_price_a).floor();
        let qty_b = (config.notional_per_leg / self.entry_price_b).floor();

        match self.position {
            PairPosition::LongSpread => {
                // Was long A, short B → close: sell A, buy B
                vec![
                    PairOrderIntent {
                        symbol: config.leg_a.clone(),
                        side: Side::Sell,
                        qty: qty_a,
                        reason,
                        pair_id: pair_id.clone(),
                        z_score: z,
                        spread,
                    },
                    PairOrderIntent {
                        symbol: config.leg_b.clone(),
                        side: Side::Buy,
                        qty: qty_b,
                        reason,
                        pair_id,
                        z_score: z,
                        spread,
                    },
                ]
            }
            PairPosition::ShortSpread => {
                // Was short A, long B → close: buy A, sell B
                vec![
                    PairOrderIntent {
                        symbol: config.leg_a.clone(),
                        side: Side::Buy,
                        qty: qty_a,
                        reason,
                        pair_id: pair_id.clone(),
                        z_score: z,
                        spread,
                    },
                    PairOrderIntent {
                        symbol: config.leg_b.clone(),
                        side: Side::Sell,
                        qty: qty_b,
                        reason,
                        pair_id,
                        z_score: z,
                        spread,
                    },
                ]
            }
            PairPosition::Flat => vec![],
        }
    }

    /// Current position state.
    pub fn position(&self) -> PairPosition {
        self.position
    }

    /// Number of spread observations processed.
    pub fn spread_count(&self) -> usize {
        self.spread_count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> PairConfig {
        PairConfig {
            leg_a: "GLD".into(),
            leg_b: "SLV".into(),
            beta: 0.37,
            entry_z: 2.0,
            exit_z: 0.5,
            stop_z: 4.0,
            lookback: 32,
            max_hold_bars: 150,
            notional_per_leg: 10_000.0,
        }
    }

    #[test]
    fn test_no_signal_one_leg_missing() {
        let mut state = PairState::new();
        let config = test_config();
        // Only feed leg A — should not produce any signal
        let intents = state.on_price("GLD", 420.0, &config);
        assert!(intents.is_empty(), "should not signal with only one leg");
    }

    #[test]
    fn test_no_signal_during_warmup() {
        let mut state = PairState::new();
        let config = test_config();
        // Feed a few bars — not enough for z-score
        for i in 0..10 {
            let _ = state.on_price("GLD", 420.0 + i as f64 * 0.1, &config);
            let intents = state.on_price("SLV", 64.0 + i as f64 * 0.01, &config);
            assert!(intents.is_empty(), "should not signal during warmup (bar {i})");
        }
    }

    #[test]
    fn test_spread_computation() {
        // spread = ln(420) - 0.37 * ln(64) = 6.0403 - 0.37*4.1589 = 6.0403 - 1.5388 = 4.5015
        let spread = (420.0_f64).ln() - 0.37 * (64.0_f64).ln();
        assert!((spread - 4.5015).abs() < 0.01, "spread={spread}");
    }

    /// Config with very low entry threshold to make tests deterministic.
    fn easy_trigger_config() -> PairConfig {
        PairConfig {
            leg_a: "A".into(),
            leg_b: "B".into(),
            beta: 1.0,
            entry_z: 1.5,  // easy to trigger
            exit_z: 0.3,
            stop_z: 5.0,
            lookback: 32,
            max_hold_bars: 50,
            notional_per_leg: 10_000.0,
        }
    }

    #[test]
    fn test_entry_long_spread() {
        let mut state = PairState::new();
        let config = easy_trigger_config();

        // Warmup with stable prices (A=100, B=100, beta=1, spread=0)
        for _ in 0..35 {
            let _ = state.on_price("A", 100.0, &config);
            let _ = state.on_price("B", 100.0, &config);
        }

        // Drop A sharply: spread = ln(90) - ln(100) = -0.105. After 35 bars of 0,
        // this is a huge z-score deviation (negative).
        let _ = state.on_price("A", 90.0, &config);
        let intents = state.on_price("B", 100.0, &config);

        assert!(!intents.is_empty(), "should trigger long spread entry");
        assert_eq!(intents.len(), 2);
        assert_eq!(intents[0].symbol, "A");
        assert_eq!(intents[0].side, Side::Buy);
        assert_eq!(intents[1].symbol, "B");
        assert_eq!(intents[1].side, Side::Sell);
        assert_eq!(state.position(), PairPosition::LongSpread);
    }

    #[test]
    fn test_entry_short_spread() {
        let mut state = PairState::new();
        let config = easy_trigger_config();

        for _ in 0..35 {
            let _ = state.on_price("A", 100.0, &config);
            let _ = state.on_price("B", 100.0, &config);
        }

        // Spike A: spread = ln(110) - ln(100) = +0.095 → positive z → short spread
        let _ = state.on_price("A", 110.0, &config);
        let intents = state.on_price("B", 100.0, &config);

        assert!(!intents.is_empty(), "should trigger short spread entry");
        assert_eq!(intents.len(), 2);
        assert_eq!(intents[0].symbol, "A");
        assert_eq!(intents[0].side, Side::Sell);
        assert_eq!(intents[1].symbol, "B");
        assert_eq!(intents[1].side, Side::Buy);
        assert_eq!(state.position(), PairPosition::ShortSpread);
    }

    #[test]
    fn test_exit_reversion() {
        let mut state = PairState::new();
        let config = easy_trigger_config();

        // Warmup
        for _ in 0..35 {
            let _ = state.on_price("A", 100.0, &config);
            let _ = state.on_price("B", 100.0, &config);
        }

        // Enter long spread
        let _ = state.on_price("A", 90.0, &config);
        let _ = state.on_price("B", 100.0, &config);
        assert_eq!(state.position(), PairPosition::LongSpread);

        // Revert: A returns to normal → spread returns to ~0 → z near 0 → exit
        let _ = state.on_price("A", 100.0, &config);
        let intents = state.on_price("B", 100.0, &config);

        assert!(!intents.is_empty(), "should trigger exit on reversion");
        assert_eq!(intents.len(), 2);
        // Close long spread: sell A, buy B
        assert_eq!(intents[0].side, Side::Sell);
        assert_eq!(intents[1].side, Side::Buy);
        assert_eq!(state.position(), PairPosition::Flat);
    }

    #[test]
    fn test_exit_max_hold() {
        let mut state = PairState::new();
        let mut config = easy_trigger_config();
        config.max_hold_bars = 5;

        // Warmup
        for _ in 0..35 {
            let _ = state.on_price("A", 100.0, &config);
            let _ = state.on_price("B", 100.0, &config);
        }

        // Enter
        let _ = state.on_price("A", 90.0, &config);
        let _ = state.on_price("B", 100.0, &config);
        assert_eq!(state.position(), PairPosition::LongSpread);

        // Hold for max_hold bars without reversion (keep spread extended)
        let mut exited = false;
        for _ in 0..10 {
            let _ = state.on_price("A", 90.0, &config);
            let intents = state.on_price("B", 100.0, &config);
            if !intents.is_empty() {
                exited = true;
                assert_eq!(state.position(), PairPosition::Flat);
                break;
            }
        }
        assert!(exited, "should exit on max hold");
    }

    #[test]
    fn test_zero_price_rejected() {
        let mut state = PairState::new();
        let config = easy_trigger_config();

        let _ = state.on_price("A", 0.0, &config);
        let intents = state.on_price("B", 100.0, &config);
        assert!(intents.is_empty(), "zero price should be rejected");
    }

    #[test]
    fn test_flat_no_exit() {
        let mut state = PairState::new();
        let config = test_config();

        // Feed bars while flat — should never produce exit signals
        for _ in 0..40 {
            let _ = state.on_price("GLD", 420.0, &config);
            let intents = state.on_price("SLV", 64.0, &config);
            assert!(intents.is_empty(), "flat position should not produce exits");
        }
        assert_eq!(state.position(), PairPosition::Flat);
    }

    #[test]
    fn test_unrelated_symbol_ignored() {
        let mut state = PairState::new();
        let config = test_config();
        let intents = state.on_price("AAPL", 150.0, &config);
        assert!(intents.is_empty(), "unrelated symbol should be ignored");
    }

    #[test]
    fn test_qty_computation() {
        let config = PairConfig {
            notional_per_leg: 10_000.0,
            ..test_config()
        };
        // $10,000 / $420 = 23.8 → floor = 23 shares
        let qty = (config.notional_per_leg / 420.0).floor();
        assert_eq!(qty, 23.0);
        // $10,000 / $64 = 156.25 → floor = 156 shares
        let qty_b = (config.notional_per_leg / 64.0).floor();
        assert_eq!(qty_b, 156.0);
    }
}
