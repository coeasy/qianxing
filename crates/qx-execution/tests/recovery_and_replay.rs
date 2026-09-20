//! 真实管线侧的恢复/回放用例。V10 P2a 自 `src/tests/recovery_and_replay.rs` 搬入
//! 其中依赖 `qx_runtime::LiveEventPipeline` 的两条（原因见 `paper_accounting.rs`
//! 头注）；hedge 恢复用例不跨 crate，仍留在单元测试里。用例函数与断言未改动。

use qx_control::{CommandKind, ControlCommand, Permission};
use qx_core::{
    InstrumentId, MarginMode, Order, OrderPolicy, OrderStatus, PositionMode, Price, Quantity, Side,
    TradingInstrumentSpec, TradingProduct, SCALE,
};
use qx_execution::{ingest_venue_events, submit_order_with_risk, RiskExecutionContext};
use qx_risk::OrderRiskPosition;
use qx_runtime::LiveEventPipeline;
use qx_zhenlu::{PaperVenue, RiskContext, Venue, VenueEvent};
use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

#[test]
fn accepted_events_replay_by_correlation_even_when_local_sequence_changes() {
    let root = std::env::temp_dir().join(format!(
        "qianxing-execution-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let mut pipeline = LiveEventPipeline::open(&root, "events", "USDT").unwrap();
    pipeline
        .register_order(
            Order {
                client_id: 1,
                instrument: InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
                side: Side::Buy,
                qty: Quantity::from_i64(1),
                limit: None,
                status: OrderStatus::PendingSubmit,
                filled: Quantity::ZERO,
                account_id: "main".into(),
                trace: None,
                policy: None,
            },
            1,
        )
        .unwrap();
    let event = VenueEvent::Accepted {
        client_order_id: 1,
        venue_order_id: "venue-1".into(),
        ts: 2,
    };
    let mut first_seq = 0;
    let first = ingest_venue_events(
        &mut pipeline,
        vec![event.clone()],
        "execution",
        2,
        &mut first_seq,
    )
    .unwrap();
    let mut second_seq = 10;
    let second =
        ingest_venue_events(&mut pipeline, vec![event], "execution", 3, &mut second_seq).unwrap();
    assert_eq!(first, 1);
    assert_eq!(second, 1);
    assert_eq!(pipeline.log().len(), 2);
    assert_eq!(pipeline.orders()[0].status, OrderStatus::Accepted);
    assert_eq!(pipeline.venue_order_id(1).as_deref(), Some("venue-1"));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn risk_preflight_rejects_before_event_log_or_venue_side_effect() {
    let root = std::env::temp_dir().join(format!(
        "qianxing-execution-risk-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let instrument = InstrumentId::parse("BTC/USDT.BINANCE").unwrap();
    let order = Order {
        client_id: 2,
        instrument: instrument.clone(),
        side: Side::Buy,
        qty: Quantity::from_i64(1),
        limit: Some(Price::from_i64(100)),
        status: OrderStatus::PendingSubmit,
        filled: Quantity::ZERO,
        account_id: "main".into(),
        trace: None,
        policy: Some(OrderPolicy {
            margin_mode: MarginMode::Cross,
            position_mode: PositionMode::OneWay,
            leverage: 10,
            ..OrderPolicy::default()
        }),
    };
    let command = ControlCommand {
        command_id: 2,
        request_id: "risk-preflight".into(),
        operator_id: "test".into(),
        reason: "risk preflight".into(),
        kind: CommandKind::SubmitOrder,
        target: "2".into(),
        payload: BTreeMap::from([("order_json".into(), serde_json::to_string(&order).unwrap())]),
        permission: Permission::Trading,
        dry_run: false,
    };
    let spec = TradingInstrumentSpec {
        instrument,
        product: TradingProduct::Perpetual,
        base_currency: "BTC".into(),
        quote_currency: "USDT".into(),
        settlement_currency: "USDT".into(),
        contract_size: SCALE,
        linear: true,
        inverse: false,
        price_tick: 1,
        qty_step: 1,
        min_qty: 1,
        max_leverage: 20,
        maintenance_margin_bps: 500,
        valid_from: 1,
        valid_to: None,
    };
    let required = spec
        .initial_margin(order.qty.raw(), order.limit.unwrap().raw(), 10)
        .unwrap();
    let risk = RiskContext {
        available_margin_raw: Some(required - 1),
        reference_price: order.limit,
        instrument_spec: Some(spec),
        ..RiskContext::default()
    };
    let mut pipeline = LiveEventPipeline::open(&root, "events", "USDT").unwrap();
    let mut venue = PaperVenue::new("paper");
    let mut source_seq = 0;
    let risk_context = RiskExecutionContext {
        risk: &risk,
        position: &OrderRiskPosition::default(),
    };
    let result = submit_order_with_risk(
        &command,
        &mut venue,
        &mut pipeline,
        "execution",
        1,
        &mut source_seq,
        &risk_context,
        None,
    );
    assert!(result.is_err());
    assert!(pipeline.orders().is_empty());
    assert!(venue.snapshot().is_empty());
    let _ = std::fs::remove_dir_all(root);
}
