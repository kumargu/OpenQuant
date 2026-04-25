//! In-process simulated broker for replay (#294c-2 / #294c-3).
//!
//! Implements [`crate::broker::Broker`] without touching Alpaca. Holds
//! its own positions / cash / latest-close state. Returns synthetic
//! [`AlpacaOrder`] records shaped like a successful fill so the
//! basket-live code path runs unchanged.
//!
//! Replay fill contract (frozen here so replay vs paper/live stays
//! comparable across PRs):
//!
//!   - Fills happen at the *latest known close* for the symbol — i.e.,
//!     the close from the bar that triggered the session-close cycle.
//!     This matches "MOC executes at the official close" used by paper.
//!   - Optional one-sided slippage in basis points: buys fill higher,
//!     sells fill lower. Default 0 bps.
//!
//! Optional failure-injection modes (#294c-3) — all default to OFF so
//! the happy-path replay behavior in #294c-2 is unchanged:
//!
//!   - `reject_rate`: per-order probability of returning an `Err`
//!     shaped like Alpaca's "buying power exceeded" / "insufficient
//!     liquidity" messages, so the `error!("ORDER FAILED")` branch in
//!     basket_live runs and downstream code sees a partial fill set.
//!   - `partial_fill_rate`: per-order probability of filling 60-90%
//!     of the requested qty. The mismatch between target intent and
//!     actual filled qty is what makes post-submit reconciliation
//!     meaningful.
//!   - `stale_position_rate`: per-`get_positions` call probability of
//!     returning a one-step-old snapshot instead of the current
//!     positions. Triggers BROKER DIVERGENCE on the post-submit
//!     reconciliation path.
//!
//! All randomness is seeded by `SimulatedBrokerConfig::seed`, so a
//! given (seed, sequence-of-calls) yields deterministic outcomes —
//! tests don't depend on wall time.
//!
//! Buying-power enforcement is independent of the failure modes: a
//! `place_order` that would push the broker past `equity * leverage`
//! always returns `Err` shaped like the string Alpaca emits, so
//! `basket_live`'s error-handling branch gets coverage even with all
//! failure rates at 0.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, RwLock};

use basket_engine::PortfolioConfig;
use chrono::NaiveDate;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

use crate::alpaca::{AlpacaAccount, AlpacaOrder, ExecutionMode};
use crate::broker::Broker;

/// Shared close-price snapshot. Written by
/// [`crate::parquet_bar_source::ParquetBarSource`] as bars are emitted,
/// read by [`SimulatedBroker`] when filling orders and computing
/// account equity.
pub type SharedCloses = Arc<RwLock<HashMap<String, f64>>>;

/// All knobs for the simulated broker, including failure-injection
/// rates. Defaults match #294c-2 happy-path behavior.
#[derive(Debug, Clone)]
pub struct SimulatedBrokerConfig {
    pub slippage_bps: f64,
    /// Per-order probability of returning a "buying power exceeded" /
    /// "insufficient liquidity"-shaped error. 0.0 = never reject.
    pub reject_rate: f64,
    /// Per-order probability of partially filling. When a partial fire
    /// fires, the filled qty is uniformly sampled in [0.6, 0.9] of
    /// the requested qty (rounded to whole shares, with a floor of 1).
    pub partial_fill_rate: f64,
    /// Per-`get_positions` probability of returning the previous
    /// snapshot instead of the current state. 0.0 = always return
    /// current. The first call after construction always returns
    /// current (no stale snapshot exists yet).
    pub stale_position_rate: f64,
    /// Deterministic seed for the failure-mode RNG.
    pub seed: u64,
}

impl Default for SimulatedBrokerConfig {
    fn default() -> Self {
        Self {
            slippage_bps: 0.0,
            reject_rate: 0.0,
            partial_fill_rate: 0.0,
            stale_position_rate: 0.0,
            seed: 0,
        }
    }
}

