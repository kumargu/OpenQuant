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

pub mod active_pairs;
pub mod engine;
// shadow.rs deleted — was fully implemented but never wired into PairsEngine.
// Promotion/demotion logic can be re-added when needed.

use crate::features::rolling_stats::RollingStats;
use crate::signals::{Side, SignalReason};
use std::collections::VecDeque;
use tracing::{debug, error, info, warn};

/// Trading parameters for the pairs strategy. Loaded from `[pairs_trading]` in `openquant.toml`.
///
/// Separated from per-pair identity (`PairConfig`) so parameters can be tuned
/// in TOML without recompilation, and all pairs share the same trading rules.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct PairsTradingConfig {
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
    /// Minimum bars to hold before strategy-driven exits (reversion).
    /// Stop loss always fires regardless of min_hold_bars.
    pub min_hold_bars: usize,
    /// Dollar notional per leg. Total exposure = 2 × notional_per_leg.
    pub notional_per_leg: f64,
    /// Last entry hour (ET, 0-23). No new entries after this hour.
    /// Default 14 = no entries after 14:59 ET (1 hour before close).
    pub last_entry_hour: u32,
    /// Force close hour (ET, 0-23). All positions closed at this hour.
    /// Default 15 = close at 15:30 ET (30 min before close).
    pub force_close_minute: u32,
    /// Round-trip cost in basis points for regime gate P&L tracking.
    /// Default 10 bps (5 bps per leg for Alpaca zero-commission, ~3-5 bps bid-ask).
    #[serde(default = "default_cost_bps")]
    pub cost_bps: f64,
    /// Timezone offset from UTC in hours. Used for entry cutoff and force close.
    /// -5 = EST, -4 = EDT. Should match `[data].timezone_offset_hours`.
    #[serde(default = "default_tz_offset")]
    pub tz_offset_hours: i32,
}

fn default_cost_bps() -> f64 {
    10.0
}

fn default_tz_offset() -> i32 {
    -5
}

impl Default for PairsTradingConfig {
    fn default() -> Self {
        Self {
            entry_z: 2.0,
            exit_z: 0.5,
            stop_z: 4.0,
            lookback: 32,
            max_hold_bars: 150,
            min_hold_bars: 30,
            notional_per_leg: 10_000.0,
            last_entry_hour: 14,
            force_close_minute: 930, // 15:30 ET = 15*60+30 = 930
            cost_bps: 10.0,
            tz_offset_hours: -5,
        }
    }
}

/// Identity for a single pair. Loaded from `active_pairs.json` (produced by pair-picker).
///
/// Contains only pair-specific data (symbols and regression coefficients).
/// Trading parameters live in `PairsTradingConfig`.
#[derive(Debug, Clone)]
pub struct PairConfig {
    /// Symbol for leg A (the "long" side when going long the spread).
    pub leg_a: String,
    /// Symbol for leg B (the "short" side when going long the spread).
    pub leg_b: String,
    /// OLS intercept: log_a = alpha + beta × log_b.
    /// Subtracted from spread for correct z-score computation.
    /// Estimated via OLS regression by pair-picker.
    pub alpha: f64,
    /// Hedge ratio: spread = ln(price_A) - alpha - beta × ln(price_B).
    /// Estimated via OLS regression on historical log-prices.
    pub beta: f64,
    /// OU mean-reversion rate κ (per day), derived from the OU half-life:
    /// κ = ln(2) / half_life_days.
    ///
    /// Used to compute the priority score at entry:
    ///   priority = |z| × sqrt(κ) / σ_spread
    ///
    /// A value of 0.0 means the half-life was unknown or invalid.
    pub kappa: f64,
    /// Per-pair max hold in bars (days for daily data). 0 = use global max_hold_bars.
    /// Set by pair-picker: min(ceil(2.5 × half_life), 10).
    pub max_hold_bars: usize,
}

impl Default for PairConfig {
    fn default() -> Self {
        Self {
            leg_a: String::new(),
            leg_b: String::new(),
            alpha: 0.0,
            beta: 1.0,
            kappa: 0.0,
            max_hold_bars: 0, // 0 = use global
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
    /// Priority score for signal-queue ranking:
    ///   priority = |z| × sqrt(κ) / σ_spread
    ///
    /// Larger = higher priority.  0.0 means κ was unknown (legacy pairs
    /// loaded without half_life_days, or σ_spread was degenerate).
    ///
    /// Reference: Avellaneda & Lee (2010).
    pub priority_score: f64,
}

/// Frozen spread statistics captured at trade entry.
///
/// The exit z-score is computed against these frozen stats, not the current
/// rolling window. This prevents rolling-stat drift from producing false exit
/// signals: if the spread drifts permanently, the rolling mean adapts and z
/// mechanically decays — but the frozen mean stays at the entry-time level,
/// so z can only decay if the spread *actually* moves back toward entry.
///
/// Ref: Issue #182 — "Guard against fake reversion from rolling-stat drift".
#[derive(Debug, Clone, Copy)]
pub struct ExitContext {
    /// Rolling mean of the spread at the moment of trade entry.
    pub entry_mean: f64,
    /// Rolling std-dev of the spread at the moment of trade entry.
    /// Must be > 0 (enforced at capture time).
    pub entry_std: f64,
}

impl ExitContext {
    /// Compute the exit z-score against the frozen entry-time statistics.
    ///
    /// `exit_z = (current_spread - entry_mean) / entry_std`
    ///
    /// This z-score measures reversion relative to the entry-time baseline.
    /// Unlike a rolling z-score, it cannot decay due to rolling-stat drift alone.
    #[inline]
    pub fn exit_z(&self, current_spread: f64) -> f64 {
        (current_spread - self.entry_mean) / self.entry_std
    }
}

/// Mutable state for a single pair, updated on each bar.
pub struct PairState {
    /// Most recent close price for leg A (cleared after spread computation).
    last_price_a: Option<f64>,
    /// Most recent close price for leg B (cleared after spread computation).
    last_price_b: Option<f64>,
    /// Rolling statistics of the spread for z-score computation.
    /// Window of 256 bars supports lookback up to 256 (configurable via TOML).
    /// Rolling window for spread z-score. Must match lookback config (default 32).
    /// Was <256> which caused z-scores to diverge from expected behavior — see #115.
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
    /// Per-pair exit_z override (for graceful exit of removed pairs).
    pub exit_z_override: Option<f64>,
    /// Per-pair stop_z override (for graceful exit of removed pairs).
    pub stop_z_override: Option<f64>,
    /// Recent trade P&L in bps (for regime gate). Tracks last 10 trades.
    trade_pnl_history: VecDeque<f64>,
    /// Whether entries are paused due to consecutive losses.
    paused: bool,
    /// Bars since pause started (for cooldown-based resume).
    pause_bars: usize,
    /// Number of consecutive stop-loss exits.
    consecutive_stops: u8,
    /// Frozen spread statistics captured at trade entry.
    ///
    /// `Some` when a position is open, `None` when flat.
    /// Used to compute exit z-score against the entry-time baseline,
    /// preventing rolling-stat drift from producing false exit signals.
    /// See `ExitContext` and issue #182.
    exit_context: Option<ExitContext>,
}

impl Default for PairState {
    fn default() -> Self {
        Self::new()
    }
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
            exit_z_override: None,
            stop_z_override: None,
            trade_pnl_history: VecDeque::with_capacity(10),
            paused: false,
            pause_bars: 0,
            consecutive_stops: 0,
            exit_context: None,
        }
    }

