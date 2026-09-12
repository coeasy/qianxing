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
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct TargetPosition {
    pub instrument: String,
    pub quantity: i128,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RebalancePlan {
    pub positions: Vec<TargetPosition>,
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
