//! Clock abstraction for the basket live loop.
//!
//! [`SystemClock`] returns wall time and is used in production. A
//! `BarDrivenClock` (#294c-2) will return time derived from the latest
//! emitted bar's timestamp, so replay can advance simulated time as fast
//! as the parquet bar source emits.

use chrono::{DateTime, Utc};

/// Abstraction over "what time is it".
///
/// Live and paper use [`SystemClock`]. Replay (#294c-2) will use a
/// bar-timestamp-driven clock so heartbeats, catch-up logic, and
/// past-close detection all see the simulated wall time instead of the
/// host's wall time.
pub trait Clock: Send + Sync {
    fn now(&self) -> DateTime<Utc>;
}

/// Production clock — returns `Utc::now()`.
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}
