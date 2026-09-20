//! 订单维度的对账裁决：全仓唯一的"本地 vs 远端订单"判定（V10 §4.8）。
//!
//! [`order_reconcile_verdict`] 是唯一的状态感知裁决函数；批量入口
//! [`reconcile_order_verdicts`] 把缺边方向与重复键也折叠进同一口径。
//! [`reconcile_order_facts`]、[`reconcile_orders`] 与 qx-adapter 的
//! `reconcile_remote` 只投影裁决结果为各自的差异类型，禁止重复比较。

use super::Discrepancy;
use qx_core::{Order, OrderStatus};
use std::collections::BTreeMap;

/// 订单对账的"事实"中性表示：只携带判定所需字段，让 qx-genglu 与 qx-adapter 共享
/// 同一套对账逻辑，而不必耦合各自的订单类型。
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct OrderReconcileFact {
    pub client_order_id: u64,
    /// 订单状态；`None` 表示该侧未提供状态，此时跳过状态维度比较（不臆断）。
    pub status: Option<OrderStatus>,
    /// 已成交数量（定点 raw）。
    pub filled_raw: i128,
}

/// 裁决携带的各维度差异，`(本地, 远端)`；`None` 表示该维度一致或不可判定。
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct OrderDimensionDiff {
    pub status: Option<(OrderStatus, OrderStatus)>,
    pub filled: Option<(i128, i128)>,
}

/// 单笔订单对账的唯一裁决口径（canonical verdict）。
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ReconcileVerdict {
    /// 本地与远端事实一致，无需动作。
    Consistent,
    /// 待对账：仅本地有该单、远端无回报，结果未知。唯一安全动作是继续复查确认，
    /// **禁止自动补单**——本口径根本不表达"补单"这种动作。
    PendingReconcile,
    /// 可自动收敛：远端事实是权威的合法推进（状态沿生命周期前进、成交数量不回退），
    /// 以远端为准更新本地即可，无需人工。
    AutoConverge(OrderDimensionDiff),
    /// 需要人工：事实冲突（成交数量回退、状态互斥迁移、远端有而本地无、重复键），
    /// 任何一方都不得单方面覆盖。
    NeedsHuman(OrderDimensionDiff),
}

/// 裁决映射出的唯一动作口径：调用点（对账 worker、报告与 `ReconcilePort`）
/// 只能按它分流与记 reason，不得再自行比较状态或数量。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum VerdictAction {
    /// 一致，无需动作。
    NoAction,
    /// 可自动收敛：以远端权威事实机械推进本地，无需人工。
    Resync,
    /// 待对账：结果未知，只允许继续查询并降级，禁止自动补单。
    ReconcileRequired,
    /// 需人工介入处理。
    ManualReview,
}

impl VerdictAction {
    /// 动作的稳定 reason 码：对账报告与待对账事实的 reason 共用这一套词汇。
    pub const fn reason_code(self) -> &'static str {
        match self {
            Self::NoAction => "consistent",
            Self::Resync => "resync",
            Self::ReconcileRequired => "pending_reconcile",
            Self::ManualReview => "manual_review",
        }
    }
}

impl ReconcileVerdict {
    /// 裁决 → 动作的唯一映射口径。
    pub fn action(&self) -> VerdictAction {
        match self {
            Self::Consistent => VerdictAction::NoAction,
            Self::PendingReconcile => VerdictAction::ReconcileRequired,
            Self::AutoConverge(_) => VerdictAction::Resync,
            Self::NeedsHuman(_) => VerdictAction::ManualReview,
        }
    }

    /// `AutoConverge` 时远端的权威状态（`None` 表示状态维度不可判定或需人工）。
    pub fn venue_status(&self) -> Option<OrderStatus> {
        match self {
            Self::AutoConverge(diff) => diff.status.map(|(_, venue)| venue),
            _ => None,
        }
    }

    /// `AutoConverge` 时远端的权威成交数量（定点 raw）。
    pub fn venue_filled_raw(&self) -> Option<i128> {
        match self {
            Self::AutoConverge(diff) => diff.filled.map(|(_, venue)| venue),
            _ => None,
        }
    }

    /// 裁决携带的维度差异，供差异报告层投影；一致与待对账为空。
    pub fn dimensions(&self) -> OrderDimensionDiff {
        match self {
            Self::AutoConverge(diff) | Self::NeedsHuman(diff) => diff.clone(),
            _ => OrderDimensionDiff::default(),
        }
    }
}

