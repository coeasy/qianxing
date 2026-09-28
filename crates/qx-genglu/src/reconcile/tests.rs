//! 对账归一（V10 §4.8）的用例：唯一裁决口径与它的投影链。
//!
//! 裁决三要素用例（待对账 / 自动收敛 / 需人工）必须同时钉住
//! `reconcile_order_facts` → `order_reconcile_verdict` 这条委托链：
//! 投影若改回独立判定，这里都会有用例变红。

use super::*;
use qx_core::OrderStatus;

fn fact(client_order_id: u64, status: Option<OrderStatus>, filled_raw: i128) -> OrderReconcileFact {
    OrderReconcileFact {
        client_order_id,
        status,
        filled_raw,
    }
}

#[test]
fn submitted_without_remote_report_is_pending_reconcile_not_resubmitted() {
    // (a) 本地 Submitted + 远端无回报 → 待对账，而不是自动补单。
    // 投影链只允许报"柜台缺单"……
    assert_eq!(
        reconcile_order_facts(&[fact(1, Some(OrderStatus::Submitted), 0)], &[]),
        vec![OrderReconcileDiff::MissingAtVenue { client_order_id: 1 }]
    );
    // ……而动作口径必须是 ReconcileRequired（复查确认）；口径里不存在
    // "按本地意图自动补单"这种动作，收敛值也一律为空。
    let verdict = order_reconcile_verdict(&fact(1, Some(OrderStatus::Submitted), 0), None);
    assert_eq!(verdict, ReconcileVerdict::PendingReconcile);
    assert_eq!(verdict.action(), VerdictAction::ReconcileRequired);
    assert_eq!(verdict.action().reason_code(), "pending_reconcile");
    assert_eq!(verdict.dimensions().status, None);
    assert_eq!(verdict.dimensions().filled, None);
}

#[test]
fn partial_local_converges_to_remote_filled_terminal() {
    // (b) 本地 PartiallyFilled + 远端 Filled → 可自动收敛为终态。
    let local = fact(1, Some(OrderStatus::PartiallyFilled), 4_000_000_000);
    let venue = fact(1, Some(OrderStatus::Filled), 6_000_000_000);
    // 委托链给出的逐维度差异报告……
    assert_eq!(
        reconcile_order_facts(std::slice::from_ref(&local), std::slice::from_ref(&venue)),
        vec![
            OrderReconcileDiff::StatusMismatch {
                client_order_id: 1,
                local: OrderStatus::PartiallyFilled,
                venue: OrderStatus::Filled,
            },
            OrderReconcileDiff::FilledMismatch {
                client_order_id: 1,
                local_raw: 4_000_000_000,
                venue_raw: 6_000_000_000,
            },
        ]
    );
    // ……与裁决唯一口径同源：动作是自动收敛，且收敛目标就是远端终态。
    let verdict = order_reconcile_verdict(&local, Some(&venue));
    assert_eq!(verdict.action(), VerdictAction::Resync);
    let diff = verdict.dimensions();
    let (_, terminal) = diff.status.expect("必须给出远端权威状态");
    assert_eq!(terminal, OrderStatus::Filled);
    assert!(terminal.is_terminal());
    assert_eq!(diff.filled, Some((4_000_000_000, 6_000_000_000)));
}

#[test]
fn remote_qty_conflict_is_needs_human() {
    // (c) 远端数量回退（少于本地）→ 判为需人工，绝不自动覆盖任一侧。
    let local = fact(1, Some(OrderStatus::Filled), 6_000_000_000);
    let venue = fact(1, Some(OrderStatus::Filled), 4_000_000_000);
    assert_eq!(
        order_reconcile_verdict(&local, Some(&venue)).action(),
        VerdictAction::ManualReview
    );
    // 冲突同样要逐维度如实进入报告，供审计看到是哪一维需要人工。
    assert_eq!(
        reconcile_order_facts(&[local], &[venue]),
        vec![OrderReconcileDiff::FilledMismatch {
            client_order_id: 1,
            local_raw: 6_000_000_000,
            venue_raw: 4_000_000_000,
        }]
    );
    // "远端有、本地无"的孤单必须被报出来：不能凭单次回报静默丢弃。
    // 它的动作归类（需人工，不可自动入账）由 qx-adapter 的投影用例咬住。
    assert_eq!(
        reconcile_order_facts(&[], &[fact(2, Some(OrderStatus::Working), 0)]),
        vec![OrderReconcileDiff::MissingLocally { client_order_id: 2 }]
    );
}

