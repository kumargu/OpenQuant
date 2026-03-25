//! OpenQuant core engine — all math and logic lives here.
//!
//! This crate has zero Python dependency. It is fully testable in pure Rust.
//! Python interacts with it only through the thin PyO3 bridge in `openquant` crate.
//!
//! Module layout:
//! - `market_data` — canonical bar/tick types
//! - `features`    — incremental feature computation (rolling stats, indicators)
//! - `signals/`    — strategies as independent modules (mean_reversion, etc.)
//! - `risk`        — position sizing, kill switch, cost filter
//! - `portfolio`   — position tracking, P&L accounting
//! - `engine`      — ties it all together: feed bar → get order intents
//! - `backtest`    — replay historical bars through engine, compute stats

pub mod backtest;
pub mod capital_metrics;
pub mod config;
pub mod engine;
pub mod exit;
pub mod features;
pub mod hot_metrics;
pub mod market_data;
pub mod pairs;
pub mod portfolio;
pub mod risk;
pub mod signals;