/// 全仓唯一的状态感知对账裁决函数（canonical verdict）：输入本地订单状态 + 交易所
/// 回报，输出 [`ReconcileVerdict`]。判定约定：
/// - 仅本地有、远端无回报 → [`ReconcileVerdict::PendingReconcile`]（动作
///   [`VerdictAction::ReconcileRequired`]），结果未知只能复查，禁止自动补单；
/// - 成交数量远端回退（少于本地）→ 事实冲突，需人工；
/// - 状态只在两侧都提供时判定：不等时以 `OrderStatus::can_transition_to` 为准，
///   合法前进视为远端权威推进可自动收敛，互斥迁移（含终态之间）需人工；
/// - 无任何维度差异 → [`ReconcileVerdict::Consistent`]。
pub fn order_reconcile_verdict(
    local: &OrderReconcileFact,
    remote: Option<&OrderReconcileFact>,
) -> ReconcileVerdict {
    let Some(remote) = remote else {
        return ReconcileVerdict::PendingReconcile;
    };
    let dimensions = OrderDimensionDiff {
        status: match (local.status, remote.status) {
            (Some(ls), Some(rs)) if ls != rs => Some((ls, rs)),
            _ => None,
        },
        filled: (local.filled_raw != remote.filled_raw)
            .then_some((local.filled_raw, remote.filled_raw)),
    };
    let conflicts = matches!(dimensions.filled, Some((local_raw, venue_raw)) if local_raw > venue_raw)
        || matches!(dimensions.status, Some((ls, rs)) if !ls.can_transition_to(rs));
    if conflicts {
        ReconcileVerdict::NeedsHuman(dimensions)
    } else if dimensions.status.is_none() && dimensions.filled.is_none() {
        ReconcileVerdict::Consistent
    } else {
        ReconcileVerdict::AutoConverge(dimensions)
    }
}

/// 按 `client_order_id` 建索引；返回索引与出现重复的键（禁止静默覆盖，交由上层判需人工）。
fn index_order_facts(
    facts: &[OrderReconcileFact],
) -> (BTreeMap<u64, &OrderReconcileFact>, Vec<u64>) {
    let mut map = BTreeMap::new();
    let mut duplicates = Vec::new();
    for fact in facts {
        if map.insert(fact.client_order_id, fact).is_some() {
            duplicates.push(fact.client_order_id);
        }
    }
    (map, duplicates)
}

