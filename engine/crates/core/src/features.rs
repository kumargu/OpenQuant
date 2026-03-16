//! Incremental feature computation from price bars.
//!
//! Features are the quantitative inputs to strategies. Every feature updates
//! in O(1) per bar using fixed-size stack buffers — zero heap allocation
//! in the hot path.
//!
//! ```text
//!  Bar (close, high, low, volume)
//!   │
//!   ├──► RingBuf<32> (closes)  ──► SMA-20 (running sum, O(1))
//!   │                           ──► N-bar returns (lookback)
//!   │
//!   ├──► RollingStats<32>      ──► return std dev (running sum + sum_sq)
//!   │    (1-bar returns)        ──► z-score = return / std_dev
//!   │
//!   ├──► RollingStats<32>      ──► relative volume = vol / avg_vol
//!   │    (volume)
//!   │
//!   └──► direct math           ──► bar range, close location
//! ```
//!
//! V1 features:
//! - Returns: 1-bar, 5-bar, 20-bar simple returns
//! - SMA: 20-bar simple moving average of close (running sum, not iter)
//! - Volatility: 20-bar rolling std dev of returns
//! - Z-score: current return / rolling volatility
//! - Volume: current volume / 20-bar avg volume
//! - Bar shape: range (high - low), close location within bar
//!
//! V2 features (momentum indicators):
//! - EMA: exponential moving average (fast=10, slow=30), O(1) no buffer
//! - ADX: average directional index for trend strength (0-100)
//! - Bollinger: %B and bandwidth from existing SMA/std

// ---------------------------------------------------------------------------
// Ring buffer — const-generic, stack-allocated, zero-alloc
// ---------------------------------------------------------------------------

/// Fixed-size ring buffer on the stack. Capacity must be a power of 2
/// so index wrapping uses bitwise AND instead of modulo.
///
/// ```text
///  capacity = 4, mask = 3 (0b11)
///
///  push(A): [A _ _ _]  head=1, len=1
///  push(B): [A B _ _]  head=2, len=2
///  push(C): [A B C _]  head=3, len=3
///  push(D): [A B C D]  head=0, len=4 (full)
///  push(E): [E B C D]  head=1, len=4 (A overwritten)
///            ^oldest    ^newest = data[(head-1) & mask]
/// ```
#[derive(Clone)]
pub struct RingBuf<const N: usize> {
    data: [f64; N],
    head: usize,
    len: usize,
}

impl<const N: usize> Default for RingBuf<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> RingBuf<N> {
    const MASK: usize = N - 1; // only valid when N is power of 2

    pub fn new() -> Self {
        assert!(N.is_power_of_two(), "RingBuf capacity must be power of 2");
        Self {
            data: [0.0; N],
            head: 0,
            len: 0,
        }
    }

    #[inline]
    pub fn push(&mut self, value: f64) {
        self.data[self.head] = value;
        self.head = (self.head + 1) & Self::MASK;
        if self.len < N {
            self.len += 1;
        }
    }

    #[inline]
    pub fn is_full(&self) -> bool {
        self.len == N
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Most recent value pushed.
    #[inline]
    pub fn last(&self) -> Option<f64> {
        if self.len == 0 {
            None
        } else {
            Some(self.data[(self.head.wrapping_sub(1)) & Self::MASK])
        }
    }

    /// Value N steps ago (0 = most recent, 1 = previous, ...).
    #[inline]
    pub fn ago(&self, n: usize) -> Option<f64> {
        if n >= self.len {
            None
        } else {
            Some(self.data[(self.head.wrapping_sub(1 + n)) & Self::MASK])
        }
    }

    /// Oldest value in the buffer (will be overwritten on next push when full).
    #[inline]
    pub fn oldest(&self) -> Option<f64> {
        if self.len == 0 {
            None
        } else if self.is_full() {
            Some(self.data[self.head]) // head points to oldest when full
        } else {
            Some(self.data[0])
        }
    }
}

// ---------------------------------------------------------------------------
// Rolling stats — running sum + sum_sq, O(1) per update
// ---------------------------------------------------------------------------

/// Rolling mean and standard deviation over a fixed window.
/// Uses running sum and sum-of-squares — no iteration per update.
///
/// ```text
///  push(new):
///    if full: sum -= oldest; sum_sq -= oldest²
///    sum += new; sum_sq += new²
///    buf.push(new)
///
///  mean     = sum / len
///  variance = sum_sq/len - mean²
///  std_dev  = sqrt(variance)
/// ```
#[derive(Clone)]
pub struct RollingStats<const N: usize> {
    buf: RingBuf<N>,
    sum: f64,
    sum_sq: f64,
}

impl<const N: usize> Default for RollingStats<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> RollingStats<N> {
    pub fn new() -> Self {
        Self {
            buf: RingBuf::new(),
            sum: 0.0,
            sum_sq: 0.0,
        }
    }

