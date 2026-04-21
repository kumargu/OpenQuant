//! Validation library for basket trading candidates.
//!
//! Fits OU/AR(1) models on log-spread series and computes Bertram (2010)
//! optimal symmetric thresholds. No dependency on pair-picker to avoid
//! HL contamination through the dep tree.

mod bertram;
mod ou;
mod schema;
mod spread;
mod universe;
mod validator;

pub use bertram::{optimize_symmetric_thresholds, BertramResult};
pub use ou::{fit_ou_ar1, OuFit};
pub use schema::{BasketCandidate, BasketFit};
pub use spread::build_spread;
pub use universe::{load_universe, SectorConfig, StrategyConfig, Universe, VersionInfo};
pub use validator::{validate, ValidatorConfig};

#[cfg(test)]
mod test_utils;
