use super::*;
use crate::ccxt_submit_args::CcxtSubmitArgs;
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
    // 决定走真实提交臂的是下面那条 `dry_run=false` 的控制命令，不是配置里的环境名；
    // 环境本身必须写成闭合名单里的写法（#245），曾经的 `"test"` 已无法通过校验。
    config.environment = "paper".into();
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
        LiveEventPipeline::open(&data_dir, binance_event_log_name(&worker).unwrap(), "USDT")
            .unwrap();
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

/// 账户/交易所拓扑一致性只有三个单点判据，各自的语义边界必须钉住：账户对不上、
/// 交易所对不上、订单载荷非法都要返回 false。返回 true 就意味着这条订单可能落进
/// 另一台 worker 的 EventLog —— 一次性提交入口和常驻执行循环共用的就是这三份。
///
/// Paper 是虚拟执行域，故意不认 instrument 的真实 venue：一台 Paper worker 可以同时
/// 服务 Binance/OKX/Bybit 的标的，把虚拟账户绑死到某一家真实交易所是错误行为。
#[test]
fn submit_topology_guard_rejects_cross_account_and_cross_venue_orders() {
    let binance_order = mk_order(
        8101,
        &InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
        Side::Buy,
        1,
    );
    let okx_order = mk_order(
        8102,
        &InstrumentId::parse("ETHUSDT.OKX").unwrap(),
        Side::Buy,
        1,
    );
    let binance_command = mk_submit_command(8101, &binance_order, false);
    let okx_command = mk_submit_command(8102, &okx_order, false);

    let paper_worker = mk_worker("paper-main", WorkerRole::Execution, "paper", None);
    let binance_worker = mk_worker("binance-main", WorkerRole::Execution, "binance", None);
    let ccxt_worker = mk_worker("ccxt-okx-main", WorkerRole::Execution, "okx", None);
    let mut other_account = mk_worker("paper-other", WorkerRole::Execution, "paper", None);
    other_account.account_id = Some("other".into());
    let mut no_account = mk_worker("paper-no-account", WorkerRole::Execution, "paper", None);
    no_account.account_id = None;

    assert!(
        paper_submit_matches_worker(&binance_command, &paper_worker),
        "Paper 账户一致"
    );
    assert!(
        binance_submit_matches_worker(&binance_command, &binance_worker),
        "Binance 账户一致"
    );
    assert!(
        ccxt_submit_matches_worker(&okx_command, &ccxt_worker),
        "CCXT 账户一致"
    );

    assert!(
        !paper_submit_matches_worker(&binance_command, &other_account),
        "Paper 跨账户必须拒绝"
    );
    assert!(
        !binance_submit_matches_worker(&binance_command, &other_account),
        "Binance 跨账户必须拒绝"
    );
    assert!(
        !ccxt_submit_matches_worker(&okx_command, &other_account),
        "CCXT 跨账户必须拒绝"
    );

    // worker 没声明账户不能被当成通配：空字符串不是「匹配任何账户」。
    assert!(
        !paper_submit_matches_worker(&binance_command, &no_account),
        "缺 account_id 的 Paper worker"
    );
    assert!(
        !binance_submit_matches_worker(&binance_command, &no_account),
        "缺 account_id 的 Binance worker"
    );
    assert!(
        !ccxt_submit_matches_worker(&okx_command, &no_account),
        "缺 account_id 的 CCXT worker"
    );

    assert!(
        !binance_submit_matches_worker(&okx_command, &binance_worker),
        "Binance worker 不得接 OKX 标的"
    );
    assert!(
        !ccxt_submit_matches_worker(&binance_command, &ccxt_worker),
        "OKX worker 不得接 Binance 标的"
    );
    assert!(
        paper_submit_matches_worker(&okx_command, &paper_worker),
        "Paper 不绑真实交易所"
    );

    let mut broken = binance_command.clone();
    broken
        .payload
        .insert("order_json".into(), "not-json".into());
    assert!(
        !paper_submit_matches_worker(&broken, &paper_worker),
        "载荷非法按不匹配处理"
    );
    assert!(
        !binance_submit_matches_worker(&broken, &binance_worker),
        "载荷非法按不匹配处理"
    );
    assert!(
        !ccxt_submit_matches_worker(&broken, &ccxt_worker),
        "载荷非法按不匹配处理"
    );
}

