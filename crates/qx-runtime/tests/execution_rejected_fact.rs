use qx_core::{EventKind, InstrumentId, Order, OrderStatus, Price, Quantity, Side};
use qx_execution::{EventAppender, ExecutionEvent, ExecutionEventEnvelope};
use qx_runtime::LiveEventPipeline;
use std::time::{SystemTime, UNIX_EPOCH};

#[test]
fn execution_rejection_is_persisted_and_replayed_into_oms() {
    let root = std::env::current_dir()
        .unwrap()
        .join("target")
        .join("test-temp")
        .join(format!(
            "qx-runtime-rejected-fact-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
    std::fs::create_dir_all(&root).unwrap();
    let mut pipeline = LiveEventPipeline::open(&root, "port-main", "USDT").unwrap();
    pipeline
        .register_order(
            Order {
                client_id: 7001,
                instrument: InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
                side: Side::Sell,
                qty: Quantity::from_i64(1),
                limit: Some(Price::from_i64(100)),
                status: OrderStatus::PendingSubmit,
                filled: Quantity::ZERO,
                account_id: "port-main".into(),
                trace: None,
                policy: None,
                exact_amounts: None,
            },
            100,
        )
        .unwrap();

    pipeline
        .append_execution_event(ExecutionEventEnvelope {
            event: ExecutionEvent::Rejected {
                client_order_id: 7001,
                reason: "filter failure".into(),
            },
            event_ts: 101,
            receive_ts: 101,
            source_seq: 1,
            correlation_id: "group:rejected:7001".into(),
        })
        .unwrap();

    assert_eq!(pipeline.orders()[0].status, OrderStatus::Rejected);
    assert!(pipeline.log().events().iter().any(|event| matches!(
        &event.kind,
        EventKind::Rejected {
            client_order_id: 7001,
            reason
        } if reason == "filter failure"
    )));
    let reopened = LiveEventPipeline::open(&root, "port-main", "USDT").unwrap();
    assert_eq!(reopened.orders()[0].status, OrderStatus::Rejected);

    std::fs::remove_dir_all(root).unwrap();
}
