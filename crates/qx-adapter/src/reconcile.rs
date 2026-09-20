//! 适配器侧对账薄封装。
//!
//! V10 §4.8 要求"比较本地与远端订单事实并给出差异裁定"只有一处真判定，它是
//! `qx-genglu::order_reconcile_verdict` 的唯一裁决口径（动作归类见其 `VerdictAction`）；
//! `reconcile_order_facts` 只是该裁决的逐维度差异投影。本模块承担适配器边界内的
//! 机械翻译：把各自的 `Order` / `VenueOrderSnapshot` 装配成中性的
//! [`OrderReconcileFact`]，调用共享判定器，再把 [`OrderReconcileDiff`] 翻译回
//! `AdapterReconcileIssue`。venue 专属的前置校验（重复快照硬错误、成交越界、
//! 终态过滤、状态机迁移）留在各适配器，不在此重复实现判定逻辑。

use super::*;
use qx_genglu::{reconcile_order_facts, OrderReconcileDiff, OrderReconcileFact};

impl AdapterReconcileIssue {
    /// 差异归属的本地客户单号。
    pub fn client_order_id(&self) -> u64 {
        match self {
            Self::MissingLocally { client_order_id }
            | Self::MissingAtVenue { client_order_id }
            | Self::StatusMismatch {
                client_order_id, ..
            }
            | Self::FilledMismatch {
                client_order_id, ..
            } => *client_order_id,
        }
    }

    /// 差异种类的稳定 reason 码：对账报告的 `kind` 与写入待对账事实时的 reason
    /// 共用这一处定义，调用点不再逐种类复制 match 枚举。动作归类（待对账 /
    /// 需人工 / 可自动收敛）由 qx-genglu 的裁决口径唯一决定。
    pub const fn reason_code(&self) -> &'static str {
        match self {
            Self::MissingLocally { .. } => "missing_locally",
            Self::MissingAtVenue { .. } => "missing_at_venue",
            Self::StatusMismatch { .. } => "status_mismatch",
            Self::FilledMismatch { .. } => "filled_mismatch",
        }
    }
}

/// 远端快照 → 中性事实。`remote` 已由调用方保证 `client_order_id` 唯一。
pub(crate) fn remote_order_facts(
    remote: &BTreeMap<u64, &VenueOrderSnapshot>,
) -> Vec<OrderReconcileFact> {
    remote
        .iter()
        .map(|(id, snapshot)| OrderReconcileFact {
            client_order_id: *id,
            status: Some(snapshot.status),
            filled_raw: snapshot.filled.raw(),
        })
        .collect()
}

/// 本地订单 → 中性事实。
pub(crate) fn local_order_fact(id: &u64, order: &Order) -> OrderReconcileFact {
    OrderReconcileFact {
        client_order_id: *id,
        status: Some(order.status),
        filled_raw: order.filled.raw(),
    }
}

/// 委托 [`reconcile_order_facts`] 得到归一化差异，再翻译回 `AdapterReconcileIssue`。
///
/// 两侧快照均以 `client_order_id` 为唯一键（重复已在各适配器构建阶段以硬错误拦截），
/// 故重复类差异在本边界不可达，直接过滤。
pub(crate) fn reconcile_issues(
    local: &[OrderReconcileFact],
    remote: &[OrderReconcileFact],
) -> Vec<AdapterReconcileIssue> {
    reconcile_order_facts(local, remote)
        .into_iter()
        .filter_map(|diff| match diff {
            OrderReconcileDiff::MissingAtVenue { client_order_id } => {
                Some(AdapterReconcileIssue::MissingAtVenue { client_order_id })
            }
            OrderReconcileDiff::MissingLocally { client_order_id } => {
                Some(AdapterReconcileIssue::MissingLocally { client_order_id })
            }
            OrderReconcileDiff::StatusMismatch {
                client_order_id,
                local,
                venue,
            } => Some(AdapterReconcileIssue::StatusMismatch {
                client_order_id,
                local,
                venue,
            }),
            OrderReconcileDiff::FilledMismatch {
                client_order_id,
                local_raw,
                venue_raw,
            } => Some(AdapterReconcileIssue::FilledMismatch {
                client_order_id,
                local: qx_core::Quantity::from_raw(local_raw),
                venue: qx_core::Quantity::from_raw(venue_raw),
            }),
            OrderReconcileDiff::DuplicateLocal { .. }
            | OrderReconcileDiff::DuplicateRemote { .. } => None,
        })
        .collect()
}