/// 点名了一台 worker、订单却属于另一个账户：拓扑不一致要落成终态 Failed（不能把命令
/// 卡在 Accepted 让重投撞幂等闸门），而且拒绝发生在进交易所之前，EventLog 不留订单事实。
/// 用例刻意不给这台 worker 配风控规格，借此钉住拓扑判据排在风控判据之前。
#[test]
fn binance_submit_rejects_order_from_another_account_as_terminal_failure() {
    let root = temp_cli_case_dir("binance-cross-account");
    let data_dir = root.join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let template = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy")
        .join("qianxing.runtime.production.example.json");
    let mut config = read_runtime_config(&template).unwrap();
    config.config_fingerprint = None;
    config.environment = "paper".into();
    config.storage.data_dir = data_dir.to_string_lossy().into_owned();
    config.storage.backend = StorageBackend::Files;
    config.storage.consistency = qx_runtime::StorageConsistency::LocalDurable;
    config.storage.sqlite_path = None;
    let worker_id = "binance-user-main";
    let config_path = root.join("runtime.json");
    std::fs::write(&config_path, config.to_json().unwrap()).unwrap();

    let worker = config
        .workers
        .iter()
        .find(|worker| worker.id == worker_id)
        .cloned()
        .unwrap();
    let worker_account = worker.account_id.clone().unwrap();
    let mut order = mk_order(
        7201,
        &InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
        Side::Buy,
        1,
    );
    order.account_id = format!("not-{worker_account}");
    let command = mk_submit_command(7201, &order, false);
    let command_path = root.join("command.json");
    std::fs::write(&command_path, serde_json::to_string(&command).unwrap()).unwrap();

    let error = run_binance_submit_order(&config_path, worker_id, &command_path).unwrap_err();
    assert!(
        error.contains("拓扑不一致"),
        "点名 worker 与订单账户不一致必须按拓扑不一致拒绝: {error}"
    );

    let state = load_control_state(&data_dir).unwrap();
    let audit = state.audit();
    assert_eq!(audit.len(), 2, "命令应留下 Accepted + 终态两条审计");
    assert_eq!(audit[0].status, CommandStatus::Accepted);
    assert_eq!(audit[1].status, CommandStatus::Failed);
    assert!(
        audit[1].result_code.contains("拓扑不一致"),
        "终态原因码要带出拓扑不一致: {}",
        audit[1].result_code
    );

    let pipeline =
        LiveEventPipeline::open(&data_dir, binance_event_log_name(&worker).unwrap(), "USDT")
            .unwrap();
    assert!(
        pipeline.orders().is_empty(),
        "拓扑拒绝发生在提交之前，EventLog 不得出现订单事实"
    );
    assert!(pipeline.ledger().entries().is_empty());
    let _ = std::fs::remove_dir_all(root);
}