    #[inline]
    pub fn push(&mut self, value: f64) {
        if self.buf.is_full() {
            let old = self.buf.oldest().unwrap();
            self.sum -= old;
            self.sum_sq -= old * old;
        }
        self.sum += value;
        self.sum_sq += value * value;
        self.buf.push(value);
    }

    #[inline]
    pub fn is_ready(&self) -> bool {
        self.buf.is_full()
    }

    #[inline]
    pub fn mean(&self) -> f64 {
        if self.buf.is_empty() {
            return 0.0;
        }
        self.sum / self.buf.len() as f64
    }

    #[inline]
    pub fn variance(&self) -> f64 {
        let n = self.buf.len() as f64;
        if n < 2.0 {
            return 0.0;
        }
        let mean = self.sum / n;
        (self.sum_sq / n - mean * mean).max(0.0)
    }

    #[inline]
    pub fn std_dev(&self) -> f64 {
        self.variance().sqrt()
    }
}

// ---------------------------------------------------------------------------
// SMA — running sum, O(1) per update
// ---------------------------------------------------------------------------

/// Simple moving average using a running sum.
/// Does NOT iterate the buffer — just adds new, subtracts oldest.
///
/// ```text
///  push(new):
///    if full: sum -= oldest
///    sum += new
///    sma = sum / len
/// ```
#[derive(Clone)]
pub struct Sma<const N: usize> {
    buf: RingBuf<N>,
    sum: f64,
}

impl<const N: usize> Default for Sma<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> Sma<N> {
    pub fn new() -> Self {
        Self {
            buf: RingBuf::new(),
            sum: 0.0,
        }
    }

    #[inline]
    pub fn push(&mut self, value: f64) -> f64 {
        if self.buf.is_full() {
            self.sum -= self.buf.oldest().unwrap();
        }
        self.sum += value;
        self.buf.push(value);
        self.sum / self.buf.len() as f64
    }

    #[inline]
    pub fn is_ready(&self) -> bool {
        self.buf.is_full()
    }
}

// ---------------------------------------------------------------------------
// EMA — exponential moving average, O(1) per update, no buffer
// ---------------------------------------------------------------------------

/// Exponential Moving Average — weights recent prices exponentially more.
///
/// ```text
///  EMA(t) = α × Price(t) + (1 - α) × EMA(t-1)
///
///  where α = 2 / (period + 1)    ← smoothing factor
///
///  Weight of bar k steps ago = α × (1-α)^k
///  → latest bar dominates; old bars decay exponentially.
/// ```
///
/// Unlike SMA which needs a ring buffer, EMA only stores one value.
/// Memory: 32 bytes total (vs 256+ bytes for an SMA with buffer).
#[derive(Clone)]
pub struct Ema {
    alpha: f64,
    one_minus_alpha: f64,
    value: f64,
    count: usize,
    period: usize,
}

impl Ema {
    pub fn new(period: usize) -> Self {
        let alpha = 2.0 / (period as f64 + 1.0);
        Self {
            alpha,
            one_minus_alpha: 1.0 - alpha,
            value: 0.0,
            count: 0,
            period,
        }
    }

    /// Push a new value and return the updated EMA.
    /// First value seeds the EMA (no smoothing applied).
    #[inline]
    pub fn push(&mut self, value: f64) -> f64 {
        self.count += 1;
        if self.count == 1 {
            self.value = value;
        } else {
            self.value = self.alpha * value + self.one_minus_alpha * self.value;
        }
        self.value
    }

    #[inline]
    pub fn value(&self) -> f64 {
        self.value
    }

