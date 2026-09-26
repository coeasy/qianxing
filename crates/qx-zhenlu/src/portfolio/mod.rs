//! 组合与再平衡：目标仓位 → 调仓增量的计划层。原 `qx-portfolio` crate，
//! V10 P2a 按"目标→增量是路由决策的同族"并入针路。

pub mod constraint;
pub mod rebalance;

pub use constraint::PortfolioConstraint;
pub use qx_core::TargetPosition;
pub use rebalance::{build_rebalance, rebalance, PortfolioState, RebalanceDelta, RebalancePlan};

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
        let remainder = 10_000 - weight * signals.len() as i128;
        signals
            .iter()
            .enumerate()
            .map(|(index, _)| weight + if (index as i128) < remainder { 1 } else { 0 })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::{Allocator, EqualWeight};

    #[test]
    fn equal_weight_preserves_full_bps_budget() {
        let weights = EqualWeight.allocate(&[1, 2, 3]);
        assert_eq!(weights, vec![3334, 3333, 3333]);
        assert_eq!(weights.iter().sum::<i128>(), 10_000);
    }
}