/// Paper 一次性入口没有 worker_id 参数：订单账户挑不出任何一台启用的 Paper worker 时，
/// 必须 fail-closed 而不是退回「取第一台」把订单写进别的账户账本，也不得落下初始资金。
#[test]
fn paper_submit_rejects_order_without_a_matching_worker_as_terminal_failure() {
    let root = temp_cli_case_dir("paper-no-matching-worker");
    let data_dir = root.join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let config = paper_runtime_config(&data_dir);
    let config_path = root.join("runtime.json");
    std::fs::write(&config_path, config.to_json().unwrap()).unwrap();

    let mut order = mk_order(
        7301,
        &InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
        Side::Buy,
        1,
    );
    order.account_id = "not-main".into();
    let command = mk_submit_command(7301, &order, false);
    let command_path = root.join("command.json");
    std::fs::write(&command_path, serde_json::to_string(&command).unwrap()).unwrap();

    let error = run_paper_submit_order(&config_path, &command_path).unwrap_err();
    assert!(
        error.contains("找不到与订单账户一致"),
        "挑不出匹配 worker 必须 fail-closed: {error}"
    );

    let state = load_control_state(&data_dir).unwrap();
    let audit = state.audit();
    assert_eq!(audit.len(), 2, "命令应留下 Accepted + 终态两条审计");
    assert_eq!(audit[0].status, CommandStatus::Accepted);
    assert_eq!(audit[1].status, CommandStatus::Failed);

    let pipeline = LiveEventPipeline::open(&data_dir, paper_account_log(), "USDT").unwrap();
    assert!(
        pipeline.ledger().entries().is_empty(),
        "没有匹配 worker 就一条初始资金都不能入账"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// CCXT 一次性入口显式点名 worker，必须与 Binance 一次性入口同源地拒绝跨账户订单。
/// 拓扑判据排在风控/交易所以前，所以用例不给这台 worker 配风控规格也能验证到拒单点；
/// CCXT 配置只要 `exchange_id` 与 `venue_id` 一致即可通过绑定校验（凭据只在真实发单时才需要）。
#[test]
fn ccxt_submit_rejects_order_from_another_account_as_terminal_failure() {
    let root = temp_cli_case_dir("ccxt-cross-account");
    let data_dir = root.join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    let template = workspace_root
        .join("deploy")
        .join("qianxing.runtime.ccxt.example.json");
    let mut config = read_runtime_config(&template).unwrap();
    config.config_fingerprint = None;
    config.environment = "paper".into();
    config.storage.data_dir = data_dir.to_string_lossy().into_owned();
    config.storage.backend = StorageBackend::Files;
    config.storage.consistency = qx_runtime::StorageConsistency::LocalDurable;
    config.storage.sqlite_path = None;
    let config_path = root.join("runtime.json");
    std::fs::write(&config_path, config.to_json().unwrap()).unwrap();

    let worker_id = "ccxt-execution-main";
    let ccxt_config_path = root.join("ccxt.okx.json");
    std::fs::write(
        &ccxt_config_path,
        serde_json::to_string(&serde_json::json!({ "exchange_id": "okx", "credential_env": null }))
            .unwrap(),
    )
    .unwrap();

    let worker = config
        .workers
        .iter()
        .find(|worker| worker.id == worker_id)
        .cloned()
        .unwrap();
    let worker_account = worker.account_id.clone().unwrap();
    let mut order = mk_order(
        7401,
        &InstrumentId::parse("ETHUSDT.OKX").unwrap(),
        Side::Buy,
        1,
    );
    order.account_id = format!("not-{worker_account}");
    let command = mk_submit_command(7401, &order, false);
    let command_path = root.join("command.json");
    std::fs::write(&command_path, serde_json::to_string(&command).unwrap()).unwrap();

    let error = run_ccxt_submit_order(&CcxtSubmitArgs {
        path: config_path.clone(),
        worker_id: worker_id.to_string(),
        ccxt_config: ccxt_config_path,
        command_path,
    })
    .unwrap_err();
    assert!(
        error.contains("拓扑不一致"),
        "点名 worker 与订单账户不一致必须按拓扑不一致拒绝: {error}"
    );

    let state = load_control_state(&data_dir).unwrap();
    let audit = state.audit();
    assert_eq!(audit.len(), 2, "命令应留下 Accepted + 终态两条审计");
    assert_eq!(audit[0].status, CommandStatus::Accepted);
    assert_eq!(audit[1].status, CommandStatus::Failed);
    assert!(
        audit[1].result_code.contains("拓扑不一致"),
        "终态原因码要带出拓扑不一致: {}",
        audit[1].result_code
    );

    let pipeline = LiveEventPipeline::open(
        &data_dir,
        required_account_event_log(&worker).unwrap(),
        "USDT",
    )
    .unwrap();
    assert!(
        pipeline.orders().is_empty(),
        "拓扑拒绝发生在提交之前，EventLog 不得出现订单事实"
    );
    assert!(pipeline.ledger().entries().is_empty());
    let _ = std::fs::remove_dir_all(root);
}
