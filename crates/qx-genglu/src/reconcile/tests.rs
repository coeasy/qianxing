//! 对账归一（V10 §4.8）的用例：唯一裁决口径与它的投影链。
//!
//! 裁决三要素用例（待对账 / 自动收敛 / 需人工）必须同时钉住
//! `reconcile_orders` → `reconcile_order_facts` → `order_reconcile_verdict`
//! 这条委托链：任何一环改回各自独立判定，这里都会有用例变红。

use super::*;
use qx_core::{Fill, Order, OrderStatus};

fn order(client_id: u64, status: OrderStatus, filled: i128) -> Order {
    Order {
        client_id,
        instrument: qx_core::InstrumentId::parse("T.V").unwrap(),
        side: qx_core::Side::Buy,
        qty: qx_core::Quantity::from_i64(10),
        limit: None,
        status,
        filled: qx_core::Quantity::from_raw(filled),
        account_id: "a".into(),
        trace: None,
        policy: None,
    }
}

fn fact(client_order_id: u64, status: Option<OrderStatus>, filled_raw: i128) -> OrderReconcileFact {
    OrderReconcileFact {
        client_order_id,
        status,
        filled_raw,
    }
}

#[test]
fn detects_missing_at_venue() {
    let local = vec![order(1, OrderStatus::Filled, 1_000_000_000)];
    let diffs = reconcile_orders(&local, &[]);
    assert_eq!(diffs, vec![Discrepancy::MissingAtVenue { client_id: 1 }]);
}

#[test]
fn detects_qty_mismatch() {
    let local = vec![order(1, OrderStatus::PartiallyFilled, 4_000_000_000)];
    let diffs = reconcile_orders(&local, &[(1, 6_000_000_000, None)]);
    assert!(matches!(diffs[0], Discrepancy::QtyMismatch { .. }));
}

#[test]
fn reconciles_cash_and_positions() {
    let cash = reconcile_cash(
        &[CashSnapshot {
            currency: "USD".into(),
            amount: 10,
        }],
        &[CashSnapshot {
            currency: "USD".into(),
            amount: 9,
        }],
    );
    assert!(matches!(cash[0], Discrepancy::CashMismatch { .. }));
    let fees = reconcile_fees(
        &[FlowSnapshot {
            currency: "USD".into(),
            amount: 3,
        }],
        &[FlowSnapshot {
            currency: "USD".into(),
            amount: 2,
        }],
    );
    assert!(matches!(fees[0], Discrepancy::FeeMismatch { .. }));
}

#[test]
fn reconciles_fill_facts_without_using_order_status_as_a_proxy() {
    let local = Fill {
        order_id: 1,
        qty: qx_core::Quantity::from_i64(2),
        price: qx_core::Price::from_i64(10),
        ts: 5,
        ..Fill::default()
    };
    let venue = Fill {
        order_id: 1,
        qty: qx_core::Quantity::from_i64(1),
        price: qx_core::Price::from_i64(11),
        ts: 5,
        ..Fill::default()
    };
    assert!(matches!(
        reconcile_fills(&[local], &[venue]).as_slice(),
        [Discrepancy::FillMismatch { .. }]
    ));
}

#[test]
fn duplicate_snapshot_keys_are_reported_instead_of_overwritten() {
    let duplicates = reconcile_cash(
        &[
            CashSnapshot {
                currency: "USD".into(),
                amount: 10,
            },
            CashSnapshot {
                currency: "USD".into(),
                amount: 11,
            },
        ],
        &[],
    );
    assert!(duplicates.iter().any(|item| matches!(
        item,
        Discrepancy::DuplicateSnapshot { domain, side, key }
            if domain == "cash" && side == "local" && key == "USD"
    )));
}

// —— 归一裁决的三要素咬合用例（方案收尾要求）——

