use super::*;
use qx_control::CommandStatus;

/// V10 §4.1 / D1 的收口用例：缺少冻结 market spec 的实盘执行 worker 不得降级为
/// "无风控提交"。这里刻意用 `dry_run=false` 走真实副作用分支，并断言命令在控制面
/// 被判 Failed、EventLog 里没有任何订单事实——即拒绝发生在 venue 装配之前。
#[test]
fn binance_submit_without_market_spec_fails_closed_with_no_order_fact() {
    let root = temp_cli_case_dir("p0a-binance-fail-closed");
    let data_dir = root.join("data");
    let template = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy")
        .join("qianxing.runtime.production.example.json");
    let mut config = read_runtime_config(&template).unwrap();
    config.config_fingerprint = None;
    config.environment = "test".into();
    config.storage.data_dir = data_dir.to_string_lossy().into_owned();
    config.storage.backend = StorageBackend::Files;
    config.storage.consistency = qx_runtime::StorageConsistency::LocalDurable;
    config.storage.sqlite_path = None;
    let worker_id = "binance-user-main";
    {
        let worker = config
            .workers
            .iter_mut()
            .find(|worker| worker.id == worker_id)
            .unwrap();
        worker.instrument_spec_path = None;
    }
    let config_path = root.join("runtime.json");
    std::fs::write(&config_path, config.to_json().unwrap()).unwrap();

    let account_id = config
        .workers
        .iter()
        .find(|worker| worker.id == worker_id)
        .unwrap()
        .account_id
        .clone()
        .unwrap();
    let mut order = mk_order(
        7101,
        &InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
        Side::Buy,
        1,
    );
    order.account_id = account_id;
    let command = mk_submit_command(7101, &order, false);
    let command_path = root.join("command.json");
    std::fs::write(&command_path, serde_json::to_string(&command).unwrap()).unwrap();

    let error = run_binance_submit_order(&config_path, worker_id, &command_path).unwrap_err();
    assert!(
        error.contains("FAIL_CLOSED") && error.contains(worker_id),
        "缺风控配置的实盘提交必须以 FAIL_CLOSED 拒绝: {error}"
    );

    let state = load_control_state(&data_dir).unwrap();
    let audit = state.audit();
    assert_eq!(audit.len(), 2, "命令应留下 Accepted + 终态两条审计");
    assert_eq!(audit[0].status, CommandStatus::Accepted);
    assert_eq!(audit[1].status, CommandStatus::Failed);
    assert!(audit[1].result_code.contains("FAIL_CLOSED"));

    let worker = config
        .workers
        .iter()
        .find(|worker| worker.id == worker_id)
        .cloned()
        .unwrap();
    let pipeline =
        LiveEventPipeline::open(&data_dir, binance_event_log_name(&worker), "USDT").unwrap();
    assert!(
        pipeline.orders().is_empty(),
        "拒绝发生在提交之前，EventLog 不得出现订单事实"
    );
    assert!(pipeline.ledger().entries().is_empty());
    let _ = std::fs::remove_dir_all(root);
}

/// 三条链共用同一个缺配置判定：CCXT worker 与 Binance 得到同样的 FAIL_CLOSED 文案；
/// 配置齐全时放行；spec 文件损坏或不可读同样报错而不是回落到无风控。回报归约侧
/// （`worker_report_spec`）不产生新订单，因此仍允许返回 `None`。
#[test]
fn submit_risk_gate_is_shared_across_venues_and_rejects_broken_specs() {
    let root = temp_cli_case_dir("p0a-shared-gate");
    let order = mk_order(
        7102,
        &InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
        Side::Buy,
        1,
    );
    let command = mk_submit_command(7102, &order, false);

    let ccxt_worker = mk_worker("ccxt-okx-main", WorkerRole::Execution, "okx", None);
    let error = require_worker_risk_spec(&ccxt_worker, &command, None).unwrap_err();
    assert!(
        error.contains("FAIL_CLOSED") && error.contains("ccxt-okx-main"),
        "CCXT 链必须与 Binance 共用同一 fail-closed 口径: {error}"
    );

    let spec_path = workspace_binance_spot_spec().to_string_lossy().into_owned();
    let configured = mk_worker(
        "binance-exec-main",
        WorkerRole::Execution,
        "binance",
        Some(&spec_path),
    );
    assert!(require_worker_risk_spec(&configured, &command, None).is_ok());

    let missing = mk_worker(
        "binance-exec-main",
        WorkerRole::Execution,
        "binance",
        Some("deploy/does-not-exist.spec.json"),
    );
    let error = require_worker_risk_spec(&missing, &command, None).unwrap_err();
    assert!(
        error.contains("读取 worker market spec 失败"),
        "spec 不可读要报错，不得静默当作未配置: {error}"
    );

    let pipeline = LiveEventPipeline::open(&root, "p0a-report-events", "USDT").unwrap();
    assert!(
        worker_report_spec(&ccxt_worker, &[], &pipeline, None)
            .unwrap()
            .is_none(),
        "回报归约侧不产生新订单，允许无规格"
    );
    let _ = std::fs::remove_dir_all(root);
}
