//! 更路：本地事实 vs 远端（柜台）事实的对账判定。
//!
//! V10 §4.8 归一要求"对账 = 比较本地与远端事实并给出差异裁定"只有一处真判定。
//! 订单维度的唯一口径是 [`order_reconcile_verdict`]：输入本地订单状态与交易所回报，
//! 输出一致 / 待对账 / 可自动收敛 / 需人工（[`ReconcileVerdict`]），动作归类见
//! [`VerdictAction`]。差异报告 [`reconcile_order_facts`]、[`reconcile_orders`] 与
//! qx-adapter 的 `reconcile_remote` 都只把裁决投影回各自的领域类型，不再各自
//! 实现"缺哪边/数量对不对/状态对不对"的判定；资金、费率与成交维度只在
//! [`account`] 子模块各实现一次。

use qx_core::OrderStatus;

mod account;
mod order;

pub use account::*;
pub use order::*;

/// 对账差异。**"本地账簿 = 柜台账簿"不能作为默认假设。**
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Discrepancy {
    /// 同一对账快照出现重复键，禁止用 BTreeMap 的 last-write-wins 静默覆盖。
    DuplicateSnapshot {
        domain: String,
        side: String,
        key: String,
    },
    /// 本地有、柜台无。
    MissingAtVenue {
        client_id: u64,
    },
    /// 柜台有、本地无。
    MissingLocally {
        client_id: u64,
    },
    QtyMismatch {
        client_id: u64,
        local: i128,
        venue: i128,
    },
    /// 订单状态维度冲突：由唯一裁决口径（[`order_reconcile_verdict`]）投影而来，
    /// 只在两侧都提供状态时产出。
    StatusMismatch {
        client_id: u64,
        local: OrderStatus,
        venue: OrderStatus,
    },
    CashMismatch {
        currency: String,
        local: i128,
        venue: i128,
    },
    FeeMismatch {
        currency: String,
        local: i128,
        venue: i128,
    },
    FundingMismatch {
        currency: String,
        local: i128,
        venue: i128,
    },
    FillMissingAtVenue {
        order_id: u64,
        ts: u64,
    },
    FillMissingLocally {
        order_id: u64,
        ts: u64,
    },
    FillMismatch {
        order_id: u64,
        ts: u64,
        local_qty: i128,
        venue_qty: i128,
        local_price: i128,
        venue_price: i128,
    },
}

#[cfg(test)]
mod tests;