#[test]
fn terminal_status_conflict_is_needs_human_not_auto_converge() {
    // 两个互斥终态（本地 Filled vs 远端 Cancelled）不可自动收敛：需人工。
    let verdict = order_reconcile_verdict(
        &fact(1, Some(OrderStatus::Filled), 1_000_000_000),
        Some(&fact(1, Some(OrderStatus::Cancelled), 0)),
    );
    assert!(matches!(verdict, ReconcileVerdict::NeedsHuman(_)));
    assert_eq!(verdict.action(), VerdictAction::ManualReview);
    // 但逐维度差异仍要如实投影进报告，供审计（判定与报告同源）。
    let diffs = reconcile_order_facts(
        &[fact(1, Some(OrderStatus::Filled), 1_000_000_000)],
        &[fact(1, Some(OrderStatus::Cancelled), 0)],
    );
    assert_eq!(diffs.len(), 2);
    assert!(matches!(
        diffs[0],
        OrderReconcileDiff::StatusMismatch {
            local: OrderStatus::Filled,
            venue: OrderStatus::Cancelled,
            ..
        }
    ));
}

// —— 共享判定器 reconcile_order_facts 自身的用例：证明状态与数量维度
// 各判一次、缺边方向正确，且不依赖任何具体订单/适配器类型。
#[test]
fn canonical_flags_status_and_filled_independently() {
    let local = vec![fact(1, Some(OrderStatus::PartiallyFilled), 4_000_000_000)];
    let remote = vec![fact(1, Some(OrderStatus::Filled), 6_000_000_000)];
    let diffs = reconcile_order_facts(&local, &remote);
    assert!(matches!(
        diffs[0],
        OrderReconcileDiff::StatusMismatch {
            local: OrderStatus::PartiallyFilled,
            venue: OrderStatus::Filled,
            ..
        }
    ));
    assert!(matches!(
        diffs[1],
        OrderReconcileDiff::FilledMismatch {
            local_raw: 4_000_000_000,
            venue_raw: 6_000_000_000,
            ..
        }
    ));
}

#[test]
fn canonical_skips_status_when_a_side_is_unknown() {
    let local = vec![fact(1, Some(OrderStatus::Filled), 5_000_000_000)];
    let remote = vec![fact(1, None, 5_000_000_000)];
    assert!(reconcile_order_facts(&local, &remote).is_empty());
    // 裁决口径同样跳过状态维度：数量一致即一致。
    let verdict = order_reconcile_verdict(&local[0], Some(&remote[0]));
    assert_eq!(verdict, ReconcileVerdict::Consistent);
    assert_eq!(verdict.action(), VerdictAction::NoAction);
}

#[test]
fn canonical_reports_directional_presence_and_duplicates() {
    let local = vec![fact(1, None, 0), fact(1, None, 0)];
    let remote = vec![fact(2, None, 0)];
    let diffs = reconcile_order_facts(&local, &remote);
    assert!(matches!(
        diffs[0],
        OrderReconcileDiff::DuplicateLocal { .. }
    ));
    assert!(matches!(
        diffs[1],
        OrderReconcileDiff::MissingAtVenue { client_order_id: 1 }
    ));
    assert!(matches!(
        diffs[2],
        OrderReconcileDiff::MissingLocally { client_order_id: 2 }
    ));
    // 重复键的快照本身不可信：远端与本地都必须能报出重复，禁止静默覆盖。
    let remote_duplicates = reconcile_order_facts(&[], &[fact(3, None, 0), fact(3, None, 0)]);
    assert!(matches!(
        remote_duplicates[0],
        OrderReconcileDiff::DuplicateRemote { .. }
    ));
}