/// In-process broker. Cheaply cloneable — internal state is `Arc`-shared
/// so multiple references see the same positions/cash.
#[derive(Clone)]
pub struct SimulatedBroker {
    state: Arc<SimulatedState>,
}

struct SimulatedState {
    positions: RwLock<HashMap<String, (f64, f64)>>, // sym → (qty, avg_price)
    cash: RwLock<f64>,
    closes: SharedCloses,
    initial_capital: f64,
    leverage: f64,
    config: SimulatedBrokerConfig,
    next_order_id: std::sync::atomic::AtomicU64,
    rng: Mutex<StdRng>,
    /// Snapshot of positions returned by the previous `get_positions`
    /// call. Used by `stale_position_rate` to occasionally serve a
    /// one-step-old view, exercising the BROKER DIVERGENCE path.
    prev_positions: Mutex<Option<HashMap<String, (f64, f64)>>>,
    /// End-of-day equity time series. Populated by `record_eod`,
    /// consumed by the parity wrapper after replay.
    daily_equity: Mutex<BTreeMap<NaiveDate, f64>>,
}

impl SimulatedBroker {
    /// Test-only convenience constructor: no failure injection, just
    /// slippage. Production replay uses [`Self::with_config`] from
    /// `parquet_bar_source::new_replay_components`.
    #[cfg(test)]
    pub fn new(config: &PortfolioConfig, closes: SharedCloses, slippage_bps: f64) -> Self {
        Self::with_config(
            config,
            closes,
            SimulatedBrokerConfig {
                slippage_bps,
                ..Default::default()
            },
        )
    }

    /// Construct a broker with explicit failure-injection config.
    pub fn with_config(
        portfolio: &PortfolioConfig,
        closes: SharedCloses,
        config: SimulatedBrokerConfig,
    ) -> Self {
        let rng = StdRng::seed_from_u64(config.seed);
        Self {
            state: Arc::new(SimulatedState {
                positions: RwLock::new(HashMap::new()),
                cash: RwLock::new(portfolio.capital),
                closes,
                initial_capital: portfolio.capital,
                leverage: portfolio.leverage,
                config,
                next_order_id: std::sync::atomic::AtomicU64::new(1),
                rng: Mutex::new(rng),
                prev_positions: Mutex::new(None),
                daily_equity: Mutex::new(BTreeMap::new()),
            }),
        }
    }

    fn fill_price(&self, symbol: &str, side: &str) -> Result<f64, String> {
        let close = self
            .state
            .closes
            .read()
            .unwrap()
            .get(symbol)
            .copied()
            .ok_or_else(|| format!("no close price for {symbol} (bar source not seeded)"))?;
        if !close.is_finite() || close <= 0.0 {
            return Err(format!("non-finite close price for {symbol}: {close}"));
        }
        let bps = self.state.config.slippage_bps / 1e4;
        let signed_bps = match side {
            "buy" => bps,
            "sell" => -bps,
            other => return Err(format!("unknown side: {other}")),
        };
        Ok(close * (1.0 + signed_bps))
    }

    fn equity_unlocked(&self, positions: &HashMap<String, (f64, f64)>, cash: f64) -> f64 {
        // Sort by symbol before summing — `HashMap` iteration order is
        // randomized per-process in Rust (anti-hash-collision defense),
        // and f64 addition isn't associative, so summing the same set
        // of values in different orders yields different floats.
        // Without this we'd get small (~1%) P&L drift between
        // identical replay runs, breaking reproducibility (#315).
        let closes = self.state.closes.read().unwrap();
        let mut keys: Vec<&String> = positions.keys().collect();
        keys.sort();
        let pos_val: f64 = keys
            .into_iter()
            .filter_map(|sym| {
                let (qty, _) = positions.get(sym)?;
                closes.get(sym.as_str()).map(|p| qty * p)
            })
            .sum();
        cash + pos_val
    }

