//! Pair Picker — statistical validation library for pairs trading candidates.
//!
//! Used by the runner's `pair_picker_service` to validate candidate pairs
//! from quant-lab against structural quality gates (ADF cointegration,
//! half-life, R², beta stability, ETF exclusion). Lab discovers pairs;
//! this crate validates them. No standalone binary — all invocation goes
//! through the runner.

pub mod etf_filter;
pub mod pipeline;
pub mod scorer;
pub mod stats;
#[cfg(test)]
pub mod test_utils;
pub mod types;
