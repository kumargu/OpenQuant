//! Pairs trading — market-neutral mean-reversion on spread between two correlated assets.
//!
//! # Data flow and rolling stats lifecycle
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────────────┐
//! │                        BAR ARRIVES (per minute)                     │
//! │                                                                     │
//! │  Alpaca WebSocket / REST  ──►  PairsEngine::on_bar(symbol, ts, px) │
//! │                                    │                                │
//! │                         ┌──────────▼──────────┐                    │
//! │                         │   PairState::on_price │ (for each pair   │
//! │                         │   matching this symbol)│                  │
//! │                         └──────────┬──────────┘                    │
//! │                                    │                                │
//! │                    ┌───────────────▼───────────────┐               │
//! │                    │  Both legs have fresh prices?  │               │
//! │                    │  (last_price_a AND last_price_b)│              │
//! │                    └───────────────┬───────────────┘               │
//! │                              No: return              Yes: ▼        │
//! │                                                                     │
//! │              spread = ln(price_A) - α - β × ln(price_B)           │
//! │              (β from Kalman filter if warm, else OLS from picker)  │
//! │                                                                     │
//! │  ┌─────────────────────────────────────────────────────────────┐   │
//! │  │                   TWO-CLOCK ARCHITECTURE                     │   │
//! │  │                                                               │   │
//! │  │  SIGNAL CLOCK (daily):                                        │   │
//! │  │    When calendar day changes (is_new_day):                    │   │
//! │  │    1. Push yesterday's spread into RollingStats               │   │
//! │  │    2. spread_count += 1                                       │   │
//! │  │    3. daily_bar_count += 1                                    │   │
//! │  │    4. Compute z = (spread - rolling_mean) / rolling_std       │   │
//! │  │    5. Check entry signals (z vs entry_z threshold)            │   │
//! │  │                                                               │   │
//! │  │  RISK CLOCK (every bar):                                      │   │
//! │  │    On every minute bar (whether is_new_day or not):           │   │
//! │  │    1. Compute spread from current prices                      │   │
//! │  │    2. If holding: check stop loss via frozen ExitContext       │   │
//! │  │    3. If holding: check max_hold (uses daily_bar_count)       │   │
//! │  │    4. If intraday_entries: check persistence filter for entry  │   │
//! │  └─────────────────────────────────────────────────────────────┘   │
//! │                                                                     │
//! │  ┌─────────────────────────────────────────────────────────────┐   │
//! │  │                   ROLLING STATS LIFECYCLE                     │   │
//! │  │                                                               │   │
//! │  │  RollingStats: Welford's online mean/variance over N bars     │   │
//! │  │                                                               │   │
//! │  │  Created: PairState::for_pair() — window = 3 × ceil(HL)      │   │
//! │  │  Fed:     once per day (is_new_day) with daily-close spread   │   │
//! │  │  Ready:   when spread_count >= window (e.g., 15 daily bars)   │   │
//! │  │                                                               │   │
//! │  │  On weekly reload (pair-picker regen):                        │   │
//! │  │    If pair stays with same window → stats PRESERVED           │   │
//! │  │    If window changes → RollingStats::resize() (preserves      │   │
//! │  │      existing observations, adjusts capacity)                 │   │
//! │  │    If pair is new → cold start, needs N days to warm up       │   │
//! │  │                                                               │   │
//! │  │  NOT persisted to disk — rebuilt from warmup bars on restart.  │   │
//! │  │  Warmup: runner fetches ~30 daily bars before replay/live     │   │
//! │  │  start, feeds them through on_bar() to fill rolling stats.    │   │
//! │  └─────────────────────────────────────────────────────────────┘   │
//! │                                                                     │
//! │  ┌─────────────────────────────────────────────────────────────┐   │
//! │  │                   ENTRY FLOW                                  │   │
//! │  │                                                               │   │
//! │  │  Daily entry (default):                                       │   │
//! │  │    is_new_day AND spread_count >= window AND |z| > entry_z    │   │
//! │  │    → one chance per pair per day at daily close                │   │
//! │  │                                                               │   │
//! │  │  Intraday entry (if intraday_entries=true):                   │   │
//! │  │    On non-daily bars, if daily entry missed:                  │   │
//! │  │    |z| > intraday_entry_z for intraday_confirm_bars in a row  │   │
//! │  │    → max one intraday entry per pair per day (last_entry_day) │   │
//! │  │    → max max_daily_entries new entries per day globally        │   │
//! │  │                                                               │   │
//! │  │  Guards (checked in order):                                   │   │
//! │  │    1. One entry per pair per day (last_entry_day == bar_day)  │   │
//! │  │    2. Earnings blackout (entry_blocked_until)                 │   │
//! │  │    3. Regime gate (paused after consecutive stops)            │   │
//! │  │    4. Last entry hour (no entries after market close)         │   │
//! │  │    5. Position cap (max_concurrent_pairs)                    │   │
//! │  │    6. Daily entry cap (max_daily_entries)                    │   │
//! │  │    7. z beyond stop_z (would immediately stop out)           │   │
//! │  │                                                               │   │
//! │  │  On entry:                                                    │   │
//! │  │    Freeze ExitContext (entry-time mean + std for exit z)      │   │
//! │  │    Set position = LongSpread or ShortSpread                   │   │
//! │  │    Record entry_daily_bar, last_entry_day, entry prices       │   │
//! │  └─────────────────────────────────────────────────────────────┘   │
//! │                                                                     │
//! │  ┌─────────────────────────────────────────────────────────────┐   │
//! │  │                   EXIT FLOW                                   │   │
//! │  │                                                               │   │
//! │  │  Runs on EVERY bar (not just daily) via frozen ExitContext:   │   │
//! │  │    exit_z = (current_spread - entry_mean) / entry_std         │   │
//! │  │                                                               │   │
//! │  │  Exit triggers (checked every bar):                           │   │
//! │  │    1. Reversion: |exit_z| < exit_z_threshold (past min_hold) │   │
//! │  │    2. Stop loss: |exit_z| > stop_z (immediate, no min_hold)  │   │
//! │  │    3. Max hold: days_held >= max_hold_bars (daily count)      │   │
//! │  │    4. Force close: past force_close_minute (EOD)              │   │
//! │  │    5. Drift stop: |rolling_z - exit_z| > max_drift_z         │   │
//! │  │                                                               │   │
//! │  │  Frozen ExitContext prevents rolling-stat drift from          │   │
//! │  │  producing false reversion signals (issue #182).              │   │
//! │  └─────────────────────────────────────────────────────────────┘   │
//! └─────────────────────────────────────────────────────────────────────┘
//! ```

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
    /// Default rolling window size for spread z-score computation.
    /// Used when a pair's `lookback_bars` is 0 (no per-pair override).
    /// Per-pair windows derived from half-life take precedence.
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
    /// Maximum number of concurrent open pair positions.
    /// New entries are blocked when this limit is reached.
    /// 0 = no limit. Default 25 (fits ~$50K notional at $1K/leg).
    #[serde(default = "default_max_concurrent")]
    pub max_concurrent_pairs: usize,
    /// Maximum allowed drift between rolling z and frozen exit z.
    /// When the gap exceeds this, force exit — the spread dynamics have
    /// shifted and the frozen context is unreliable.
    /// 0.0 = disabled. Default 0.0 (off for S&P where drift is rare).
    #[serde(default)]
    pub max_drift_z: f64,
    /// Spread trend gate: block entries when z-score has been on the same
    /// side of 0 for this many consecutive daily bars. Indicates the spread
    /// is trending, not mean-reverting. 0 = disabled.
    /// For metals: set to 5 (block after 5 consecutive same-side days).
    #[serde(default)]
    pub spread_trend_gate: usize,
    /// Allow entries on any bar (not just daily close).
    /// Uses daily rolling stats for z-score but evaluates spread every bar.
    /// Requires z to persist above entry_z for `intraday_confirm_bars`
    /// consecutive bars before firing. Default: false.
    #[serde(default)]
    pub intraday_entries: bool,
    /// Number of consecutive bars z must stay above entry_z before intraday
    /// entry fires. Filters noise spikes. Only used when intraday_entries=true.
    /// At 1-min bars, 30 = 30 minutes of sustained deviation.
    /// Default: 30 (30 minutes).
    #[serde(default = "default_intraday_confirm")]
    pub intraday_confirm_bars: usize,
    /// Z-score threshold for intraday entries (higher than daily to filter noise).
    /// 0.0 = use entry_z (same threshold for daily and intraday).
    #[serde(default)]
    pub intraday_entry_z: f64,
    /// Maximum new entries per day across all pairs. 0 = no limit.
    /// Prevents over-trading on volatile days. Default 4.
    #[serde(default = "default_max_daily_entries")]
    pub max_daily_entries: usize,
    /// Intraday rolling z-score mode. When non-zero, the spread's rolling
    /// mean and std are computed from a rolling window of this many
    /// **intraday bars** (i.e. minutes, if bars are 1-min), and spread
    /// observations are pushed on every bar rather than only at the daily
    /// close.
    ///
    /// 0 = disabled (default) → daily mode: rolling stats sample once per
    /// day at the daily close; warm-up takes `lookback` days. This matches
    /// the classical pairs-trading regime used by the live engine.
    ///
    /// `> 0` = intraday rolling mode: warm-up takes this many bars
    /// (approx minutes); entries are evaluated on every bar once warmed up
    /// (no `is_new_day` gate, no `intraday_entries` persistence filter).
    /// When set, `intraday_rolling_bars` overrides the window size that
    /// would otherwise come from `lookback` or per-pair `lookback_bars`.
    #[serde(default)]
    pub intraday_rolling_bars: usize,
}