    fn record_eod_inner(&self, date: NaiveDate) {
        let positions = self.state.positions.read().unwrap();
        let cash = *self.state.cash.read().unwrap();
        let equity = self.equity_unlocked(&positions, cash);
        self.state.daily_equity.lock().unwrap().insert(date, equity);
    }
}

impl Broker for SimulatedBroker {
    async fn place_order(
        &self,
        symbol: &str,
        qty: f64,
        side: &str,
        _execution: ExecutionMode,
    ) -> Result<AlpacaOrder, String> {
        if !qty.is_finite() || qty <= 0.0 {
            return Err(format!("non-positive qty for {symbol}: {qty}"));
        }
        let price = self.fill_price(symbol, side)?;
        let signed_qty_full = match side {
            "buy" => qty,
            "sell" => -qty,
            _ => return Err(format!("unknown side: {side}")),
        };

        // Failure injection — sample BEFORE we acquire the position
        // locks so the RNG draw doesn't depend on lock contention.
        let cfg = &self.state.config;
        let (reject, partial_qty_opt) = {
            let mut rng = self.state.rng.lock().unwrap();
            let reject = cfg.reject_rate > 0.0 && rng.gen::<f64>() < cfg.reject_rate;
            let partial = if !reject
                && cfg.partial_fill_rate > 0.0
                && rng.gen::<f64>() < cfg.partial_fill_rate
            {
                let frac: f64 = rng.gen_range(0.6..0.9);
                Some((qty * frac).floor().max(1.0))
            } else {
                None
            };
            (reject, partial)
        };

        if reject {
            // Match the shape of Alpaca's rejection messages so
            // basket_live's error logging looks identical to live.
            return Err(format!(
                "buying power exceeded: simulated rejection for {symbol} qty {qty}"
            ));
        }

        // Effective filled qty (may be smaller than requested).
        let filled_qty = partial_qty_opt.unwrap_or(qty);
        let signed_filled_qty = signed_qty_full.signum() * filled_qty;

        let mut positions = self.state.positions.write().unwrap();
        let mut cash = self.state.cash.write().unwrap();

        // Buying-power check (per-symbol projection from the closes
        // map; see #302 review). Uses the FULL requested qty (not
        // partial) since Alpaca rejects on submit, before any partial
        // fill happens.
        let closes_snapshot = self.state.closes.read().unwrap();
        let current_equity = self.equity_unlocked(&positions, *cash);
        let buying_power = current_equity * self.state.leverage;
        let prev_qty_signed = positions.get(symbol).map(|(q, _)| *q).unwrap_or(0.0);
        let new_qty_signed_full = prev_qty_signed + signed_qty_full;
        let mut projected_gross: f64 = 0.0;
        let mut traded_symbol_seen = false;
        // Sort by symbol so the f64 sum is deterministic across runs
        // — see equity_unlocked for the same reason (#315).
        let mut sorted_keys: Vec<&String> = positions.keys().collect();
        sorted_keys.sort();
        for sym in sorted_keys {
            let (q, _) = positions.get(sym).expect("key from iterator");
            if sym == symbol {
                projected_gross += new_qty_signed_full.abs() * price;
                traded_symbol_seen = true;
            } else if let Some(p) = closes_snapshot.get(sym) {
                projected_gross += q.abs() * p;
            } else {
                let avg = positions.get(sym).map(|(_, a)| *a).unwrap_or(0.0);
                projected_gross += q.abs() * avg;
            }
        }
        if !traded_symbol_seen {
            projected_gross += new_qty_signed_full.abs() * price;
        }
        drop(closes_snapshot);
        if projected_gross > buying_power * 1.01 {
            return Err(format!(
                "buying power exceeded: gross {projected_gross:.2} > buying_power {buying_power:.2}"
            ));
        }

        // Apply the fill (partial or full).
        let entry = positions.entry(symbol.to_string()).or_insert((0.0, price));
        let prev_qty = entry.0;
        let prev_avg = entry.1;
        let new_qty = prev_qty + signed_filled_qty;
        let new_avg = if new_qty.abs() > 1e-9 && prev_qty.signum() == signed_filled_qty.signum() {
            (prev_avg * prev_qty.abs() + price * filled_qty) / new_qty.abs()
        } else {
            price
        };
        entry.0 = new_qty;
        entry.1 = new_avg;
        if entry.0.abs() < 1e-9 {
            positions.remove(symbol);
        }
        *cash -= signed_filled_qty * price;

        let id = self
            .state
            .next_order_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let status = if partial_qty_opt.is_some() {
            "partially_filled"
        } else {
            "filled"
        };
        Ok(AlpacaOrder {
            id: format!("sim-{id:08}"),
            status: status.to_string(),
            symbol: symbol.to_string(),
            side: side.to_string(),
            qty: format!("{filled_qty}"),
        })
    }

