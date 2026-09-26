//! 组合与再平衡：目标仓位 → 调仓增量的计划层。原 `qx-portfolio` crate，
//! V10 P2a 按"目标→增量是路由决策的同族"并入针路。
//!
//! 入口只有一个：`rebalance` 用类型化 `TargetPosition` 产出 `RebalancePlan`。
//! V11 §41 E3 删掉三处无人消费的平行实现——`optimizer`（按条数截断 targets 的桩）、
//! `build_rebalance` + `RebalanceDelta`（裸字符串键的旧口径，与 `rebalance` 同时存在
//! 就是第二套调仓真相源）、`Allocator` / `EqualWeight`（只有自家用例在跑的扩展点）。

pub mod constraint;
pub mod rebalance;

pub use constraint::PortfolioConstraint;
pub use qx_core::TargetPosition;
pub use rebalance::{rebalance, PortfolioState, RebalancePlan};