#[test]
fn submitted_without_remote_report_is_pending_reconcile_not_resubmitted() {
    // (a) 本地 Submitted + 远端无回报 → 待对账，而不是自动补单。
    // 委托链（reconcile_orders → 唯一裁决口径）只允许报"柜台缺单"……
    let local = vec![order(1, OrderStatus::Submitted, 0)];
    assert_eq!(
        reconcile_orders(&local, &[]),
        vec![Discrepancy::MissingAtVenue { client_id: 1 }]
    );
    // ……而动作口径必须是 ReconcileRequired（复查确认）；口径里不存在
    // "按本地意图自动补单"这种动作，收敛值也一律为空。
    let verdicts = reconcile_order_verdicts(&[fact(1, Some(OrderStatus::Submitted), 0)], &[]);
    let verdict = &verdicts[&1];
    assert_eq!(verdict, &ReconcileVerdict::PendingReconcile);
    assert_eq!(verdict.action(), VerdictAction::ReconcileRequired);
    assert_eq!(verdict.action().reason_code(), "pending_reconcile");
    assert_eq!(verdict.venue_status(), None);
    assert_eq!(verdict.venue_filled_raw(), None);
}

#[test]
fn partial_local_converges_to_remote_filled_terminal() {
    // (b) 本地 PartiallyFilled + 远端 Filled → 可自动收敛为终态。
    let local = vec![order(1, OrderStatus::PartiallyFilled, 4_000_000_000)];
    let venue = [(1, 6_000_000_000, Some(OrderStatus::Filled))];
    // 委托链给出的逐维度差异报告……
    assert_eq!(
        reconcile_orders(&local, &venue),
        vec![
            Discrepancy::StatusMismatch {
                client_id: 1,
                local: OrderStatus::PartiallyFilled,
                venue: OrderStatus::Filled,
            },
            Discrepancy::QtyMismatch {
                client_id: 1,
                local: 4_000_000_000,
                venue: 6_000_000_000,
            },
        ]
    );
    // ……与裁决唯一口径同源：动作是自动收敛，且收敛目标就是远端终态。
    let verdict = order_reconcile_verdict(
        &fact(1, Some(OrderStatus::PartiallyFilled), 4_000_000_000),
        Some(&fact(1, Some(OrderStatus::Filled), 6_000_000_000)),
    );
    assert_eq!(verdict.action(), VerdictAction::Resync);
    let terminal = verdict.venue_status().expect("必须给出远端权威状态");
    assert_eq!(terminal, OrderStatus::Filled);
    assert!(terminal.is_terminal());
    assert_eq!(verdict.venue_filled_raw(), Some(6_000_000_000));
}

#[test]
fn remote_qty_conflict_is_needs_human() {
    // (c) 远端数量与本地冲突（远端回退）→ 判为需人工，绝不自动覆盖任一侧。
    let local = vec![order(1, OrderStatus::Filled, 6_000_000_000)];
    assert_eq!(
        reconcile_orders(&local, &[(1, 4_000_000_000, Some(OrderStatus::Filled))]),
        vec![Discrepancy::QtyMismatch {
            client_id: 1,
            local: 6_000_000_000,
            venue: 4_000_000_000,
        }]
    );
    let verdicts = reconcile_order_verdicts(
        &[fact(1, Some(OrderStatus::Filled), 6_000_000_000)],
        &[fact(1, Some(OrderStatus::Filled), 4_000_000_000)],
    );
    assert_eq!(verdicts[&1].action(), VerdictAction::ManualReview);
    // "远端有、本地无"的孤单同样需人工：不能凭单次回报自动入账。
    let orphan = reconcile_order_verdicts(&[], &[fact(2, Some(OrderStatus::Working), 0)]);
    assert_eq!(orphan[&2].action(), VerdictAction::ManualReview);
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
    // 重复键在裁决口径里覆盖逐单判定，直接判需人工。
    let verdicts = reconcile_order_verdicts(&local, &remote);
    assert_eq!(
        verdicts[&1],
        ReconcileVerdict::NeedsHuman(OrderDimensionDiff::default())
    );
}
