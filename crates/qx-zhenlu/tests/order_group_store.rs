use qx_core::{
    ConditionalOrderIntent, ConditionalOrderKind, ExactDecimal, ExactOrderAmounts, InstrumentId,
    Order, OrderGroupExecution, OrderGroupIntent, OrderGroupKind, OrderGroupLegIntent,
    OrderGroupLegRole, OrderGroupState, OrderStatus, Quantity, Side, TriggerPriceType,
};
use qx_zhenlu::{FileOrderGroupStore, OrderGroupStore};
use std::time::{SystemTime, UNIX_EPOCH};

fn protection_order(client_id: u64, kind: ConditionalOrderKind) -> Order {
    let trigger = match kind {
        ConditionalOrderKind::TakeProfit => "70000",
        ConditionalOrderKind::StopLoss => "60000",
        _ => unreachable!(),
    };
    Order {
        client_id,
        instrument: InstrumentId::parse("BTC/USDT.OKX").unwrap(),
        side: Side::Sell,
        qty: Quantity::ZERO,
        limit: None,
        status: OrderStatus::PendingSubmit,
        filled: Quantity::ZERO,
        account_id: "main".into(),
        trace: None,
        policy: None,
        exact_amounts: Some(ExactOrderAmounts {
            quantity: ExactDecimal::parse("0.01").unwrap(),
            limit_price: None,
            filled: ExactDecimal::ZERO,
            quote_budget: None,
            quote_filled: ExactDecimal::ZERO,
            market_reference_price: None,
            conditional: Some(ConditionalOrderIntent {
                kind,
                trigger_price: Some(ExactDecimal::parse(trigger).unwrap()),
                trigger_price_type: Some(TriggerPriceType::Mark),
                trailing_percent: None,
                trailing_amount: None,
                trailing_trigger_price: None,
            }),
        }),
    }
}

fn oco_state(group_id: &str) -> OrderGroupState {
    OrderGroupState::from_intent(
        OrderGroupIntent::new(
            group_id,
            OrderGroupKind::Oco,
            OrderGroupExecution::NativeRequired,
            vec![
                OrderGroupLegIntent {
                    role: OrderGroupLegRole::TakeProfit,
                    order: protection_order(701, ConditionalOrderKind::TakeProfit),
                },
                OrderGroupLegIntent {
                    role: OrderGroupLegRole::StopLoss,
                    order: protection_order(702, ConditionalOrderKind::StopLoss),
                },
            ],
        )
        .unwrap(),
    )
    .unwrap()
}

fn bracket_state(group_id: &str) -> OrderGroupState {
    let mut entry = protection_order(711, ConditionalOrderKind::TakeProfit);
    entry.side = Side::Buy;
    entry.exact_amounts.as_mut().unwrap().conditional = None;
    OrderGroupState::from_intent(
        OrderGroupIntent::new(
            group_id,
            OrderGroupKind::Bracket,
            OrderGroupExecution::NativeRequired,
            vec![
                OrderGroupLegIntent {
                    role: OrderGroupLegRole::Entry,
                    order: entry,
                },
                OrderGroupLegIntent {
                    role: OrderGroupLegRole::TakeProfit,
                    order: protection_order(712, ConditionalOrderKind::TakeProfit),
                },
                OrderGroupLegIntent {
                    role: OrderGroupLegRole::StopLoss,
                    order: protection_order(713, ConditionalOrderKind::StopLoss),
                },
            ],
        )
        .unwrap(),
    )
    .unwrap()
}