/// 批量订单裁决：逐单判定唯一委托 [`order_reconcile_verdict`]，"远端有、本地无"与
/// 重复键直接判 [`ReconcileVerdict::NeedsHuman`]。返回按 `client_order_id` 升序的裁决表。
pub fn reconcile_order_verdicts(
    local: &[OrderReconcileFact],
    remote: &[OrderReconcileFact],
) -> BTreeMap<u64, ReconcileVerdict> {
    let (local_map, local_duplicates) = index_order_facts(local);
    let (remote_map, remote_duplicates) = index_order_facts(remote);
    let mut verdicts = local_map
        .iter()
        .map(|(&id, fact)| {
            (
                id,
                order_reconcile_verdict(fact, remote_map.get(&id).copied()),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let needs_human = ReconcileVerdict::NeedsHuman(OrderDimensionDiff::default());
    for &id in remote_map.keys() {
        if !local_map.contains_key(&id) {
            verdicts.insert(id, needs_human.clone());
        }
    }
    // 重复键的快照本身不可信，逐单判定结果必须被"需人工"覆盖。
    for id in local_duplicates.into_iter().chain(remote_duplicates) {
        verdicts.insert(id, needs_human.clone());
    }
    verdicts
}

/// 归一化订单差异报告：全仓唯一的"本地 vs 远端订单"差异输出（V10 §4.8）。
///
/// 这里只把 [`order_reconcile_verdict`] 的裁决投影为 [`OrderReconcileDiff`]，
/// 绝不重复比较。输出顺序稳定：本地重复、远端重复、按 client_order_id 升序的
/// 逐单判定（状态先于数量）、最后为远端多余单。
pub fn reconcile_order_facts(
    local: &[OrderReconcileFact],
    remote: &[OrderReconcileFact],
) -> Vec<OrderReconcileDiff> {
    let (local_map, local_duplicates) = index_order_facts(local);
    let (remote_map, remote_duplicates) = index_order_facts(remote);
    let mut out = local_duplicates
        .into_iter()
        .map(|client_order_id| OrderReconcileDiff::DuplicateLocal { client_order_id })
        .chain(
            remote_duplicates
                .into_iter()
                .map(|client_order_id| OrderReconcileDiff::DuplicateRemote { client_order_id }),
        )
        .collect::<Vec<_>>();
    for (&id, l) in &local_map {
        match order_reconcile_verdict(l, remote_map.get(&id).copied()) {
            ReconcileVerdict::Consistent => {}
            ReconcileVerdict::PendingReconcile => {
                out.push(OrderReconcileDiff::MissingAtVenue {
                    client_order_id: id,
                });
            }
            verdict => {
                let diff = verdict.dimensions();
                if let Some((local, venue)) = diff.status {
                    out.push(OrderReconcileDiff::StatusMismatch {
                        client_order_id: id,
                        local,
                        venue,
                    });
                }
                if let Some((local_raw, venue_raw)) = diff.filled {
                    out.push(OrderReconcileDiff::FilledMismatch {
                        client_order_id: id,
                        local_raw,
                        venue_raw,
                    });
                }
            }
        }
    }
    for &id in remote_map.keys() {
        if !local_map.contains_key(&id) {
            out.push(OrderReconcileDiff::MissingLocally {
                client_order_id: id,
            });
        }
    }
    out
}

/// 订单对账（Orders 视角入口）：`venue` 参数为 (client_id, filled_qty, 远端状态)，
/// 远端状态为 `None` 表示交易所回报未携带状态（该维度按不可判定跳过）。
///
/// 判定完全委托唯一口径 [`order_reconcile_verdict`]（经 [`reconcile_order_facts`]），
/// 此处只把中性差异翻译回 [`Discrepancy`]。
pub fn reconcile_orders(
    local: &[Order],
    venue: &[(u64, i128, Option<OrderStatus>)],
) -> Vec<Discrepancy> {
    let local_facts = local.iter().map(|order| OrderReconcileFact {
        client_order_id: order.client_id,
        status: Some(order.status),
        filled_raw: order.filled.raw(),
    });
    let venue_facts = venue
        .iter()
        .map(|(client_id, filled, status)| OrderReconcileFact {
            client_order_id: *client_id,
            status: *status,
            filled_raw: *filled,
        });
    reconcile_order_facts(
        &local_facts.collect::<Vec<_>>(),
        &venue_facts.collect::<Vec<_>>(),
    )
    .into_iter()
    // 裁决投影后的每一维差异都必须如实进入报告，这里只做类型翻译，不筛除差异。
    .map(|diff| match diff {
        OrderReconcileDiff::DuplicateLocal { client_order_id } => Discrepancy::DuplicateSnapshot {
            domain: "order".into(),
            side: "local".into(),
            key: client_order_id.to_string(),
        },
        OrderReconcileDiff::DuplicateRemote { client_order_id } => Discrepancy::DuplicateSnapshot {
            domain: "order".into(),
            side: "venue".into(),
            key: client_order_id.to_string(),
        },
        OrderReconcileDiff::MissingAtVenue { client_order_id } => Discrepancy::MissingAtVenue {
            client_id: client_order_id,
        },
        OrderReconcileDiff::MissingLocally { client_order_id } => Discrepancy::MissingLocally {
            client_id: client_order_id,
        },
        OrderReconcileDiff::FilledMismatch {
            client_order_id,
            local_raw,
            venue_raw,
        } => Discrepancy::QtyMismatch {
            client_id: client_order_id,
            local: local_raw,
            venue: venue_raw,
        },
        OrderReconcileDiff::StatusMismatch {
            client_order_id,
            local,
            venue,
        } => Discrepancy::StatusMismatch {
            client_id: client_order_id,
            local,
            venue,
        },
    })
    .collect()
}

/// [`reconcile_order_facts`] 输出的归一化差异裁定（裁决的逐维度投影）。
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum OrderReconcileDiff {
    /// 本地快照出现重复 client_order_id，禁止静默 last-write-wins。
    DuplicateLocal { client_order_id: u64 },
    /// 远端快照出现重复 client_order_id。
    DuplicateRemote { client_order_id: u64 },
    /// 本地有、远端无（裁决 `PendingReconcile` 的投影）。
    MissingAtVenue { client_order_id: u64 },
    /// 两侧都有但状态不符（仅当两侧状态均为 `Some` 时判定）。
    StatusMismatch {
        client_order_id: u64,
        local: OrderStatus,
        venue: OrderStatus,
    },
    /// 两侧都有但已成交数量不符。
    FilledMismatch {
        client_order_id: u64,
        local_raw: i128,
        venue_raw: i128,
    },
    /// 远端有、本地无。
    MissingLocally { client_order_id: u64 },
}