fn default_max_daily_entries() -> usize {
    4
}

fn default_intraday_confirm() -> usize {
    30
}

fn default_max_concurrent() -> usize {
    25
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
            max_concurrent_pairs: 25,
            max_drift_z: 0.0,
            spread_trend_gate: 0, // disabled by default
            intraday_entries: false,
            intraday_confirm_bars: 30,
            intraday_entry_z: 0.0,
            max_daily_entries: 4,
            intraday_rolling_bars: 0,
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
    /// Per-pair rolling window size for spread z-score computation.
    /// Derived from half-life: min(2 × ceil(half_life_bars), 60).
    /// 0 = use global lookback from PairsTradingConfig.
    pub lookback_bars: usize,
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
            lookback_bars: 0, // 0 = use global
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

/// Kalman filter for dynamic hedge ratio estimation.
/// State: [alpha, beta] (intercept, hedge ratio).
/// Updates on each daily spread observation.
/// Palomar (2025): "Kalman filtering is a must in pairs trading."
///
/// Default parameters:
/// - q=1e-5: slow-moving state (hedge ratio changes slowly between days)
/// - r=1e-3: moderate observation noise (log-price measurement uncertainty)
/// - warmup=10: minimum observations before Kalman overrides static OLS
/// - max_beta=5.0: sanity cap on Kalman beta (resets filter if exceeded)
const KALMAN_PROCESS_NOISE: f64 = 1e-5;
const KALMAN_OBS_NOISE: f64 = 1e-3;
const KALMAN_WARMUP: usize = 10;
const KALMAN_MAX_BETA: f64 = 5.0;

struct KalmanHedge {
    /// State estimate: [alpha, beta]
    x: [f64; 2],
    /// Error covariance matrix (2x2, stored as [p00, p01, p10, p11])
    p: [f64; 4],
    /// Process noise variance (controls how fast beta can change)
    q: f64,
    /// Observation noise variance (measurement uncertainty)
    r: f64,
    /// Number of updates (for warmup)
    n: usize,
}

impl KalmanHedge {
    fn new(alpha: f64, beta: f64) -> Self {
        Self {
            x: [alpha, beta],
            // Start with moderate uncertainty
            p: [1.0, 0.0, 0.0, 1.0],
            q: KALMAN_PROCESS_NOISE,
            r: KALMAN_OBS_NOISE,
            n: 0,
        }
    }

    /// Update the filter with a new observation: y1 = alpha + beta * y2 + noise.
    /// Returns the updated (alpha, beta).
    fn update(&mut self, y1: f64, y2: f64) -> (f64, f64) {
        // Prediction step: state doesn't change (random walk model)
        // P = P + Q*I
        self.p[0] += self.q;
        self.p[3] += self.q;

        // Observation model: H = [1, y2]
        let h0 = 1.0;
        let h1 = y2;

        // Innovation: y_hat = H * x
        let y_hat = h0 * self.x[0] + h1 * self.x[1];
        let innovation = y1 - y_hat;

        // Innovation covariance: S = H * P * H' + R
        let s = h0 * (self.p[0] * h0 + self.p[1] * h1)
            + h1 * (self.p[2] * h0 + self.p[3] * h1)
            + self.r;

        if s.abs() < 1e-15 || !s.is_finite() {
            return (self.x[0], self.x[1]);
        }

        // Kalman gain: K = P * H' / S
        let k0 = (self.p[0] * h0 + self.p[1] * h1) / s;
        let k1 = (self.p[2] * h0 + self.p[3] * h1) / s;

        // State update: x = x + K * innovation
        self.x[0] += k0 * innovation;
        self.x[1] += k1 * innovation;

        // Covariance update: P = (I - K*H) * P
        let p00 = self.p[0] - k0 * (h0 * self.p[0] + h1 * self.p[2]);
        let p01 = self.p[1] - k0 * (h0 * self.p[1] + h1 * self.p[3]);
        let p10 = self.p[2] - k1 * (h0 * self.p[0] + h1 * self.p[2]);
        let p11 = self.p[3] - k1 * (h0 * self.p[1] + h1 * self.p[3]);
        self.p = [p00, p01, p10, p11];

        self.n += 1;

        (self.x[0], self.x[1])
    }

    fn alpha(&self) -> f64 {
        self.x[0]
    }
    fn beta(&self) -> f64 {
        self.x[1]
    }
    fn is_warm(&self) -> bool {
        self.n >= KALMAN_WARMUP
    }
}

/// Mutable state for a single pair, updated on each bar.
pub struct PairState {
    /// Most recent close price for leg A (cleared after spread computation).
    last_price_a: Option<f64>,
    /// Most recent close price for leg B (cleared after spread computation).
    last_price_b: Option<f64>,
    /// Rolling statistics of the spread for z-score computation.
    /// Window size is set per pair: min(2 × half_life_bars, 60), or global lookback.
    /// Uses Welford's algorithm for numerically stable variance. See #207.
    spread_stats: RollingStats,
    /// Number of spread observations (for warmup detection).
    spread_count: usize,
    /// Pending daily close spread — buffered until the next day confirms it was the last bar.
    /// When a new day starts, this value is pushed into rolling stats.
    pending_daily_spread: Option<f64>,
    /// Calendar day (ms / 86_400_000) of the last bar seen. Used for new-day detection.
    last_bar_day: i64,
    /// Current position state.
    position: PairPosition,
    /// Daily bar counter at entry (for max-hold tracking in days).
    entry_daily_bar: usize,
    /// Prices at entry (for real dollar P&L on exit).
    entry_price_a: f64,
    entry_price_b: f64,
    /// Hedge ratio at entry (for consistent close sizing if beta refreshes mid-trade).
    entry_beta: f64,
    /// Incremented every bar (minute or daily). Used for logging only.
    bar_count: usize,
    /// Incremented only on daily close bars. Used for max_hold/min_hold counting.
    daily_bar_count: usize,
    /// Entry blackout: block new entries until this timestamp (millis).
    /// Used by the runner to prevent entries around earnings announcements.
    /// 0 = no blackout.
    entry_blocked_until: i64,
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
    /// Kalman filter for dynamic hedge ratio estimation.
    /// Updates alpha and beta on each daily close, replacing static OLS values.
    kalman: Option<KalmanHedge>,
    /// Spread trend tracking: consecutive daily bars with z on same side of 0.
    /// High values (5+) indicate a trending spread — entries are riskier.
    z_same_side_count: usize,
    /// Sign of z on the previous daily bar (-1, 0, or 1).
    z_last_sign: i8,
    /// Intraday entry persistence: consecutive bars where |z| > entry_z.
    /// Resets to 0 when |z| drops below threshold. Used with intraday_entries
    /// to require sustained deviation before entry (filters noise spikes).
    intraday_persist_count: usize,
    /// Whether the last intraday z was above entry threshold (for persistence tracking).
    intraday_persist_side: i8, // -1 = below -entry_z, +1 = above +entry_z, 0 = within
    /// Calendar day of last entry (timestamp / 86_400_000). Prevents re-entry same day.
    last_entry_day: i64,
}

impl Default for PairState {
    fn default() -> Self {
        Self::with_window(32)
    }
}

impl PairState {
    /// Create a new PairState with the appropriate rolling window size.
    ///
    /// Uses the pair's `lookback_bars` if set (> 0), otherwise falls back
    /// to the global `lookback` from trading config.
    /// Create with default window (32). Used in tests.
    pub fn new() -> Self {
        Self::with_window(32)
    }

    /// Create a new PairState with the appropriate rolling window size.
    ///
    /// Window precedence (highest to lowest):
    ///   1. `trading.intraday_rolling_bars` — intraday rolling mode (minute window)
    ///   2. `config.lookback_bars` — per-pair override (daily window)
    ///   3. `trading.lookback` — global default (daily window)
    pub fn for_pair(config: &PairConfig, trading: &PairsTradingConfig) -> Self {
        let window = if trading.intraday_rolling_bars > 0 {
            trading.intraday_rolling_bars
        } else if config.lookback_bars > 0 {
            config.lookback_bars
        } else {
            trading.lookback.max(1) // guard against misconfigured lookback=0
        };
        let mut state = Self::with_window(window);
        // Initialize Kalman filter with OLS alpha/beta from pair-picker
        state.kalman = Some(KalmanHedge::new(config.alpha, config.beta));
        state
    }

    pub fn with_window(window: usize) -> Self {
        Self {
            last_price_a: None,
            last_price_b: None,
            spread_stats: RollingStats::new(window),
            spread_count: 0,
            pending_daily_spread: None,
            last_bar_day: 0,
            position: PairPosition::Flat,
            entry_daily_bar: 0,
            entry_price_a: 0.0,
            entry_price_b: 0.0,
            entry_beta: 1.0,
            bar_count: 0,
            daily_bar_count: 0,
            entry_blocked_until: 0,
            exit_z_override: None,
            stop_z_override: None,
            trade_pnl_history: VecDeque::with_capacity(10),
            paused: false,
            pause_bars: 0,
            consecutive_stops: 0,
            exit_context: None,
            kalman: None,
            z_same_side_count: 0,
            z_last_sign: 0,
            intraday_persist_count: 0,
            intraday_persist_side: 0,
            last_entry_day: 0,
        }
    }

    /// Resize the spread rolling window without resetting accumulated observations.
    /// Used during weekly reload when half-life changes slightly.
    pub fn resize_spread_window(&mut self, new_window: usize) {
        self.spread_stats.resize(new_window);
    }

    /// Reset Kalman filter to fresh OLS values.
    /// Called on window resize to prevent stale alpha/beta from the old window.
    pub fn reset_kalman(&mut self, alpha: f64, beta: f64) {
        self.kalman = Some(KalmanHedge::new(alpha, beta));
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

        // Compute log-spread using Kalman-filtered alpha/beta if available,
        // otherwise fall back to static OLS values from pair-picker.
        let (alpha, beta) = if let Some(ref kf) = self.kalman {
            if kf.is_warm() {
                (kf.alpha(), kf.beta())
            } else {
                (config.alpha, config.beta)
            }
        } else {
            (config.alpha, config.beta)
        };
        let spread = price_a.ln() - alpha - beta * price_b.ln();

        // ── Two-clock architecture ──
        // Signal clock (daily): push spread into rolling stats once per day.
        // Risk clock (every bar): check stop loss via frozen ExitContext.
        //
        // New-day detection: when the calendar day changes, the PREVIOUS bar
        // was the daily close. Push the buffered spread into rolling stats.
        // No timezone math needed for the gate — just date comparison.
        // ── New-day detection ──
        // Uses UTC midnight boundary (timestamp / 86_400_000). This works for
        // US market hours (9:30-16:00 ET = 13:30-20:00 UTC) because all trading
        // happens within one UTC day. The daily counters (last_entry_day,
        // max_daily_entries, intraday persistence) reset at 00:00 UTC = 20:00 ET,
        // well after market close. Would need tz-aware day boundary for 24h markets.
        //
        // We update last_bar_day only when BOTH legs have arrived (spread computed).
        // This prevents the first leg from consuming the new-day flag before
        // the second leg can use it for entry evaluation.
        let bar_day = timestamp / 86_400_000; // calendar day from epoch
        let is_new_day = self.last_bar_day > 0 && bar_day != self.last_bar_day;

        if is_new_day {
            // Previous bar was the daily close — push its spread into rolling
            // stats in DAILY mode. In INTRADAY rolling mode, spreads are
            // pushed per-bar below, so we only increment the day counter here.
            if trading.intraday_rolling_bars == 0 {
                if let Some(daily_spread) = self.pending_daily_spread.take() {
                    self.spread_stats.push(daily_spread);
                    self.spread_count += 1;
                    self.daily_bar_count += 1;
                }
            } else {
                // Intraday mode: daily_bar_count still ticks once per
                // calendar day so max_hold / min_hold (measured in days)
                // keep working.
                self.daily_bar_count += 1;
            }
            // Reset intraday persistence — bars from yesterday don't count toward today.
            self.intraday_persist_count = 0;
            self.intraday_persist_side = 0;
            // Update Kalman filter with daily closes (only when flat — don't
            // change hedge ratio mid-trade as it would invalidate exit context).
            if self.position == PairPosition::Flat
                && let Some(ref mut kf) = self.kalman
            {
                let (new_alpha, new_beta) = kf.update(price_a.ln(), price_b.ln());
                // Guard: reject insane Kalman updates
                if new_beta.is_finite() && new_beta.abs() < KALMAN_MAX_BETA && new_alpha.is_finite()
                {
                    // Log significant beta shifts (>5%) for debugging
                    let beta_shift = ((new_beta - config.beta) / config.beta).abs();
                    if beta_shift > 0.05 && kf.n % 5 == 0 {
                        info!(
                            pair = format!("{}/{}", config.leg_a, config.leg_b).as_str(),
                            ols_beta = format!("{:.4}", config.beta).as_str(),
                            kalman_beta = format!("{:.4}", new_beta).as_str(),
                            shift_pct = format!("{:.1}%", beta_shift * 100.0).as_str(),
                            "Kalman beta diverged from OLS"
                        );
                    }
                } else {
                    // bug! — this should not happen with valid price data.
                    // If it does, the pair's price relationship has broken badly.
                    error!(
                        pair_a = config.leg_a.as_str(),
                        pair_b = config.leg_b.as_str(),
                        kalman_alpha = format!("{:.4}", new_alpha).as_str(),
                        kalman_beta = format!("{:.4}", new_beta).as_str(),
                        bug = true,
                        "Kalman produced insane hedge ratio — resetting"
                    );
                    *kf = KalmanHedge::new(config.alpha, config.beta);
                }
            }
        }
        self.last_bar_day = bar_day; // safe: both legs have arrived at this point

        // ── Spread buffering: push previous, buffer current ──
        // Both modes use a one-bar delay: the PREVIOUS bar's spread is pushed
        // into rolling stats, and the CURRENT bar's spread is buffered to be
        // pushed next time. This ensures z is computed against a window that
        // does NOT include the current spread (standard convention).
        //
        // Daily mode: the push only happens on is_new_day (handled above). The
        // buffer simply holds the most recent spread until the next day.
        //
        // Intraday rolling mode: the push happens on EVERY bar (here). The
        // buffer holds the previous bar's spread until the next bar arrives.
        if trading.intraday_rolling_bars > 0
            && let Some(prev_spread) = self.pending_daily_spread.take()
        {
            self.spread_stats.push(prev_spread);
            self.spread_count += 1;
        }
        self.pending_daily_spread = Some(spread);
        self.bar_count += 1;

        // Convert UTC timestamp to market-local time.
        // All internal state (bar_day, last_entry_day, daily counters) uses raw
        // UTC timestamps. Market-local time is ONLY used for:
        //   - force_close_minute: EOD position close
        //   - last_entry_hour: no entries after this hour
        // The tz_offset_hours config says where the market lives (e.g., -4 = EDT).
        let tz_offset_ms: i64 = (trading.tz_offset_hours as i64) * 3600 * 1000;
        let local_ms = timestamp + tz_offset_ms;
        let secs_of_day = ((local_ms / 1000) % 86400 + 86400) % 86400;
        let market_hour = (secs_of_day / 3600) as u32;
        let market_min = ((secs_of_day % 3600) / 60) as u32;
        let market_minutes = market_hour * 60 + market_min;

        // Rolling z-score — only valid after enough daily observations
        let min_lookback = self.spread_stats.window();
        let rolling_z_ready = self.spread_count >= min_lookback;

        let z = if rolling_z_ready {
            let mean = self.spread_stats.mean();
            let sd = self.spread_stats.std_dev();
            if sd < 1e-10 {
                debug!(
                    pair = format!("{}/{}", config.leg_a, config.leg_b).as_str(),
                    std_dev = %format_args!("{sd:.12}"),
                    spread_count = self.spread_count,
                    "pairs: SKIPPED — zero spread variance (flat spread)"
                );
                return vec![];
            }
            (spread - mean) / sd
        } else {
            // Not enough daily observations for z-score.
            // But if we have a position, still check exits below (stop loss uses ExitContext).
            if self.position == PairPosition::Flat {
                debug!(
                    pair = format!("{}/{}", config.leg_a, config.leg_b).as_str(),
                    count = self.spread_count,
                    needed = min_lookback,
                    "pairs: warming up spread stats (daily observations)"
                );
                return vec![];
            }
            0.0 // dummy — exits use ExitContext, not rolling z
        };

        // ── Check exits first (if we have a position) ──
        if self.position != PairPosition::Flat {
            let days_held = self.daily_bar_count.saturating_sub(self.entry_daily_bar);

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
                            days_held,
                            "pairs: DRIFT DETECTED — rolling z diverges from fixed exit z (rolling-stat adaptation)"
                        );
                    }
                    ez
                }
                None => {
                    // Defensive fallback: no context means entry was not captured properly.
                    // Use rolling z to avoid blocking exits entirely.
                    // bug! — exit_context should always be set when position is open.
                    // If we reach here, the entry logic failed to freeze stats.
                    error!(
                        pair_a = config.leg_a.as_str(),
                        pair_b = config.leg_b.as_str(),
                        bug = true,
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

            // Exit condition: cointegration drift — rolling z and frozen z diverge
            // beyond max_drift_z, indicating the spread relationship has shifted.
            // This catches cointegration breakdown during a live trade.
            let drift = (z - exit_z).abs();
            let drift_stopped = trading.max_drift_z > 0.0 && drift > trading.max_drift_z;
            if drift_stopped {
                let pair_id = format!("{}/{}", config.leg_a, config.leg_b);
                error!(
                    pair = pair_id.as_str(),
                    ts = timestamp,
                    rolling_z = %format_args!("{z:.2}"),
                    fixed_exit_z = %format_args!("{exit_z:.2}"),
                    drift = %format_args!("{drift:.2}"),
                    max_drift_z = trading.max_drift_z,
                    days_held,
                    "pairs: STOP LOSS — cointegration drift exceeded threshold"
                );
            }

            // Exit condition: max hold (per-pair override from pair-picker, or global fallback)
            let effective_max_hold = if config.max_hold_bars > 0 {
                config.max_hold_bars // per-pair HL-adaptive: ceil(2.5 * HL) capped at 10
            } else {
                trading.max_hold_bars // global fallback from TOML
            };
            let max_held = effective_max_hold > 0 && days_held >= effective_max_hold;

            // Exit condition: force close before end of day (no overnight holding)
            let force_close = market_minutes >= trading.force_close_minute;
            if force_close {
                info!(
                    pair = format!("{}/{}", config.leg_a, config.leg_b).as_str(),
                    ts = timestamp,
                    market_hour,
                    market_min,
                    market_minutes,
                    force_close_minute = trading.force_close_minute,
                    "pairs: EOD FORCE CLOSE triggered"
                );
            }

            // Minimum hold: block reversion exits (but NOT stop loss) until min_hold_bars
            let past_min_hold = days_held >= trading.min_hold_bars;
            let can_exit_reversion = reverted && past_min_hold;

            if can_exit_reversion || stopped || drift_stopped || max_held || force_close {
                let reason = if stopped || drift_stopped {
                    SignalReason::StopLoss
                } else if force_close || max_held {
                    SignalReason::MaxHoldTime
                } else {
                    SignalReason::PairsExit
                };

                let pair_id = format!("{}/{}", config.leg_a, config.leg_b);
                let exit_reason = if stopped {
                    "stop_loss"
                } else if drift_stopped {
                    "drift_stop"
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
                        days_held,
                        "pairs: STOP LOSS — spread diverged further from entry baseline"
                    );
                }
                // Record trade P&L for regime gate
                // P&L weighted by beta to match actual position sizing.
                // Leg A has weight 1.0, leg B has weight |entry_beta|.
                // Total weight = 1 + |beta|; normalize to get bps per unit capital.
                let ret_a = (price_a - self.entry_price_a) / self.entry_price_a;
                let ret_b = (price_b - self.entry_price_b) / self.entry_price_b;
                let abs_beta = self.entry_beta.abs();
                let weight_sum = 1.0 + abs_beta;
                let gross_bps = match self.position {
                    PairPosition::LongSpread => (ret_a - abs_beta * ret_b) / weight_sum * 10_000.0,
                    PairPosition::ShortSpread => (abs_beta * ret_b - ret_a) / weight_sum * 10_000.0,
                    PairPosition::Flat => 0.0,
                };
                let net_bps = gross_bps - trading.cost_bps;

                info!(
                    pair = pair_id.as_str(),
                    ts = timestamp,
                    rolling_z = format!("{:.2}", z).as_str(),
                    fixed_exit_z = format!("{:.2}", exit_z).as_str(),
                    days_held,
                    price_a = format!("{:.2}", price_a).as_str(),
                    price_b = format!("{:.2}", price_b).as_str(),
                    net_bps = format!("{:.1}", net_bps).as_str(),
                    exit = exit_reason,
                    "pairs: EXIT"
                );
                if self.trade_pnl_history.len() >= 10 {
                    self.trade_pnl_history.pop_front();
                }
                self.trade_pnl_history.push_back(net_bps);

                if stopped || drift_stopped {
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

            // Log holding status:
            // - Daily close: full HOLDING log with z-scores, days_held
            // - Every 30 bars (~30 min): intraday risk snapshot showing exit_z
            //   approach toward stop threshold (important for debugging)
            if is_new_day {
                info!(
                    pair = format!("{}/{}", config.leg_a, config.leg_b).as_str(),
                    rolling_z = %format_args!("{z:.2}"),
                    frozen_exit_z = %format_args!("{exit_z:.2}"),
                    days_held,
                    effective_max_hold,
                    price_a = %format_args!("{price_a:.2}"),
                    price_b = %format_args!("{price_b:.2}"),
                    spread = %format_args!("{spread:.6}"),
                    pos = ?self.position,
                    "pairs: HOLDING"
                );
            } else if self.bar_count.is_multiple_of(10) {
                // Intraday risk snapshot every ~10 minutes
                info!(
                    pair = format!("{}/{}", config.leg_a, config.leg_b).as_str(),
                    frozen_exit_z = %format_args!("{exit_z:.2}"),
                    stop_z = %format_args!("{effective_stop_z:.2}"),
                    exit_threshold = %format_args!("{effective_exit_z:.2}"),
                    spread = %format_args!("{spread:.6}"),
                    price_a = %format_args!("{price_a:.2}"),
                    price_b = %format_args!("{price_b:.2}"),
                    pos = ?self.position,
                    "pairs: RISK CHECK"
                );
            }
            return vec![];
        }

        // ── Check entries (if flat) ──
        // Three entry regimes, checked in this order:
        //
        //  1. `intraday_rolling_bars > 0` — intraday rolling mode. The rolling
        //     stats are already computed from minute bars (see the push above),
        //     so z is valid on every bar once warmed up. Entries fire on any
        //     bar when |z| >= entry_z. No is_new_day gate, no persistence
        //     filter (the minute-bar window is already the noise floor).
        //
        //  2. `intraday_entries = true` — classical daily-rolling z, but
        //     allow entry evaluation on intraday bars. Requires z to persist
        //     above threshold for `intraday_confirm_bars` bars to filter noise.
        //
        //  3. Default (neither) — entries only fire at the daily-close bar
        //     (is_new_day), using daily-rolling stats.
        if trading.intraday_rolling_bars > 0 {
            if !rolling_z_ready {
                return vec![];
            }
            // Fall through to the common entry logic below.
        } else if trading.intraday_entries && rolling_z_ready && !is_new_day {
            // Intraday uses a higher z threshold to filter noise (default: entry_z)
            let intra_z = if trading.intraday_entry_z > 0.0 {
                trading.intraday_entry_z
            } else {
                trading.entry_z
            };
            // Track persistence: how many consecutive bars has |z| > intra_z?
            let side: i8 = if z < -intra_z {
                -1
            } else if z > intra_z {
                1
            } else {
                0
            };
            if side != 0 && side == self.intraday_persist_side {
                self.intraday_persist_count += 1;
            } else if side != 0 {
                self.intraday_persist_side = side;
                self.intraday_persist_count = 1;
            } else {
                self.intraday_persist_count = 0;
                self.intraday_persist_side = 0;
            }

            // Fire intraday entry only if z persisted for confirm_bars
            if self.intraday_persist_count >= trading.intraday_confirm_bars {
                info!(
                    pair = format!("{}/{}", config.leg_a, config.leg_b).as_str(),
                    z = %format_args!("{z:.2}"),
                    persist = self.intraday_persist_count,
                    confirm = trading.intraday_confirm_bars,
                    "pairs: INTRADAY ENTRY — z persisted above threshold"
                );
                // Reset counter so we don't re-enter next bar
                self.intraday_persist_count = 0;
                // Fall through to entry logic below
            } else {
                return vec![];
            }
        } else if !is_new_day || !rolling_z_ready {
            return vec![];
        }

        // One entry per pair per day: block if already entered today
        // (bar_day computed at line 660)
        if self.last_entry_day == bar_day {
            return vec![];
        }

        // Track spread trend: count consecutive daily bars with z on the same side of 0.
        // High values indicate a trending spread (not mean-reverting).
        if rolling_z_ready {
            let current_sign: i8 = if z > 0.0 { 1 } else { -1 };
            if current_sign == self.z_last_sign {
                self.z_same_side_count += 1;
            } else {
                self.z_same_side_count = 1;
            }
            self.z_last_sign = current_sign;
        }

        // Earnings blackout: block entries around announcement dates.
        // The runner sets entry_blocked_until via block_entry().
        if self.entry_blocked_until > 0 && timestamp < self.entry_blocked_until {
            debug!(
                pair = format!("{}/{}", config.leg_a, config.leg_b).as_str(),
                "pairs: ENTRY BLOCKED — earnings blackout"
            );
            return vec![];
        }

        // Spread trend gate: block entries when spread has been trending (same side of 0)
        // for too many consecutive days. A trending spread won't revert on our timescale.
        if trading.spread_trend_gate > 0 && self.z_same_side_count >= trading.spread_trend_gate {
            debug!(
                pair = format!("{}/{}", config.leg_a, config.leg_b).as_str(),
                z = %format_args!("{z:.2}"),
                consecutive = self.z_same_side_count,
                gate = trading.spread_trend_gate,
                "pairs: ENTRY BLOCKED — spread trending (same side for too many days)"
            );
            return vec![];
        }

        // Regime gate: block entries when paused due to consecutive losses.
        // Cooldown uses bar_count which increments every minute — this means
        // cooldown is in minute bars, but the threshold scales with max_hold_bars
        // (which is in daily units). For daily-close-only entries, the cooldown
        // effectively counts trading days of exposure.
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
        if market_hour >= trading.last_entry_hour {
            debug!(
                pair_a = config.leg_a.as_str(),
                pair_b = config.leg_b.as_str(),
                market_hour,
                z = %format_args!("{z:.2}"),
                "pairs: ENTRY BLOCKED — past last_entry_hour"
            );
            return vec![];
        }

        if z < -trading.entry_z {
            // Guard: don't enter if z is already beyond stop_z — would immediately stop out.
            // Guard: z is already beyond stop_z — entering would immediately stop out.
            if z.abs() > trading.stop_z {
                let pair_id = format!("{}/{}", config.leg_a, config.leg_b);
                warn!(
                    pair = pair_id.as_str(),
                    z = format!("{:.2}", z).as_str(),
                    stop_z = format!("{:.2}", trading.stop_z).as_str(),
                    "pairs: BLOCKED ENTRY — z already beyond stop_z"
                );
                return vec![];
            }
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
            let priority_score = if config.kappa > 0.0 && entry_std > 1e-10 {
                z.abs() * config.kappa.sqrt() / entry_std
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
            self.entry_daily_bar = self.daily_bar_count;
            self.last_entry_day = timestamp / 86_400_000;
            self.entry_price_a = price_a;
            self.entry_price_b = price_b;
            self.entry_beta = beta; // use Kalman-filtered beta if available

            // Beta-weighted sizing: use the SAME beta as entry_beta (Kalman if available).
            // This ensures open and close quantities match, preventing residual exposure.
            let qty_a = (trading.notional_per_leg / price_a).floor();
            let qty_b = (trading.notional_per_leg * beta.abs() / price_b).floor();

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
            // Guard: don't enter if z is already beyond stop_z
            if z.abs() > trading.stop_z {
                let pair_id = format!("{}/{}", config.leg_a, config.leg_b);
                error!(
                    pair = pair_id.as_str(),
                    z = format!("{:.2}", z).as_str(),
                    stop_z = format!("{:.2}", trading.stop_z).as_str(),
                    bug = true,
                    "pairs: BLOCKED ENTRY — z already beyond stop_z, would immediately stop out"
                );
                return vec![];
            }
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
            let priority_score = if config.kappa > 0.0 && entry_std > 1e-10 {
                z.abs() * config.kappa.sqrt() / entry_std
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
            self.entry_daily_bar = self.daily_bar_count;
            self.last_entry_day = timestamp / 86_400_000;
            self.entry_price_a = price_a;
            self.entry_price_b = price_b;
            self.entry_beta = beta; // use Kalman-filtered beta if available

            let qty_a = (trading.notional_per_leg / price_a).floor();
            let qty_b = (trading.notional_per_leg * beta.abs() / price_b).floor();

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
        // Use entry-time beta for close sizing — config.beta may have been refreshed
        // mid-trade, which would cause qty mismatch and residual exposure.
        let qty_a = (trading.notional_per_leg / self.entry_price_a).floor();
        let qty_b = (trading.notional_per_leg * self.entry_beta.abs() / self.entry_price_b).floor();

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
    /// Resets last_bar_day so the first live bar triggers is_new_day,
    /// allowing entry evaluation immediately (not waiting until next calendar day).
    pub fn force_flat(&mut self) {
        self.position = PairPosition::Flat;
        self.exit_context = None;
        self.last_bar_day = 0;
        // Reset entry context so blocked entries don't leave stale values
        // that corrupt days_held on the next legitimate entry.
        self.entry_daily_bar = 0;
        self.entry_price_a = 0.0;
        self.entry_price_b = 0.0;
        self.entry_beta = 1.0;
        // Reset per-day entry marker so engine-level cap cancellations
        // (max_concurrent, max_daily_entries) don't block retries same day.
        self.last_entry_day = 0;
        self.intraday_persist_count = 0;
        self.intraday_persist_side = 0;
    }

    /// Restore position from external state (e.g., Alpaca positions on restart).
    /// Sets position direction and entry prices so stop loss monitoring works.
    /// Rolling stats and exit context are NOT set — they require spread history
    /// which will build up from live bars. Exits will use rolling z until
    /// exit_context is populated on the next entry.
    pub fn restore_position(
        &mut self,
        position: PairPosition,
        entry_price_a: f64,
        entry_price_b: f64,
        entry_beta: f64,
    ) {
        self.position = position;
        self.entry_price_a = entry_price_a;
        self.entry_price_b = entry_price_b;
        self.entry_beta = entry_beta;
        self.entry_daily_bar = self.daily_bar_count;
        // Compute exit context from current spread stats if available
        if self.spread_count > 0 {
            let mean = self.spread_stats.mean();
            let sd = self.spread_stats.std_dev();
            if sd > 1e-10 {
                self.exit_context = Some(ExitContext {
                    entry_mean: mean,
                    entry_std: sd,
                });
            }
        }
    }

    /// Reset rolling spread stats (mean, variance, count).
    /// Used when switching timeframes (e.g., daily warmup → minute replay)
    /// to avoid corrupted z-scores from mixed-timeframe variance.
    pub fn reset_spread_stats(&mut self) {
        let window = self.spread_stats.window();
        self.spread_stats = RollingStats::new(window);
        self.spread_count = 0;
    }

    /// Block new entries until the given timestamp (millis).
    /// Used by the runner to prevent entries around earnings announcements.
    pub fn block_entry_until(&mut self, until_ts: i64) {
        self.entry_blocked_until = until_ts;
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

    /// One day in milliseconds — tests step by this amount between bars.
    const DAY: i64 = 86_400_000;

    /// Feed N warmup bars with oscillating leg_a prices (95/105), returning the next timestamp.
    ///
    /// Alternates leg_a between 95 and 105 so the rolling window builds:
    ///   mean ≈ 0  (spreads are symmetric around 0)
    ///   std  ≈ 0.05
    ///
    /// This gives a realistic z-scale: entry at A=90 (spread ≈ -0.105) yields
    /// exit_z ≈ -2.1, which is well inside stop_z=5 and triggers entry at entry_z=1.5.
    fn warmup(
        state: &mut PairState,
        config: &PairConfig,
        trading: &PairsTradingConfig,
        n: usize,
    ) -> i64 {
        let mut ts = DAY; // start at day 1 (not 0, so is_new_day fires on day 2)
        for i in 0..n {
            let a = if i % 2 == 0 { 95.0 } else { 105.0 };
            let _ = state.on_price(&config.leg_a, a, config, trading, ts);
            let _ = state.on_price(&config.leg_b, 100.0, config, trading, ts);
            ts += DAY;
        }
        ts
    }

    fn test_config() -> PairConfig {
        PairConfig {
            leg_a: "GLD".into(),
            leg_b: "SLV".into(),
            alpha: 0.0,
            beta: 0.37,
            kappa: 0.0, // unknown in unit tests
            max_hold_bars: 0,
            lookback_bars: 0,
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
        let intents = state.on_price("GLD", 420.0, &config, &trading, DAY);
        assert!(intents.is_empty(), "should not signal with only one leg");
    }

    #[test]
    fn test_no_signal_during_warmup() {
        let mut state = PairState::new();
        let config = test_config();
        let trading = test_trading();
        // Feed a few bars — not enough for z-score
        let mut ts = DAY;
        for i in 0..10 {
            let _ = state.on_price("GLD", 420.0 + i as f64 * 0.1, &config, &trading, ts);
            let intents = state.on_price("SLV", 64.0 + i as f64 * 0.01, &config, &trading, ts);
            assert!(
                intents.is_empty(),
                "should not signal during warmup (bar {i})"
            );
            ts += DAY;
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
            lookback_bars: 0,
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
            max_concurrent_pairs: 0, // no limit in tests
            ..Default::default()
        }
    }

    #[test]
    fn test_entry_long_spread() {
        let mut state = PairState::new();
        let config = easy_trigger_config();
        let trading = easy_trigger_trading();

        // Warmup with stable prices (A=100, B=100, beta=1, spread=0)
        let ts = warmup(&mut state, &config, &trading, 35);

        // Drop A sharply: spread = ln(90) - ln(100) = -0.105. After 35 bars of 0,
        // this is a huge z-score deviation (negative).
        let _ = state.on_price("A", 90.0, &config, &trading, ts);
        let intents = state.on_price("B", 100.0, &config, &trading, ts);

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

        let ts = warmup(&mut state, &config, &trading, 35);

        // Spike A: spread = ln(110) - ln(100) = +0.095 → positive z → short spread
        let _ = state.on_price("A", 110.0, &config, &trading, ts);
        let intents = state.on_price("B", 100.0, &config, &trading, ts);

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
        let mut ts = warmup(&mut state, &config, &trading, 35);

        // Enter long spread
        let _ = state.on_price("A", 90.0, &config, &trading, ts);
        let _ = state.on_price("B", 100.0, &config, &trading, ts);
        assert_eq!(state.position(), PairPosition::LongSpread);

        // Revert: A returns to normal → spread returns to ~0 → z near 0 → exit
        ts += DAY;
        let _ = state.on_price("A", 100.0, &config, &trading, ts);
        let intents = state.on_price("B", 100.0, &config, &trading, ts);

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
        let mut ts = warmup(&mut state, &config, &trading, 35);

        // Enter
        let _ = state.on_price("A", 90.0, &config, &trading, ts);
        let _ = state.on_price("B", 100.0, &config, &trading, ts);
        assert_eq!(state.position(), PairPosition::LongSpread);

        // Hold for max_hold bars without reversion (keep spread extended)
        let mut exited = false;
        for _ in 0..10 {
            ts += DAY;
            let _ = state.on_price("A", 90.0, &config, &trading, ts);
            let intents = state.on_price("B", 100.0, &config, &trading, ts);
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
    /// Feed 35 warmup bars with symmetric oscillating prices so rolling stats have realistic
    /// variance with mean ≈ 0.
    ///
    /// Returns the next timestamp to use for the first signal bar (a new calendar day).
    fn warmup_with_variance(
        state: &mut PairState,
        config: &PairConfig,
        trading: &PairsTradingConfig,
    ) -> i64 {
        // Alternate A between 95 and 105 to produce:
        //   mean ≈ 0  (symmetric around 0)
        //   std  ≈ 0.05
        // This keeps exit_z at a sane scale (e.g., entry at A=90 → exit_z ≈ -2.1)
        // so stop_z=20 tests do not trip immediately.
        let mut ts = DAY;
        for i in 0..35 {
            let a = if i % 2 == 0 { 95.0 } else { 105.0 };
            let _ = state.on_price("A", a, config, trading, ts);
            let _ = state.on_price("B", 100.0, config, trading, ts);
            ts += DAY;
        }
        ts
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
        let mut ts = warmup_with_variance(&mut state, &config, &trading);

        // Enter long spread (A drops sharply, spread becomes very negative)
        let _ = state.on_price("A", 90.0, &config, &trading, ts);
        let _ = state.on_price("B", 100.0, &config, &trading, ts);
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
            ts += DAY;
            let _ = state.on_price("A", 90.0, &config, &trading, ts);
            let intents = state.on_price("B", 100.0, &config, &trading, ts);
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
        ts += DAY;
        let _ = state.on_price("A", 100.0, &config, &trading, ts);
        let intents = state.on_price("B", 100.0, &config, &trading, ts);
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
        let mut ts = warmup_with_variance(&mut state, &config, &trading);

        // Enter long spread (z negative, around entry_z threshold)
        let _ = state.on_price("A", 90.0, &config, &trading, ts);
        let _ = state.on_price("B", 100.0, &config, &trading, ts);
        assert_eq!(state.position(), PairPosition::LongSpread);

        // Spread diverges FURTHER (A drops more) relative to the entry-time baseline.
        // With entry_std ≈ 0.005, entry_mean ≈ 0, and stop_z = 3.0:
        // Stop fires when exit_z < -3.0, i.e., spread < entry_mean - 3 * entry_std ≈ -0.015.
        // A=70 gives spread ≈ ln(70/100) ≈ -0.357, which is far below the stop threshold.
        ts += DAY;
        let _ = state.on_price("A", 70.0, &config, &trading, ts);
        let intents = state.on_price("B", 100.0, &config, &trading, ts);
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

        let _ = state.on_price("A", 0.0, &config, &trading, DAY);
        let intents = state.on_price("B", 100.0, &config, &trading, DAY);
        assert!(intents.is_empty(), "zero price should be rejected");
    }

    #[test]
    fn test_flat_no_exit() {
        let mut state = PairState::new();
        let config = test_config();
        let trading = test_trading();

        // Feed bars while flat — should never produce exit signals
        let mut ts = DAY;
        for _ in 0..40 {
            let _ = state.on_price("GLD", 420.0, &config, &trading, ts);
            let intents = state.on_price("SLV", 64.0, &config, &trading, ts);
            assert!(intents.is_empty(), "flat position should not produce exits");
            ts += DAY;
        }
        assert_eq!(state.position(), PairPosition::Flat);
    }

    #[test]
    fn test_unrelated_symbol_ignored() {
        let mut state = PairState::new();
        let config = test_config();
        let trading = test_trading();
        let intents = state.on_price("AAPL", 150.0, &config, &trading, DAY);
        assert!(intents.is_empty(), "unrelated symbol should be ignored");
    }

    #[test]
    fn test_qty_beta_weighted() {
        let trading = PairsTradingConfig {
            notional_per_leg: 10_000.0,
            ..PairsTradingConfig::default()
        };
        // Leg A: $10,000 / $420 = 23.8 → floor = 23 shares
        let qty_a = (trading.notional_per_leg / 420.0).floor();
        assert_eq!(qty_a, 23.0);

        // Leg B with beta=0.50: $10,000 * 0.50 / $64 = 78.125 → floor = 78
        let beta = 0.50;
        let qty_b = (trading.notional_per_leg * beta / 64.0).floor();
        assert_eq!(qty_b, 78.0);

        // With beta=1.0 (equal), sizing matches old equal-dollar behavior
        let qty_b_unit = (trading.notional_per_leg * 1.0 / 64.0).floor();
        assert_eq!(qty_b_unit, 156.0);
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
        let ts = warmup(&mut state, &config, &trading, 35);

        // Enter long spread (A drops, making spread very negative)
        let _ = state.on_price("A", 90.0, &config, &trading, ts);
        let _ = state.on_price("B", 100.0, &config, &trading, ts);
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
        let mut ts = warmup(&mut state, &config, &trading, 35);
        let _ = state.on_price("A", 90.0, &config, &trading, ts);
        let _ = state.on_price("B", 100.0, &config, &trading, ts);
        assert_eq!(state.position(), PairPosition::LongSpread);
        assert!(state.exit_context().is_some());

        // Revert to trigger exit
        ts += DAY;
        let _ = state.on_price("A", 100.0, &config, &trading, ts);
        let _ = state.on_price("B", 100.0, &config, &trading, ts);
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
            max_concurrent_pairs: 0,
            ..Default::default()
        };

        // Warmup: stable spread = ln(100) - ln(100) = 0
        let mut ts = warmup(&mut state, &config, &trading, 35);

        // Enter long spread: A drops to 90, spread becomes very negative, z << -1.5
        let _ = state.on_price("A", 90.0, &config, &trading, ts);
        let _ = state.on_price("B", 100.0, &config, &trading, ts);
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
            ts += DAY;
            let _ = state.on_price("A", 90.0, &config, &trading, ts);
            let intents = state.on_price("B", 100.0, &config, &trading, ts);
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
        let mut ts = warmup(&mut state, &config, &trading, 35);

        // Enter long spread: A drops to 90
        let _ = state.on_price("A", 90.0, &config, &trading, ts);
        let _ = state.on_price("B", 100.0, &config, &trading, ts);
        assert_eq!(state.position(), PairPosition::LongSpread);

        // True reversion: A returns to 100. Spread = 0 ≈ entry_mean.
        // fixed exit_z ≈ (0 - entry_mean) / entry_std ≈ 0 → well above -exit_z (= -0.3)
        ts += DAY;
        let _ = state.on_price("A", 100.0, &config, &trading, ts);
        let intents = state.on_price("B", 100.0, &config, &trading, ts);

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
            max_concurrent_pairs: 0,
            ..Default::default()
        };

        // Warmup: stable spread = 0
        let mut ts = warmup(&mut state, &config, &trading, 35);

        // Enter short spread: A spikes to 115, spread = ln(115) - ln(100) = +0.139 > 0
        // After 35 bars of 0, this is a large positive z → should trigger short spread entry
        let _ = state.on_price("A", 115.0, &config, &trading, ts);
        let _ = state.on_price("B", 100.0, &config, &trading, ts);
        assert_eq!(
            state.position(),
            PairPosition::ShortSpread,
            "should enter short on large positive z"
        );

        // Permanent drift: A stays at 115. Rolling window fills with high spread.
        // Rolling z decays mechanically. Fixed exit z stays at entry level.
        let mut false_exits = 0usize;
        for _ in 0..40 {
            ts += DAY;
            let _ = state.on_price("A", 115.0, &config, &trading, ts);
            let intents = state.on_price("B", 100.0, &config, &trading, ts);
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
        let mut ts = warmup(&mut state, &config, &trading, 35);

        // Enter short spread: A spikes to 110
        let _ = state.on_price("A", 110.0, &config, &trading, ts);
        let _ = state.on_price("B", 100.0, &config, &trading, ts);
        assert_eq!(state.position(), PairPosition::ShortSpread);

        // True reversion: A returns to 100 → spread returns to ~0 ≈ entry_mean
        ts += DAY;
        let _ = state.on_price("A", 100.0, &config, &trading, ts);
        let intents = state.on_price("B", 100.0, &config, &trading, ts);

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
        let ts = warmup(&mut state, &config, &trading, 35);

        // Trigger long spread entry
        let _ = state.on_price("A", 90.0, &config, &trading, ts);
        let intents = state.on_price("B", 100.0, &config, &trading, ts);

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

        let ts = warmup(&mut state, &config, &trading, 35);
        let _ = state.on_price("A", 90.0, &config, &trading, ts);
        let intents = state.on_price("B", 100.0, &config, &trading, ts);

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
        let mut ts = warmup(&mut state, &config, &trading, 35);
        let _ = state.on_price("A", 90.0, &config, &trading, ts);
        let _ = state.on_price("B", 100.0, &config, &trading, ts);
        assert_eq!(state.position(), PairPosition::LongSpread);

        // Revert to trigger close
        ts += DAY;
        let _ = state.on_price("A", 100.0, &config, &trading, ts);
        let intents = state.on_price("B", 100.0, &config, &trading, ts);

        assert!(!intents.is_empty(), "should close");
        for intent in &intents {
            assert_eq!(
                intent.priority_score, 0.0,
                "close intents must have priority_score=0.0, got {}",
                intent.priority_score
            );
        }
    }

    // ── Intraday rolling mode tests ──
    //
    // These verify the new `intraday_rolling_bars` feature:
    //   1. warm-up completes in N bars (minutes), not N days
    //   2. spread stats are populated every bar, not just at daily close
    //   3. entries fire intraday once warmed up (no is_new_day gate)
    //   4. daily mode (intraday_rolling_bars = 0) is unchanged

    /// One minute in milliseconds — intraday tests step by this.
    const MINUTE: i64 = 60_000;

    fn intraday_trading(rolling_bars: usize) -> PairsTradingConfig {
        PairsTradingConfig {
            intraday_rolling_bars: rolling_bars,
            min_hold_bars: 0,
            // Force close very late so EOD doesn't interfere with these tests
            force_close_minute: 23 * 60 + 59,
            last_entry_hour: 23,
            ..PairsTradingConfig::default()
        }
    }

    #[test]
    fn test_intraday_rolling_warmup_in_bars_not_days() {
        // In intraday rolling mode with window=30, the rolling z becomes
        // ready after 30 observed minute-bars. There is a one-bar delay
        // because the first bar buffers without pushing (same pattern as
        // daily mode), so feeding N bars produces spread_count = N-1.
        let mut state = PairState::new();
        let config = test_config();
        let trading = intraday_trading(30);

        // Feed 30 minute-bars — buffered-delayed, spread_count should be 29
        let mut ts: i64 = DAY;
        for i in 0..30 {
            let a = if i % 2 == 0 { 99.5 } else { 100.5 };
            let _ = state.on_price(&config.leg_a, a, &config, &trading, ts);
            let _ = state.on_price(&config.leg_b, 100.0, &config, &trading, ts);
            ts += MINUTE;
        }
        assert_eq!(
            state.spread_count, 29,
            "spread_count should be N-1 after N bars (one-bar buffer delay)"
        );

        // Bar 31 pushes bar 30's buffered spread → count = 30 → warmed up
        let _ = state.on_price(&config.leg_a, 99.5, &config, &trading, ts);
        let _ = state.on_price(&config.leg_b, 100.0, &config, &trading, ts);
        assert_eq!(state.spread_count, 30);
    }

    #[test]
    fn test_intraday_rolling_entry_fires_intraday() {
        // In intraday rolling mode, once warmed up, entries should fire on
        // any bar when |z| exceeds entry_z — NOT only on is_new_day.
        //
        // Uses a clean pair with beta=1.0 so log-spread math is easy.
        // Warmup alternates leg_a between 99 and 101 giving mean≈0, std≈0.01.
        // Then a moderate shock to a=97.5 produces z ≈ -2.5 — above entry_z=2.0
        // but well below stop_z=4.0, so entry should fire (not be blocked).
        let mut state = PairState::new();
        let config = PairConfig {
            leg_a: "GLD".into(),
            leg_b: "SLV".into(),
            alpha: 0.0,
            beta: 1.0,
            kappa: 0.0,
            max_hold_bars: 0,
            lookback_bars: 0,
        };
        let trading = intraday_trading(30);

        // Warm up: 35 bars alternating a∈{99,101}, b=100 → spread alternates
        // between ln(0.99)≈-0.01 and ln(1.01)≈+0.01. Mean≈0, std≈0.01.
        let mut ts: i64 = DAY;
        for i in 0..35 {
            let a = if i % 2 == 0 { 99.0 } else { 101.0 };
            let _ = state.on_price(&config.leg_a, a, &config, &trading, ts);
            let _ = state.on_price(&config.leg_b, 100.0, &config, &trading, ts);
            ts += MINUTE;
        }
        assert!(
            state.spread_count >= 30,
            "should be warmed up after 35 bars, got {}",
            state.spread_count
        );

        // Moderate shock: a=97.5, b=100 → spread = ln(0.975) ≈ -0.0253
        // Against mean≈0, std≈0.01 → z ≈ -2.5 (above entry_z=2.0, below stop_z=4.0)
        // Same day as the warmup → NOT is_new_day. Entry must fire anyway in
        // intraday rolling mode.
        let _ = state.on_price(&config.leg_a, 97.5, &config, &trading, ts);
        let intents = state.on_price(&config.leg_b, 100.0, &config, &trading, ts);
        assert!(
            !intents.is_empty(),
            "intraday rolling mode should fire entry mid-day (no is_new_day gate)"
        );
        assert_eq!(state.position, PairPosition::LongSpread);
    }

    #[test]
    fn test_intraday_rolling_window_size_overrides() {
        // for_pair() should use intraday_rolling_bars as the window when set,
        // ignoring the per-pair lookback_bars and the global lookback.
        let config = PairConfig {
            leg_a: "A".into(),
            leg_b: "B".into(),
            alpha: 0.0,
            beta: 1.0,
            kappa: 0.0,
            max_hold_bars: 0,
            lookback_bars: 60, // per-pair: should be ignored
        };
        let trading = PairsTradingConfig {
            lookback: 120, // global: should be ignored
            intraday_rolling_bars: 30,
            ..PairsTradingConfig::default()
        };
        let state = PairState::for_pair(&config, &trading);
        assert_eq!(
            state.spread_stats.window(),
            30,
            "intraday_rolling_bars should take precedence over lookback_bars and lookback"
        );
    }

    #[test]
    fn test_daily_mode_unchanged_when_intraday_rolling_zero() {
        // When intraday_rolling_bars=0 (default), behavior must match
        // the original daily-rolling mode. spread_count only increments
        // on new-day boundaries.
        let mut state = PairState::new();
        let config = test_config();
        let trading = test_trading(); // default → intraday_rolling_bars = 0

        // Feed 10 intraday bars on the SAME day — spread_count should stay 0
        // (because daily mode only pushes on new-day)
        let mut ts: i64 = DAY;
        for i in 0..10 {
            let a = if i % 2 == 0 { 99.5 } else { 100.5 };
            let _ = state.on_price(&config.leg_a, a, &config, &trading, ts);
            let _ = state.on_price(&config.leg_b, 100.0, &config, &trading, ts);
            ts += MINUTE;
        }
        assert_eq!(
            state.spread_count, 0,
            "daily mode must not push on intraday bars"
        );
    }
}
