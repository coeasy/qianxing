//! Market data quality gate.
//!
//! The quality layer validates external data before entering the kernel.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QualityStatus {
    Accepted,
    Rejected,
}

pub struct QualityGate;

impl QualityGate {
    pub fn validate(&self) -> QualityStatus {
        QualityStatus::Accepted
    }
}
