use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RiskSnapshot {
    pub portfolio_id: String,
    pub timestamp: u64,
    pub gross_exposure: i128,
    pub net_exposure: i128,
    pub volatility_bps: u32,
    pub drawdown_bps: i32,
    pub factor_exposure: BTreeMap<String, i32>,
}

impl RiskSnapshot {
    pub fn validate(&self) -> Result<(), String> {
        if self.portfolio_id.trim().is_empty() {
            return Err("portfolio id is required".into());
        }
        if self.drawdown_bps > 0 {
            return Err("drawdown cannot be positive".into());
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
    pub fn evaluate(snapshot: &RiskSnapshot, max_exposure: i128, max_drawdown_bps: i32) -> RiskDecision {
        if snapshot.gross_exposure > max_exposure {
            return RiskDecision::Reduce;
        }
        if snapshot.drawdown_bps < max_drawdown_bps {
            return RiskDecision::Reject;
        }
        RiskDecision::Allow
    }
}
