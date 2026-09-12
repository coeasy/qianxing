use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RiskSnapshot {
    pub portfolio_id: String,
    pub timestamp: u64,
    pub gross_exposure: i128,
    pub net_exposure: i128,
    pub drawdown_bps: i32,
}

impl RiskSnapshot {
    pub fn validate(&self) -> Result<(), String> {
        if self.portfolio_id.trim().is_empty() {
            return Err("portfolio id is required".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum RiskDecision {
    Allow,
    Reject,
    Reduce,
    Rebalance,
}

pub struct RiskEngine;

impl RiskEngine {
    pub fn evaluate(snapshot: &RiskSnapshot, limit_bps: i128) -> RiskDecision {
        if snapshot.gross_exposure > limit_bps {
            RiskDecision::Reduce
        } else {
            RiskDecision::Allow
        }
    }
}
