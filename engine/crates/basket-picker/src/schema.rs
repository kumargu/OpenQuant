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
    /// Canonical ID: "{sector}:{target}:{fit_date}:{members_hash}".
    ///
    /// The members_hash is a stable 8-char hex hash of the sorted peer symbols,
    /// ensuring uniqueness when peer composition changes.
    pub fn id(&self) -> String {
        let members_hash = self.members_hash();
        format!(
            "{}:{}:{}:{}",
            self.sector, self.target, self.fit_date, members_hash
        )
    }

    /// Compute a stable 8-char hex hash of the sorted members.
    fn members_hash(&self) -> String {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let mut sorted_members = self.members.clone();
        sorted_members.sort();

        let mut hasher = DefaultHasher::new();
        sorted_members.hash(&mut hasher);
        let hash = hasher.finish();
        format!("{:08x}", hash as u32)
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
