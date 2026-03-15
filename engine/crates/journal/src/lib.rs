//! Trade journal with async SQLite persistence.
//!
//! This crate provides the data-path infrastructure for OpenQuant:
//! - SQLite schema for logging every bar, feature, signal, and fill
//! - Async writer that receives records via channel (never blocks hot path)
//! - Dual-runtime architecture separating trading from data concerns
//!
//! The trading hot path (synchronous, zero-alloc) sends records through
//! an mpsc channel to the journal writer running on a Tokio data runtime.

pub mod runtime;
pub mod schema;
pub mod writer;

pub use runtime::DataRuntime;
pub use writer::{BarRecord, FillRecord, JournalHandle, JournalMessage};
