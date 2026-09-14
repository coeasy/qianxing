use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub mod decision;
pub mod exposure;
pub mod volatility;

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
        if self.gross_exposure < 0 {
            return Err("gross exposure cannot be negative".into());
        }
        if self.drawdown_bps > 0 {
            return Err("drawdown cannot be positive".into());
        }
        if self.factor_exposure.keys().any(|key| key.trim().is_empty()) {
            return Err("factor id cannot be empty".into());
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
    pub fn evaluate(
        snapshot: &RiskSnapshot,
        max_exposure: i128,
        max_drawdown_bps: i32,
    ) -> RiskDecision {
        if snapshot.validate().is_err() {
            return RiskDecision::Reject;
        }
        if snapshot.gross_exposure > max_exposure {
            return RiskDecision::Reduce;
        }
        if snapshot.drawdown_bps < max_drawdown_bps {
            return RiskDecision::Reject;
        }
        RiskDecision::Allow
    }
}

#[cfg(test)]
mod tests {
    use super::{RiskDecision, RiskEngine, RiskSnapshot};
    use std::collections::BTreeMap;

    fn snapshot() -> RiskSnapshot {
        RiskSnapshot {
            portfolio_id: "portfolio-1".into(),
            timestamp: 1,
            gross_exposure: 100,
            net_exposure: 100,
            volatility_bps: 10,
            drawdown_bps: 0,
            factor_exposure: BTreeMap::new(),
        }
    }

    #[test]
    fn invalid_snapshot_fails_closed() {
        let mut value = snapshot();
        value.gross_exposure = -1;
        assert_eq!(
            RiskEngine::evaluate(&value, 1_000, -500),
            RiskDecision::Reject
        );
    }

    #[test]
    fn exposure_and_drawdown_limits_are_ordered() {
        assert_eq!(
            RiskEngine::evaluate(&snapshot(), 50, -500),
            RiskDecision::Reduce
        );
        assert_eq!(
            RiskEngine::evaluate(&snapshot(), 1_000, 100),
            RiskDecision::Reject
        );
        assert_eq!(
            RiskEngine::evaluate(&snapshot(), 1_000, -500),
            RiskDecision::Allow
        );
    }
}
