//! Statistical tests for pairs trading validation.
//!
//! All math implemented in pure Rust — no external dependencies.
//!
//! ## Tests
//! - **OLS**: Ordinary least squares regression for hedge ratio estimation
//! - **ADF**: Augmented Dickey-Fuller unit root test (Engle-Granger cointegration)
//! - **Half-life**: Ornstein-Uhlenbeck mean-reversion speed estimation
//! - **Beta stability**: Rolling-window CV + CUSUM structural break detection

pub mod adf;
pub mod beta_stability;
pub mod halflife;
pub mod ols;
