//! Session-close trigger abstraction for the basket live loop.
//!
//! [`IntervalSessionTrigger`] polls the clock on a 30s interval and yields
//! the trading date once it crosses session close + grace. A
//! `BarDrivenSessionTrigger` (#294c-2) will fire when an emitted bar's
//! timestamp crosses the same boundary, so replay drives session-close
//! cadence off bar timestamps instead of wall-clock ticks.

use chrono::NaiveDate;

use crate::clock::Clock;
use crate::market_session;

/// Abstraction over "when should we run the session-close cycle, and for
/// which trading date."
///
/// Implementations are responsible for dedup — each trading date must be
/// yielded at most once. The consumer in [`crate::basket_live`] still
/// maintains its own `processed_sessions` set against persisted state, so
/// duplicate yields are filtered there too, but the trigger is the
/// primary scheduler.
///
/// Returns `None` when the trigger source has been exhausted (replay
/// finished walking its parquet bars). The live `IntervalSessionTrigger`
/// never returns `None`. Returning `None` is the replay loop's exit
/// signal.
pub trait SessionTrigger: Send + Sync {
    /// Block until the next session-close event. Returns the trading
    /// date that just closed, or `None` if no further sessions will
    /// arrive (replay exhausted).
    async fn next(&mut self) -> Option<NaiveDate>;
}

/// Production trigger — polls the clock on a 30s wall-clock interval.
///
/// Fires when the clock is past session close + grace for a date that
/// hasn't been yielded yet. Used by live / paper.
pub struct IntervalSessionTrigger<C: Clock> {
    clock: C,
    interval: tokio::time::Interval,
    grace_minutes: u32,
    last_yielded: Option<NaiveDate>,
}

impl<C: Clock> IntervalSessionTrigger<C> {
    pub fn new(clock: C, grace_minutes: u32) -> Self {
        Self {
            clock,
            interval: tokio::time::interval(std::time::Duration::from_secs(30)),
            grace_minutes,
            last_yielded: None,
        }
    }
}

impl<C: Clock> SessionTrigger for IntervalSessionTrigger<C> {
    async fn next(&mut self) -> Option<NaiveDate> {
        loop {
            self.interval.tick().await;
            let now = self.clock.now();
            let today = market_session::trading_day_utc(now);
            if market_session::is_after_close_grace_utc(now, self.grace_minutes)
                && self.last_yielded != Some(today)
            {
                self.last_yielded = Some(today);
                return Some(today);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, TimeZone, Utc};
    use std::sync::{Arc, Mutex};

    /// Test clock whose returned time is settable from outside.
    struct FakeClock {
        now: Arc<Mutex<DateTime<Utc>>>,
    }

    impl FakeClock {
        fn new(initial: DateTime<Utc>) -> (Self, Arc<Mutex<DateTime<Utc>>>) {
            let cell = Arc::new(Mutex::new(initial));
            (FakeClock { now: cell.clone() }, cell)
        }
    }

    impl Clock for FakeClock {
        fn now(&self) -> DateTime<Utc> {
            *self.now.lock().unwrap()
        }
    }

    /// `IntervalSessionTrigger` should fire each trading date at most once.
    /// Two `.next()` calls inside the same session must not both resolve.
    #[tokio::test(start_paused = true)]
    async fn dedupes_same_date() {
        // 21:00 UTC on a Friday — past 20:00 close + 2min grace, in RTH window.
        let (clock, cell) = FakeClock::new(Utc.with_ymd_and_hms(2026, 4, 24, 21, 0, 0).unwrap());
        let mut trig = IntervalSessionTrigger::new(clock, 2);

        let first = trig.next().await;
        assert_eq!(
            first,
            Some(chrono::NaiveDate::from_ymd_opt(2026, 4, 24).unwrap())
        );

        // Same wall time → second call must NOT immediately resolve.
        // Use timeout to assert it's pending.
        let result = tokio::time::timeout(std::time::Duration::from_secs(120), trig.next()).await;
        assert!(
            result.is_err(),
            "second .next() should not resolve while still on the same date"
        );

        // Advance to the next trading day past close+grace → should fire.
        *cell.lock().unwrap() = Utc.with_ymd_and_hms(2026, 4, 27, 21, 0, 0).unwrap();
        let second = trig.next().await;
        assert_eq!(
            second,
            Some(chrono::NaiveDate::from_ymd_opt(2026, 4, 27).unwrap())
        );
    }
}
