use super::*;

#[test]
fn hedge_recovery_worker_is_idempotent_and_fails_closed_on_unknown_state() {
    let root = std::env::temp_dir().join(format!(
        "qianxing-hedge-recovery-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let mut first = port_order(401);
    first.side = Side::Buy;
    let mut second = port_order(402);
    second.side = Side::Sell;
    let mut group = SpreadOrderGroup::new(
        "hedge-recovery-1",
        "basis-recovery",
        vec![
            SpreadOrderLeg {
                leg_id: "spot".into(),
                venue_id: "binance".into(),
                order: first,
            },
            SpreadOrderLeg {
                leg_id: "future".into(),
                venue_id: "okx".into(),
                order: second,
            },
        ],
    )
    .unwrap();
    group.begin_submission().unwrap();
    group.record_accepted("spot").unwrap();
    group
        .record_fill(
            "spot",
            &qx_core::Fill {
                order_id: 401,
                qty: Quantity::from_i64(1),
                price: Price::from_i64(100),
                ..qx_core::Fill::default()
            },
        )
        .unwrap();
    group.record_cancelled("future").unwrap();
    assert_eq!(group.status, SpreadOrderGroupStatus::HedgeRequired);
    let mut risk_blocked_group = group.clone();
    risk_blocked_group.group_id = "hedge-recovery-risk-blocked".into();

    let mut state = PortState::default();
    let mut router = PortRouter::default();
    let mut store = FileSpreadOrderGroupStore::new(root.join("groups")).unwrap();
    let other_token = store
        .try_claim("hedge-recovery-1", "other-worker", 500, 30_000)
        .unwrap()
        .expect("other worker should acquire claim");
    let mut blocked_state = PortState::default();
    let mut blocked_router = PortRouter::default();
    let mut blocked_seq = 0;
    let blocked = HedgeRecoveryWorker::new(
        group.clone(),
        &mut store,
        &mut blocked_router,
        &mut blocked_state,
        "hedge-worker",
        501,
        &mut blocked_seq,
    )
    .unwrap()
    .execute()
    .unwrap();
    assert!(!blocked.completed);
    assert!(blocked.errors[0].contains("其他恢复 owner"));
    store
        .release_claim("hedge-recovery-1", "other-worker", other_token)
        .unwrap();
    let mut source_seq = 0;
    let outcome = HedgeRecoveryWorker::new(
        group,
        &mut store,
        &mut router,
        &mut state,
        "hedge-worker",
        500,
        &mut source_seq,
    )
    .unwrap()
    .execute()
    .unwrap();
    assert!(outcome.completed);
    assert_eq!(outcome.attempted, 1);
    assert_eq!(outcome.group.status, SpreadOrderGroupStatus::Hedged);
    assert_eq!(router.calls, vec!["binance"]);
    assert_eq!(state.orders.len(), 1);
    assert!(state.orders[0].policy.unwrap().reduce_only);
    let persisted = store.load("hedge-recovery-1").unwrap().unwrap();
    assert_eq!(persisted.status, SpreadOrderGroupStatus::Hedged);

    let mut guarded_state = PortState::default();
    let mut guarded_router = PortRouter::default();
    let mut guarded_store = FileSpreadOrderGroupStore::new(root.join("guarded-groups")).unwrap();
    let guarded_validator =
        |_order: &Order| -> Result<(), String> { Err("risk snapshot unavailable".into()) };
    let guarded = HedgeRecoveryWorker::new(
        risk_blocked_group,
        &mut guarded_store,
        &mut guarded_router,
        &mut guarded_state,
        "hedge-worker",
        500,
        &mut source_seq,
    )
    .unwrap()
    .execute_with_validator(Some(&guarded_validator))
    .unwrap();
    assert!(!guarded.completed);
    assert!(guarded.errors[0].contains("风控"));
    assert!(guarded_router.calls.is_empty());
    assert_eq!(guarded.attempted, 0);

    let mut retry_state = PortState::default();
    let mut retry_router = PortRouter::default();
    let mut retry_seq = source_seq;
    let retry = HedgeRecoveryWorker::new(
        outcome.group.clone(),
        &mut store,
        &mut retry_router,
        &mut retry_state,
        "hedge-worker",
        501,
        &mut retry_seq,
    )
    .unwrap()
    .execute()
    .unwrap();
    assert!(retry.completed);
    assert!(retry_router.calls.is_empty());

    let mut unknown = outcome.group;
    unknown.group_id = "hedge-recovery-unknown".into();
    unknown.status = SpreadOrderGroupStatus::ReconcileRequired;
    let mut unknown_state = PortState::default();
    let mut unknown_router = PortRouter::default();
    let mut unknown_seq = retry_seq;
    let blocked = HedgeRecoveryWorker::new(
        unknown,
        &mut store,
        &mut unknown_router,
        &mut unknown_state,
        "hedge-worker",
        502,
        &mut unknown_seq,
    )
    .unwrap()
    .execute()
    .unwrap();
    assert!(!blocked.completed);
    assert!(blocked.errors[0].contains("对账"));
    assert!(unknown_router.calls.is_empty());
    let _ = std::fs::remove_dir_all(root);
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
