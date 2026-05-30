//! Data schemas for basket candidates and validation results.

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use crate::bertram::BertramResult;
use crate::ou::OuFit;

/// One component's weighted variance contribution inside the basket spread.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DominanceContribution {
    pub symbol: String,
    pub weight: f64,
    pub contribution: f64,
}

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
        let mut sorted_members = self.members.clone();
        sorted_members.sort();

        // FNV-1a is simple, deterministic, and independent of Rust's
        // randomized `DefaultHasher`, which is not a stable persistence contract.
        const FNV_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
        const FNV_PRIME: u64 = 0x100000001b3;

        let mut hash = FNV_OFFSET_BASIS;
        for member in sorted_members {
            for byte in member.as_bytes() {
                hash ^= u64::from(*byte);
                hash = hash.wrapping_mul(FNV_PRIME);
            }
            // Separate member boundaries so concatenation remains unambiguous.
            hash ^= 0x1f;
            hash = hash.wrapping_mul(FNV_PRIME);
        }

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
    /// ADF test statistic on the spread fit window, if computed.
    #[serde(default)]
    pub adf_statistic: Option<f64>,
    /// ADF p-value on the spread fit window, if computed.
    #[serde(default)]
    pub adf_pvalue: Option<f64>,
    /// Maximum absolute component contribution share to spread variance.
    #[serde(default)]
    pub dominance_score: Option<f64>,
    /// Per-component weighted variance contributions used by the dominance gate.
    #[serde(default)]
    pub dominance_contributions: Vec<DominanceContribution>,
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
            adf_statistic: None,
            adf_pvalue: None,
            dominance_score: None,
            dominance_contributions: Vec::new(),
            valid: false,
            reject_reason: Some(reason.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate_with_members(members: &[&str]) -> BasketCandidate {
        BasketCandidate {
            target: "AMD".to_string(),
            members: members.iter().map(|member| member.to_string()).collect(),
            sector: "chips".to_string(),
            fit_date: NaiveDate::from_ymd_opt(2026, 4, 20).unwrap(),
        }
    }

    #[test]
    fn test_members_hash_is_order_independent() {
        let a = candidate_with_members(&["NVDA", "INTC"]);
        let b = candidate_with_members(&["INTC", "NVDA"]);
        assert_eq!(a.id(), b.id());
    }

    #[test]
    fn test_members_hash_is_stable_for_known_members() {
        let candidate = candidate_with_members(&["NVDA", "INTC"]);
        assert_eq!(candidate.id(), "chips:AMD:2026-04-20:398f398c");
    }
}