#[test]
fn file_store_recovers_and_confirms_pending_native_oco_amendment() {
    let root = std::env::current_dir()
        .unwrap()
        .join("target")
        .join("test-temp")
        .join(format!(
            "qianxing-oco-amend-recovery-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
    let mut group = oco_state("oco-amend-recovery");
    group.status = qx_core::OrderGroupStatus::Active;
    group.venue_group_id = Some("90001".into());
    group.legs[0].order.status = OrderStatus::Working;
    group.legs[0].venue_order_id = Some("70001".into());
    group.legs[1].order.status = OrderStatus::Cancelled;
    let mut store = FileOrderGroupStore::new(&root).unwrap();
    store.create(&group).unwrap();

    let mut requested = group.legs[0].order.clone();
    requested.exact_amounts.as_mut().unwrap().quantity = ExactDecimal::parse("0.012").unwrap();
    store
        .begin_child_amendment(
            "oco-amend-recovery",
            701,
            "control:amend:44:701".into(),
            &requested,
        )
        .unwrap();

    drop(store);
    let mut store = FileOrderGroupStore::new(&root).unwrap();
    let pending = store.load("oco-amend-recovery").unwrap().unwrap();
    assert!(pending.has_pending_amendment());
    assert_eq!(pending.status, qx_core::OrderGroupStatus::ReconcileRequired);

    requested.status = OrderStatus::Working;
    store
        .confirm_child_amendment("oco-amend-recovery", "control:amend:44:701", &requested)
        .unwrap();
    let restored = store.load("oco-amend-recovery").unwrap().unwrap();
    assert!(!restored.has_pending_amendment());
    assert_eq!(restored.status, qx_core::OrderGroupStatus::Active);
    assert_eq!(restored.legs[0].order, requested);

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn file_order_group_store_is_idempotent_and_restores_reduced_state() {
    let test_root = std::env::current_dir()
        .unwrap()
        .join("target")
        .join("test-temp");
    std::fs::create_dir_all(&test_root).unwrap();
    let root = test_root.join(format!(
        "qianxing-order-groups-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let mut store = FileOrderGroupStore::new(&root).unwrap();
    let group = oco_state("persisted-oco");
    store.create(&group).unwrap();
    store.create(&group).unwrap();
    assert!(store.begin_submission("persisted-oco").unwrap());
    assert!(!store.begin_submission("persisted-oco").unwrap());

    let mut submitted = group.legs[0].order.clone();
    submitted.status = OrderStatus::Submitted;
    store
        .apply_order_snapshot("persisted-oco", &submitted, None)
        .unwrap();
    let mut accepted = submitted.clone();
    accepted.status = OrderStatus::Accepted;
    store
        .apply_order_snapshot(
            "persisted-oco",
            &accepted,
            Some("exchange-order-701".into()),
        )
        .unwrap();
    store
        .record_venue_group_id("persisted-oco", "exchange-list-9001".into())
        .unwrap();
    store.require_reconciliation("persisted-oco").unwrap();
    store
        .record_order_snapshot_during_reconciliation(
            "persisted-oco",
            &accepted,
            Some("exchange-order-701".into()),
        )
        .unwrap();

    let reopened = FileOrderGroupStore::new(&root).unwrap();
    let restored = reopened.load("persisted-oco").unwrap().unwrap();
    assert_eq!(
        restored.status,
        qx_core::OrderGroupStatus::ReconcileRequired
    );
    assert_eq!(restored.legs[0].order.status, OrderStatus::Accepted);
    assert_eq!(
        restored.legs[0].venue_order_id.as_deref(),
        Some("exchange-order-701")
    );
    assert_eq!(
        restored.venue_group_id.as_deref(),
        Some("exchange-list-9001")
    );
    assert!(store
        .record_venue_group_id("persisted-oco", "conflicting-list".into())
        .is_err());
    assert_eq!(reopened.group_ids().unwrap(), vec!["persisted-oco"]);

    let mut changed = oco_state("persisted-oco");
    changed.intent.legs[0]
        .order
        .exact_amounts
        .as_mut()
        .unwrap()
        .conditional
        .as_mut()
        .unwrap()
        .trigger_price = Some(ExactDecimal::parse("71000").unwrap());
    changed.legs[0].order = changed.intent.legs[0].order.clone();
    assert!(store.create(&changed).is_err());

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn native_group_cancel_fence_survives_store_reopen() {
    let test_root = std::env::current_dir()
        .unwrap()
        .join("target")
        .join("test-temp");
    std::fs::create_dir_all(&test_root).unwrap();
    let root = test_root.join(format!(
        "qianxing-native-group-cancel-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let mut store = FileOrderGroupStore::new(&root).unwrap();
    let group = oco_state("cancel-fenced-oco");
    store.create(&group).unwrap();
    assert!(store.begin_submission("cancel-fenced-oco").unwrap());
    for leg in &group.legs {
        let mut submitted = leg.order.clone();
        submitted.status = OrderStatus::Submitted;
        store
            .apply_order_snapshot("cancel-fenced-oco", &submitted, None)
            .unwrap();
    }
    store
        .record_venue_group_id("cancel-fenced-oco", "okx-algo-123".into())
        .unwrap();
    store.activate_native_group("cancel-fenced-oco").unwrap();
    store.begin_native_cancel("cancel-fenced-oco").unwrap();

    let mut reopened = FileOrderGroupStore::new(&root).unwrap();
    let restored = reopened.load("cancel-fenced-oco").unwrap().unwrap();
    assert_eq!(restored.status, qx_core::OrderGroupStatus::CancelPending);
    assert_eq!(restored.venue_group_id.as_deref(), Some("okx-algo-123"));
    assert!(restored.legs.iter().all(|leg| leg.venue_order_id.is_none()));
    assert!(store.begin_native_cancel("cancel-fenced-oco").is_err());

    for leg in &restored.legs {
        let mut cancel_pending = leg.order.clone();
        cancel_pending.status = OrderStatus::CancelPending;
        reopened
            .apply_order_snapshot("cancel-fenced-oco", &cancel_pending, None)
            .unwrap();
    }
    let cancelling = reopened.load("cancel-fenced-oco").unwrap().unwrap();
    for leg in &cancelling.legs {
        let mut cancelled = leg.order.clone();
        cancelled.status = OrderStatus::Cancelled;
        reopened
            .apply_order_snapshot("cancel-fenced-oco", &cancelled, None)
            .unwrap();
    }
    reopened
        .complete_native_cancel("cancel-fenced-oco")
        .unwrap();
    assert_eq!(
        reopened.load("cancel-fenced-oco").unwrap().unwrap().status,
        qx_core::OrderGroupStatus::Cancelled
    );

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn loading_legacy_rejected_oco_repairs_status_when_a_peer_is_still_live() {
    let test_root = std::env::current_dir()
        .unwrap()
        .join("target")
        .join("test-temp");
    std::fs::create_dir_all(&test_root).unwrap();
    let root = test_root.join(format!(
        "qianxing-legacy-rejected-oco-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let mut store = FileOrderGroupStore::new(&root).unwrap();
    let mut legacy = oco_state("legacy-rejected-oco");
    legacy.status = qx_core::OrderGroupStatus::Rejected;
    legacy.venue_group_id = Some("okx-algo-legacy".into());
    legacy.legs[0].order.status = OrderStatus::Rejected;
    legacy.legs[1].order.status = OrderStatus::Working;
    legacy.legs[1].venue_order_id = Some("okx-child-live".into());
    store.create(&legacy).unwrap();

    let repaired = store.load("legacy-rejected-oco").unwrap().unwrap();
    assert_eq!(
        repaired.status,
        qx_core::OrderGroupStatus::PeerCancelRequired
    );
    assert_eq!(repaired.legs[0].order.status, OrderStatus::Rejected);
    assert_eq!(repaired.legs[1].order.status, OrderStatus::Working);
    let persisted: OrderGroupState =
        serde_json::from_slice(&std::fs::read(root.join("legacy-rejected-oco.json")).unwrap())
            .unwrap();
    assert_eq!(
        persisted.status,
        qx_core::OrderGroupStatus::PeerCancelRequired
    );

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn loading_legacy_rejected_bracket_preserves_live_protection_for_reconciliation() {
    let test_root = std::env::current_dir()
        .unwrap()
        .join("target")
        .join("test-temp");
    std::fs::create_dir_all(&test_root).unwrap();
    let root = test_root.join(format!(
        "qianxing-legacy-rejected-bracket-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let mut store = FileOrderGroupStore::new(&root).unwrap();
    let mut legacy = bracket_state("legacy-rejected-bracket");
    legacy.status = qx_core::OrderGroupStatus::Rejected;
    legacy.legs[0].order.status = OrderStatus::Rejected;
    legacy.legs[1].order.status = OrderStatus::Working;
    legacy.legs[1].venue_order_id = Some("venue-tp-live".into());
    legacy.legs[2].order.status = OrderStatus::Submitted;
    store.create(&legacy).unwrap();

    let repaired = store.load("legacy-rejected-bracket").unwrap().unwrap();
    assert_eq!(
        repaired.status,
        qx_core::OrderGroupStatus::ReconcileRequired
    );
    assert_eq!(repaired.legs[1].order.status, OrderStatus::Working);
    let persisted: OrderGroupState =
        serde_json::from_slice(&std::fs::read(root.join("legacy-rejected-bracket.json")).unwrap())
            .unwrap();
    assert_eq!(
        persisted.status,
        qx_core::OrderGroupStatus::ReconcileRequired
    );

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn loading_legacy_filled_bracket_with_dual_protection_fills_repairs_to_reconciliation() {
    let test_root = std::env::current_dir()
        .unwrap()
        .join("target")
        .join("test-temp");
    std::fs::create_dir_all(&test_root).unwrap();
    let root = test_root.join(format!(
        "qianxing-legacy-dual-filled-bracket-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let store = FileOrderGroupStore::new(&root).unwrap();
    let mut legacy = bracket_state("legacy-dual-filled-bracket");
    legacy.status = qx_core::OrderGroupStatus::Filled;
    for leg in &mut legacy.legs {
        leg.order.status = OrderStatus::Filled;
        leg.order.exact_amounts.as_mut().unwrap().filled = ExactDecimal::parse("0.01").unwrap();
    }
    // An older reducer accepted all three Filled child states as a successful
    // Bracket. Seed that historical snapshot directly so the load migration
    // must place it behind the reconciliation barrier.
    std::fs::write(
        root.join("legacy-dual-filled-bracket.json"),
        serde_json::to_vec_pretty(&legacy).unwrap(),
    )
    .unwrap();

    let repaired = store.load("legacy-dual-filled-bracket").unwrap().unwrap();
    assert_eq!(
        repaired.status,
        qx_core::OrderGroupStatus::ReconcileRequired
    );
    let persisted: OrderGroupState = serde_json::from_slice(
        &std::fs::read(root.join("legacy-dual-filled-bracket.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        persisted.status,
        qx_core::OrderGroupStatus::ReconcileRequired
    );

    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn loading_legacy_cancelled_bracket_with_open_position_repairs_to_reconciliation() {
    let test_root = std::env::current_dir()
        .unwrap()
        .join("target")
        .join("test-temp");
    std::fs::create_dir_all(&test_root).unwrap();
    let root = test_root.join(format!(
        "qianxing-legacy-unprotected-bracket-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let store = FileOrderGroupStore::new(&root).unwrap();
    let mut legacy = bracket_state("legacy-unprotected-bracket");
    legacy.status = qx_core::OrderGroupStatus::Cancelled;
    legacy.legs[0].order.status = OrderStatus::Cancelled;
    legacy.legs[0].order.exact_amounts.as_mut().unwrap().filled =
        ExactDecimal::parse("0.004").unwrap();
    for leg in &mut legacy.legs[1..] {
        leg.order.status = OrderStatus::Cancelled;
    }
    std::fs::write(
        root.join("legacy-unprotected-bracket.json"),
        serde_json::to_vec_pretty(&legacy).unwrap(),
    )
    .unwrap();

    let repaired = store.load("legacy-unprotected-bracket").unwrap().unwrap();
    assert_eq!(
        repaired.status,
        qx_core::OrderGroupStatus::ReconcileRequired
    );
    let persisted: OrderGroupState = serde_json::from_slice(
        &std::fs::read(root.join("legacy-unprotected-bracket.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        persisted.status,
        qx_core::OrderGroupStatus::ReconcileRequired
    );

    std::fs::remove_dir_all(root).unwrap();
}
