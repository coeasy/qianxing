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
    let snapshot = load_api_account_snapshot(&config).unwrap().unwrap();
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
    let result = execute_paper_submit_effect(
        &command,
        &mut pipeline,
        10,
        Some(smoke_paper_risk_context()),
        Some(OrderRiskPosition::new(0, 0)),
        None,
        true,
        // 本例的命令不带 `spread_group_id`（上一行 `strategy_submit_command(.., None)`），
        // 单腿提交不经过多腿屏障，因此无需组存储。
        None,
    )
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
    assert_eq!(restored.ledger().entries().len(), 2);
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
    execute_paper_submit_effect(
        &command,
        &mut pipeline,
        10,
        Some(smoke_paper_risk_context()),
        Some(OrderRiskPosition::new(0, 0)),
        None,
        true,
        // 单腿命令（`spread_group_id` 缺省），不经过多腿屏障。
        None,
    )
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
    assert_eq!(restored.ledger().entries().len(), 3);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn paper_e2e_entrypoint_runs_scheduler_strategy_execution_and_ledger() {
    let root = std::env::temp_dir().join(format!(
        "qianxing-cli-paper-e2e-{}-{}",
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
        .join("qianxing.runtime.paper-strategy.example.json");
    let mut config = read_runtime_config(&template).unwrap();
    config.storage.data_dir = data_dir.to_string_lossy().into_owned();
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
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
    run_paper_pipeline_once(&config_path).unwrap();
    let pipeline = LiveEventPipeline::open(&data_dir, paper_account_log(), "USDT").unwrap();
    assert_eq!(pipeline.orders().len(), 1);
    assert_eq!(pipeline.orders()[0].status, OrderStatus::Filled);
    assert_eq!(pipeline.ledger().entries().len(), 3);
    let _ = std::fs::remove_dir_all(root);
}

/// 单机 SQLite backend 的 Paper 主链路验收：调度→策略→执行→Ledger
/// 全部事实必须落在同一个事务数据库里，重启打开后可完整回放。
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
        venue_raw: Money::from_i64(10).raw(),
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
        balance_discrepancies: &[discrepancy],
        position_snapshots_count: 0,
        funding_rate_snapshots_count: 0,
        cashflow_count: 0,
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
        cash: BTreeMap::from([("USDT".into(), 100_000_000_i128)]),
        available_margin_raw: Some(100_000_000),
        risk_state: "ready".into(),
        research_targets: BTreeMap::from([("BTC/USDT.OKX".into(), 3_i128)]),
        bars: None,
    };
    let output = invoke_python_strategy(module.to_string_lossy().as_ref(), &input).unwrap();
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
        cash: BTreeMap::from([("USDT".into(), 100_000_000_i128)]),
        available_margin_raw: Some(100_000_000),
        risk_state: "ready".into(),
        research_targets: BTreeMap::new(),
        bars: None,
    };
    let output = invoke_python_strategy(module.to_string_lossy().as_ref(), &input).unwrap();
    assert_eq!(output.intents.len(), 2);
    assert_eq!(output.intents[0].side, "buy");
    assert_eq!(output.intents[1].intent_id, 802);
}