    /// Update with a new price for one leg. Returns order intents if a signal fires.
    ///
    /// The caller feeds bars for all symbols. This method checks if the symbol
    /// matches either leg, buffers the price, and evaluates the spread when
    /// both legs have fresh data.
    ///
    /// `config` provides pair identity (legs, alpha, beta).
    /// `trading` provides shared trading parameters (thresholds, sizing).
    pub fn on_price(
        &mut self,
        symbol: &str,
        price: f64,
        config: &PairConfig,
        trading: &PairsTradingConfig,
        timestamp: i64,
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

        // Compute log-spread: spread = ln(price_A) - alpha - beta × ln(price_B)
        // Alpha from OLS ensures z-scores are centered correctly.
        let spread = price_a.ln() - config.alpha - config.beta * price_b.ln();

        // Feed spread into rolling stats
        self.spread_stats.push(spread);
        self.spread_count += 1;

        // Need enough spread history for z-score
        let min_lookback = trading.lookback.min(32); // capped by RollingStats<32>
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

        // Compute time of day in local timezone (minutes since midnight)
        let tz_offset_ms: i64 = (trading.tz_offset_hours as i64) * 3600 * 1000;
        let local_ms = timestamp + tz_offset_ms;
        let secs_of_day = ((local_ms / 1000) % 86400 + 86400) % 86400;
        let et_hour = (secs_of_day / 3600) as u32;
        let et_min = ((secs_of_day % 3600) / 60) as u32;
        let et_minutes = et_hour * 60 + et_min;

        // ── Check exits first (if we have a position) ──
        if self.position != PairPosition::Flat {
            let bars_held = self.bar_count - self.entry_bar;

            // Use per-pair overrides if set (for graceful exit of removed pairs)
            let effective_exit_z = self.exit_z_override.unwrap_or(trading.exit_z);
            let effective_stop_z = self.stop_z_override.unwrap_or(trading.stop_z);

            // Compute exit z-score against frozen entry-time statistics.
            //
            // The rolling z (used for entries) can mechanically decay as the rolling
            // window adapts to a drifting spread — producing a false reversion signal.
            // The fixed-reference exit_z can only decay if the spread *actually* moves
            // back toward the entry-time mean. This eliminates fake exits from drift.
            //
            // Ref: Issue #182 — "Guard against fake reversion from rolling-stat drift".
            let exit_z = match &self.exit_context {
                Some(ctx) => {
                    let ez = ctx.exit_z(spread);
                    // Structured log when rolling z and fixed exit z disagree significantly.
                    // A large gap (> 1.0) means rolling-stat drift is in play — the rolling
                    // window has adapted but the spread has not actually reverted.
                    let drift = (z - ez).abs();
                    if drift > 1.0 {
                        let pair_id = format!("{}/{}", config.leg_a, config.leg_b);
                        warn!(
                            pair = pair_id.as_str(),
                            rolling_z = %format_args!("{z:.2}"),
                            fixed_exit_z = %format_args!("{ez:.2}"),
                            drift = %format_args!("{drift:.2}"),
                            bars_held,
                            "pairs: DRIFT DETECTED — rolling z diverges from fixed exit z (rolling-stat adaptation)"
                        );
                    }
                    ez
                }
                None => {
                    // Defensive fallback: no context means entry was not captured properly.
                    // Use rolling z to avoid blocking exits entirely.
                    warn!(
                        pair_a = config.leg_a.as_str(),
                        pair_b = config.leg_b.as_str(),
                        "pairs: exit_context missing for open position — falling back to rolling z"
                    );
                    z
                }
            };

            // Exit condition: spread reverted (against frozen entry-time baseline)
            let reverted = match self.position {
                PairPosition::LongSpread => exit_z > -effective_exit_z,
                PairPosition::ShortSpread => exit_z < effective_exit_z,
                PairPosition::Flat => unreachable!(),
            };

            // Exit condition: stop loss (spread diverged FURTHER from entry baseline)
            // Also uses frozen reference — stop fires when spread moves further from
            // the entry-time mean, not from the current rolling mean.
            let stopped = match self.position {
                PairPosition::LongSpread => exit_z < -effective_stop_z,
                PairPosition::ShortSpread => exit_z > effective_stop_z,
                PairPosition::Flat => false,
            };

            // Exit condition: max hold (per-pair override from pair-picker, or global fallback)
            let effective_max_hold = if config.max_hold_bars > 0 {
                config.max_hold_bars // per-pair HL-adaptive: ceil(2.5 * HL) capped at 10
            } else {
                trading.max_hold_bars // global fallback from TOML
            };
            let max_held = effective_max_hold > 0 && bars_held >= effective_max_hold;

            // Exit condition: force close before end of day (no overnight holding)
            let force_close = et_minutes >= trading.force_close_minute;
            if force_close {
                info!(
                    pair = format!("{}/{}", config.leg_a, config.leg_b).as_str(),
                    ts = timestamp,
                    et_hour,
                    et_min,
                    et_minutes,
                    force_close_minute = trading.force_close_minute,
                    "pairs: EOD FORCE CLOSE triggered"
                );
            }

            // Minimum hold: block reversion exits (but NOT stop loss) until min_hold_bars
            let past_min_hold = bars_held >= trading.min_hold_bars;
            let can_exit_reversion = reverted && past_min_hold;

            if can_exit_reversion || stopped || max_held || force_close {
                let reason = if stopped {
                    SignalReason::StopLoss
                } else if force_close || max_held {
                    SignalReason::MaxHoldTime
                } else {
                    SignalReason::PairsExit
                };

                let pair_id = format!("{}/{}", config.leg_a, config.leg_b);
                let exit_reason = if stopped {
                    "stop_loss"
                } else if force_close {
                    "eod_close"
                } else if max_held {
                    "max_hold"
                } else {
                    "reversion"
                };

                // Use error! for stop loss (risk event), warn! for max hold, info! for reversion
                if stopped {
                    error!(
                        pair = pair_id.as_str(),
                        ts = timestamp,
                        rolling_z = %format_args!("{z:.2}"),
                        fixed_exit_z = %format_args!("{exit_z:.2}"),
                        bars_held,
                        "pairs: STOP LOSS — spread diverged further from entry baseline"
                    );
                }
                info!(
                    pair = pair_id.as_str(),
                    ts = timestamp,
                    rolling_z = format!("{:.2}", z).as_str(),
                    fixed_exit_z = format!("{:.2}", exit_z).as_str(),
                    bars_held,
                    price_a = format!("{:.2}", price_a).as_str(),
                    price_b = format!("{:.2}", price_b).as_str(),
                    exit = exit_reason,
                    "pairs: EXIT"
                );

                // Record trade P&L for regime gate
                let ret_a = (price_a - self.entry_price_a) / self.entry_price_a;
                let ret_b = (price_b - self.entry_price_b) / self.entry_price_b;
                let gross_bps = match self.position {
                    PairPosition::LongSpread => (ret_a - ret_b) * 10_000.0,
                    PairPosition::ShortSpread => (ret_b - ret_a) * 10_000.0,
                    PairPosition::Flat => 0.0,
                };
                let net_bps = gross_bps - trading.cost_bps;
                if self.trade_pnl_history.len() >= 10 {
                    self.trade_pnl_history.pop_front();
                }
                self.trade_pnl_history.push_back(net_bps);

                if stopped {
                    self.consecutive_stops += 1;
                } else {
                    self.consecutive_stops = 0;
                }

                // Check regime gate: pause if last 5 trades all negative or 3 consecutive stops
                let recent_all_negative = self.trade_pnl_history.len() >= 5
                    && self
                        .trade_pnl_history
                        .iter()
                        .rev()
                        .take(5)
                        .all(|&p| p < 0.0);
                if (recent_all_negative || self.consecutive_stops >= 3) && !self.paused {
                    self.paused = true;
                    warn!(
                        pair = pair_id.as_str(),
                        consecutive_stops = self.consecutive_stops,
                        last_5_negative = recent_all_negative,
                        "pairs: REGIME GATE — pausing entries (recent trades losing)"
                    );
                }

                let intents = self.close_position(config, trading, reason, exit_z, spread);
                self.position = PairPosition::Flat;
                self.exit_context = None;
                return intents;
            }

            // Position held — log z-scores for spread tracking.
            // info! level: for daily bars this fires once/day/pair — essential for monitoring.
            info!(
                pair = format!("{}/{}", config.leg_a, config.leg_b).as_str(),
                rolling_z = %format_args!("{z:.2}"),
                frozen_exit_z = %format_args!("{exit_z:.2}"),
                bars_held,
                effective_max_hold,
                price_a = %format_args!("{price_a:.2}"),
                price_b = %format_args!("{price_b:.2}"),
                pos = ?self.position,
                "pairs: HOLDING"
            );
            return vec![];
        }

        // ── Check entries (if flat) ──
        // Regime gate: block entries when paused due to consecutive losses.
        // Cooldown: unpause after 500 bars (~2 trading days) to retry.
        // If still losing after retry, will re-pause within 5 more trades.
        if self.paused {
            self.pause_bars += 1;
            // Cooldown: unpause after enough bars to retry.
            // For daily bars: 5 bars = 1 week. For 1-min bars: 500 = ~2 trading days.
            // Use max_hold_bars as proxy: cooldown = 2× max hold period.
            let effective_max_hold = if config.max_hold_bars > 0 {
                config.max_hold_bars
            } else {
                trading.max_hold_bars
            };
            let cooldown = effective_max_hold.max(5) * 2;
            if self.pause_bars >= cooldown {
                self.paused = false;
                self.consecutive_stops = 0;
                self.pause_bars = 0;
                info!(
                    pair = format!("{}/{}", config.leg_a, config.leg_b).as_str(),
                    cooldown, "pairs: REGIME GATE lifted — cooldown expired, retrying"
                );
            } else {
                return vec![];
            }
        }

        // Block entries after last_entry_hour (avoid overnight risk)
        if et_hour >= trading.last_entry_hour {
            debug!(
                pair_a = config.leg_a.as_str(),
                pair_b = config.leg_b.as_str(),
                et_hour,
                z = %format_args!("{z:.2}"),
                "pairs: ENTRY BLOCKED — past last_entry_hour"
            );
            return vec![];
        }

        if z < -trading.entry_z {
            // Spread too low → expect reversion UP → LONG spread (buy A, sell B)
            let pair_id = format!("{}/{}", config.leg_a, config.leg_b);

            // Freeze the rolling stats at entry time for fixed-reference exit z-score.
            // Exit decisions will use these frozen values, not the evolving rolling window.
            // This prevents rolling-stat drift from producing false reversion signals.
            let entry_mean = self.spread_stats.mean();
            let entry_std = self.spread_stats.std_dev();
            // entry_std > 0 is guaranteed: we checked std < 1e-10 and returned early above.
            self.exit_context = Some(ExitContext {
                entry_mean,
                entry_std,
            });

            // Priority score: |z| × sqrt(κ) / σ_spread
            // Ranks this signal against concurrent pair signals.
            // 0.0 when κ=0 (unknown half-life from legacy pairs without half_life_days).
            let priority_score = if config.kappa > 0.0 && std > 1e-10 {
                z.abs() * config.kappa.sqrt() / std
            } else {
                0.0
            };

            info!(
                pair = pair_id.as_str(),
                ts = timestamp,
                z = format!("{:.2}", z).as_str(),
                spread = format!("{:.6}", spread).as_str(),
                entry_mean = format!("{:.6}", entry_mean).as_str(),
                entry_std = format!("{:.6}", entry_std).as_str(),
                price_a = format!("{:.2}", price_a).as_str(),
                price_b = format!("{:.2}", price_b).as_str(),
                priority_score = format!("{:.4}", priority_score).as_str(),
                "pairs: ENTRY long spread (buy {}, sell {})",
                config.leg_a,
                config.leg_b,
            );

            self.position = PairPosition::LongSpread;
            self.entry_bar = self.bar_count;
            self.entry_price_a = price_a;
            self.entry_price_b = price_b;

            let qty_a = (trading.notional_per_leg / price_a).floor();
            let qty_b = (trading.notional_per_leg / price_b).floor();

            return vec![
                PairOrderIntent {
                    symbol: config.leg_a.clone(),
                    side: Side::Buy,
                    qty: qty_a,
                    reason: SignalReason::PairsEntry,
                    pair_id: pair_id.clone(),
                    z_score: z,
                    spread,
                    priority_score,
                },
                PairOrderIntent {
                    symbol: config.leg_b.clone(),
                    side: Side::Sell,
                    qty: qty_b,
                    reason: SignalReason::PairsEntry,
                    pair_id,
                    z_score: z,
                    spread,
                    priority_score,
                },
            ];
        } else if z > trading.entry_z {
            // Spread too high → expect reversion DOWN → SHORT spread (sell A, buy B)
            let pair_id = format!("{}/{}", config.leg_a, config.leg_b);

            // Freeze the rolling stats at entry time for fixed-reference exit z-score.
            // Exit decisions will use these frozen values, not the evolving rolling window.
            // This prevents rolling-stat drift from producing false reversion signals.
            let entry_mean = self.spread_stats.mean();
            let entry_std = self.spread_stats.std_dev();
            // entry_std > 0 is guaranteed: we checked std < 1e-10 and returned early above.
            self.exit_context = Some(ExitContext {
                entry_mean,
                entry_std,
            });

            // Priority score: |z| × sqrt(κ) / σ_spread
            // Ranks this signal against concurrent pair signals.
            // 0.0 when κ=0 (unknown half-life from legacy pairs without half_life_days).
            let priority_score = if config.kappa > 0.0 && std > 1e-10 {
                z.abs() * config.kappa.sqrt() / std
            } else {
                0.0
            };

            info!(
                pair = pair_id.as_str(),
                ts = timestamp,
                z = format!("{:.2}", z).as_str(),
                spread = format!("{:.6}", spread).as_str(),
                entry_mean = format!("{:.6}", entry_mean).as_str(),
                entry_std = format!("{:.6}", entry_std).as_str(),
                price_a = format!("{:.2}", price_a).as_str(),
                price_b = format!("{:.2}", price_b).as_str(),
                priority_score = format!("{:.4}", priority_score).as_str(),
                "pairs: ENTRY short spread (sell {}, buy {})",
                config.leg_a,
                config.leg_b,
            );

            self.position = PairPosition::ShortSpread;
            self.entry_bar = self.bar_count;
            self.entry_price_a = price_a;
            self.entry_price_b = price_b;

            let qty_a = (trading.notional_per_leg / price_a).floor();
            let qty_b = (trading.notional_per_leg / price_b).floor();

            return vec![
                PairOrderIntent {
                    symbol: config.leg_a.clone(),
                    side: Side::Sell,
                    qty: qty_a,
                    reason: SignalReason::PairsEntry,
                    pair_id: pair_id.clone(),
                    z_score: z,
                    spread,
                    priority_score,
                },
                PairOrderIntent {
                    symbol: config.leg_b.clone(),
                    side: Side::Buy,
                    qty: qty_b,
                    reason: SignalReason::PairsEntry,
                    pair_id,
                    z_score: z,
                    spread,
                    priority_score,
                },
            ];
        }

        // No signal — z-score within entry thresholds.
        // info! level for daily bars: fires once/day/pair, essential for spread monitoring.
        info!(
            pair = format!("{}/{}", config.leg_a, config.leg_b).as_str(),
            z = %format_args!("{z:.2}"),
            entry_z = %format_args!("{:.2}", trading.entry_z),
            price_a = %format_args!("{price_a:.2}"),
            price_b = %format_args!("{price_b:.2}"),
            spread_count = self.spread_count,
            "pairs: FLAT — z within thresholds"
        );
        vec![]
    }

