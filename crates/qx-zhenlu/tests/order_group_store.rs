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
    assert!(
        store
            .record_venue_group_id("persisted-oco", "conflicting-list".into())
            .is_err()
    );
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
        reopened
            .load("cancel-fenced-oco")
            .unwrap()
            .unwrap()
            .status,
        qx_core::OrderGroupStatus::Cancelled
    );

    std::fs::remove_dir_all(root).unwrap();
}
