use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

use crate::portfolio::constraint::PortfolioConstraint;
// `TargetPosition` 全仓唯一定义在 qx-core（V10 §4.8）；本 crate 只消费与转出。
use qx_core::{InstrumentId, TargetPosition};

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
        if self.positions.keys().any(|key| key.trim().is_empty()) {
            return Err("empty instrument id".into());
        }
        Ok(())
    }

    pub fn position(&self, instrument: &str) -> i128 {
        self.positions.get(instrument).copied().unwrap_or_default()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RebalancePlan {
    /// The quantity delta to submit, not the final target quantity.
    pub positions: Vec<TargetPosition>,
    pub turnover_bps: u32,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct RebalanceDelta {
    pub instrument: String,
    pub quantity_delta: i128,
}

pub fn rebalance(
    current: &PortfolioState,
    target: &[TargetPosition],
    constraints: &PortfolioConstraint,
) -> Result<RebalancePlan, String> {
    current.validate()?;
    constraints.validate()?;

    let mut targets = BTreeMap::new();
    for item in target {
        if item.instrument.symbol.trim().is_empty()
            || item.instrument.venue.as_str().trim().is_empty()
        {
            return Err("empty target instrument id".into());
        }
        if targets
            .insert(item.instrument.to_string(), item.target_qty)
            .is_some()
        {
            return Err(format!("duplicate target instrument: {}", item.instrument));
        }
    }

    let instruments: BTreeSet<String> = current
        .positions
        .keys()
        .cloned()
        .chain(targets.keys().cloned())
        .collect();
    let mut positions = Vec::new();
    let mut turnover_quantity = 0_u128;
    let mut current_quantity = 0_u128;
    let mut target_quantity = 0_u128;
    for instrument in instruments {
        let current_qty = current.position(&instrument);
        let target_qty = targets.get(&instrument).copied().unwrap_or_default();
        current_quantity = current_quantity.saturating_add(current_qty.unsigned_abs());
        target_quantity = target_quantity.saturating_add(target_qty.unsigned_abs());
        let delta = target_qty.saturating_sub(current_qty);
        turnover_quantity = turnover_quantity.saturating_add(delta.unsigned_abs());
        if delta.unsigned_abs() >= constraints.min_trade_size as u128 {
            let instrument = InstrumentId::parse(&instrument)
                .ok_or_else(|| format!("invalid target instrument id: {instrument}"))?;
            positions.push(TargetPosition::single(instrument, delta));
        }
    }

    // Turnover is a ratio in basis points, not a raw quantity. With no target
    // quantity, any non-zero delta is treated as 100% turnover.
    let denominator = current_quantity.max(target_quantity).max(1);
    let turnover_bps = turnover_quantity
        .saturating_mul(10_000)
        .saturating_div(denominator)
        .min(u32::MAX as u128) as u32;
    if turnover_bps > constraints.max_turnover_bps {
        return Err("rebalance turnover exceeds limit".into());
    }
    Ok(RebalancePlan {
        positions,
        turnover_bps,
    })
}

pub fn build_rebalance(
    current: &PortfolioState,
    target: &[(String, i128)],
    constraints: &PortfolioConstraint,
) -> Result<Vec<RebalanceDelta>, String> {
    constraints.validate()?;
    current.validate()?;
    let mut result = Vec::new();
    for (instrument, target_qty) in target {
        if instrument.trim().is_empty() {
            return Err("empty target instrument id".into());
        }
        let current_qty = current
            .positions
            .get(instrument)
            .copied()
            .unwrap_or_default();
        let delta = target_qty.saturating_sub(current_qty);
        if delta.unsigned_abs() >= constraints.min_trade_size as u128 {
            result.push(RebalanceDelta {
                instrument: instrument.clone(),
                quantity_delta: delta,
            });
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn state(quantity: i128) -> PortfolioState {
        PortfolioState {
            portfolio_id: "portfolio-1".into(),
            timestamp: 1,
            cash: 1_000,
            positions: BTreeMap::from([(String::from("BTC.SIM"), quantity)]),
        }
    }

    fn btc() -> InstrumentId {
        InstrumentId::parse("BTC.SIM").unwrap()
    }

    #[test]
    fn turnover_is_reported_in_basis_points() {
        let plan = rebalance(
            &state(100),
            &[TargetPosition::single(btc(), 110)],
            &PortfolioConstraint {
                max_turnover_bps: 1_000,
                min_trade_size: 1,
            },
        )
        .unwrap();
        assert_eq!(plan.positions[0].target_qty, 10);
        assert_eq!(plan.turnover_bps, 909);
    }

    #[test]
    fn invalid_constraints_are_rejected_before_planning() {
        let error = rebalance(
            &state(0),
            &[],
            &PortfolioConstraint {
                max_turnover_bps: 10_001,
                min_trade_size: 1,
            },
        )
        .unwrap_err();
        assert!(error.contains("turnover"));
    }

    #[test]
    fn positions_missing_from_target_are_closed() {
        let plan = rebalance(
            &state(2),
            &[],
            &PortfolioConstraint {
                max_turnover_bps: 10_000,
                min_trade_size: 1,
            },
        )
        .unwrap();
        assert_eq!(plan.positions.len(), 1);
        assert_eq!(plan.positions[0].instrument, btc());
        assert_eq!(plan.positions[0].target_qty, -2);
        assert_eq!(plan.turnover_bps, 10_000);
    }
}