    /// Generate close orders for both legs.
    fn close_position(
        &self,
        config: &PairConfig,
        trading: &PairsTradingConfig,
        reason: SignalReason,
        z: f64,
        spread: f64,
    ) -> Vec<PairOrderIntent> {
        let pair_id = format!("{}/{}", config.leg_a, config.leg_b);
        let qty_a = (trading.notional_per_leg / self.entry_price_a).floor();
        let qty_b = (trading.notional_per_leg / self.entry_price_b).floor();

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
                        priority_score: 0.0, // close orders do not compete for capital
                    },
                    PairOrderIntent {
                        symbol: config.leg_b.clone(),
                        side: Side::Buy,
                        qty: qty_b,
                        reason,
                        pair_id,
                        z_score: z,
                        spread,
                        priority_score: 0.0,
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
                        priority_score: 0.0,
                    },
                    PairOrderIntent {
                        symbol: config.leg_b.clone(),
                        side: Side::Sell,
                        qty: qty_b,
                        reason,
                        pair_id,
                        z_score: z,
                        spread,
                        priority_score: 0.0,
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

    /// Force flat — reset position without emitting orders.
    /// Used after warmup to clear phantom positions while keeping rolling stats warm.
    pub fn force_flat(&mut self) {
        self.position = PairPosition::Flat;
        self.exit_context = None;
    }

    /// Number of spread observations processed.
    pub fn spread_count(&self) -> usize {
        self.spread_count
    }

    /// Frozen exit context (entry-time mean and std for fixed-reference exit z-score).
    ///
    /// `Some` when a position is open, `None` when flat.
    /// Used in tests to verify entry-time stats were captured correctly.
    pub fn exit_context(&self) -> Option<ExitContext> {
        self.exit_context
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> PairConfig {
        PairConfig {
            leg_a: "GLD".into(),
            leg_b: "SLV".into(),
            alpha: 0.0,
            beta: 0.37,
            kappa: 0.0, // unknown in unit tests
            max_hold_bars: 0,
        }
    }

    fn test_trading() -> PairsTradingConfig {
        PairsTradingConfig {
            min_hold_bars: 0,
            ..PairsTradingConfig::default()
        }
    }

    #[test]
    fn test_no_signal_one_leg_missing() {
        let mut state = PairState::new();
        let config = test_config();
        let trading = test_trading();
        // Only feed leg A — should not produce any signal
        let intents = state.on_price("GLD", 420.0, &config, &trading, 0);
        assert!(intents.is_empty(), "should not signal with only one leg");
    }

    #[test]
    fn test_no_signal_during_warmup() {
        let mut state = PairState::new();
        let config = test_config();
        let trading = test_trading();
        // Feed a few bars — not enough for z-score
        for i in 0..10 {
            let _ = state.on_price("GLD", 420.0 + i as f64 * 0.1, &config, &trading, 0);
            let intents = state.on_price("SLV", 64.0 + i as f64 * 0.01, &config, &trading, 0);
            assert!(
                intents.is_empty(),
                "should not signal during warmup (bar {i})"
            );
        }
    }

    #[test]
    fn test_spread_computation() {
        // spread = ln(420) - 0.37 * ln(64) = 6.0403 - 0.37*4.1589 = 6.0403 - 1.5388 = 4.5015
        let spread = (420.0_f64).ln() - 0.37 * (64.0_f64).ln();
        assert!((spread - 4.5015).abs() < 0.01, "spread={spread}");
    }

    fn easy_trigger_config() -> PairConfig {
        PairConfig {
            leg_a: "A".into(),
            leg_b: "B".into(),
            alpha: 0.0,
            beta: 1.0,
            kappa: f64::ln(2.0) / 10.0, // 10-day OU half-life for test priority scoring
            max_hold_bars: 0,
        }
    }

    /// Trading config with very low entry threshold to make tests deterministic.
    /// Time-of-day guards are fully permissive so timestamp=0 works in unit tests.
    fn easy_trigger_trading() -> PairsTradingConfig {
        PairsTradingConfig {
            entry_z: 1.5, // easy to trigger
            exit_z: 0.3,
            stop_z: 5.0,
            lookback: 32,
            max_hold_bars: 50,
            min_hold_bars: 0, // no minimum for unit tests
            notional_per_leg: 10_000.0,
            last_entry_hour: 24,       // never block entries
            force_close_minute: 1_500, // never force close
            tz_offset_hours: -5,
            cost_bps: 10.0,
        }
    }

    #[test]
    fn test_entry_long_spread() {
        let mut state = PairState::new();
        let config = easy_trigger_config();
        let trading = easy_trigger_trading();

        // Warmup with stable prices (A=100, B=100, beta=1, spread=0)
        for _ in 0..35 {
            let _ = state.on_price("A", 100.0, &config, &trading, 0);
            let _ = state.on_price("B", 100.0, &config, &trading, 0);
        }

        // Drop A sharply: spread = ln(90) - ln(100) = -0.105. After 35 bars of 0,
        // this is a huge z-score deviation (negative).
        let _ = state.on_price("A", 90.0, &config, &trading, 0);
        let intents = state.on_price("B", 100.0, &config, &trading, 0);

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
        let trading = easy_trigger_trading();

        for _ in 0..35 {
            let _ = state.on_price("A", 100.0, &config, &trading, 0);
            let _ = state.on_price("B", 100.0, &config, &trading, 0);
        }

        // Spike A: spread = ln(110) - ln(100) = +0.095 → positive z → short spread
        let _ = state.on_price("A", 110.0, &config, &trading, 0);
        let intents = state.on_price("B", 100.0, &config, &trading, 0);

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
        let trading = easy_trigger_trading();

        // Warmup
        for _ in 0..35 {
            let _ = state.on_price("A", 100.0, &config, &trading, 0);
            let _ = state.on_price("B", 100.0, &config, &trading, 0);
        }

        // Enter long spread
        let _ = state.on_price("A", 90.0, &config, &trading, 0);
        let _ = state.on_price("B", 100.0, &config, &trading, 0);
        assert_eq!(state.position(), PairPosition::LongSpread);

        // Revert: A returns to normal → spread returns to ~0 → z near 0 → exit
        let _ = state.on_price("A", 100.0, &config, &trading, 0);
        let intents = state.on_price("B", 100.0, &config, &trading, 0);

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
        let config = easy_trigger_config();
        let mut trading = easy_trigger_trading();
        trading.max_hold_bars = 5;

        // Warmup
        for _ in 0..35 {
            let _ = state.on_price("A", 100.0, &config, &trading, 0);
            let _ = state.on_price("B", 100.0, &config, &trading, 0);
        }

        // Enter
        let _ = state.on_price("A", 90.0, &config, &trading, 0);
        let _ = state.on_price("B", 100.0, &config, &trading, 0);
        assert_eq!(state.position(), PairPosition::LongSpread);

        // Hold for max_hold bars without reversion (keep spread extended)
        let mut exited = false;
        for _ in 0..10 {
            let _ = state.on_price("A", 90.0, &config, &trading, 0);
            let intents = state.on_price("B", 100.0, &config, &trading, 0);
            if !intents.is_empty() {
                exited = true;
                assert_eq!(state.position(), PairPosition::Flat);
                break;
            }
        }
        assert!(exited, "should exit on max hold");
    }

    /// Warmup with oscillating spread so rolling stats have realistic variance.
    ///
    /// The fixed-reference exit z-score uses entry_mean and entry_std from the rolling
    /// window at entry time. A constant-spread warmup produces entry_std ≈ 0, which makes
    /// all subsequent spreads appear as huge z-scores. Realistic warmup gives entry_std
    /// proportional to normal spread volatility, so the stop-loss and exit thresholds
    /// behave as intended.
    fn warmup_with_variance(
        state: &mut PairState,
        config: &PairConfig,
        trading: &PairsTradingConfig,
    ) {
        // Alternate A between 100 and 101 to produce spread variance ≈ ln(101/100) ≈ 0.01
        // This gives entry_std ≈ 0.005 after 32+ bars.
        for i in 0..35 {
            let a = if i % 2 == 0 { 100.0 } else { 101.0 };
            let _ = state.on_price("A", a, config, trading, 0);
            let _ = state.on_price("B", 100.0, config, trading, 0);
        }
    }

    #[test]
    fn test_min_hold_blocks_reversion() {
        let mut state = PairState::new();
        let config = easy_trigger_config();
        let mut trading = easy_trigger_trading();
        trading.min_hold_bars = 10;
        // Widen stop so that holding at entry spread level (not diverging further) doesn't
        // trigger the stop loss. With realistic entry_std ≈ 0.005 and entry z ≈ -1.5,
        // the stop at 20σ below entry_mean will not be triggered by holding at entry level.
        trading.stop_z = 20.0;

        // Warmup with variance so entry_std is realistic
        warmup_with_variance(&mut state, &config, &trading);

        // Enter long spread (A drops sharply, spread becomes very negative)
        let _ = state.on_price("A", 90.0, &config, &trading, 0);
        let _ = state.on_price("B", 100.0, &config, &trading, 0);
        assert_eq!(state.position(), PairPosition::LongSpread);

        let ctx = state.exit_context().expect("exit_context set at entry");
        let entry_spread = (90.0_f64).ln() - (100.0_f64).ln(); // ≈ -0.105
        let entry_exit_z = ctx.exit_z(entry_spread);
        // At entry, fixed exit_z ≈ rolling z (entry z), which triggered the long entry.
        // It must be below -exit_z so no exit fires while spread stays at entry level.
        assert!(
            entry_exit_z < -trading.exit_z,
            "at entry, fixed exit_z={entry_exit_z:.3} should be below -exit_z={:.1}",
            trading.exit_z
        );
        // And it must NOT be below -stop_z (we'd stop-loss immediately if so)
        assert!(
            entry_exit_z > -trading.stop_z,
            "at entry, fixed exit_z={entry_exit_z:.3} should be above -stop_z={:.1}",
            trading.stop_z
        );

        // Hold with spread extended (A stays at 90) for min_hold bars.
        // The fixed exit_z stays at the entry level → no exit or stop fires.
        for bar in 0..9 {
            let _ = state.on_price("A", 90.0, &config, &trading, 0);
            let intents = state.on_price("B", 100.0, &config, &trading, 0);
            assert!(
                intents.is_empty(),
                "should hold position during min_hold (bar {bar}), spread at entry level"
            );
        }
        assert_eq!(
            state.position(),
            PairPosition::LongSpread,
            "should still be in position during min_hold"
        );

        // Revert: A returns to 100 → spread → 0 ≈ entry_mean → exit_z ≈ 0 → exit fires
        let _ = state.on_price("A", 100.0, &config, &trading, 0);
        let intents = state.on_price("B", 100.0, &config, &trading, 0);
        assert!(
            !intents.is_empty(),
            "should exit after min_hold_bars when spread reverts"
        );
        assert_eq!(state.position(), PairPosition::Flat);
    }

    #[test]
    fn test_min_hold_does_not_block_stop_loss() {
        let mut state = PairState::new();
        let config = easy_trigger_config();
        let mut trading = easy_trigger_trading();
        trading.min_hold_bars = 100; // very high — should not block stop loss
        trading.stop_z = 3.0; // stop at 3σ below entry_mean

        // Warmup with variance so entry_std is realistic (≈ 0.005)
        warmup_with_variance(&mut state, &config, &trading);

        // Enter long spread (z negative, around entry_z threshold)
        let _ = state.on_price("A", 90.0, &config, &trading, 0);
        let _ = state.on_price("B", 100.0, &config, &trading, 0);
        assert_eq!(state.position(), PairPosition::LongSpread);

        // Spread diverges FURTHER (A drops more) relative to the entry-time baseline.
        // With entry_std ≈ 0.005, entry_mean ≈ 0, and stop_z = 3.0:
        // Stop fires when exit_z < -3.0, i.e., spread < entry_mean - 3 * entry_std ≈ -0.015.
        // A=70 gives spread ≈ ln(70/100) ≈ -0.357, which is far below the stop threshold.
        let _ = state.on_price("A", 70.0, &config, &trading, 0);
        let intents = state.on_price("B", 100.0, &config, &trading, 0);
        assert!(
            !intents.is_empty(),
            "stop loss must fire regardless of min_hold_bars when spread diverges further"
        );
        assert_eq!(state.position(), PairPosition::Flat);
    }

    #[test]
    fn test_zero_price_rejected() {
        let mut state = PairState::new();
        let config = easy_trigger_config();
        let trading = easy_trigger_trading();

        let _ = state.on_price("A", 0.0, &config, &trading, 0);
        let intents = state.on_price("B", 100.0, &config, &trading, 0);
        assert!(intents.is_empty(), "zero price should be rejected");
    }

    #[test]
    fn test_flat_no_exit() {
        let mut state = PairState::new();
        let config = test_config();
        let trading = test_trading();

        // Feed bars while flat — should never produce exit signals
        for _ in 0..40 {
            let _ = state.on_price("GLD", 420.0, &config, &trading, 0);
            let intents = state.on_price("SLV", 64.0, &config, &trading, 0);
            assert!(intents.is_empty(), "flat position should not produce exits");
        }
        assert_eq!(state.position(), PairPosition::Flat);
    }

    #[test]
    fn test_unrelated_symbol_ignored() {
        let mut state = PairState::new();
        let config = test_config();
        let trading = test_trading();
        let intents = state.on_price("AAPL", 150.0, &config, &trading, 0);
        assert!(intents.is_empty(), "unrelated symbol should be ignored");
    }

    #[test]
    fn test_qty_computation() {
        let trading = PairsTradingConfig {
            notional_per_leg: 10_000.0,
            ..PairsTradingConfig::default()
        };
        // $10,000 / $420 = 23.8 → floor = 23 shares
        let qty = (trading.notional_per_leg / 420.0).floor();
        assert_eq!(qty, 23.0);
        // $10,000 / $64 = 156.25 → floor = 156 shares
        let qty_b = (trading.notional_per_leg / 64.0).floor();
        assert_eq!(qty_b, 156.0);
    }

    // ── ExitContext and fixed-reference exit z-score tests ──────────────────
    // Ref: Issue #182 — Guard against fake reversion from rolling-stat drift

    /// ExitContext::exit_z computes the correct deviation from frozen baseline.
    #[test]
    fn test_exit_context_exit_z() {
        let ctx = ExitContext {
            entry_mean: 0.5,
            entry_std: 0.1,
        };
        // Spread at entry mean → exit_z = 0.0
        assert!((ctx.exit_z(0.5) - 0.0).abs() < 1e-12, "at mean, exit_z=0");
        // Spread at mean + 2 std → exit_z = 2.0
        assert!((ctx.exit_z(0.7) - 2.0).abs() < 1e-12, "exit_z=2.0");
        // Spread at mean - 3 std → exit_z = -3.0
        assert!((ctx.exit_z(0.2) - -3.0).abs() < 1e-12, "exit_z=-3.0");
    }

    /// Entry captures exit_context with current rolling mean and std.
    #[test]
    fn test_exit_context_captured_at_entry() {
        let mut state = PairState::new();
        let config = easy_trigger_config();
        let trading = easy_trigger_trading();

        // Initially no context
        assert!(state.exit_context().is_none(), "no context before entry");

        // Warmup with stable prices (spread = 0)
        for _ in 0..35 {
            let _ = state.on_price("A", 100.0, &config, &trading, 0);
            let _ = state.on_price("B", 100.0, &config, &trading, 0);
        }

        // Enter long spread (A drops, making spread very negative)
        let _ = state.on_price("A", 90.0, &config, &trading, 0);
        let _ = state.on_price("B", 100.0, &config, &trading, 0);
        assert_eq!(state.position(), PairPosition::LongSpread);

        // ExitContext should now be set
        let ctx = state
            .exit_context()
            .expect("exit_context must be set after entry");
        // entry_mean ≈ 0 (warmup was all zeros), entry_std > 0 (small from warmup noise)
        assert!(
            ctx.entry_mean.is_finite(),
            "entry_mean must be finite, got {}",
            ctx.entry_mean
        );
        assert!(
            ctx.entry_std > 0.0 && ctx.entry_std.is_finite(),
            "entry_std must be > 0 and finite, got {}",
            ctx.entry_std
        );
    }

    /// Exit context is cleared on position close.
    #[test]
    fn test_exit_context_cleared_on_close() {
        let mut state = PairState::new();
        let config = easy_trigger_config();
        let trading = easy_trigger_trading();

        // Warmup and enter
        for _ in 0..35 {
            let _ = state.on_price("A", 100.0, &config, &trading, 0);
            let _ = state.on_price("B", 100.0, &config, &trading, 0);
        }
        let _ = state.on_price("A", 90.0, &config, &trading, 0);
        let _ = state.on_price("B", 100.0, &config, &trading, 0);
        assert_eq!(state.position(), PairPosition::LongSpread);
        assert!(state.exit_context().is_some());

        // Revert to trigger exit
        let _ = state.on_price("A", 100.0, &config, &trading, 0);
        let _ = state.on_price("B", 100.0, &config, &trading, 0);
        assert_eq!(state.position(), PairPosition::Flat);
        assert!(
            state.exit_context().is_none(),
            "exit_context must be cleared after position closes"
        );
    }

    /// Permanent spread drift must NOT produce a false exit signal.
    ///
    /// Scenario: spread enters at -2σ (z = -2.0), then drifts permanently lower.
    /// The rolling window adapts to the new level, so rolling z decays toward 0.
    /// The fixed exit z stays at the entry-time baseline → no false exit.
    ///
    /// This is the core correctness test for Issue #182.
    #[test]
    fn test_permanent_drift_no_false_exit() {
        let mut state = PairState::new();
        let config = easy_trigger_config();
        let trading = PairsTradingConfig {
            entry_z: 1.5,
            exit_z: 0.5,
            stop_z: 6.0, // wide stop to not confuse with drift test
            lookback: 32,
            max_hold_bars: 0, // disable max hold — we want to test rolling drift specifically
            min_hold_bars: 0,
            notional_per_leg: 10_000.0,
            last_entry_hour: 24,
            force_close_minute: 1_500,
            tz_offset_hours: -5,
            cost_bps: 10.0,
        };

        // Warmup: stable spread = ln(100) - ln(100) = 0
        for _ in 0..35 {
            let _ = state.on_price("A", 100.0, &config, &trading, 0);
            let _ = state.on_price("B", 100.0, &config, &trading, 0);
        }

        // Enter long spread: A drops to 90, spread becomes very negative, z << -1.5
        let _ = state.on_price("A", 90.0, &config, &trading, 0);
        let _ = state.on_price("B", 100.0, &config, &trading, 0);
        assert_eq!(
            state.position(),
            PairPosition::LongSpread,
            "should enter on large negative z"
        );

        let ctx = state.exit_context().expect("exit_context set at entry");
        let entry_mean = ctx.entry_mean;
        let entry_std = ctx.entry_std;
        assert!(
            entry_std > 0.0,
            "entry_std must be positive, got {entry_std}"
        );

        // Permanent drift: A stays at 90, spread stays locked at entry level.
        // Over 32+ bars, the rolling window fills with the new (lower) spread value.
        // Rolling mean adapts → rolling z decays toward 0 mechanically.
        // Fixed exit z = (spread - entry_mean) / entry_std should stay at entry level.
        let mut false_exits = 0usize;
        for _ in 0..40 {
            let _ = state.on_price("A", 90.0, &config, &trading, 0);
            let intents = state.on_price("B", 100.0, &config, &trading, 0);
            if !intents.is_empty() && state.position() == PairPosition::Flat {
                false_exits += 1;
            }
        }
        assert_eq!(
            false_exits, 0,
            "permanent drift must NOT produce a false exit (rolling-stat drift guard)"
        );
        assert_eq!(
            state.position(),
            PairPosition::LongSpread,
            "position should remain open — spread never reverted"
        );

        // Verify: the fixed exit z stays approximately at the entry-time level.
        // current_spread ≈ ln(90) - ln(100) = -0.105, entry_mean ≈ 0.
        let current_spread = (90.0_f64).ln() - (100.0_f64).ln();
        let fixed_exit_z = ctx.exit_z(current_spread);
        // The fixed exit_z must still be clearly below -exit_z (= -0.5),
        // confirming no reversion has occurred.
        assert!(
            fixed_exit_z < -trading.exit_z,
            "fixed_exit_z={fixed_exit_z:.3} should remain below -exit_z={:.1} after permanent drift",
            trading.exit_z
        );

        // Sanity: entry_mean and entry_std from the frozen context match what we captured
        assert!(
            (ctx.entry_mean - entry_mean).abs() < 1e-12,
            "entry_mean should not change"
        );
        assert!(
            (ctx.entry_std - entry_std).abs() < 1e-12,
            "entry_std should not change"
        );
    }

    /// True reversion produces a correct exit signal.
    ///
    /// Scenario: spread enters at -2σ, then genuinely reverts back to the entry-time
    /// mean. The fixed exit z should cross the exit threshold → position closes.
    #[test]
    fn test_true_reversion_triggers_exit() {
        let mut state = PairState::new();
        let config = easy_trigger_config();
        let trading = easy_trigger_trading();

        // Warmup: stable spread = 0
        for _ in 0..35 {
            let _ = state.on_price("A", 100.0, &config, &trading, 0);
            let _ = state.on_price("B", 100.0, &config, &trading, 0);
        }

        // Enter long spread: A drops to 90
        let _ = state.on_price("A", 90.0, &config, &trading, 0);
        let _ = state.on_price("B", 100.0, &config, &trading, 0);
        assert_eq!(state.position(), PairPosition::LongSpread);

        // True reversion: A returns to 100. Spread = 0 ≈ entry_mean.
        // fixed exit_z ≈ (0 - entry_mean) / entry_std ≈ 0 → well above -exit_z (= -0.3)
        let _ = state.on_price("A", 100.0, &config, &trading, 0);
        let intents = state.on_price("B", 100.0, &config, &trading, 0);

        assert!(
            !intents.is_empty(),
            "true reversion must trigger exit signal"
        );
        assert_eq!(
            intents.len(),
            2,
            "exit closes both legs, got {} intents",
            intents.len()
        );
        assert_eq!(
            state.position(),
            PairPosition::Flat,
            "position must close on reversion"
        );
    }

    /// Short spread: permanent drift must not produce false exit.
    ///
    /// Scenario: spread enters at +2σ (short), then drifts further UP permanently.
    /// Rolling window adapts → rolling z decays. Fixed exit z stays at entry level.
    #[test]
    fn test_short_spread_permanent_drift_no_false_exit() {
        let mut state = PairState::new();
        let config = easy_trigger_config();
        let trading = PairsTradingConfig {
            entry_z: 1.5,
            exit_z: 0.5,
            stop_z: 6.0,
            lookback: 32,
            max_hold_bars: 0,
            min_hold_bars: 0,
            notional_per_leg: 10_000.0,
            last_entry_hour: 24,
            force_close_minute: 1_500,
            tz_offset_hours: -5,
            cost_bps: 10.0,
        };

        // Warmup: stable spread = 0
        for _ in 0..35 {
            let _ = state.on_price("A", 100.0, &config, &trading, 0);
            let _ = state.on_price("B", 100.0, &config, &trading, 0);
        }

        // Enter short spread: A spikes to 115, spread = ln(115) - ln(100) = +0.139 > 0
        // After 35 bars of 0, this is a large positive z → should trigger short spread entry
        let _ = state.on_price("A", 115.0, &config, &trading, 0);
        let _ = state.on_price("B", 100.0, &config, &trading, 0);
        assert_eq!(
            state.position(),
            PairPosition::ShortSpread,
            "should enter short on large positive z"
        );

        // Permanent drift: A stays at 115. Rolling window fills with high spread.
        // Rolling z decays mechanically. Fixed exit z stays at entry level.
        let mut false_exits = 0usize;
        for _ in 0..40 {
            let _ = state.on_price("A", 115.0, &config, &trading, 0);
            let intents = state.on_price("B", 100.0, &config, &trading, 0);
            if !intents.is_empty() && state.position() == PairPosition::Flat {
                false_exits += 1;
            }
        }
        assert_eq!(
            false_exits, 0,
            "permanent drift (short) must NOT produce a false exit"
        );
        assert_eq!(
            state.position(),
            PairPosition::ShortSpread,
            "position should remain open — spread never reverted"
        );
    }

    /// Short spread: true reversion triggers correct exit.
    #[test]
    fn test_short_spread_true_reversion_triggers_exit() {
        let mut state = PairState::new();
        let config = easy_trigger_config();
        let trading = easy_trigger_trading();

        // Warmup
        for _ in 0..35 {
            let _ = state.on_price("A", 100.0, &config, &trading, 0);
            let _ = state.on_price("B", 100.0, &config, &trading, 0);
        }

        // Enter short spread: A spikes to 110
        let _ = state.on_price("A", 110.0, &config, &trading, 0);
        let _ = state.on_price("B", 100.0, &config, &trading, 0);
        assert_eq!(state.position(), PairPosition::ShortSpread);

        // True reversion: A returns to 100 → spread returns to ~0 ≈ entry_mean
        let _ = state.on_price("A", 100.0, &config, &trading, 0);
        let intents = state.on_price("B", 100.0, &config, &trading, 0);

        assert!(
            !intents.is_empty(),
            "true reversion (short) must trigger exit"
        );
        assert_eq!(state.position(), PairPosition::Flat);
    }

    // ── Priority score tests ─────────────────────────────────────────────────

    /// Entry intents carry a positive priority_score when κ is set.
    #[test]
    fn test_entry_priority_score_positive_with_kappa() {
        let mut state = PairState::new();
        // Use a config with a known kappa (10-day half-life)
        let config = easy_trigger_config(); // kappa = ln(2)/10
        let trading = easy_trigger_trading();

        // Warmup
        for _ in 0..35 {
            let _ = state.on_price("A", 100.0, &config, &trading, 0);
            let _ = state.on_price("B", 100.0, &config, &trading, 0);
        }

        // Trigger long spread entry
        let _ = state.on_price("A", 90.0, &config, &trading, 0);
        let intents = state.on_price("B", 100.0, &config, &trading, 0);

        assert!(!intents.is_empty(), "must trigger entry");
        // Both legs carry the same priority_score
        assert!(
            intents[0].priority_score > 0.0,
            "priority_score should be positive when κ > 0, got {}",
            intents[0].priority_score
        );
        assert!(
            (intents[0].priority_score - intents[1].priority_score).abs() < 1e-12,
            "both legs must have equal priority_score"
        );
        // Score must be finite
        assert!(
            intents[0].priority_score.is_finite(),
            "priority_score must be finite"
        );
    }

    /// Entry intents carry priority_score = 0.0 when κ = 0 (unknown half-life).
    #[test]
    fn test_entry_priority_score_zero_when_kappa_zero() {
        let mut state = PairState::new();
        let config = PairConfig {
            kappa: 0.0, // unknown half-life
            max_hold_bars: 0,
            ..easy_trigger_config()
        };
        let trading = easy_trigger_trading();

        for _ in 0..35 {
            let _ = state.on_price("A", 100.0, &config, &trading, 0);
            let _ = state.on_price("B", 100.0, &config, &trading, 0);
        }
        let _ = state.on_price("A", 90.0, &config, &trading, 0);
        let intents = state.on_price("B", 100.0, &config, &trading, 0);

        assert!(!intents.is_empty(), "must trigger entry");
        assert_eq!(
            intents[0].priority_score, 0.0,
            "priority_score must be 0.0 when κ=0"
        );
    }

    /// Close intents always carry priority_score = 0.0 (they don't compete for capital).
    #[test]
    fn test_close_priority_score_always_zero() {
        let mut state = PairState::new();
        let config = easy_trigger_config();
        let trading = easy_trigger_trading();

        // Enter
        for _ in 0..35 {
            let _ = state.on_price("A", 100.0, &config, &trading, 0);
            let _ = state.on_price("B", 100.0, &config, &trading, 0);
        }
        let _ = state.on_price("A", 90.0, &config, &trading, 0);
        let _ = state.on_price("B", 100.0, &config, &trading, 0);
        assert_eq!(state.position(), PairPosition::LongSpread);

        // Revert to trigger close
        let _ = state.on_price("A", 100.0, &config, &trading, 0);
        let intents = state.on_price("B", 100.0, &config, &trading, 0);

        assert!(!intents.is_empty(), "should close");
        for intent in &intents {
            assert_eq!(
                intent.priority_score, 0.0,
                "close intents must have priority_score=0.0, got {}",
                intent.priority_score
            );
        }
    }
}