    async fn get_positions(
        &self,
        _execution: ExecutionMode,
    ) -> Result<HashMap<String, (f64, f64)>, String> {
        let current = self.state.positions.read().unwrap().clone();
        // Stale-position injection: with probability `stale_position_rate`,
        // return the previous snapshot instead of the current one.
        // The first call always returns current (no prior snapshot).
        let cfg = &self.state.config;
        let prev = self.state.prev_positions.lock().unwrap().clone();
        let serve_stale = cfg.stale_position_rate > 0.0
            && prev.is_some()
            && self.state.rng.lock().unwrap().gen::<f64>() < cfg.stale_position_rate;
        let served = if serve_stale {
            prev.unwrap()
        } else {
            current.clone()
        };
        // Update the prev-snapshot with what we ACTUALLY observed
        // ("served" → caller's view of the world). Using `current`
        // here would let stale repeats see the latest state via the
        // next call, which is too forgiving.
        *self.state.prev_positions.lock().unwrap() = Some(served.clone());
        Ok(served)
    }

    async fn get_account(&self, _execution: ExecutionMode) -> Result<AlpacaAccount, String> {
        let positions = self.state.positions.read().unwrap();
        let cash = *self.state.cash.read().unwrap();
        let equity = self.equity_unlocked(&positions, cash);
        let buying_power = equity * self.state.leverage;
        Ok(AlpacaAccount {
            status: "ACTIVE".to_string(),
            buying_power: format!("{buying_power:.2}"),
            equity: format!("{equity:.2}"),
            trading_blocked: false,
            account_blocked: false,
        })
    }

    async fn record_eod(&self, date: NaiveDate) {
        self.record_eod_inner(date);
    }
}

impl SimulatedBroker {
    /// Snapshot for end-of-replay reporting. Kept separate from the
    /// trait so `basket_live` doesn't grow a replay-specific call.
    pub fn final_snapshot(&self) -> ReplaySnapshot {
        let positions = self.state.positions.read().unwrap().clone();
        let cash = *self.state.cash.read().unwrap();
        let equity = self.equity_unlocked(&positions, cash);
        ReplaySnapshot {
            initial_capital: self.state.initial_capital,
            cash,
            equity,
            positions,
        }
    }

    /// Daily-equity time series collected via [`Self::record_eod`].
    /// The parity wrapper reads this to compute portfolio stats
    /// (Sharpe, drawdown, etc.) for the TSV / baseline comparison.
    pub fn daily_equity(&self) -> Vec<(NaiveDate, f64)> {
        self.state
            .daily_equity
            .lock()
            .unwrap()
            .iter()
            .map(|(d, e)| (*d, *e))
            .collect()
    }
}

#[derive(Debug, Clone)]
pub struct ReplaySnapshot {
    pub initial_capital: f64,
    pub cash: f64,
    pub equity: f64,
    pub positions: HashMap<String, (f64, f64)>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shared_closes(pairs: &[(&str, f64)]) -> SharedCloses {
        Arc::new(RwLock::new(
            pairs.iter().map(|(s, p)| (s.to_string(), *p)).collect(),
        ))
    }