    /// Ready after `period` bars (EMA is technically valid from bar 1,
    /// but needs ~period bars for the exponential weights to stabilize).
    #[inline]
    pub fn is_ready(&self) -> bool {
        self.count >= self.period
    }
}

// ---------------------------------------------------------------------------
// Wilder EMA — Welles Wilder's smoothing (α = 1/N), used by ADX/RSI
// ---------------------------------------------------------------------------

/// Wilder's smoothing method — a variant of EMA with α = 1/N.
///
/// ```text
///  Wilder(t) = Wilder(t-1) + (value - Wilder(t-1)) / N
///            = (1 - 1/N) × Wilder(t-1) + (1/N) × value
///
///  Standard EMA: α = 2/(N+1)  →  for N=14: α = 0.1333 (faster)
///  Wilder's:     α = 1/N      →  for N=14: α = 0.0714 (slower)
/// ```
///
/// Wilder's smoothing is the canonical method for ADX, ATR, and RSI.
/// Using standard EMA would produce values that diverge from Bloomberg,
/// TradingView, and TA-Lib reference implementations.
#[derive(Clone)]
pub struct WilderEma {
    alpha: f64,
    one_minus_alpha: f64,
    value: f64,
    count: usize,
    period: usize,
}

impl WilderEma {
    pub fn new(period: usize) -> Self {
        let alpha = 1.0 / period as f64;
        Self {
            alpha,
            one_minus_alpha: 1.0 - alpha,
            value: 0.0,
            count: 0,
            period,
        }
    }

    #[inline]
    pub fn push(&mut self, value: f64) -> f64 {
        self.count += 1;
        if self.count == 1 {
            self.value = value;
        } else {
            self.value = self.alpha * value + self.one_minus_alpha * self.value;
        }
        self.value
    }

    #[inline]
    pub fn value(&self) -> f64 {
        self.value
    }

    #[inline]
    pub fn is_ready(&self) -> bool {
        self.count >= self.period
    }
}

// ---------------------------------------------------------------------------
// ADX — average directional index, trend strength 0-100
// ---------------------------------------------------------------------------

/// Average Directional Index — measures trend strength regardless of direction.
///
/// ```text
///  +DM = max(High(t) - High(t-1), 0)   if > -DM, else 0
///  -DM = max(Low(t-1) - Low(t), 0)     if > +DM, else 0
///
///  +DI = 100 × EMA(+DM) / EMA(TrueRange)
///  -DI = 100 × EMA(-DM) / EMA(TrueRange)
///
///  DX  = 100 × |+DI - -DI| / (+DI + -DI)
///  ADX = EMA(DX)
///
///  ADX < 20  → no trend (mean-reversion territory)
///  ADX 20-40 → moderate trend (momentum starts working)
///  ADX > 40  → strong trend (momentum's sweet spot)
/// ```
///
/// Implementation uses 4 Wilder EMAs (α=1/N) — all O(1) per bar, ~200 bytes total.
/// Uses Wilder's smoothing to match Bloomberg/TradingView/TA-Lib reference values.
#[derive(Clone)]
pub struct Adx {
    plus_dm_ema: WilderEma,
    minus_dm_ema: WilderEma,
    tr_ema: WilderEma,
    adx_ema: WilderEma,
    prev_high: f64,
    prev_low: f64,
    prev_close: f64,
    count: usize,
    period: usize,
}

impl Adx {
    pub fn new(period: usize) -> Self {
        Self {
            plus_dm_ema: WilderEma::new(period),
            minus_dm_ema: WilderEma::new(period),
            tr_ema: WilderEma::new(period),
            adx_ema: WilderEma::new(period),
            prev_high: 0.0,
            prev_low: 0.0,
            prev_close: 0.0,
            count: 0,
            period,
        }
    }

    /// Update with a new bar. Returns `(adx, +DI, -DI)`.
    #[inline]
    pub fn update(&mut self, high: f64, low: f64, close: f64) -> (f64, f64, f64) {
        self.count += 1;
        if self.count == 1 {
            self.prev_high = high;
            self.prev_low = low;
            self.prev_close = close;
            return (0.0, 0.0, 0.0);
        }

        // Directional Movement: only the larger direction counts
        let up_move = high - self.prev_high;
        let down_move = self.prev_low - low;

        let plus_dm = if up_move > down_move && up_move > 0.0 {
            up_move
        } else {
            0.0
        };
        let minus_dm = if down_move > up_move && down_move > 0.0 {
            down_move
        } else {
            0.0
        };

        // True Range
        let hl = high - low;
        let hc = (high - self.prev_close).abs();
        let lc = (low - self.prev_close).abs();
        let tr = hl.max(hc).max(lc);

        self.prev_high = high;
        self.prev_low = low;
        self.prev_close = close;

        // Smooth with EMAs
        let smoothed_plus_dm = self.plus_dm_ema.push(plus_dm);
        let smoothed_minus_dm = self.minus_dm_ema.push(minus_dm);
        let smoothed_tr = self.tr_ema.push(tr);

        if smoothed_tr < 1e-10 {
            return (0.0, 0.0, 0.0);
        }

        // Directional Indicators
        let plus_di = 100.0 * smoothed_plus_dm / smoothed_tr;
        let minus_di = 100.0 * smoothed_minus_dm / smoothed_tr;

        // Directional Index → ADX
        let di_sum = plus_di + minus_di;
        let dx = if di_sum > 1e-10 {
            100.0 * (plus_di - minus_di).abs() / di_sum
        } else {
            0.0
        };

        let adx = self.adx_ema.push(dx);

        (adx, plus_di, minus_di)
    }

