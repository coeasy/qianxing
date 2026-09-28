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
use qx_genglu::{
    filled_action, reconcile_order_facts, status_action, OrderReconcileDiff, OrderReconcileFact,
    VerdictAction,
};

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

    /// 差异种类的稳定维度码：只说明"哪个维度对不上"，写进对账报告的 `kind` 供诊断。
    /// 动作归类（待对账 / 需人工 / 可自动收敛）另有唯一口径，见 [`Self::action`]。
    pub const fn reason_code(&self) -> &'static str {
        match self {
            Self::MissingLocally { .. } => "missing_locally",
            Self::MissingAtVenue { .. } => "missing_at_venue",
            Self::StatusMismatch { .. } => "status_mismatch",
            Self::FilledMismatch { .. } => "filled_mismatch",
        }
    }

    /// 差异 → 动作的唯一归类：委托 qx-genglu 裁决所用的同一对维度判据
    /// （[`status_action`] / [`filled_action`]），本模块不重复比较状态或数量。
    /// 状态维度既可能是远端权威推进（可自动收敛），也可能是互斥迁移（需人工），
    /// 只看 [`Self::reason_code`] 的维度码分不出这两类，因此落待对账事实的 reason
    /// 必须来这儿（V12 §18 TX7）。
    pub fn action(&self) -> VerdictAction {
        match self {
            Self::MissingAtVenue { .. } => VerdictAction::ReconcileRequired,
            Self::MissingLocally { .. } => VerdictAction::ManualReview,
            Self::StatusMismatch { local, venue, .. } => status_action(*local, *venue),
            Self::FilledMismatch { local, venue, .. } => filled_action(local.raw(), venue.raw()),
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

#[cfg(test)]
mod tests {
    use super::*;
    use qx_core::OrderStatus;

    fn fact(client_order_id: u64, status: OrderStatus, filled_raw: i128) -> OrderReconcileFact {
        OrderReconcileFact {
            client_order_id,
            status: Some(status),
            filled_raw,
        }
    }

    /// 夹具：一对本地/远端事实过完整投影链，取出指定维度差异的 "维度码|动作码"。
    fn codes(local: (OrderStatus, i128), venue: (OrderStatus, i128), kind: &str) -> String {
        let issues = reconcile_issues(&[fact(1, local.0, local.1)], &[fact(1, venue.0, venue.1)]);
        let issue = issues
            .iter()
            .find(|issue| issue.reason_code() == kind)
            .unwrap_or_else(|| panic!("夹具应产出 {kind} 差异，实得 {issues:?}"));
        format!("{}|{}", issue.reason_code(), issue.action().reason_code())
    }

    #[test]
    fn the_same_status_dimension_reports_two_different_actions() {
        // 本地部分成交、远端已成交：维度码同样是 status_mismatch，动作是可自动收敛。
        assert_eq!(
            codes(
                (OrderStatus::PartiallyFilled, 4_000_000_000),
                (OrderStatus::Filled, 6_000_000_000),
                "status_mismatch"
            ),
            "status_mismatch|resync"
        );
        // 本地已成交、远端已撤销：同一维度码，动作必须是需人工——只看 kind 分不出这两类。
        assert_eq!(
            codes(
                (OrderStatus::Filled, 6_000_000_000),
                (OrderStatus::Cancelled, 6_000_000_000),
                "status_mismatch"
            ),
            "status_mismatch|manual_review"
        );
    }

    #[test]
    fn filled_direction_and_absences_keep_their_own_actions() {
        // 数量前进可收敛，数量回退需人工。
        assert_eq!(
            codes(
                (OrderStatus::Working, 4_000_000_000),
                (OrderStatus::PartiallyFilled, 6_000_000_000),
                "filled_mismatch"
            ),
            "filled_mismatch|resync"
        );
        assert_eq!(
            codes(
                (OrderStatus::Working, 6_000_000_000),
                (OrderStatus::PartiallyFilled, 4_000_000_000),
                "filled_mismatch"
            ),
            "filled_mismatch|manual_review"
        );
        // 柜台缺单是"结果未知，只能继续复查"，远端孤单是"归属不明，需人工"。
        let at_venue = reconcile_issues(&[fact(7, OrderStatus::Submitted, 0)], &[]);
        assert_eq!(at_venue[0].action().reason_code(), "pending_reconcile");
        let locally = reconcile_issues(&[], &[fact(7, OrderStatus::Submitted, 0)]);
        assert_eq!(locally[0].action().reason_code(), "manual_review");
    }
}