    fn portfolio_config() -> PortfolioConfig {
        PortfolioConfig {
            capital: 10_000.0,
            leverage: 4.0,
            n_active_baskets: 5,
            stop_loss_z: None,
        }
    }

    #[tokio::test]
    async fn fills_at_close_with_zero_slippage() {
        let closes = shared_closes(&[("AMD", 100.0)]);
        let broker = SimulatedBroker::new(&portfolio_config(), closes, 0.0);
        let order = broker
            .place_order("AMD", 10.0, "buy", ExecutionMode::Paper)
            .await
            .unwrap();
        assert_eq!(order.status, "filled");
        let positions = broker.get_positions(ExecutionMode::Paper).await.unwrap();
        assert_eq!(
            positions.get("AMD").map(|(q, p)| (*q, *p)),
            Some((10.0, 100.0))
        );
        let snap = broker.final_snapshot();
        assert!((snap.cash - (10_000.0 - 1000.0)).abs() < 1e-6);
        assert!((snap.equity - 10_000.0).abs() < 1e-6);
    }

    #[tokio::test]
    async fn applies_slippage_one_sided() {
        let closes = shared_closes(&[("AMD", 100.0)]);
        let broker = SimulatedBroker::new(&portfolio_config(), closes, 10.0);
        let _ = broker
            .place_order("AMD", 1.0, "buy", ExecutionMode::Paper)
            .await
            .unwrap();
        let snap = broker.final_snapshot();
        assert!((snap.cash - (10_000.0 - 100.10)).abs() < 1e-6);
    }

    #[tokio::test]
    async fn rejects_when_buying_power_exceeded() {
        let closes = shared_closes(&[("AMD", 100.0)]);
        let broker = SimulatedBroker::new(&portfolio_config(), closes, 0.0);
        let err = broker
            .place_order("AMD", 1000.0, "buy", ExecutionMode::Paper)
            .await
            .unwrap_err();
        assert!(err.contains("buying power exceeded"), "got: {err}");
    }

    #[tokio::test]
    async fn buying_power_uses_each_symbols_own_close() {
        let closes = shared_closes(&[("EXPENSIVE", 1000.0), ("CHEAP", 10.0)]);
        let broker = SimulatedBroker::new(&portfolio_config(), closes, 0.0);
        broker
            .place_order("EXPENSIVE", 30.0, "buy", ExecutionMode::Paper)
            .await
            .unwrap();
        broker
            .place_order("CHEAP", 20.0, "buy", ExecutionMode::Paper)
            .await
            .unwrap();
        let err = broker
            .place_order("CHEAP", 2000.0, "buy", ExecutionMode::Paper)
            .await
            .unwrap_err();
        assert!(err.contains("buying power exceeded"), "got: {err}");
    }

    #[tokio::test]
    async fn closing_a_position_does_not_increase_projected_gross() {
        let closes = shared_closes(&[("AMD", 100.0)]);
        let broker = SimulatedBroker::new(&portfolio_config(), closes, 0.0);
        broker
            .place_order("AMD", 100.0, "buy", ExecutionMode::Paper)
            .await
            .unwrap();
        broker
            .place_order("AMD", 50.0, "sell", ExecutionMode::Paper)
            .await
            .unwrap();
        let positions = broker.get_positions(ExecutionMode::Paper).await.unwrap();
        assert_eq!(positions.get("AMD").map(|(q, _)| *q), Some(50.0));
    }

    #[tokio::test]
    async fn errors_on_missing_close() {
        let closes = shared_closes(&[]);
        let broker = SimulatedBroker::new(&portfolio_config(), closes, 0.0);
        let err = broker
            .place_order("AMD", 10.0, "buy", ExecutionMode::Paper)
            .await
            .unwrap_err();
        assert!(err.contains("no close price"), "got: {err}");
    }

