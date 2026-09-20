use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PortfolioConstraint {
    pub max_turnover_bps: u32,
    pub min_trade_size: i128,
}

impl Default for PortfolioConstraint {
    fn default() -> Self {
        Self {
            max_turnover_bps: 10_000,
            min_trade_size: 1,
        }
    }
}

impl PortfolioConstraint {
    pub fn validate(&self) -> Result<(), String> {
        if self.min_trade_size <= 0 {
            return Err("minimum trade size must be positive".into());
        }
        if self.max_turnover_bps > 10_000 {
            return Err("turnover limit exceeds 100%".into());
        }
        Ok(())
    }
}
