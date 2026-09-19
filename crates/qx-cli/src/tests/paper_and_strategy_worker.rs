use super::*;

#[test]
fn paper_submit_order_runs_queue_pipeline_ledger_and_ack() {
    let root = std::env::temp_dir().join(format!(
        "qianxing-cli-paper-submit-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let data_dir = root.join("data");
    std::fs::create_dir_all(&root).unwrap();
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    let template = workspace_root
        .join("deploy")
        .join("qianxing.runtime.paper-strategy.example.json");
    let mut config = read_runtime_config(&template).unwrap();
    config.storage.data_dir = data_dir.to_string_lossy().into_owned();
    // runtime.json 写到临时目录后，相对 spec 会被解析到临时目录之外；
    // 这里固定为工作区内已验证的 BTCUSDT 现货规格，保证账户级风控可加载。
    for worker in config.workers.iter_mut() {
        if worker.instrument_spec_path.is_some() {
            worker.instrument_spec_path = Some(
                workspace_root
                    .join("deploy")
                    .join("qianxing.binance.spot.spec.json")
                    .to_string_lossy()
                    .into_owned(),
            );
        }
    }
    let config_path = root.join("runtime.json");
    std::fs::write(&config_path, config.to_json().unwrap()).unwrap();
    // fail-closed 语义要求撮合行情来自 EventLog 行情事实，先注入可审计 L1 报价。
    {
        let mut pipeline = LiveEventPipeline::open(&data_dir, "paper-events", "USDT").unwrap();
        let market_ts = runtime_timestamp_ms();
        pipeline
            .ingest(RuntimeEventEnvelope::market_quote(
                InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
                QuoteTick::new(
                    market_ts,
                    Price::from_i64(99),
                    Quantity::from_i64(1_000),
                    Price::from_i64(100),
                    Quantity::from_i64(1_000),
                    market_ts,
                ),
                market_ts,
                market_ts,
                "paper-events:submit-order-test-quote",
            ))
            .unwrap();
    }
    let order = mk_order(
        8001,
        &InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
        Side::Buy,
        1,
    );
    let command = ControlCommand {
        command_id: 8001,
        request_id: "paper-submit-8001".into(),
        operator_id: "paper".into(),
        reason: "paper execution integration".into(),
        kind: CommandKind::SubmitOrder,
        target: "8001".into(),
        payload: BTreeMap::from([("order_json".into(), serde_json::to_string(&order).unwrap())]),
        permission: Permission::Trading,
        dry_run: false,
    };
    let command_path = root.join("command.json");
    std::fs::write(&command_path, serde_json::to_string(&command).unwrap()).unwrap();
    run_paper_submit_order(&config_path, &command_path).unwrap();
    let state = load_control_state(&data_dir).unwrap();
    assert_eq!(state.audit().len(), 2);
    let pipeline = LiveEventPipeline::open(&data_dir, "paper-events", "USDT").unwrap();
    // 初始资金 + 成交双 Ledger 事实；行情注入不产生账本条目。
    assert_eq!(pipeline.ledger().entries().len(), 3);
    assert_eq!(pipeline.orders()[0].status, OrderStatus::Filled);
    assert!(
        qx_storage::ControlCommandQueue::new(data_dir.join("control-queue"))
            .pending()
            .unwrap()
            .is_empty()
    );
    let _ = std::fs::remove_dir_all(root);
}

/// 跨腿屏障的 worker 级证据：腿 A 结果未知后，Paper 执行 worker 必须拒绝腿 B 的
/// SubmitOrder 命令。撮合行情与风控配置在此都是齐备的，所以唯一的拒绝理由就是屏障
/// 本身——摘掉 `spread_group_barrier` 后本例会变成 Filled 订单而不是 Failed 命令。
#[test]
fn paper_worker_refuses_next_leg_when_spread_group_awaits_reconciliation() {
    let root = std::env::temp_dir().join(format!(
        "qianxing-cli-spread-barrier-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let data_dir = root.join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    let mut config = read_runtime_config(
        &workspace_root
            .join("deploy")
            .join("qianxing.runtime.paper-strategy.example.json"),
    )
    .unwrap();
    config.storage.data_dir = data_dir.to_string_lossy().into_owned();
    for worker in config.workers.iter_mut() {
        if worker.instrument_spec_path.is_some() {
            worker.instrument_spec_path = Some(
                workspace_root
                    .join("deploy")
                    .join("qianxing.binance.spot.spec.json")
                    .to_string_lossy()
                    .into_owned(),
            );
        }
    }
    let config_path = root.join("runtime.json");
    std::fs::write(&config_path, config.to_json().unwrap()).unwrap();

    let log_name = "paper-main-paper-events";
    let market_ts = runtime_timestamp_ms();
    LiveEventPipeline::open(&data_dir, log_name, "USDT")
        .unwrap()
        .ingest(RuntimeEventEnvelope::market_quote(
            InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
            QuoteTick::new(
                market_ts,
                Price::from_i64(99),
                Quantity::from_i64(1_000),
                Price::from_i64(100),
                Quantity::from_i64(1_000),
                market_ts,
            ),
            market_ts,
            market_ts,
            "paper-events:spread-barrier-test-quote",
        ))
        .unwrap();

    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let blocked_leg = mk_order(9401, &instrument, Side::Buy, 1);
    let pending_leg = mk_order(9402, &instrument, Side::Sell, 1);
    let group_id = spread_group_id("strategy-paper", 1, 1);
    persist_strategy_spread_group(
        &data_dir,
        &group_id,
        "strategy-paper",
        &[blocked_leg.clone(), pending_leg.clone()],
    )
    .unwrap();
    // 生产归约路径把腿 A 置为 Unknown 后落盘的就是这个状态，这里直接复现它。
    let mut group_store = FileSpreadOrderGroupStore::new(data_dir.join("spread-groups")).unwrap();
    let mut group = group_store.load(&group_id).unwrap().unwrap();
    group.begin_submission().unwrap();
    group.record_unknown("leg-9401").unwrap();
    group_store.save(&group).unwrap();
    assert_eq!(
        group.status,
        qx_zhenlu::SpreadOrderGroupStatus::ReconcileRequired
    );

    let command =
        strategy_submit_command("strategy-paper", &pending_leg, false, Some(&group_id)).unwrap();
    let control = ControlStateBackend::Files(JsonStateStore::new(&data_dir));
    control
        .transact(|plane| plane.submit_as(command.clone(), Permission::Trading, 10))
        .unwrap()
        .1
        .unwrap();
    let queue = ControlCommandQueue::new(data_dir.join("control-queue"));
    queue.enqueue(command.clone(), 10).unwrap();

    run_paper_execution_worker(&config_path, "paper-execution", true).unwrap();

    let state = load_control_state(&data_dir).unwrap();
    let record = state
        .audit()
        .iter()
        .rev()
        .find(|record| record.command_id == command.command_id)
        .expect("屏障拒绝也必须留下审计记录");
    assert_eq!(record.status, CommandStatus::Failed);
    assert!(
        record.result_code.contains("FAIL_CLOSED") && record.result_code.contains(&group_id),
        "拒绝原因必须来自跨腿屏障: {}",
        record.result_code
    );
    let pipeline = LiveEventPipeline::open(&data_dir, log_name, "USDT").unwrap();
    assert!(
        pipeline
            .orders()
            .iter()
            .all(|order| order.client_id != pending_leg.client_id),
        "被屏障拒绝的腿不得留下任何订单事实"
    );
    assert!(queue.pending().unwrap().is_empty());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn strategy_worker_executes_pause_command_through_persistent_queue() {
    let root = std::env::temp_dir().join(format!(
        "qianxing-cli-strategy-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let data_dir = root.join("data");
    std::fs::create_dir_all(&root).unwrap();
    let template = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy")
        .join("qianxing.runtime.example.json");
    let mut config = read_runtime_config(&template).unwrap();
    config.storage.data_dir = data_dir.to_string_lossy().into_owned();
    let config_path = root.join("runtime.json");
    std::fs::write(&config_path, config.to_json().unwrap()).unwrap();

    let command = ControlCommand {
        command_id: 9001,
        request_id: "pause-strategy-9001".into(),
        operator_id: "ops".into(),
        reason: "strategy control integration".into(),
        kind: CommandKind::PauseStrategy,
        target: "strategy-paper".into(),
        payload: BTreeMap::new(),
        permission: Permission::Trading,
        dry_run: false,
    };
    let store = JsonStateStore::new(&data_dir);
    let (_, accepted) = store
        .transact_control(|plane| plane.submit_as(command.clone(), Permission::Trading, 1))
        .unwrap();
    assert!(accepted.is_ok());
    ControlCommandQueue::new(data_dir.join("control-queue"))
        .enqueue(command, 1)
        .unwrap();

    run_strategy_worker(&config_path, "strategy-paper", true).unwrap();
    let state = load_control_state(&data_dir).unwrap();
    assert_eq!(state.audit().len(), 2);
    assert_eq!(state.audit()[1].status, CommandStatus::Executed);
    assert_eq!(state.audit()[1].result_code, "STRATEGY_PAUSED");
    assert!(ControlCommandQueue::new(data_dir.join("control-queue"))
        .pending()
        .unwrap()
        .is_empty());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn strategy_signal_portfolio_risk_emits_idempotent_submit_order() {
    let root = std::env::temp_dir().join(format!(
        "qianxing-cli-strategy-order-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let data_dir = root.join("data");
    std::fs::create_dir_all(&root).unwrap();
    let template = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy")
        .join("qianxing.runtime.example.json");
    let mut config = read_runtime_config(&template).unwrap();
    config.storage.data_dir = data_dir.to_string_lossy().into_owned();
    config.strategy.account_id = Some("main".into());
    config.strategy.venue_id = Some("binance-testnet".into());
    config.strategy.instrument = Some("BTCUSDT.BINANCE".into());
    config.strategy.target_qty = 1;
    let order = build_strategy_order(
        &config,
        "strategy-paper",
        9101,
        10,
        0,
        config.strategy.target_qty,
    )
    .unwrap()
    .unwrap();
    let command = strategy_submit_command("strategy-paper", &order, false, None).unwrap();
    let store = ControlStateBackend::Files(JsonStateStore::new(&data_dir));
    let queue = ControlCommandQueue::new(data_dir.join("control-queue"));
    let result = persist_strategy_submit(&store, &queue, &command, 10).unwrap();
    assert_eq!(result, "ORDER_INTENT_ACCEPTED");
    let retry = persist_strategy_submit(&store, &queue, &command, 11).unwrap();
    assert_eq!(retry, "ORDER_INTENT_ALREADY_ACCEPTED");
    let state = load_control_state(&data_dir).unwrap();
    assert_eq!(state.audit().len(), 1);
    assert_eq!(queue.pending().unwrap().len(), 1);
    let queued = queue.pending().unwrap().pop().unwrap();
    assert_eq!(queued.command.command_id, 9101);
    assert!(queued.command.payload.contains_key("order_json"));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn strategy_target_zero_emits_close_order_for_existing_position() {
    let template = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy")
        .join("qianxing.runtime.example.json");
    let mut config = read_runtime_config(&template).unwrap();
    config.strategy.account_id = Some("main".into());
    config.strategy.venue_id = Some("binance-testnet".into());
    config.strategy.instrument = Some("BTCUSDT.BINANCE".into());
    config.strategy.target_qty = 0;

    let order = build_strategy_order(&config, "strategy-close", 9102, 10, 2, 0)
        .unwrap()
        .expect("target zero must close an existing position");
    assert_eq!(order.side, Side::Sell);
    assert_eq!(order.qty.raw(), 2);
}

#[test]
fn strategy_worker_reads_candidate_factor_bundle_as_context() {
    let root = std::env::temp_dir().join(format!(
        "qianxing-cli-research-context-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let data_dir = root.join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let template = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy")
        .join("qianxing.runtime.example.json");
    let mut config = read_runtime_config(&template).unwrap();
    config.storage.data_dir = data_dir.to_string_lossy().into_owned();
    config.strategy.target_qty = 0;
    config.strategy.target_snapshot_path = None;
    config.strategy.research_snapshot_path =
        Some(root.join("research.json").to_string_lossy().into_owned());
    config.strategy.research_data_fingerprint = Some("bars-1".into());
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let mut catalog = FactorCatalog::default();
    catalog
        .register_definition(FeatureDefinition {
            name: "momentum".into(),
            version: "v1".into(),
            formula: "close / close[-20] - 1".into(),
            input_fields: vec!["close".into()],
            dependencies: Vec::new(),
            point_in_time: true,
        })
        .unwrap();
    let artifact = FeatureArtifact {
        feature_key: "momentum@v1".into(),
        input_fingerprint: "bars-1".into(),
        as_of: 20,
        coverage_bps: 10_000,
        values: [(instrument.clone(), 100)].into_iter().collect(),
    };
    let report = qx_factor::FactorReport {
        feature_key: "momentum@v1".into(),
        input_fingerprint: "bars-1".into(),
        observation_hash: 1,
        analysis_start: 1,
        analysis_end: 20,
        sample_count: 1,
        coverage_bps: 10_000,
        ic_bps: 1,
        rank_ic_bps: 1,
        turnover_bps: 1,
        transform: None,
        missing_policy: "reject".into(),
        decay_bps: 0,
        capacity_raw: 1,
        exposures: BTreeMap::new(),
    };
    catalog.publish_artifact(artifact.clone()).unwrap();
    catalog.publish_report(report.clone()).unwrap();
    let candidate = catalog
        .bind_candidate(CandidateRequest {
            strategy_version: config.strategy.version.clone(),
            universe_version: "universe-v1".into(),
            parameters: qx_guanxing::ParameterSet::default(),
            data_fingerprint: "bars-1".into(),
            factor_keys: vec!["momentum@v1".into()],
            cost_bps: 1,
            train_start: 1,
            train_end: 10,
            validation_start: 11,
            validation_end: 20,
            intended_exposure: [(instrument.clone(), 2)].into_iter().collect(),
            constraints: BTreeMap::new(),
            execution_model: "event-backtest@v1".into(),
            risk_model: "default-risk@v1".into(),
        })
        .unwrap();
    let research = StrategyResearchSnapshot {
        schema_version: StrategyResearchSnapshot::SCHEMA_VERSION,
        candidate,
        artifacts: vec![artifact],
        reports: vec![report],
        as_of: 20,
    };
    std::fs::write(
        config.strategy.research_snapshot_path.as_ref().unwrap(),
        research.to_json().unwrap(),
    )
    .unwrap();
    assert_eq!(
        strategy_target_qty(&data_dir, &config, &instrument, 21).unwrap(),
        2
    );
    config.strategy.research_data_fingerprint = Some("wrong-fingerprint".into());
    assert!(strategy_target_qty(&data_dir, &config, &instrument, 21).is_err());
    config.strategy.research_data_fingerprint = Some("bars-1".into());
    config.strategy.version = "wrong-version".into();
    assert!(strategy_target_qty(&data_dir, &config, &instrument, 21).is_err());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn derivative_strategy_emits_leveraged_short_policy_without_no_short_rule() {
    let template = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy")
        .join("qianxing.runtime.example.json");
    let mut config = read_runtime_config(&template).unwrap();
    config.strategy.product = Some(TradingProduct::Perpetual);
    config.strategy.margin_mode = Some(MarginMode::Isolated);
    config.strategy.position_mode = Some(PositionMode::OneWay);
    config.strategy.leverage = Some(5);
    config.strategy.allow_short = Some(true);
    config.strategy.target_qty = -1;
    config.validate().unwrap();
    let order = build_strategy_order(&config, "strategy-perp", 9301, 10, 0, -1)
        .unwrap()
        .unwrap();
    let policy = order.policy.unwrap();
    assert_eq!(policy.leverage, 5);
    assert_eq!(policy.margin_mode, MarginMode::Isolated);
    assert_eq!(policy.position_mode, PositionMode::OneWay);
    assert_eq!(policy.position_side, PositionSide::Net);
}
