//! OpenQuant Metrics — JSONL recorder for the `metrics` facade.
//!
//! Uses the [`metrics`](https://docs.rs/metrics) crate's standard macros
//! (`counter!`, `gauge!`, `histogram!`) with a custom recorder that
//! periodically flushes aggregated metrics to a JSONL file.
//!
//! When no recorder is installed (disabled), all metric macros are noop
//! (~1ns atomic load).
//!
//! # Usage
//!
//! ```rust,ignore
//! // Install once at startup:
//! openquant_metrics::install("data/metrics", std::time::Duration::from_secs(10))?;
//!
//! // Then anywhere in the codebase:
//! use metrics::{counter, histogram};
//! counter!("engine.bars_processed", "symbol" => "BTCUSD").increment(1);
//! histogram!("engine.on_bar.duration_ns", "symbol" => "BTCUSD").record(63.0);
//! ```

mod recorder;
mod sink;

pub use recorder::install;
pub use sink::MetricsSink;

/// Shut down the metrics system, flushing final snapshot.
/// Call this before process exit.
pub async fn shutdown() {
    recorder::shutdown().await;
}