    #[tokio::test]
    async fn closes_position_resets_cost_basis() {
        let closes = shared_closes(&[("AMD", 100.0)]);
        let broker = SimulatedBroker::new(&portfolio_config(), closes.clone(), 0.0);
        broker
            .place_order("AMD", 10.0, "buy", ExecutionMode::Paper)
            .await
            .unwrap();
        closes.write().unwrap().insert("AMD".to_string(), 110.0);
        broker
            .place_order("AMD", 10.0, "sell", ExecutionMode::Paper)
            .await
            .unwrap();
        let snap = broker.final_snapshot();
        assert!((snap.cash - (10_000.0 + 100.0)).abs() < 1e-6);
        assert!(snap.positions.is_empty());
    }

    // ── failure injection (#294c-3) ───────────────────────────────

    #[tokio::test]
    async fn inject_rejects_at_configured_rate() {
        let closes = shared_closes(&[("AMD", 100.0)]);
        let broker = SimulatedBroker::with_config(
            &portfolio_config(),
            closes,
            SimulatedBrokerConfig {
                reject_rate: 1.0, // always reject
                seed: 42,
                ..Default::default()
            },
        );
        let err = broker
            .place_order("AMD", 1.0, "buy", ExecutionMode::Paper)
            .await
            .unwrap_err();
        assert!(
            err.contains("buying power exceeded"),
            "rejection should be shaped like Alpaca's, got: {err}"
        );
        let positions = broker.get_positions(ExecutionMode::Paper).await.unwrap();
        assert!(positions.is_empty(), "rejected order must not fill");
    }

    #[tokio::test]
    async fn inject_partial_fills_use_60_to_90_pct() {
        let closes = shared_closes(&[("AMD", 100.0)]);
        let broker = SimulatedBroker::with_config(
            &portfolio_config(),
            closes,
            SimulatedBrokerConfig {
                partial_fill_rate: 1.0, // always partial
                seed: 7,
                ..Default::default()
            },
        );
        let order = broker
            .place_order("AMD", 10.0, "buy", ExecutionMode::Paper)
            .await
            .unwrap();
        assert_eq!(order.status, "partially_filled");
        let filled: f64 = order.qty.parse().unwrap();
        assert!(
            (1.0..=9.0).contains(&filled),
            "partial fill {filled} not in expected band [floor(0.6*10)=6, floor(0.9*10)=9]; \
             actually accept anything ≥1 and < requested 10"
        );
        // Position reflects partial qty.
        let positions = broker.get_positions(ExecutionMode::Paper).await.unwrap();
        assert_eq!(positions.get("AMD").map(|(q, _)| *q), Some(filled));
    }

    #[tokio::test]
    async fn inject_stale_positions_returns_old_snapshot() {
        let closes = shared_closes(&[("AMD", 100.0)]);
        let broker = SimulatedBroker::with_config(
            &portfolio_config(),
            closes,
            SimulatedBrokerConfig {
                stale_position_rate: 1.0, // always stale (after the priming call)
                seed: 1,
                ..Default::default()
            },
        );
        // Prime: first call returns current (no prior snapshot exists).
        let snap1 = broker.get_positions(ExecutionMode::Paper).await.unwrap();
        assert!(snap1.is_empty());
        // Place an order; the position now has 5 shares of AMD.
        broker
            .place_order("AMD", 5.0, "buy", ExecutionMode::Paper)
            .await
            .unwrap();
        // With stale_position_rate=1.0, the second call serves the
        // prior (empty) snapshot, NOT the current 5-share state.
        let snap2 = broker.get_positions(ExecutionMode::Paper).await.unwrap();
        assert!(
            snap2.is_empty(),
            "stale-position injection should serve the prior empty snapshot, got {snap2:?}"
        );
    }

