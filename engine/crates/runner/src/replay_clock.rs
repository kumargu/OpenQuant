//! Replay-side `Clock` and `SessionTrigger` impls.
//!
//! These are constructed together with [`crate::parquet_bar_source::ParquetBarSource`]
//! via [`new_replay_components`]. The bar source's emitter task drives both
//! signals as it walks through parquet bars in chronological order:
//!
//!   - Each emitted bar updates the [`BarDrivenClock`]'s shared timestamp
//!     so `clock.now()` returns the bar-OPEN time of the most recently
//!     emitted bar.
//!   - After all RTH bars for a trading date have been emitted (and
//!     consumed by the basket-live select loop), the source pushes that
//!     date onto the [`BarDrivenSessionTrigger`]'s mpsc channel.
//!
//! The single ordering invariant the rest of the system relies on:
//! every bar belonging to date D arrives on the bar channel before the
//! session-close signal for D arrives on the trigger channel. The
//! `select! { biased; ... }` in `basket_live` plus a small bar buffer
//! enforce this end-to-end.
//!
//! When the parquet source is exhausted, both the clock's watch sender
//! and the trigger's mpsc sender are dropped. The trigger then yields
//! `None`, which is `basket_live`'s replay-exit signal.

use chrono::{DateTime, NaiveDate, Utc};
use tokio::sync::{mpsc, watch};

use crate::clock::Clock;
use crate::session_trigger::SessionTrigger;

/// Clock whose "now" is the bar-OPEN time of the most recently emitted
/// parquet bar.
///
/// Constructed by [`new_replay_components`]. Reads from a `watch` channel
/// the bar source updates after each bar.
pub struct BarDrivenClock {
    rx: watch::Receiver<DateTime<Utc>>,
}

impl Clock for BarDrivenClock {
    fn now(&self) -> DateTime<Utc> {
        *self.rx.borrow()
    }
}

/// Session-close trigger driven by parquet bar emission.
///
/// The bar source pushes `NaiveDate` onto the channel after all RTH
/// bars for that date have been emitted (and consumed by the basket
/// live loop, thanks to `select! { biased; ... }` and the rendezvous-
/// sized bar channel).
///
/// Returns `None` when the channel is closed by the bar source — that
/// is `basket_live`'s exit condition for replay.
pub struct BarDrivenSessionTrigger {
    rx: mpsc::Receiver<NaiveDate>,
}

impl SessionTrigger for BarDrivenSessionTrigger {
    async fn next(&mut self) -> Option<NaiveDate> {
        self.rx.recv().await
    }
}

/// Construction helpers used by `ParquetBarSource`. Not part of the
/// public replay API — `main.rs` calls
/// [`crate::parquet_bar_source::new_replay_components`] which wires
/// everything together.
pub(crate) struct ReplayChannels {
    pub clock_tx: watch::Sender<DateTime<Utc>>,
    pub session_tx: mpsc::Sender<NaiveDate>,
}

pub(crate) fn make_replay_clock_and_trigger(
    initial_time: DateTime<Utc>,
) -> (BarDrivenClock, BarDrivenSessionTrigger, ReplayChannels) {
    let (clock_tx, clock_rx) = watch::channel(initial_time);
    let (session_tx, session_rx) = mpsc::channel(8);
    (
        BarDrivenClock { rx: clock_rx },
        BarDrivenSessionTrigger { rx: session_rx },
        ReplayChannels {
            clock_tx,
            session_tx,
        },
    )
}
