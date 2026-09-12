use serde::{Deserialize, Serialize};

use crate::state::PortfolioState;
use crate::constraint::PortfolioConstraint;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RebalanceDelta {
    pub instrument: String,
    pub quantity_delta: i128,
}

pub fn build_rebalance(
    current: &PortfolioState,
    target: &[(String, i128)],
    constraints: &PortfolioConstraint,
) -> Result<Vec<RebalanceDelta>, String> {
    constraints.validate()?;
    let mut result = Vec::new();
    for (instrument, target_qty) in target {
        let current_qty = current.positions.get(instrument).copied().unwrap_or_default();
        let delta = target_qty.saturating_sub(current_qty);
        if delta.abs() >= constraints.min_trade_size {
            result.push(RebalanceDelta {
                instrument: instrument.clone(),
                quantity_delta: delta,
            });
        }
    }
    Ok(result)
}