    /// Ready after 2×period bars (DM smoothing + ADX smoothing).
    #[inline]
    pub fn is_ready(&self) -> bool {
        self.count >= self.period * 2
    }
}

// ---------------------------------------------------------------------------
// Feature output + per-symbol state
// ---------------------------------------------------------------------------

/// All computed features for a single symbol at current bar.
#[derive(Debug, Clone, Default)]
pub struct FeatureValues {
    // --- V1: mean-reversion features ---
    pub return_1: f64,        // 1-bar return
    pub return_5: f64,        // 5-bar return
    pub return_20: f64,       // 20-bar return
    pub sma_20: f64,          // simple moving average of close (32-bar window, power-of-2 constraint)
    pub sma_50: f64,          // 50-bar simple moving average of close (trend)
    pub atr: f64,             // average true range (14-bar)
    pub return_std_20: f64,   // 20-bar rolling std dev of 1-bar returns
    pub return_z_score: f64,  // return_1 / return_std_20
    pub relative_volume: f64, // current volume / 20-bar avg volume
    pub bar_range: f64,       // high - low
    pub close_location: f64,  // (close - low) / (high - low)
    pub trend_up: bool,       // true when close > sma_50 (bullish trend)
    pub warmed_up: bool,      // true once all features have enough data

    // --- V2: momentum features ---
    pub ema_fast: f64,        // EMA(10) — fast exponential moving average
    pub ema_slow: f64,        // EMA(30) — slow exponential moving average
    pub ema_fast_above_slow: bool,  // true when EMA(10) > EMA(30) (level, not event)
    pub adx: f64,             // trend strength 0-100
    pub plus_di: f64,         // +DI: bullish directional indicator
    pub minus_di: f64,        // -DI: bearish directional indicator

    // --- V2: Bollinger Band features (uses rolling std of close prices) ---
    pub bollinger_upper: f64,    // SMA(32) + 2 × std_dev(close, 32)
    pub bollinger_lower: f64,    // SMA(32) - 2 × std_dev(close, 32)
    pub bollinger_pct_b: f64,    // (close - lower) / (upper - lower), 0-1 normally
    pub bollinger_bandwidth: f64, // (upper - lower) / SMA(32), normalized width
}

/// Per-symbol feature state. All buffers are stack-allocated, fixed-size.
/// Uses power-of-2 capacity (32) for a 20-bar lookback window.
#[derive(Clone)]
pub struct FeatureState {
    // V1 state
    closes: RingBuf<64>,         // last N closes for lookback returns (64 for SMA-50)
    sma: Sma<32>,                // 20-bar SMA via running sum
    sma_long: Sma<64>,           // 50-bar SMA for trend detection
    atr_stats: RollingStats<16>, // 14-bar ATR via rolling mean of true range
    return_stats: RollingStats<32>, // rolling std of 1-bar returns
    volume_stats: RollingStats<32>, // rolling avg of volume
    prev_close: Option<f64>,     // previous close for true range calculation
    bar_count: usize,
    warmup_period: usize,

    // V2 state: momentum indicators
    ema_fast: Ema,    // EMA(10) for momentum crossover
    ema_slow: Ema,    // EMA(30) for momentum crossover
    adx: Adx,         // ADX(14) for trend strength

    // V2 state: Bollinger Bands
    close_stats: RollingStats<32>,  // rolling std of close prices (for Bollinger)
}

impl Default for FeatureState {
    fn default() -> Self {
        Self::new()
    }
}

impl FeatureState {
    pub fn new() -> Self {
        Self {
            closes: RingBuf::new(),
            sma: Sma::new(),
            sma_long: Sma::new(),
            atr_stats: RollingStats::new(),
            return_stats: RollingStats::new(),
            volume_stats: RollingStats::new(),
            prev_close: None,
            bar_count: 0,
            warmup_period: 50, // SMA-50 needs 50 bars (bottleneck)

            ema_fast: Ema::new(10),  // 10-bar EMA for momentum
            ema_slow: Ema::new(30),  // 30-bar EMA for momentum
            adx: Adx::new(14),       // 14-bar ADX for trend strength

            close_stats: RollingStats::new(), // rolling std of prices for Bollinger
        }
    }