    #[tokio::test]
    async fn deterministic_failure_sequences_for_same_seed() {
        // Two brokers with the same seed and same call sequence must
        // produce the same outcomes — important so golden-snapshot
        // tests don't go non-deterministic.
        let make = |seed: u64| {
            let closes = shared_closes(&[("AMD", 100.0)]);
            SimulatedBroker::with_config(
                &portfolio_config(),
                closes,
                SimulatedBrokerConfig {
                    reject_rate: 0.5,
                    seed,
                    ..Default::default()
                },
            )
        };
        let a = make(123);
        let b = make(123);
        let mut rejects_a = 0;
        let mut rejects_b = 0;
        for _ in 0..50 {
            if a.place_order("AMD", 1.0, "buy", ExecutionMode::Paper)
                .await
                .is_err()
            {
                rejects_a += 1;
            }
            if b.place_order("AMD", 1.0, "buy", ExecutionMode::Paper)
                .await
                .is_err()
            {
                rejects_b += 1;
            }
        }
        assert_eq!(rejects_a, rejects_b);
    }

    /// Two brokers with the same starting state and the same call
    /// sequence must produce identical equity numbers — even when the
    /// `HashMap` iteration order would otherwise drift between
    /// process invocations. Regression for #315.
    #[tokio::test]
    async fn equity_is_deterministic_across_runs() {
        let symbols = [
            "AAPL", "AMD", "GOOGL", "INTC", "META", "MSFT", "NVDA", "TSLA",
        ];
        let mut prices: Vec<(&str, f64)> = symbols
            .iter()
            .enumerate()
            .map(|(i, s)| (*s, 100.0 + (i as f64) * 7.3))
            .collect();
        // Run twice with different shuffle orders for the closes input
        // — the broker's own iteration must not depend on insertion order.
        // Run with insertion order A.
        let broker_a = SimulatedBroker::with_config(
            &portfolio_config(),
            shared_closes(&prices),
            SimulatedBrokerConfig::default(),
        );
        for sym in &symbols {
            let _ = broker_a
                .place_order(sym, 1.0, "buy", ExecutionMode::Paper)
                .await
                .unwrap();
        }
        let snap_a = broker_a.final_snapshot();

        // Same calls, reversed insertion order — `HashMap` iteration
        // depends on hash + insertion, so this differs from run A in
        // the buggy implementation.
        prices.reverse();
        let broker_b = SimulatedBroker::with_config(
            &portfolio_config(),
            shared_closes(&prices),
            SimulatedBrokerConfig::default(),
        );
        for sym in &symbols {
            let _ = broker_b
                .place_order(sym, 1.0, "buy", ExecutionMode::Paper)
                .await
                .unwrap();
        }
        let snap_b = broker_b.final_snapshot();
        assert_eq!(
            snap_a.equity.to_bits(),
            snap_b.equity.to_bits(),
            "equity drifted across HashMap orderings: {} vs {}",
            snap_a.equity,
            snap_b.equity
        );
        assert_eq!(snap_a.cash.to_bits(), snap_b.cash.to_bits());
    }

    #[tokio::test]
    async fn record_eod_tracks_daily_equity() {
        use chrono::NaiveDate;
        let closes = shared_closes(&[("AMD", 100.0)]);
        let broker = SimulatedBroker::new(&portfolio_config(), closes.clone(), 0.0);
        broker
            .place_order("AMD", 10.0, "buy", ExecutionMode::Paper)
            .await
            .unwrap();
        broker
            .record_eod(NaiveDate::from_ymd_opt(2024, 7, 1).unwrap())
            .await;
        // Move price.
        closes.write().unwrap().insert("AMD".to_string(), 110.0);
        broker
            .record_eod(NaiveDate::from_ymd_opt(2024, 7, 2).unwrap())
            .await;
        let series = broker.daily_equity();
        assert_eq!(series.len(), 2);
        assert_eq!(series[0].0, NaiveDate::from_ymd_opt(2024, 7, 1).unwrap());
        assert!((series[0].1 - 10_000.0).abs() < 1e-6); // mark at 100 → equity unchanged
        assert_eq!(series[1].0, NaiveDate::from_ymd_opt(2024, 7, 2).unwrap());
        // 10 shares × $110 + ($10000 - $1000) cash = $10100
        assert!((series[1].1 - 10_100.0).abs() < 1e-6);
    }
}
