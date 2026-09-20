use super::*;

#[test]
fn paper_derivative_fill_uses_spec_pnl_accounting_instead_of_spot_cash() {
    let root = std::env::temp_dir().join(format!(
        "qianxing-execution-paper-derivative-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let instrument = InstrumentId::parse("BTC/USDT.BINANCE").unwrap();
    let order = Order {
        client_id: 3,
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
            leverage: 5,
            ..OrderPolicy::default()
        }),
    };
    let command = ControlCommand {
        command_id: order.client_id,
        request_id: "paper-derivative".into(),
        operator_id: "test".into(),
        reason: "paper derivative accounting".into(),
        kind: CommandKind::SubmitOrder,
        target: order.client_id.to_string(),
        payload: BTreeMap::from([("order_json".into(), serde_json::to_string(&order).unwrap())]),
        permission: Permission::Trading,
        dry_run: false,
    };
    let spec = TradingInstrumentSpec {
        instrument: instrument.clone(),
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
    let risk = RiskContext {
        available_margin_raw: Some(10_000 * SCALE),
        reference_price: order.limit,
        instrument_spec: Some(spec),
        ..RiskContext::default()
    };
    // 离线 smoke fixture：本用例没有行情事实，显式允许合成盘口；
    // 生产 Paper 路径必须传 EventLog 的最新行情并关闭该开关。
    let mut pipeline = LiveEventPipeline::open(&root, "events", "USDT").unwrap();
    let result = execute_paper_submit_effect(
        &command,
        &mut pipeline,
        1,
        Some(risk),
        Some(OrderRiskPosition::default()),
        None,
        true,
        None,
    )
    .unwrap();
    assert!(result.contains("fills=1"));
    assert_eq!(
        pipeline
            .ledger()
            .position_for("main", &instrument)
            .quantity
            .raw(),
        Quantity::from_i64(1).raw()
    );
    assert_eq!(pipeline.ledger().cash_for("main", "USDT"), 0);
    assert!(pipeline
        .ledger()
        .entries()
        .iter()
        .all(|entry| entry.kind != qx_core::LedgerEntryKind::TradeCash));
    let _ = std::fs::remove_dir_all(root);
}

/// P0 fail-closed：风控快照或行情事实缺失时禁止执行，且不得留下任何事实。
#[test]
fn paper_submit_fails_closed_without_risk_context_or_market_quote() {
    let root = std::env::temp_dir().join(format!(
        "qianxing-execution-fail-closed-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let order = Order {
        client_id: 7,
        instrument: InstrumentId::parse("BTC/USDT.BINANCE").unwrap(),
        side: Side::Buy,
        qty: Quantity::from_i64(1),
        limit: Some(Price::from_i64(100)),
        status: OrderStatus::PendingSubmit,
        filled: Quantity::ZERO,
        account_id: "main".into(),
        trace: None,
        policy: None,
    };
    let command = ControlCommand {
        command_id: 7,
        request_id: "fail-closed".into(),
        operator_id: "test".into(),
        reason: "fail closed paper".into(),
        kind: CommandKind::SubmitOrder,
        target: "7".into(),
        payload: BTreeMap::from([("order_json".into(), serde_json::to_string(&order).unwrap())]),
        permission: Permission::Trading,
        dry_run: false,
    };
    let risk = RiskContext {
        reference_price: order.limit,
        ..RiskContext::default()
    };
    let mut pipeline = LiveEventPipeline::open(&root, "events", "USDT").unwrap();
    let missing_risk =
        execute_paper_submit_effect(&command, &mut pipeline, 1, None, None, None, false, None)
            .unwrap_err();
    assert!(missing_risk.contains("FAIL_CLOSED: risk context missing"));
    // 只提供 RiskContext、缺持仓快照同样拒绝执行。
    let missing_position = execute_paper_submit_effect(
        &command,
        &mut pipeline,
        1,
        Some(risk.clone()),
        None,
        None,
        false,
        None,
    )
    .unwrap_err();
    assert!(missing_position.contains("FAIL_CLOSED: risk context missing"));
    // 风控快照齐备但缺行情事实时仍然拒绝撮合，不再伪造盘口价。
    let missing_quote = execute_paper_submit_effect(
        &command,
        &mut pipeline,
        1,
        Some(risk),
        Some(OrderRiskPosition::default()),
        None,
        false,
        None,
    )
    .unwrap_err();
    assert!(missing_quote.contains("FAIL_CLOSED: market quote missing"));
    // fail-closed 必须发生在写入任何事实之前：EventLog 仍为空。
    assert!(pipeline.orders().is_empty());
    assert!(pipeline.ledger().entries().is_empty());
    let restored = LiveEventPipeline::open(&root, "events", "USDT").unwrap();
    assert!(restored.ledger().entries().is_empty());
    let _ = std::fs::remove_dir_all(root);
}

/// Paper 入口必须沿用 ExecutionGateway 的幂等口径：同一 client_id 已有终态事实时
/// 直接返回 `ALREADY_APPLIED_FROM_EVENT_LOG`，且不再追加任何行情/成交事实。
#[test]
fn paper_submit_reuses_gateway_idempotency_without_new_facts() {
    let root = std::env::temp_dir().join(format!(
        "qianxing-execution-paper-idempotency-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let instrument = InstrumentId::parse("BTC/USDT.BINANCE").unwrap();
    let order = Order {
        client_id: 71,
        instrument: instrument.clone(),
        side: Side::Buy,
        qty: Quantity::from_i64(1),
        limit: Some(Price::from_i64(100)),
        status: OrderStatus::PendingSubmit,
        filled: Quantity::ZERO,
        account_id: "main".into(),
        trace: None,
        policy: None,
    };
    let command = ControlCommand {
        command_id: 71,
        request_id: "paper-idempotent".into(),
        operator_id: "test".into(),
        reason: "gateway idempotency".into(),
        kind: CommandKind::SubmitOrder,
        target: "71".into(),
        payload: BTreeMap::from([("order_json".into(), serde_json::to_string(&order).unwrap())]),
        permission: Permission::Trading,
        dry_run: false,
    };
    let risk = RiskContext {
        available_margin_raw: Some(10_000 * SCALE),
        reference_price: order.limit,
        instrument_spec: Some(TradingInstrumentSpec {
            instrument: instrument.clone(),
            product: TradingProduct::Spot,
            base_currency: "BTC".into(),
            quote_currency: "USDT".into(),
            settlement_currency: "USDT".into(),
            contract_size: SCALE,
            linear: false,
            inverse: false,
            price_tick: 1,
            qty_step: 1,
            min_qty: 1,
            max_leverage: 1,
            maintenance_margin_bps: 0,
            valid_from: 1,
            valid_to: None,
        }),
        ..RiskContext::default()
    };
    let quote = QuoteTick::new(
        2,
        Price::from_i64(99),
        Quantity::from_i64(1_000),
        Price::from_i64(100),
        Quantity::from_i64(1_000),
        7,
    );
    let mut pipeline = LiveEventPipeline::open(&root, "events", "USDT").unwrap();
    let executed = execute_paper_submit_effect(
        &command,
        &mut pipeline,
        1,
        Some(risk.clone()),
        Some(OrderRiskPosition::default()),
        Some(quote),
        false,
        None,
    )
    .unwrap();
    assert!(executed.starts_with("PAPER_EXECUTED fills=1"), "{executed}");
    let orders = pipeline.orders().len();
    let entries = pipeline.ledger().entries().len();
    let replayed = execute_paper_submit_effect(
        &command,
        &mut pipeline,
        1,
        Some(risk),
        Some(OrderRiskPosition::default()),
        Some(quote),
        false,
        None,
    )
    .unwrap();
    assert!(
        replayed.starts_with("ALREADY_APPLIED_FROM_EVENT_LOG"),
        "Paper 重投必须走 gateway 幂等分支: {replayed}"
    );
    assert_eq!(pipeline.orders().len(), orders);
    assert_eq!(pipeline.ledger().entries().len(), entries);
    let _ = std::fs::remove_dir_all(root);
}