    /// Update features with a new bar. Returns computed values.
    /// This is the hot path — zero heap allocation, O(1) per call.
    #[inline]
    pub fn update(&mut self, close: f64, high: f64, low: f64, volume: f64) -> FeatureValues {
        let prev_close = self.closes.last();
        self.closes.push(close);
        self.bar_count += 1;

        // 1-bar return
        let return_1 = match prev_close {
            Some(pc) if pc != 0.0 => (close - pc) / pc,
            _ => 0.0,
        };
        self.return_stats.push(return_1);

        // N-bar returns via lookback
        let return_5 = self
            .closes
            .ago(5)
            .filter(|&pc| pc != 0.0)
            .map(|pc| (close - pc) / pc)
            .unwrap_or(0.0);

        let return_20 = self
            .closes
            .ago(19)
            .filter(|&pc| pc != 0.0)
            .map(|pc| (close - pc) / pc)
            .unwrap_or(0.0);

        // SMA-20 via running sum (O(1), no iteration)
        let sma_20 = self.sma.push(close);

        // SMA-50 for trend detection
        let sma_50 = self.sma_long.push(close);

        // ATR: True Range = max(H-L, |H-prev_close|, |L-prev_close|)
        let true_range = match self.prev_close {
            Some(pc) => {
                let hl = high - low;
                let hc = (high - pc).abs();
                let lc = (low - pc).abs();
                hl.max(hc).max(lc)
            }
            None => high - low, // first bar: just use range
        };
        self.atr_stats.push(true_range);
        self.prev_close = Some(close);
        let atr = self.atr_stats.mean();

        // Volume
        self.volume_stats.push(volume);
        let avg_volume = self.volume_stats.mean();
        let relative_volume = if avg_volume > 0.0 {
            volume / avg_volume
        } else {
            1.0
        };

        // Z-score
        let std_dev = self.return_stats.std_dev();
        let return_z_score = if std_dev > 1e-10 {
            return_1 / std_dev
        } else {
            0.0
        };

        // Bar shape
        let range = high - low;
        let close_location = if range > 0.0 {
            (close - low) / range
        } else {
            0.5
        };

        // Trend: close above SMA-50 = bullish
        let trend_up = close > sma_50;

        // --- V2: Momentum indicators ---
        let ema_fast = self.ema_fast.push(close);
        let ema_slow = self.ema_slow.push(close);
        let ema_fast_above_slow = ema_fast > ema_slow;
        let (adx_val, plus_di, minus_di) = self.adx.update(high, low, close);

        // --- V2: Bollinger Bands ---
        // Standard Bollinger: SMA(N) ± 2 × std_dev(close_prices, N)
        // Uses rolling std of close prices (not returns) — matches canonical definition.
        self.close_stats.push(close);
        let close_std = self.close_stats.std_dev();
        let bollinger_upper = sma_20 + 2.0 * close_std;
        let bollinger_lower = sma_20 - 2.0 * close_std;
        let bb_width = bollinger_upper - bollinger_lower;
        let bollinger_pct_b = if bb_width > 1e-10 {
            (close - bollinger_lower) / bb_width
        } else {
            0.5
        };
        let bollinger_bandwidth = if sma_20 > 1e-10 {
            bb_width / sma_20
        } else {
            0.0
        };

        FeatureValues {
            return_1,
            return_5,
            return_20,
            sma_20,
            sma_50,
            atr,
            return_std_20: std_dev,
            return_z_score,
            relative_volume,
            bar_range: range,
            close_location,
            trend_up,
            warmed_up: self.bar_count >= self.warmup_period,

            ema_fast,
            ema_slow,
            ema_fast_above_slow,
            adx: adx_val,
            plus_di,
            minus_di,
            bollinger_upper,
            bollinger_lower,
            bollinger_pct_b,
            bollinger_bandwidth,
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // --- RingBuf tests ---

    #[test]
    fn ringbuf_empty() {
        let rb = RingBuf::<4>::new();
        assert_eq!(rb.len(), 0);
        assert!(!rb.is_full());
        assert_eq!(rb.last(), None);
        assert_eq!(rb.ago(0), None);
        assert_eq!(rb.oldest(), None);
    }

    #[test]
    fn ringbuf_push_and_access() {
        let mut rb = RingBuf::<4>::new();
        rb.push(10.0);
        rb.push(20.0);
        rb.push(30.0);
        assert_eq!(rb.len(), 3);
        assert!(!rb.is_full());
        assert_eq!(rb.last(), Some(30.0));
        assert_eq!(rb.ago(0), Some(30.0));
        assert_eq!(rb.ago(1), Some(20.0));
        assert_eq!(rb.ago(2), Some(10.0));
        assert_eq!(rb.ago(3), None);
    }

    #[test]
    fn ringbuf_wraps_correctly() {
        let mut rb = RingBuf::<4>::new();
        rb.push(1.0);
        rb.push(2.0);
        rb.push(3.0);
        rb.push(4.0);
        assert!(rb.is_full());
        assert_eq!(rb.oldest(), Some(1.0));

        // Push overwrites oldest
        rb.push(5.0);
        assert_eq!(rb.last(), Some(5.0));
        assert_eq!(rb.oldest(), Some(2.0)); // 1.0 is gone
        assert_eq!(rb.ago(0), Some(5.0));
        assert_eq!(rb.ago(1), Some(4.0));
        assert_eq!(rb.ago(2), Some(3.0));
        assert_eq!(rb.ago(3), Some(2.0));
    }

    #[test]
    fn ringbuf_full_cycle() {
        // Push more than capacity to test multiple wraps
        let mut rb = RingBuf::<4>::new();
        for i in 0..20 {
            rb.push(i as f64);
        }
        assert_eq!(rb.last(), Some(19.0));
        assert_eq!(rb.ago(1), Some(18.0));
        assert_eq!(rb.ago(3), Some(16.0));
    }

    // --- RollingStats tests ---

    #[test]
    fn rolling_stats_mean() {
        let mut rs = RollingStats::<4>::new();
        rs.push(2.0);
        rs.push(4.0);
        rs.push(6.0);
        assert!((rs.mean() - 4.0).abs() < 1e-10);
    }

    #[test]
    fn rolling_stats_variance() {
        let mut rs = RollingStats::<4>::new();
        rs.push(2.0);
        rs.push(4.0);
        rs.push(6.0);
        // Var of [2,4,6]: mean=4, var = ((4+0+4)/3) = 8/3
        assert!((rs.variance() - 8.0 / 3.0).abs() < 1e-10);
    }

    #[test]
    fn rolling_stats_window_evicts() {
        let mut rs = RollingStats::<4>::new();
        rs.push(10.0);
        rs.push(10.0);
        rs.push(10.0);
        rs.push(10.0);
        assert!(rs.is_ready());
        assert!((rs.mean() - 10.0).abs() < 1e-10);

        // Push a different value — oldest 10.0 is evicted
        rs.push(20.0);
        // Window is now [10, 10, 10, 20], mean = 12.5
        assert!((rs.mean() - 12.5).abs() < 1e-10);
    }

    #[test]
    fn rolling_stats_std_dev_zero_for_constant() {
        let mut rs = RollingStats::<4>::new();
        for _ in 0..4 {
            rs.push(5.0);
        }
        assert!(
            rs.std_dev() < 1e-10,
            "constant values should have zero std dev"
        );
    }

    // --- SMA tests ---

    #[test]
    fn sma_before_full() {
        let mut sma = Sma::<4>::new();
        let v = sma.push(10.0);
        assert!((v - 10.0).abs() < 1e-10); // 10/1
        let v = sma.push(20.0);
        assert!((v - 15.0).abs() < 1e-10); // 30/2
    }

    #[test]
    fn sma_full_window() {
        let mut sma = Sma::<4>::new();
        sma.push(10.0);
        sma.push(20.0);
        sma.push(30.0);
        let v = sma.push(40.0);
        assert!((v - 25.0).abs() < 1e-10); // (10+20+30+40)/4
        assert!(sma.is_ready());
    }

    #[test]
    fn sma_rolling_eviction() {
        let mut sma = Sma::<4>::new();
        sma.push(10.0);
        sma.push(20.0);
        sma.push(30.0);
        sma.push(40.0);
        // Window: [10,20,30,40], avg=25
        let v = sma.push(50.0);
        // Window: [20,30,40,50], avg=35
        assert!((v - 35.0).abs() < 1e-10);
    }

    // --- FeatureState tests ---

    #[test]
    fn feature_warmup() {
        let mut state = FeatureState::new();
        for i in 0..49 {
            let f = state.update(100.0 + i as f64, 101.0 + i as f64, 99.0 + i as f64, 1000.0);
            assert!(!f.warmed_up, "should not be warmed up at bar {i}");
        }
        let f = state.update(120.0, 121.0, 119.0, 1000.0);
        assert!(f.warmed_up, "should be warmed up at bar 50");
    }

    #[test]
    fn return_1_computation() {
        let mut state = FeatureState::new();
        state.update(100.0, 101.0, 99.0, 1000.0);
        let f = state.update(105.0, 106.0, 104.0, 1000.0);
        // (105 - 100) / 100 = 0.05
        assert!((f.return_1 - 0.05).abs() < 1e-10, "expected 5% return");
    }

    #[test]
    fn return_1_first_bar_is_zero() {
        let mut state = FeatureState::new();
        let f = state.update(100.0, 101.0, 99.0, 1000.0);
        assert_eq!(f.return_1, 0.0, "first bar has no previous close");
    }

    #[test]
    fn relative_volume_spike() {
        let mut state = FeatureState::new();
        for _ in 0..20 {
            state.update(100.0, 101.0, 99.0, 1000.0);
        }
        let f = state.update(100.0, 101.0, 99.0, 2000.0);
        // 2000 bar is included in rolling avg: (19*1000 + 2000) / 20 = 1050
        // Relative = 2000/1050 ≈ 1.905
        assert!(
            f.relative_volume > 1.5,
            "expected high relative volume, got {}",
            f.relative_volume
        );
    }

    #[test]
    fn z_score_extreme_drop() {
        let mut state = FeatureState::new();
        for _ in 0..20 {
            state.update(100.0, 100.5, 99.5, 1000.0);
        }
        let f = state.update(95.0, 100.0, 94.0, 1500.0);
        assert!(
            f.return_z_score < -2.0,
            "expected z < -2, got {}",
            f.return_z_score
        );
    }

    #[test]
    fn z_score_zero_for_constant_prices() {
        let mut state = FeatureState::new();
        for _ in 0..25 {
            let f = state.update(100.0, 100.0, 100.0, 1000.0);
            assert!(
                f.return_z_score.abs() < 1e-10,
                "constant prices should give z=0, got {}",
                f.return_z_score
            );
        }
    }

    #[test]
    fn bar_range_and_close_location() {
        let mut state = FeatureState::new();
        // Close at high
        let f = state.update(110.0, 110.0, 90.0, 1000.0);
        assert!((f.bar_range - 20.0).abs() < 1e-10);
        assert!((f.close_location - 1.0).abs() < 1e-10);

        // Close at low
        let f = state.update(90.0, 110.0, 90.0, 1000.0);
        assert!((f.close_location - 0.0).abs() < 1e-10);

        // Close at midpoint
        let f = state.update(100.0, 110.0, 90.0, 1000.0);
        assert!((f.close_location - 0.5).abs() < 1e-10);
    }

    #[test]
    fn zero_range_bar_close_location() {
        let mut state = FeatureState::new();
        let f = state.update(100.0, 100.0, 100.0, 1000.0);
        assert!(
            (f.close_location - 0.5).abs() < 1e-10,
            "zero range bar should default to 0.5"
        );
    }

    #[test]
    fn sma_matches_manual_calculation() {
        let mut state = FeatureState::new();
        let prices = [100.0, 102.0, 104.0, 103.0, 101.0];
        let mut f = FeatureValues::default();
        for &p in &prices {
            f = state.update(p, p + 1.0, p - 1.0, 1000.0);
        }
        // SMA of 5 values = (100+102+104+103+101)/5 = 102.0
        // But our SMA window is 32 (not 5), so it won't be full yet.
        // With 5 values in a 32-window, SMA = sum/5 = 102.0
        assert!((f.sma_20 - 102.0).abs() < 1e-10);
    }

    // --- EMA tests ---

    #[test]
    fn ema_first_value_equals_input() {
        let mut ema = Ema::new(10);
        let v = ema.push(100.0);
        assert_eq!(v, 100.0);
    }

    #[test]
    fn ema_converges_to_constant() {
        let mut ema = Ema::new(10);
        for _ in 0..100 {
            ema.push(50.0);
        }
        assert!((ema.value() - 50.0).abs() < 1e-10);
    }

    #[test]
    fn ema_weights_recent_more() {
        let mut ema = Ema::new(10);
        for _ in 0..20 {
            ema.push(100.0);
        }
        ema.push(110.0);
        assert!(ema.value() > 100.0, "EMA should move toward spike");
        assert!(ema.value() < 110.0, "EMA should not reach spike in one bar");
    }

    #[test]
    fn ema_is_ready_after_period() {
        let mut ema = Ema::new(10);
        for i in 0..10 {
            ema.push(i as f64);
            if i < 9 {
                assert!(!ema.is_ready());
            }
        }
        assert!(ema.is_ready());
    }

    #[test]
    fn ema_alpha_calculation() {
        let ema = Ema::new(10);
        // α = 2 / (10 + 1) ≈ 0.1818
        assert!((ema.alpha - 2.0 / 11.0).abs() < 1e-10);
    }

    // --- ADX tests ---

    #[test]
    fn adx_returns_zero_on_first_bar() {
        let mut adx = Adx::new(14);
        let (val, pdi, mdi) = adx.update(100.0, 98.0, 99.0);
        assert_eq!(val, 0.0);
        assert_eq!(pdi, 0.0);
        assert_eq!(mdi, 0.0);
    }

    #[test]
    fn adx_rises_in_strong_uptrend() {
        let mut adx = Adx::new(14);
        for i in 0..50 {
            let base = 100.0 + i as f64 * 2.0;
            adx.update(base + 1.0, base - 1.0, base);
        }
        let (val, pdi, mdi) = adx.update(201.0, 199.0, 200.0);
        assert!(val > 20.0, "ADX should be high in uptrend, got {val}");
        assert!(pdi > mdi, "+DI should exceed -DI in uptrend");
    }

    #[test]
    fn adx_low_in_ranging_market() {
        let mut adx = Adx::new(14);
        for i in 0..50 {
            let offset = if i % 2 == 0 { 1.0 } else { -1.0 };
            adx.update(100.0 + offset, 99.0 + offset, 99.5 + offset);
        }
        let (val, _, _) = adx.update(100.0, 99.0, 99.5);
        assert!(val < 25.0, "ADX should be low in ranging market, got {val}");
    }

    #[test]
    fn adx_is_ready_check() {
        let mut adx = Adx::new(14);
        for i in 0..27 {
            adx.update(100.0 + i as f64, 99.0 + i as f64, 99.5 + i as f64);
        }
        assert!(!adx.is_ready(), "should not be ready at 27 bars");
        adx.update(130.0, 128.0, 129.0);
        assert!(adx.is_ready(), "should be ready at 28 bars (2×14)");
    }

    // --- Bollinger Band tests ---

    #[test]
    fn bollinger_bands_computed() {
        let mut state = FeatureState::new();
        // Feed enough bars to warm up
        for _ in 0..50 {
            state.update(100.0, 101.0, 99.0, 1000.0);
        }
        let f = state.update(100.0, 101.0, 99.0, 1000.0);
        // With constant prices, std ≈ 0, so bands should be tight around SMA
        assert!(f.bollinger_upper >= f.sma_20);
        assert!(f.bollinger_lower <= f.sma_20);
        // %B should be near 0.5 for a constant price at the center
        assert!((f.bollinger_pct_b - 0.5).abs() < 0.2,
            "expected %B near 0.5 for constant price, got {}", f.bollinger_pct_b);
    }

    #[test]
    fn bollinger_pct_b_above_one_for_breakout() {
        let mut state = FeatureState::new();
        // Build a stable range
        for _ in 0..50 {
            state.update(100.0, 100.5, 99.5, 1000.0);
        }
        // Spike above the bands
        let f = state.update(115.0, 116.0, 114.0, 2000.0);
        assert!(f.bollinger_pct_b > 1.0,
            "expected %B > 1.0 for breakout, got {}", f.bollinger_pct_b);
    }

    // --- EMA crossover in FeatureState ---

    #[test]
    fn ema_fast_above_slow_detected_in_uptrend() {
        let mut state = FeatureState::new();
        // Start low, then trend up strongly
        for i in 0..60 {
            let price = 100.0 + i as f64 * 0.5;
            state.update(price, price + 0.5, price - 0.5, 1000.0);
        }
        let f = state.update(130.0, 130.5, 129.5, 1000.0);
        assert!(f.ema_fast > f.ema_slow, "fast EMA should be above slow in uptrend");
        assert!(f.ema_fast_above_slow, "crossover should be true in uptrend");
    }

    #[test]
    fn adx_available_in_feature_state() {
        let mut state = FeatureState::new();
        // Strong uptrend for ADX to register
        for i in 0..60 {
            let base = 100.0 + i as f64 * 2.0;
            state.update(base, base + 1.0, base - 1.0, 1000.0);
        }
        let f = state.update(220.0, 221.0, 219.0, 1000.0);
        assert!(f.adx > 0.0, "ADX should be positive after warmup, got {}", f.adx);
    }
}
