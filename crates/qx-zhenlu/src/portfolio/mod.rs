//! 组合与再平衡：目标仓位 → 调仓增量的计划层。原 `qx-portfolio` crate，
//! V10 P2a 按"目标→增量是路由决策的同族"并入针路。

pub mod constraint;
pub mod rebalance;

pub use constraint::PortfolioConstraint;
pub use qx_core::TargetPosition;
pub use rebalance::{rebalance, PortfolioState, RebalancePlan};
