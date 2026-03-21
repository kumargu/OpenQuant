//! Pair Picker — statistical validation for pairs trading candidates.
//!
//! Standalone binary that reads candidate pairs, runs statistical validation
//! (cointegration, half-life, beta stability, ETF exclusion), and writes
//! `active_pairs.json` for the trading engine.
//!
//! ## Architecture
//!
//! - Runs as a separate binary, not linked into the trading engine
//! - Intended to run daily (lock file ensures once-per-day)
//! - All math in Rust (zero Python dependencies)
//! - Pluggable price provider (in-memory for tests, API for production)
//!
//! ## Usage
//!
//! ```text
//! pair-picker --candidates data/pair_candidates.json --output data/active_pairs.json
//! pair-picker --check  # exit 0 if already run today, 1 if not
//! ```

pub mod etf_filter;
pub mod lockfile;
pub mod pipeline;
pub mod scorer;
pub mod stats;
#[cfg(test)]
pub mod test_utils;
pub mod thompson;
pub mod types;
