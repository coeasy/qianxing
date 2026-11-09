use super::*;

#[test]
fn api_snapshot_is_rebuilt_from_persisted_account_eventlog() {
    let root = std::env::temp_dir().join(format!(
        "qianxing-cli-api-read-model-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let template = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy")
        .join("qianxing.runtime.paper-strategy.example.json");
    let mut config = read_runtime_config(&template).unwrap();
    config.storage.data_dir = root.to_string_lossy().into_owned();
    config.storage.event_log_segment_events = Some(2);
    let trading_days = root.join("account-trading-days.json");
    std::fs::write(
        &trading_days,
        r#"{"schema_version":1,"account_trading_days":{"main":{"paper":"20260911"}}}"#,
    )
    .unwrap();
    config.api.account_trading_days_path = Some(trading_days.to_string_lossy().into_owned());
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let order = mk_order(9401, &instrument, Side::Buy, 1);
    let mut pipeline = LiveEventPipeline::open_configured(
        &root,
        paper_account_log(),
        "USDT",
        config.storage.event_log_segment_events,
    )
    .unwrap();
    pipeline.register_order(order, 1).unwrap();
    pipeline
        .ingest(RuntimeEventEnvelope::venue(
            RuntimeExternalEvent::Accepted {
                client_order_id: 9401,
                venue_order_id: Some("paper-9401".into()),
            },
            2,
            2,
            1,
            "api-read-model:accepted",
        ))
        .unwrap();
    pipeline
        .ingest(RuntimeEventEnvelope::venue(
            RuntimeExternalEvent::Fill {
                fill: qx_core::Fill {
                    order_id: 9401,
                    qty: Quantity::from_i64(1),
                    price: Price::from_i64(100),
                    ts: 3,
                    account_id: "main".into(),
                    ..qx_core::Fill::default()
                },
            },
            3,
            3,
            2,
            "api-read-model:fill",
        ))
        .unwrap();
    let snapshot = qx_service::api_read_model::load_api_account_snapshots_with_market_specs(
        &config,
        &std::collections::BTreeMap::new(),
    )
    .unwrap()
    .into_iter()
    .find(|snapshot| snapshot.header.account_id == "main")
    .unwrap();
    assert_eq!(snapshot.header.account_id, "main");
    assert_eq!(snapshot.orders.len(), 1);
    assert_eq!(snapshot.fills.len(), 1);
    assert_eq!(snapshot.positions[&instrument].quantity_raw, SCALE);
    assert_eq!(snapshot.reconcile.recovery_state, "eventlog-replayed");
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn paper_strategy_reads_filled_position_before_emitting_next_order() {
    let root = std::env::temp_dir().join(format!(
        "qianxing-cli-paper-position-{}-{}",
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
        .join("qianxing.runtime.paper-strategy.example.json");
    let mut config = read_runtime_config(&template).unwrap();
    config.storage.data_dir = data_dir.to_string_lossy().into_owned();
    config.storage.event_log_segment_events = Some(2);

    let order = build_strategy_order(
        &config,
        "strategy-paper",
        9201,
        10,
        0,
        config.strategy.target_qty,
    )
    .unwrap()
    .unwrap();
    let command = strategy_submit_command("strategy-paper", &order, false, None).unwrap();
    let log_name = paper_account_log();
    let mut pipeline = LiveEventPipeline::open_configured(
        &data_dir,
        &log_name,
        "USDT",
        config.storage.event_log_segment_events,
    )
    .unwrap();
    let result = execute_paper_submit_effect(qx_execution::PaperSubmitRequest {
            command: &command,
            pipeline: &mut pipeline,
            now: 10,
            risk: Some(smoke_paper_risk_context()),
            position: Some(OrderRiskPosition::new(0, 0)),
            market_quote: None,
            fee_model: default_execution_cost_binding().fee_model(),
            allow_synthetic_quote: true,
            spread_store: // 本例的命令不带 `spread_group_id`（上一行 `strategy_submit_command(.., None)`），
        // 单腿提交不经过多腿屏障，因此无需组存储。
        None
        })
    .unwrap();
    assert!(result.starts_with("PAPER_EXECUTED fills=1"));

    let current_qty = strategy_current_qty(&data_dir, &config).unwrap();
    assert_eq!(current_qty, config.strategy.target_qty);
    assert!(build_strategy_order(
        &config,
        "strategy-paper",
        9202,
        11,
        current_qty,
        config.strategy.target_qty,
    )
    .unwrap()
    .is_none());

    let restored = LiveEventPipeline::open_configured(
        &data_dir,
        log_name,
        "USDT",
        config.storage.event_log_segment_events,
    )
    .unwrap();
    assert_eq!(restored.orders()[0].status, OrderStatus::Filled);
    assert_eq!(restored.ledger().entries().len(), 3);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn paper_worker_cleans_stale_queue_after_terminal_commit() {
    let root = std::env::temp_dir().join(format!(
        "qianxing-cli-paper-recovery-{}-{}",
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
        .join("qianxing.runtime.paper-strategy.example.json");
    let mut config = read_runtime_config(&template).unwrap();
    config.storage.data_dir = data_dir.to_string_lossy().into_owned();
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    let frozen_spec = workspace_root
        .join("deploy")
        .join("qianxing.binance.spot.spec.json");
    for worker in &mut config.workers {
        if worker.instrument_spec_path.is_some() {
            worker.instrument_spec_path = Some(frozen_spec.to_string_lossy().into_owned());
        }
    }
    let config_path = root.join("runtime.json");
    std::fs::write(&config_path, config.to_json().unwrap()).unwrap();

    let order = build_strategy_order(
        &config,
        "strategy-paper",
        9301,
        10,
        0,
        config.strategy.target_qty,
    )
    .unwrap()
    .unwrap();
    let command = strategy_submit_command("strategy-paper", &order, false, None).unwrap();
    let store = ControlStateBackend::Files(JsonStateStore::new(&data_dir));
    let queue = ControlCommandQueue::new(data_dir.join("control-queue"));
    store
        .transact(|plane| plane.submit_as(command.clone(), Permission::Trading, 10))
        .unwrap()
        .1
        .unwrap();
    queue.enqueue(command.clone(), 10).unwrap();

    let log_name = paper_account_log();
    let mut pipeline = LiveEventPipeline::open(&data_dir, &log_name, "USDT").unwrap();
    execute_paper_submit_effect(qx_execution::PaperSubmitRequest {
            command: &command,
            pipeline: &mut pipeline,
            now: 10,
            risk: Some(smoke_paper_risk_context()),
            position: Some(OrderRiskPosition::new(0, 0)),
            market_quote: None,
            fee_model: default_execution_cost_binding().fee_model(),
            allow_synthetic_quote: true,
            spread_store: // 单腿命令（`spread_group_id` 缺省），不经过多腿屏障。
        None
        })
    .unwrap();
    store
        .transact(|plane| plane.execute(command.command_id, 11, |_| Ok("PAPER_EXECUTED".into())))
        .unwrap()
        .1
        .unwrap();

    run_paper_execution_worker(&config_path, "paper-execution", true).unwrap();
    assert!(queue.pending().unwrap().is_empty());
    let restored = LiveEventPipeline::open(&data_dir, &log_name, "USDT").unwrap();
    assert_eq!(restored.orders()[0].status, OrderStatus::Filled);
    // Q0a 起 Paper 成交按共享费率计费：买入多出 1 条 Fee 分录（名义额、持仓、费用）。
    assert_eq!(restored.ledger().entries().len(), 4);
    let _ = std::fs::remove_dir_all(root);
}

/// SQLite Paper 主链路验收：调度→策略→执行→Ledger 同库提交，重启后可完整回放。
#[cfg(feature = "sqlite")]
#[test]
fn paper_e2e_with_sqlite_backend_replays_from_transactional_eventlog() {
    let root = std::env::temp_dir().join(format!(
        "qianxing-cli-paper-sqlite-{}-{}",
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
    config.storage.backend = StorageBackend::Sqlite;
    config.storage.sqlite_path = Some(
        root.join("qx-runtime.sqlite")
            .to_string_lossy()
            .into_owned(),
    );
    config.scheduler.jobs_path = workspace_root
        .join("deploy")
        .join("qianxing.scheduler.paper-order-smoke.json")
        .to_string_lossy()
        .into_owned();
    config.strategy.target_snapshot_path = Some(
        workspace_root
            .join("deploy")
            .join("qianxing.strategy-target.paper.json")
            .to_string_lossy()
            .into_owned(),
    );
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
    config.strategy.target_qty = 0;
    let config_path = root.join("runtime.json");
    std::fs::write(&config_path, config.to_json().unwrap()).unwrap();

    run_paper_pipeline_once(&config_path).unwrap();
    let db = root.join("qx-runtime.sqlite");
    let pipeline = LiveEventPipeline::open_sqlite(&db, paper_account_log(), "USDT").unwrap();
    assert_eq!(pipeline.orders().len(), 1);
    assert_eq!(pipeline.orders()[0].status, OrderStatus::Filled);
    assert!(!pipeline.ledger().entries().is_empty());
    // 事实必须真实进入 SQLite 表，而不是退化为文件目录。
    assert!(!data_dir.join("events").join(paper_account_log()).exists());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn reconcile_report_persists_structured_balance_discrepancy() {
    let root = std::env::temp_dir().join(format!(
        "qianxing-cli-reconcile-report-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let discrepancy = RuntimeBalanceDiscrepancy {
        account_id: "main".into(),
        venue_id: "binance".into(),
        asset: "USDT".into(),
        ledger_raw: 0,
        venue_raw: Some(Money::from_i64(10).raw()),
    };
    // 同一轮里柜台没报该币种的那条：缺席与"报了且为零"必须是两个值。
    let absent = RuntimeBalanceDiscrepancy {
        venue_raw: None,
        ..discrepancy.clone()
    };
    persist_reconcile_report(ReconcileReportInput {
        pipeline_root: &root,
        worker_id: "reconciler-main",
        account_id: "main",
        venue_id: "binance",
        observed_ts: 42,
        issues: &[],
        additional_order_issues: &[],
        balances_count: 1,
        balance_discrepancies: &[discrepancy, absent],
        position_snapshots_count: None,
        funding_rate_snapshots_count: Some(0),
        cashflow_count: Some(0),
    })
    .unwrap();
    let report: serde_json::Value = JsonStateStore::new(&root)
        .load_json_at("reconcile/reconciler-main.json")
        .unwrap();
    assert_eq!(report["schema_version"], 1);
    assert_eq!(report["balance_discrepancies"][0]["asset"], "USDT");
    assert_eq!(
        report["balance_discrepancies"][0]["venue_raw"],
        10_000_000_000_i64
    );
    assert!(
        report["balance_discrepancies"][1]["venue_raw"].is_null(),
        "柜台未报该币种必须落成 null；写 0 等于替交易所报数（V12 §18 TX6）"
    );
    // "这条链没取" 与 "取了且为空" 在报告里必须是两个值（V11 Q69）。
    assert!(report["position_snapshots_count"].is_null());
    assert_eq!(report["funding_rate_snapshots_count"], 0);
    // 老报告（三个计数是裸 0）仍要能读回来，缺席键读成"没取"。
    let legacy: qx_api::ReconcileReportSnapshot = serde_json::from_value(serde_json::json!({
        "schema_version": 1,
        "worker_id": "reconciler-main",
        "account_id": "main",
        "venue_id": "binance",
        "observed_ts": 42,
        "order_issues": [],
        "balances_count": 1,
        "balance_discrepancies": [],
        "position_snapshots_count": 0,
        "funding_rate_snapshots_count": 0,
        "cashflow_count": 0,
    }))
    .unwrap();
    assert_eq!(legacy.position_snapshots_count, Some(0));
    let legacy_without_keys: qx_api::ReconcileReportSnapshot =
        serde_json::from_value(serde_json::json!({
            "schema_version": 1,
            "worker_id": "reconciler-main",
            "account_id": "main",
            "venue_id": "binance",
            "observed_ts": 42,
            "order_issues": [],
            "balances_count": 1,
            "balance_discrepancies": [],
        }))
        .unwrap();
    assert_eq!(legacy_without_keys.cashflow_count, None);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn rust_invokes_python_strategy_jsonl_worker_through_versioned_contract() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    let module = workspace_root
        .join("python")
        .join("tests")
        .join("fixtures")
        .join("strategy_target.py");
    let input = StrategyContractInput {
        schema_version: qx_runtime::STRATEGY_CONTRACT_SCHEMA_VERSION,
        request_id: "request-python-1".into(),
        strategy_id: "strategy-python".into(),
        strategy_version: "v1".into(),
        data_fingerprint: "bars-sha256".into(),
        as_of: 1_700_000_000,
        instrument: "BTC/USDT.OKX".into(),
        positions: BTreeMap::new(),
        positions_exact: BTreeMap::new(),
        cash: BTreeMap::from([("USDT".into(), 100_000_000_i128)]),
        cash_exact: std::collections::BTreeMap::new(),
        available_margin_exact: None,
        available_margin_raw: Some(100_000_000),
        risk_state: "ready".into(),
        research_targets: BTreeMap::from([("BTC/USDT.OKX".into(), 3_i128)]),
        market_bars: BTreeMap::new(),
        execution_updates: Vec::new(),
        fill_updates: Vec::new(),
        strategy_checkpoint: None,
        market_ticks: Vec::new(),
        market_order_books: Vec::new(),
        account_updates: Vec::new(),
        bars: None,
    };
    let output = invoke_python_strategy(
        module.to_string_lossy().as_ref(),
        &input,
        PYTHON_STRATEGY_TIMEOUT_MS,
    )
    .unwrap();
    assert_eq!(output.target_qty, 3);
    assert_eq!(output.signal_id, 7);
    assert_eq!(output.confidence, 800);
    assert_eq!(output.priority, 2);
    assert_eq!(output.request_id, input.request_id);
    let artifact_sha256 = qx_strategy::sha256_hex(&std::fs::read(&module).unwrap());
    let mut client = PythonStrategyClient::start_with_transport_config(
        module.to_string_lossy().as_ref(),
        PYTHON_STRATEGY_TIMEOUT_MS,
        StrategyTransport::Jsonl,
        SharedRingConfig::default(),
        Some(&artifact_sha256),
    )
    .unwrap();
    let first = client.request(&input).unwrap();
    let second = client.request(&input).unwrap();
    assert_eq!(first.signal_id, second.signal_id);
    assert_eq!(first.target_qty, 3);
}

#[test]
fn rust_invokes_python_basestrategy_class_through_process_contract() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    let module = workspace_root
        .join("python")
        .join("tests")
        .join("fixtures")
        .join("strategy_base_class.py");
    let input = StrategyContractInput {
        schema_version: qx_runtime::STRATEGY_CONTRACT_SCHEMA_VERSION,
        request_id: "request-python-base-class-1".into(),
        strategy_id: "strategy-python-base-class".into(),
        strategy_version: "v1".into(),
        data_fingerprint: "bars-sha256".into(),
        as_of: 2,
        instrument: "BTC/USDT.OKX".into(),
        positions: BTreeMap::from([("BTC/USDT.OKX".into(), 0)]),
        positions_exact: BTreeMap::from([("BTC/USDT.OKX".into(), "0".into())]),
        cash: BTreeMap::from([("USDT".into(), 100_000_000_i128)]),
        cash_exact: std::collections::BTreeMap::new(),
        available_margin_exact: None,
        available_margin_raw: Some(100_000_000),
        risk_state: "ready".into(),
        research_targets: BTreeMap::new(),
        market_bars: BTreeMap::new(),
        execution_updates: Vec::new(),
        fill_updates: Vec::new(),
        strategy_checkpoint: None,
        market_ticks: Vec::new(),
        market_order_books: Vec::new(),
        account_updates: Vec::new(),
        bars: Some(StrategyContractBars {
            source: "base-class-fixture".into(),
            ts: vec![1, 2],
            open_raw: vec![100, 110],
            high_raw: vec![102, 112],
            low_raw: vec![99, 109],
            close_raw: vec![101, 111],
            volume_raw: vec![10, 12],
            exact_ohlcv: None,
        }),
    };
    let spec = serde_json::json!({
        "strategy": {"kind": "python-class"},
        "parameters": {
            "qty_exact": "0.000000000123456789",
            "limit_price_exact": "0.00000005"
        }
    });
    let ring_directory = workspace_root.join("target").join(format!(
        "qx-test-base-class-rings-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    for transport in [
        StrategyTransport::Jsonl,
        StrategyTransport::FramedJson,
        StrategyTransport::SharedMemoryJson,
        StrategyTransport::SharedMemoryColumnar,
    ] {
        let mut client = PythonStrategyClient::start_for_runtime_worker(
            module.to_string_lossy().as_ref(),
            PYTHON_STRATEGY_TIMEOUT_MS,
            transport,
            SharedRingConfig::default(),
            None,
            Some(&spec),
            Some("main"),
            Some("okx"),
            None,
            Some("strategy-base-class-test"),
        )
        .unwrap();

        let output = client.request(&input).unwrap();
        assert_eq!(
            output.request_id, input.request_id,
            "transport={transport:?}"
        );
        assert_eq!(output.intents.len(), 1, "transport={transport:?}");
        assert_eq!(output.intents[0].intent_id, 17, "transport={transport:?}");
        assert_eq!(output.intents[0].instrument, "BTC/USDT.OKX");
        assert_eq!(output.intents[0].qty_raw, 0);
        assert_eq!(
            output.intents[0].qty_exact.as_deref(),
            Some("0.000000000123456789")
        );
        assert_eq!(
            output.intents[0].limit_price_exact.as_deref(),
            Some("0.00000005")
        );
        assert_eq!(output.intents[0].margin_mode.as_deref(), Some("cross"));
        assert_eq!(output.intents[0].position_mode.as_deref(), Some("one_way"));
        assert_eq!(output.intents[0].leverage, Some(3));

        let checkpoint = client
            .take_strategy_checkpoint()
            .expect("BaseStrategy process must return a durable checkpoint");
        assert_eq!(checkpoint.strategy_id, input.strategy_id);
        assert_eq!(checkpoint.strategy_version, input.strategy_version);
        let mut resumed_input = input.clone();
        resumed_input.schema_version = qx_runtime::STRATEGY_CONTRACT_INPUT_V4_SCHEMA_VERSION;
        resumed_input.request_id = format!("{}-resume", input.request_id);
        resumed_input.strategy_checkpoint = Some(checkpoint);
        let resumed_output = client.request(&resumed_input).unwrap();
        assert!(
            resumed_output.intents.is_empty(),
            "restoring the committed checkpoint must not replay already processed bars; transport={transport:?}"
        );
        assert!(client.take_strategy_checkpoint().is_some());
    }
    // The real Runtime launcher supplies a host-owned catalog to class workers.
    // A fresh process must accept supported requirements and still checkpoint.
    std::fs::create_dir_all(&ring_directory).unwrap();
    let capability_module = ring_directory.join("capability_strategy.py");
    let source = std::fs::read_to_string(&module).unwrap();
    let declaration = r#"
STRATEGY_MANIFEST = {
    "schema_version": 1, "parameters": {},
    "instrument_requirements": {"requires_explicit_spec": False},
    "data_level": "bar", "warmup_bars": 1,
    "required_capabilities": ["bar", "bar_history", "fill_updates", "order_status_updates", "account_updates", "position_updates", "durable_checkpoint"]
}
"#;
    std::fs::write(&capability_module, format!("{source}\n{declaration}")).unwrap();
    for transport in [
        StrategyTransport::Jsonl,
        StrategyTransport::FramedJson,
        StrategyTransport::SharedMemoryJson,
        StrategyTransport::SharedMemoryColumnar,
    ] {
        let mut runtime_client = PythonStrategyClient::start_for_runtime_worker(
            capability_module.to_string_lossy().as_ref(),
            5000,
            transport,
            SharedRingConfig::default(),
            None,
            Some(&spec),
            Some("main"),
            Some("okx"),
            None,
            Some("strategy-capability-test"),
        )
        .unwrap();
        assert_eq!(runtime_client.request(&input).unwrap().intents.len(), 1);
        assert!(runtime_client.take_strategy_checkpoint().is_some());
    }
    // A backtest advertises its local host catalog, never the Runtime catalog.
    let local_capabilities =
        qx_strategy::capabilities::StrategyHostCapabilitiesV1::local_python(Some("bar"));
    let backtest = PythonStrategyClient::start_with_strategy_config_and_audit_path(
        capability_module.to_string_lossy().as_ref(),
        5000,
        StrategyTransport::Jsonl,
        SharedRingConfig::default(),
        None,
        Some(&spec),
        Some("main"),
        Some("okx"),
        None,
        Some("bar-backtest:capability-test"),
        Some(&local_capabilities),
    );
    assert!(
        backtest.is_err(),
        "backtest must not advertise Runtime capabilities"
    );
    // Local Python file mode receives the local host catalog and validates the
    // module-level manifest before exposing the worker for request dispatch.
    let file_module = ring_directory.join("file_strategy.py");
    let strategy_target = workspace_root
        .join("python")
        .join("tests")
        .join("fixtures")
        .join("strategy_target.py");
    let file_source = std::fs::read_to_string(strategy_target).unwrap();
    std::fs::write(
        &file_module,
        format!("{file_source}\nSTRATEGY_MANIFEST = {{'required_capabilities': ['bar']}}\n"),
    )
    .unwrap();
    let mut file_input = input.clone();
    file_input.research_targets.insert("BTC/USDT.OKX".into(), 3);
    let mut file_client = PythonStrategyClient::start_with_strategy_config_and_audit_path(
        file_module.to_string_lossy().as_ref(),
        5000,
        StrategyTransport::Jsonl,
        SharedRingConfig::default(),
        None,
        None,
        None,
        None,
        None,
        Some("local-python-file-capability"),
        Some(&local_capabilities),
    )
    .expect("local Python file accepts its delivered Bar capability");
    file_client
        .request(&file_input)
        .expect("local Python file dispatches after capability negotiation");
    let unsupported_file_module = ring_directory.join("unsupported_file_strategy.py");
    std::fs::write(
        &unsupported_file_module,
        format!("{file_source}\nSTRATEGY_MANIFEST = {{'required_capabilities': ['timer']}}\n"),
    )
    .unwrap();
    let mut rejected_file = PythonStrategyClient::start_with_strategy_config_and_audit_path(
        unsupported_file_module.to_string_lossy().as_ref(),
        5000,
        StrategyTransport::Jsonl,
        SharedRingConfig::default(),
        None,
        None,
        None,
        None,
        None,
        Some("local-python-file-unsupported-capability"),
        Some(&local_capabilities),
    )
    .expect("local Python file worker starts before its first request");
    let rejected_file = rejected_file
        .request(&file_input)
        .expect_err("local Python file rejects Timer before calling its handler");
    assert!(rejected_file.contains("timer"), "{rejected_file}");

    // Runtime module-function mode has a distinct server catalog from class mode.
    let module_function = ring_directory.join("module_function_strategy.py");
    std::fs::write(
        &module_function,
        format!("{file_source}\nSTRATEGY_STATELESS = True\nSTRATEGY_MANIFEST = {{'required_capabilities': ['bar', 'durable_checkpoint']}}\n"),
    )
    .unwrap();
    let mut module_function_client = PythonStrategyClient::start_for_runtime_worker(
        module_function.to_string_lossy().as_ref(),
        5000,
        StrategyTransport::Jsonl,
        SharedRingConfig::default(),
        None,
        None,
        None,
        None,
        None,
        Some("runtime-module-function-capability"),
    )
    .expect("Runtime module-function accepts Bar and durable checkpoint requirements");
    module_function_client
        .request(&file_input)
        .expect("Runtime module-function dispatches after capability negotiation");
    assert!(module_function_client.take_strategy_checkpoint().is_some());
    std::fs::write(
        &module_function,
        format!("{file_source}\nSTRATEGY_STATELESS = True\nSTRATEGY_MANIFEST = {{'required_capabilities': ['timer']}}\n"),
    )
    .unwrap();
    let mut rejected_module_function = PythonStrategyClient::start_for_runtime_worker(
        module_function.to_string_lossy().as_ref(),
        5000,
        StrategyTransport::Jsonl,
        SharedRingConfig::default(),
        None,
        None,
        None,
        None,
        None,
        Some("runtime-module-function-unsupported-capability"),
    )
    .expect("Runtime module-function host starts before its first request");
    let rejected_module_function = rejected_module_function
        .request(&file_input)
        .expect_err("Runtime module-function rejects Timer before calling its handler");
    assert!(
        rejected_module_function.contains("timer"),
        "{rejected_module_function}"
    );

    // Missing capability fails at startup, before the factory can write a marker.
    let marker = ring_directory.join("factory-executed");
    let rejected_source = format!(
        "STRATEGY_MANIFEST = {{'required_capabilities': ['timer']}}\ndef create_strategy(spec):\n    open({:?}, 'w').write('bad')\n", marker.to_string_lossy()
    );
    std::fs::write(&capability_module, rejected_source).unwrap();
    let rejected = PythonStrategyClient::start_for_runtime_worker(
        capability_module.to_string_lossy().as_ref(),
        5000,
        StrategyTransport::Jsonl,
        SharedRingConfig::default(),
        None,
        Some(&spec),
        Some("main"),
        Some("okx"),
        None,
        Some("strategy-capability-test"),
    );
    assert!(rejected.is_err());
    assert!(!marker.exists());
    // A factory that never finishes cannot publish Runtime readiness.
    std::fs::write(
        &capability_module,
        "import time\ndef create_strategy(spec):\n    time.sleep(30)\n",
    )
    .unwrap();
    let audit_path = ring_directory.join("hung-startup.jsonl");
    let timed_out = PythonStrategyClient::start_for_runtime_worker(
        capability_module.to_string_lossy().as_ref(),
        500,
        StrategyTransport::Jsonl,
        SharedRingConfig::default(),
        None,
        Some(&spec),
        Some("main"),
        Some("okx"),
        Some(&audit_path),
        Some("strategy-capability-timeout"),
    )
    .err()
    .expect("hung factory must fail startup");
    assert!(
        timed_out.contains("initialization timed out"),
        "{timed_out}"
    );
    assert!(!audit_path.with_extension("ready").exists());
    let _ = std::fs::remove_dir_all(ring_directory);
}

#[test]
fn rust_invokes_python_strategy_framed_json_worker_through_versioned_contract() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    let module = workspace_root
        .join("python")
        .join("tests")
        .join("fixtures")
        .join("strategy_target.py");
    let input = StrategyContractInput {
        schema_version: qx_runtime::STRATEGY_CONTRACT_SCHEMA_VERSION,
        request_id: "request-python-framed-1".into(),
        strategy_id: "strategy-python-framed".into(),
        strategy_version: "v1".into(),
        data_fingerprint: "bars-sha256".into(),
        as_of: 1_700_000_000,
        instrument: "BTC/USDT.OKX".into(),
        positions: BTreeMap::new(),
        positions_exact: BTreeMap::new(),
        cash: BTreeMap::from([("USDT".into(), 100_000_000_i128)]),
        cash_exact: std::collections::BTreeMap::new(),
        available_margin_exact: None,
        available_margin_raw: Some(100_000_000),
        risk_state: "ready".into(),
        research_targets: BTreeMap::from([("BTC/USDT.OKX".into(), 3_i128)]),
        market_bars: BTreeMap::new(),
        execution_updates: Vec::new(),
        fill_updates: Vec::new(),
        strategy_checkpoint: None,
        market_ticks: Vec::new(),
        market_order_books: Vec::new(),
        account_updates: Vec::new(),
        bars: None,
    };
    let artifact_sha256 = qx_strategy::sha256_hex(&std::fs::read(&module).unwrap());
    let mut client = PythonStrategyClient::start_with_transport_config(
        module.to_string_lossy().as_ref(),
        PYTHON_STRATEGY_TIMEOUT_MS,
        StrategyTransport::FramedJson,
        SharedRingConfig::default(),
        Some(&artifact_sha256),
    )
    .unwrap();

    let output = client.request(&input).unwrap();
    assert_eq!(output.target_qty, 3);
    assert_eq!(output.signal_id, 7);
    assert_eq!(output.request_id, input.request_id);
}

#[test]
fn rust_stops_python_strategy_workers_when_each_transport_times_out() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    let module = workspace_root
        .join("python")
        .join("tests")
        .join("fixtures")
        .join("strategy_slow.py");
    let input = StrategyContractInput {
        schema_version: qx_runtime::STRATEGY_CONTRACT_SCHEMA_VERSION,
        request_id: "request-python-timeout-1".into(),
        strategy_id: "strategy-python-timeout".into(),
        strategy_version: "v1".into(),
        data_fingerprint: "bars-sha256".into(),
        as_of: 1_700_000_000,
        instrument: "BTC/USDT.OKX".into(),
        positions: BTreeMap::new(),
        positions_exact: BTreeMap::new(),
        cash: BTreeMap::from([("USDT".into(), 100_000_000_i128)]),
        cash_exact: std::collections::BTreeMap::new(),
        available_margin_exact: None,
        available_margin_raw: Some(100_000_000),
        risk_state: "ready".into(),
        research_targets: BTreeMap::from([("BTC/USDT.OKX".into(), 3_i128)]),
        market_bars: BTreeMap::new(),
        execution_updates: Vec::new(),
        fill_updates: Vec::new(),
        strategy_checkpoint: None,
        market_ticks: Vec::new(),
        market_order_books: Vec::new(),
        account_updates: Vec::new(),
        bars: Some(StrategyContractBars {
            source: "timeout-fixture".into(),
            ts: vec![1_699_999_999],
            open_raw: vec![100],
            high_raw: vec![102],
            low_raw: vec![99],
            close_raw: vec![101],
            volume_raw: vec![10],
            exact_ohlcv: None,
        }),
    };
    let ring_directory = workspace_root.join("target").join(format!(
        "qx-test-timeout-rings-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));

    for transport in [
        StrategyTransport::Jsonl,
        StrategyTransport::FramedJson,
        StrategyTransport::SharedMemoryJson,
        StrategyTransport::SharedMemoryColumnar,
    ] {
        let mut client = PythonStrategyClient::start_with_transport_config_in(
            module.to_string_lossy().as_ref(),
            100,
            transport,
            SharedRingConfig::default(),
            None,
            &ring_directory,
        )
        .unwrap();
        let error = client.request(&input).unwrap_err();
        assert!(
            error.contains("响应超时"),
            "transport={transport:?}: {error}"
        );
        assert!(
            error.contains("timeout_ms=100"),
            "transport={transport:?}: {error}"
        );
    }
    std::fs::remove_dir_all(ring_directory).unwrap();
}

#[test]
fn rust_invokes_python_strategy_over_both_shared_memory_transports() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    let module = workspace_root
        .join("python")
        .join("tests")
        .join("fixtures")
        .join("strategy_target.py");
    let input = StrategyContractInput {
        schema_version: qx_runtime::STRATEGY_CONTRACT_SCHEMA_VERSION,
        request_id: "request-python-shared-1".into(),
        strategy_id: "strategy-python-shared".into(),
        strategy_version: "v1".into(),
        data_fingerprint: "bars-sha256".into(),
        as_of: 1_700_000_000,
        instrument: "BTC/USDT.OKX".into(),
        positions: BTreeMap::new(),
        positions_exact: BTreeMap::new(),
        cash: BTreeMap::from([("USDT".into(), 100_000_000_i128)]),
        cash_exact: std::collections::BTreeMap::new(),
        available_margin_exact: None,
        available_margin_raw: Some(100_000_000),
        risk_state: "ready".into(),
        research_targets: BTreeMap::from([("BTC/USDT.OKX".into(), 3_i128)]),
        market_bars: BTreeMap::new(),
        execution_updates: Vec::new(),
        fill_updates: Vec::new(),
        strategy_checkpoint: None,
        market_ticks: Vec::new(),
        market_order_books: Vec::new(),
        account_updates: Vec::new(),
        bars: Some(StrategyContractBars {
            source: "shared-memory-fixture".into(),
            ts: vec![1_699_999_998, 1_699_999_999],
            open_raw: vec![100, 101],
            high_raw: vec![102, 103],
            low_raw: vec![99, 100],
            close_raw: vec![101, 102],
            volume_raw: vec![10, 11],
            exact_ohlcv: None,
        }),
    };
    let artifact_sha256 = qx_strategy::sha256_hex(&std::fs::read(&module).unwrap());
    let ring_directory = workspace_root.join("target").join(format!(
        "qx-test-shared-rings-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));

    for transport in [
        StrategyTransport::SharedMemoryJson,
        StrategyTransport::SharedMemoryColumnar,
    ] {
        let mut client = PythonStrategyClient::start_with_transport_config_in(
            module.to_string_lossy().as_ref(),
            PYTHON_STRATEGY_TIMEOUT_MS,
            transport,
            SharedRingConfig::default(),
            Some(&artifact_sha256),
            &ring_directory,
        )
        .unwrap();
        let output = client.request(&input).unwrap();
        assert_eq!(output.target_qty, 3, "transport={transport:?}");
        assert_eq!(output.signal_id, 7, "transport={transport:?}");
        assert_eq!(
            output.request_id, input.request_id,
            "transport={transport:?}"
        );
    }
    std::fs::remove_dir_all(ring_directory).unwrap();
}

#[test]
fn rust_invokes_python_multi_intent_strategy_contract() {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    let module = workspace_root
        .join("python")
        .join("tests")
        .join("fixtures")
        .join("strategy_multi.py");
    let input = StrategyContractInput {
        schema_version: qx_runtime::STRATEGY_CONTRACT_SCHEMA_VERSION,
        request_id: "request-python-multi".into(),
        strategy_id: "strategy-python-multi".into(),
        strategy_version: "v1".into(),
        data_fingerprint: "bars-sha256".into(),
        as_of: 1_700_000_000,
        instrument: "BTCUSDT.BINANCE".into(),
        positions: BTreeMap::new(),
        positions_exact: BTreeMap::new(),
        cash: BTreeMap::from([("USDT".into(), 100_000_000_i128)]),
        cash_exact: std::collections::BTreeMap::new(),
        available_margin_exact: None,
        available_margin_raw: Some(100_000_000),
        risk_state: "ready".into(),
        research_targets: BTreeMap::new(),
        market_bars: BTreeMap::new(),
        execution_updates: Vec::new(),
        fill_updates: Vec::new(),
        strategy_checkpoint: None,
        market_ticks: Vec::new(),
        market_order_books: Vec::new(),
        account_updates: Vec::new(),
        bars: None,
    };
    let output = invoke_python_strategy(
        module.to_string_lossy().as_ref(),
        &input,
        PYTHON_STRATEGY_TIMEOUT_MS,
    )
    .unwrap();
    assert_eq!(output.intents.len(), 2);
    assert_eq!(output.intents[0].side, "buy");
    assert_eq!(output.intents[1].intent_id, 802);
}
