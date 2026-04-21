//! Data schemas for basket candidates and validation results.

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use crate::bertram::BertramResult;
use crate::ou::OuFit;

/// A basket trading candidate from the universe.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BasketCandidate {
    /// Target symbol (the one we trade).
    pub target: String,
    /// Peer basket members (symbols).
    pub members: Vec<String>,
    /// Sector classification.
    pub sector: String,
    /// Date the fit was computed.
    pub fit_date: NaiveDate,
}

impl BasketCandidate {
    /// Canonical ID: "{sector}:{target}".
    pub fn id(&self) -> String {
        format!("{}:{}", self.sector, self.target)
    }
}

/// Result of validating a basket candidate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BasketFit {
    /// The candidate that was validated.
    pub candidate: BasketCandidate,
    /// OU fit result (None if fit failed).
    pub ou: Option<OuFit>,
    /// Bertram threshold result (None if computation failed).
    pub bertram: Option<BertramResult>,
    /// Final threshold k (after clipping).
    pub threshold_k: f64,
    /// Whether this basket passed validation.
    pub valid: bool,
    /// Rejection reason if invalid.
    pub reject_reason: Option<String>,
}

impl BasketFit {
    /// Create a rejected fit with a reason.
    pub fn rejected(candidate: BasketCandidate, reason: impl Into<String>) -> Self {
        Self {
            candidate,
            ou: None,
            bertram: None,
            threshold_k: 0.0,
            valid: false,
            reject_reason: Some(reason.into()),
        }
    }
}
