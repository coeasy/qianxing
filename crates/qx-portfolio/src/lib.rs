use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PortfolioState {
    pub portfolio_id: String,
    pub timestamp: u64,
    pub cash: i128,
    pub positions: BTreeMap<String, i128>,
}

impl PortfolioState {
    pub fn validate(&self) -> Result<(), String> {
        if self.portfolio_id.trim().is_empty() {
            return Err("portfolio id is required".into());
        }
        if self.positions.keys().any(|k| k.trim().is_empty()) {
            return Err("empty instrument id".into());
        }
        Ok(())
    }

    pub fn position(&self, instrument: &str) -> i128 {
        self.positions.get(instrument).copied().unwrap_or_default()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PortfolioConstraint {
    pub max_turnover_bps: u32,
    pub min_trade_size: i128,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct TargetPosition {
    pub instrument: String,
    pub quantity: i128,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RebalancePlan {
    pub positions: Vec<TargetPosition>,
    pub turnover_bps: u32,
}

pub trait Allocator {
    fn allocate(&self, signals: &[i128]) -> Vec<i128>;
}

pub struct EqualWeight;

impl Allocator for EqualWeight {
    fn allocate(&self, signals: &[i128]) -> Vec<i128> {
        if signals.is_empty() {
            return Vec::new();
        }
        let weight = 10_000 / signals.len() as i128;
        signals.iter().map(|_| weight).collect()
    }
}

pub fn rebalance(current: &PortfolioState, target: &[TargetPosition], constraint: &PortfolioConstraint) -> Result<RebalancePlan, String> {
    current.validate()?;
    let mut positions = Vec::new();
    let mut turnover = 0u32;
    for item in target {
        let delta = item.quantity.saturating_sub(current.position(&item.instrument));
        if delta.abs() >= constraint.min_trade_size {
            positions.push(TargetPosition { instrument: item.instrument.clone(), quantity: delta });
        }
        turnover = turnover.saturating_add(delta.unsigned_abs().min(u32::MAX as u128) as u32);
    }
    if turnover > constraint.max_turnover_bps {
        return Err("rebalance turnover exceeds limit".into());
    }
    Ok(RebalancePlan { positions, turnover_bps: turnover })
}
