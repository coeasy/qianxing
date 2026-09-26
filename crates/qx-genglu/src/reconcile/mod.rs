//! 更路：本地订单事实 vs 远端（柜台）订单事实的对账判定。
//!
//! V10 §4.8 归一要求"对账 = 比较本地与远端事实并给出差异裁定"只有一处真判定。
//! 订单维度的唯一口径是 [`order_reconcile_verdict`]：输入本地订单状态与交易所回报，
//! 输出一致 / 待对账 / 可自动收敛 / 需人工（[`ReconcileVerdict`]），动作归类见
//! [`VerdictAction`]。差异报告 [`reconcile_order_facts`] 与 qx-adapter 的
//! `reconcile_remote` 只把裁决投影回各自的领域类型，不各自实现"缺哪边/数量对不对/
//! 状态对不对"的判定。
//!
//! 账户（现金）维度不在本 crate：它对账的是账本余额与柜台余额两份事实，唯一实现
//! 是 `qx-runtime` 的 `settlement_balance_discrepancies`。

mod order;

pub use order::*;

#[cfg(test)]
mod tests;
