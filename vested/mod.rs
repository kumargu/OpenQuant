//! Vested adapter for OpenQuant basket targets.
//!
//! This module exists because Vested is not a normal broker integration for
//! this codebase. OpenQuant's basket engine is designed around a long/short
//! portfolio, broker-side reconciliation, and Alpaca-style programmatic order
//! placement. Vested is useful for the India-to-US investment path, but it is a
//! long-only cash account workflow and does not currently give us the same API
//! surface. Treating Vested as just another `Broker` would hide that mismatch
//! and make the basket engine look safer than it is.
//!
//! The design choice here is to keep basket construction unchanged and add a
//! narrow adapter after basket targets are planned:
//!
//! 1. Core basket code still produces its normal signed target notionals.
//! 2. The Vested adapter projects those targets into a cash-account,
//!    long-only book.
//! 3. The existing share conversion, order diffing, journal, replay, and paper
//!    execution paths continue to run unchanged against the projected targets.
//!
//! This gives us a clean switch:
//!
//! - default basket mode has `vested_mode = None` and no Vested dependency;
//! - `--paper-vested` validates the projected book through Alpaca paper;
//! - `--replay-vested` tests the same projection through the replay path;
//! - `--live-vested` is intentionally noop for Alpaca orders because real Vested
//!   execution must happen through the Vested UI/browser/manual workflow.
//!
//! Projection choices are deliberately few. `DropShorts` is the baseline because
//! it removes impossible short exposure and scales remaining longs to cash.
//! `PeerMirror` and `ShortPenalty` keep short information as research signals,
//! but they are still adapter-level transforms, not changes to basket state.
//! If a leadership overlay has already transformed the book, the adapter falls
//! back to positive-notional projection instead of re-reading selected basket
//! legs and accidentally creating a second, different book.
//!
//! The regime gate is exposure-only. It can scale the Vested book to cash when
//! the recent strategy equity series is weak, but it does not change basket
//! selection or mutate the underlying engine. That keeps "what baskets are
//! active" and "how much Vested can safely express today" as separate decisions.
//!
//! Runtime files are intentionally outside this module under
//! `data/{paper,live,replay}/vested_model`. Those are operational artifacts and
//! should not be committed.

mod vested;

pub use self::vested::*;
