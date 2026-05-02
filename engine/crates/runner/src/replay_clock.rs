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
///
/// `done_tx` is the back-channel the consumer signals on after it
/// finishes `process_session_close` + `record_eod` for the just-
/// returned date. The bar source's emit loop blocks on the
/// corresponding receiver after each `session_tx.send`, so
/// `SharedCloses` cannot be overwritten with the next day's bars
/// until the consumer's fills and EOD valuation are done. This
/// closes the race that produced ~$15 cash drift across replay
/// runs (#321 investigation).
pub struct BarDrivenSessionTrigger {
    rx: mpsc::Receiver<NaiveDate>,
    done_tx: mpsc::Sender<()>,
}

impl SessionTrigger for BarDrivenSessionTrigger {
    async fn next(&mut self) -> Option<NaiveDate> {
        self.rx.recv().await
    }

    async fn ack_session_processed(&mut self) {
        // Tell the bar emitter "session-close fully done — you may
        // resume advancing SharedCloses." Channel cap=1 so the send
        // is a strict handshake; the emitter is awaiting the recv
        // and a second send would only happen after the next
        // session_tx.send.
        let _ = self.done_tx.send(()).await;
    }
}

/// Construction helpers used by `ParquetBarSource`. Not part of the
/// public replay API — `main.rs` calls
/// [`crate::parquet_bar_source::new_replay_components`] which wires
/// everything together.
pub(crate) struct ReplayChannels {
    pub clock_tx: watch::Sender<DateTime<Utc>>,
    pub session_tx: mpsc::Sender<NaiveDate>,
    /// Receiver the bar emitter awaits on after each `session_tx.send`.
    /// The consumer (basket_live) signals via
    /// `BarDrivenSessionTrigger::ack_session_processed` once
    /// `process_session_close` + `record_eod` are complete. Until then
    /// the emitter MUST NOT advance — every advance writes the next
    /// day's prices into the broker-shared `SharedCloses`, which would
    /// race with the consumer's fills.
    pub done_rx: mpsc::Receiver<()>,
}

pub(crate) fn make_replay_clock_and_trigger(
    initial_time: DateTime<Utc>,
) -> (BarDrivenClock, BarDrivenSessionTrigger, ReplayChannels) {
    let (clock_tx, clock_rx) = watch::channel(initial_time);
    let (session_tx, session_rx) = mpsc::channel(8);
    // cap=1: each session-close is a strict handshake. The emitter
    // sends `session_tx.send(date)`, then awaits `done_rx.recv()`.
    // The consumer pulls `session_rx`, processes the close, then
    // sends to `done_tx`. A larger buffer would allow the emitter
    // to race ahead which is exactly what we're trying to prevent.
    let (done_tx, done_rx) = mpsc::channel::<()>(1);
    (
        BarDrivenClock { rx: clock_rx },
        BarDrivenSessionTrigger {
            rx: session_rx,
            done_tx,
        },
        ReplayChannels {
            clock_tx,
            session_tx,
            done_rx,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    /// `BarDrivenSessionTrigger::ack_session_processed` must end up on
    /// `ReplayChannels::done_rx`. This is the back-channel that gates
    /// the bar emitter from advancing past a session-close until the
    /// consumer's fills + EOD valuation are done. If the wiring breaks,
    /// emit_loop blocks forever on the missing ack and the race
    /// (#321) silently comes back.
    #[tokio::test]
    async fn ack_drives_done_channel() {
        let initial = Utc.with_ymd_and_hms(2026, 1, 2, 14, 30, 0).unwrap();
        let (_clock, mut trigger, mut channels) = make_replay_clock_and_trigger(initial);

        // Pre-ack: nothing is on the done channel.
        assert!(
            channels.done_rx.try_recv().is_err(),
            "done channel should be empty before first ack"
        );

        // Consumer signals done → emitter should observe it.
        trigger.ack_session_processed().await;
        let received = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            channels.done_rx.recv(),
        )
        .await
        .expect("ack should arrive within the test timeout");
        assert_eq!(received, Some(()), "exactly one ack per call");

        // After consume, channel is empty again.
        assert!(
            channels.done_rx.try_recv().is_err(),
            "done channel should be drained after the receiver pulled it"
        );
    }

    /// If the consumer (`ReplayChannels::done_rx`) is dropped, the
    /// trigger's `ack_session_processed` swallows the send error
    /// rather than panicking. emit_loop's `done_rx.recv()` will
    /// return `None` instead, which is the documented "consumer gone,
    /// unwind" path.
    #[tokio::test]
    async fn ack_does_not_panic_when_emitter_gone() {
        let initial = Utc.with_ymd_and_hms(2026, 1, 2, 14, 30, 0).unwrap();
        let (_clock, mut trigger, channels) = make_replay_clock_and_trigger(initial);
        drop(channels); // simulate emitter task exiting / panicking
        // Should not panic.
        trigger.ack_session_processed().await;
    }
}
