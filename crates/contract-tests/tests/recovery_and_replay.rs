//! 真实管线侧的恢复/回放用例。V10 P2a 自 `src/tests/recovery_and_replay.rs` 搬入
//! 其中依赖 `qx_runtime::LiveEventPipeline` 的两条（原因见 `paper_accounting.rs`
//! 头注）；hedge 恢复用例不跨 crate，仍留在单元测试里。用例函数与断言未改动。

use qx_control::{CommandKind, ControlCommand, Permission};
use qx_core::{
    InstrumentId, MarginMode, Order, OrderPolicy, OrderStatus, PositionMode, Price, Quantity, Side,
    TradingInstrumentSpec, TradingProduct, SCALE,
};
use qx_execution::{
    ingest_venue_events, submit_order_with_risk, ExecutionEvent, ExecutionGateway,
    RiskExecutionContext, VenuePort, PAPER_VENUE_CAPABILITIES,
};
use qx_risk::OrderRiskPosition;
use qx_runtime::LiveEventPipeline;
use qx_zhenlu::{PaperVenue, RiskContext, Venue, VenueEvent};
use std::collections::BTreeMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::time::{SystemTime, UNIX_EPOCH};

/// 只断言回报形状 / 恢复事实的用例显式零费：费用不是这里的期望，但生产 Paper
/// 路径的成本口径必须由调用方给出（见 `PaperVenue::new`）。
fn zero_fee() -> Box<dyn qx_core::FeeModel + Send> {
    Box::new(qx_core::ZeroFeeModel)
}

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
                exact_amounts: None,
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
fn submit_side_effect_then_process_exit_recovers_as_ambiguous_and_never_resubmits() {
    struct CrashAfterRemoteAccept {
        calls: Arc<AtomicUsize>,
        crash_after_side_effect: bool,
    }
    impl VenuePort for CrashAfterRemoteAccept {
        fn venue_id(&self) -> &str {
            "PAPER"
        }

        fn capabilities(&self) -> qx_zhenlu::venue_capabilities::VenueCapabilities {
            PAPER_VENUE_CAPABILITIES
        }

        fn execution_readiness(&self) -> Result<(), String> {
            Ok(())
        }

        fn submit_order(&mut self, order: Order, _ts: u64) -> Result<Vec<ExecutionEvent>, String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.crash_after_side_effect {
                panic!("simulated process exit after remote accept, before response");
            }
            Ok(vec![ExecutionEvent::Accepted {
                client_order_id: order.client_id,
                venue_order_id: format!("remote-{}", order.client_id),
            }])
        }

        fn cancel_order(
            &mut self,
            client_order_id: u64,
            _ts: u64,
        ) -> Result<Vec<ExecutionEvent>, String> {
            Ok(vec![ExecutionEvent::Cancelled { client_order_id }])
        }
    }

    let root = std::env::temp_dir().join(format!(
        "qianxing-submit-crash-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let order = Order {
        client_id: 301,
        instrument: InstrumentId::parse("BTC/USDT.BINANCE").unwrap(),
        side: Side::Buy,
        qty: Quantity::from_i64(1),
        limit: None,
        status: OrderStatus::PendingSubmit,
        filled: Quantity::ZERO,
        account_id: "main".into(),
        trace: None,
        policy: None,
        exact_amounts: None,
    };
    let calls = Arc::new(AtomicUsize::new(0));

    let mut first_pipeline = LiveEventPipeline::open(&root, "events", "USDT").unwrap();
    let mut first_venue = CrashAfterRemoteAccept {
        calls: Arc::clone(&calls),
        crash_after_side_effect: true,
    };
    let mut source_seq = 0;
    let first_attempt = catch_unwind(AssertUnwindSafe(|| {
        ExecutionGateway::new(
            &mut first_venue,
            &mut first_pipeline,
            "submit-crash-worker",
            10,
            &mut source_seq,
        )
        .submit(order.clone())
    }));
    assert!(
        first_attempt.is_err(),
        "the simulated process must exit after the venue side effect; got {first_attempt:?}"
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "remote side effect happened once"
    );
    assert_eq!(
        first_pipeline.orders()[0].status,
        OrderStatus::Submitted,
        "the durable submit intent precedes the remote call"
    );
    drop(first_pipeline);

    let mut recovered_pipeline = LiveEventPipeline::open(&root, "events", "USDT").unwrap();
    assert_eq!(
        recovered_pipeline.orders()[0].status,
        OrderStatus::Submitted
    );
    let mut recovered_venue = CrashAfterRemoteAccept {
        calls: Arc::clone(&calls),
        crash_after_side_effect: false,
    };
    let error = ExecutionGateway::new(
        &mut recovered_venue,
        &mut recovered_pipeline,
        "submit-recovery-worker",
        11,
        &mut source_seq,
    )
    .submit(order)
    .expect_err("ambiguous submit must be reconciled before retry");
    assert!(error.contains("必须先对账"), "{error}");
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "recovery must not resubmit"
    );
    assert_eq!(recovered_pipeline.orders()[0].status, OrderStatus::Unknown);

    drop(recovered_pipeline);
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
        exact_amounts: None,
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
        min_notional_raw: 0,
        max_leverage: 20,
        maintenance_margin_bps: 500,
        valid_from: 1,
        valid_to: None,
        exact_rules: None,
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
    let mut venue = PaperVenue::new("paper", zero_fee());
    let mut source_seq = 0;
    let risk_context = RiskExecutionContext {
        risk: &risk,
        position: &OrderRiskPosition::default(),
    };
    let result = submit_order_with_risk(qx_execution::SubmitOrderWithRiskRequest {
        command: &command,
        venue: &mut venue,
        pipeline: &mut pipeline,
        worker_id: "execution",
        now: 1,
        source_seq: &mut source_seq,
        context: &risk_context,
        spread_store: None,
    });
    assert!(result.is_err());
    assert!(pipeline.orders().is_empty());
    assert!(venue.snapshot().is_empty());
    let _ = std::fs::remove_dir_all(root);
}
