//! 账户级 EventLog 记账币种的全链路口径用例。
//!
//! `apply_fill` 按管线打开时传入的币种落现金腿，而该币种会随 `LedgerApplied` 事实
//! 永久落盘：写入方和读取方只要有一段退回默认 USDT，成交扣减和保证金读取就落在
//! 两本互不相干的账上，两边各自"自洽"，对账也看不出问题。

use super::*;

/// 纸面拓扑 + `paper-execution` 声明 USDC 结算。故意用小写：账簿键必须归一化，
/// 否则 `"usdc"` 与 `"USDC"` 会分裂成两本账。规格结算币种跟着改成 USDC——
/// 记账币种和规格不一致是配置错误，风控会直接拒绝。
fn usdc_paper_runtime(data_dir: &Path) -> RuntimeConfig {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    let template = workspace_root
        .join("deploy")
        .join("qianxing.runtime.paper-strategy.example.json");
    let mut config = read_runtime_config(&template).unwrap();
    config.storage.data_dir = data_dir.to_string_lossy().into_owned();
    let usdc_spec = spot_spec_settled_in(data_dir, "USDC")
        .to_string_lossy()
        .into_owned();
    for worker in config.workers.iter_mut() {
        if worker.instrument_spec_path.is_some() {
            worker.instrument_spec_path = Some(usdc_spec.clone());
        }
    }
    config
        .workers
        .iter_mut()
        .find(|worker| worker.id == "paper-execution")
        .expect("paper 拓扑缺少 paper-execution worker")
        .settlement_currency = Some("usdc".into());
    config
}

/// 往账户级日志播种一笔 USDC 现金，作为读模型唯一的账户事实。
fn seed_usdc_account_cash(data_dir: &Path, amount: i64) {
    let mut pipeline = LiveEventPipeline::open(data_dir, paper_account_log(), "USDC").unwrap();
    pipeline
        .ingest(RuntimeEventEnvelope::venue(
            RuntimeExternalEvent::AccountCashflow {
                cashflow: AccountCashflow {
                    account_id: "main".into(),
                    venue_id: "paper".into(),
                    currency: "USDC".into(),
                    kind: CashflowKind::Transfer,
                    amount: Money::from_i64(amount),
                    external_id: format!("caliper:transfer:{amount}"),
                },
            },
            1,
            1,
            1,
            "caliper:transfer",
        ))
        .unwrap();
}

/// 写入侧证据：Paper 执行 worker 归约的成交必须记进它自己声明的结算账簿。
#[test]
fn paper_execution_worker_books_fills_in_the_worker_settlement_currency() {
    let root = temp_cli_case_dir("currency-caliper-worker");
    let data_dir = root.join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let config = usdc_paper_runtime(&data_dir);
    let config_path = root.join("runtime.json");
    std::fs::write(&config_path, config.to_json().unwrap()).unwrap();
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    // fail-closed 语义要求撮合行情来自同一账户日志的行情事实。
    {
        let mut pipeline = LiveEventPipeline::open(&data_dir, paper_account_log(), "USDC").unwrap();
        let ts = runtime_timestamp_ms();
        pipeline
            .ingest(RuntimeEventEnvelope::market_quote(
                instrument.clone(),
                QuoteTick::new(
                    ts,
                    Price::from_i64(99),
                    Quantity::from_i64(1_000),
                    Price::from_i64(100),
                    Quantity::from_i64(1_000),
                    ts,
                ),
                ts,
                ts,
                "caliper:quote",
            ))
            .unwrap();
    }
    let order = mk_order(9601, &instrument, Side::Buy, 1);
    let command = mk_submit_command(9601, &order, false);
    let control = ControlStateBackend::Files(JsonStateStore::new(&data_dir));
    control
        .transact(|plane| plane.submit_as(command.clone(), Permission::Trading, 10))
        .unwrap()
        .1
        .unwrap();
    ControlCommandQueue::new(data_dir.join("control-queue"))
        .enqueue(command.clone(), 10)
        .unwrap();

    run_paper_execution_worker(&config_path, "paper-execution", true).unwrap();

    let pipeline = LiveEventPipeline::open(&data_dir, paper_account_log(), "USDC").unwrap();
    assert_eq!(pipeline.orders()[0].status, OrderStatus::Filled);
    let ledger = pipeline.ledger();
    let tagged: Vec<(String, String)> = ledger
        .entries()
        .iter()
        .map(|entry| (format!("{:?}", entry.kind), entry.currency.clone()))
        .collect();
    assert!(
        tagged.iter().all(|(_, currency)| currency == "USDC"),
        "成交事实的现金腿必须记在 worker 声明的结算账簿里: {tagged:?}"
    );
    assert_eq!(
        ledger.cash_for("main", "USDT"),
        0,
        "未声明的 USDT 账簿不能被动用"
    );
    let settled = ledger.cash_for("main", "USDC");
    assert!(
        settled > 0 && settled < 100_000 * SCALE,
        "结算账簿必须被成交扣减，实际 {settled}"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// 读模型侧证据：API 账户快照的权益要落在账户自己的结算账簿上。
#[test]
fn api_account_snapshot_reports_the_worker_settlement_currency() {
    let root = temp_cli_case_dir("currency-caliper-api");
    let data_dir = root.join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let config = usdc_paper_runtime(&data_dir);
    seed_usdc_account_cash(&data_dir, 1_000);

    let snapshots = load_api_account_snapshots(&config).unwrap();
    let snapshot = snapshots
        .iter()
        .find(|snapshot| snapshot.header.account_id == "main")
        .expect("API 读模型必须看到 paper 账户快照");
    assert_eq!(
        snapshot.cash_raw.get("USDC").copied(),
        Some(1_000 * SCALE),
        "现金余额按账户结算币种账簿读出"
    );
    assert_eq!(
        snapshot.equity_raw,
        1_000 * SCALE,
        "权益必须与结算账簿同一口径，读成默认 USDT 会得到 0"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// StrategyContext 侧证据：可用保证金按账户日志的结算账簿计出。
#[test]
fn strategy_account_context_reads_the_worker_settlement_currency() {
    let root = temp_cli_case_dir("currency-caliper-strategy");
    let data_dir = root.join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let config = usdc_paper_runtime(&data_dir);
    seed_usdc_account_cash(&data_dir, 1_000);
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();

    let (_, cash, available_margin, provenance) =
        strategy_account_context(&data_dir, &config, &instrument).unwrap();
    assert_eq!(provenance, "ledger-replayed-account-state");
    assert_eq!(cash.get("USDC").copied(), Some(1_000 * SCALE));
    assert_eq!(
        available_margin,
        Some(1_000 * SCALE),
        "保证金口径必须与账户日志的结算账簿一致"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// 投影桥侧证据：它按 (account, venue) 建立只读投影，记账币种要跟着同一 worker 声明。
#[test]
fn api_projection_bridge_sources_use_the_worker_settlement_currency() {
    let root = temp_cli_case_dir("currency-caliper-bridge");
    let data_dir = root.join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let config = usdc_paper_runtime(&data_dir);

    let sources = configured_account_event_logs(&config).unwrap();
    let source = sources
        .iter()
        .find(|(_, _, log_name, _)| log_name == &paper_account_log())
        .expect("paper 拓扑必须暴露账户级投影源");
    assert_eq!(
        source.3, "USDC",
        "投影源不能替账户挑一本默认账簿（禁用的 spread-recovery 声明不得覆盖）"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// 让 `paper-spread-recovery` 成为 `(main, paper)` 身份的第二个启用写入方。
/// 它与 `paper-execution` 写同一本账户日志，所以两者的结算币种声明必须一致。
fn add_second_account_log_writer(config: &mut RuntimeConfig, currency: &str) {
    let worker = config
        .workers
        .iter_mut()
        .find(|worker| worker.id == "paper-spread-recovery")
        .expect("paper 拓扑缺少 paper-spread-recovery worker");
    worker.enabled = true;
    worker.settlement_currency = Some(currency.into());
}

/// 同一本账户日志的两个写入方声明了不同币种：读取侧不能按配置顺序猜一本。
#[test]
fn conflicting_settlement_declarations_on_one_account_log_fail_closed() {
    let root = temp_cli_case_dir("currency-caliper-conflict");
    let data_dir = root.join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let mut config = usdc_paper_runtime(&data_dir);
    add_second_account_log_writer(&mut config, "USDT");
    let log_name = paper_account_log();

    let error = match settlement_currency_for_log(&config, &log_name) {
        Ok(currency) => panic!("写入方币种冲突不能静默取第一个声明者，实际读出 {currency}"),
        Err(error) => error,
    };
    for token in ["paper-execution", "paper-spread-recovery", "USDC", "USDT"] {
        assert!(
            error.contains(token),
            "冲突必须点名双方 worker 和各自的币种，缺 {token}: {error}"
        );
    }
    assert!(
        configured_account_event_logs(&config).err().is_some(),
        "投影源不得带着猜测出来的账簿口径启动"
    );
    assert!(
        open_account_pipeline(&config, &data_dir, &log_name)
            .err()
            .is_some(),
        "读模型打开账户日志必须失败，而不是读到半本账"
    );

    let config_path = root.join("runtime.json");
    std::fs::write(&config_path, config.to_json().unwrap()).unwrap();
    let report = collect_doctor_report(&config_path).unwrap();
    let failures = report["failures"].as_array().unwrap();
    assert!(
        failures
            .iter()
            .any(|failure| failure.as_str().is_some_and(|message| {
                message.contains("paper-execution") && message.contains("paper-spread-recovery")
            })),
        "doctor 必须把币种冲突报成配置错误: {failures:?}"
    );
    assert_eq!(report["ok"], serde_json::json!(false));
    let _ = std::fs::remove_dir_all(root);
}

/// 口径一致时不得误报：两个写入方都声明 USDC（大小写不同）仍然是一本账。
#[test]
fn agreeing_settlement_declarations_on_one_account_log_are_accepted() {
    let root = temp_cli_case_dir("currency-caliper-agree");
    let data_dir = root.join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let mut config = usdc_paper_runtime(&data_dir);
    add_second_account_log_writer(&mut config, "usdc");

    assert_eq!(
        settlement_currency_for_log(&config, &paper_account_log()).unwrap(),
        "USDC"
    );
    assert_eq!(
        configured_account_event_logs(&config)
            .unwrap()
            .iter()
            .filter(|(_, _, log_name, _)| log_name == &paper_account_log())
            .count(),
        1
    );
    let _ = std::fs::remove_dir_all(root);
}

/// 冲突不能只停在 doctor：真正往同一本账户日志落账的写入方必须一起拒绝开册。
/// 币种随 `LedgerApplied` 永久落盘，写入方照常启动等于把冲突写进历史。
#[test]
fn conflicting_settlement_declarations_block_the_writers_that_book_the_log() {
    let root = temp_cli_case_dir("currency-caliper-writer");
    let data_dir = root.join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let mut config = usdc_paper_runtime(&data_dir);
    add_second_account_log_writer(&mut config, "USDT");

    let storage = PipelineStorage::from_config(&config).unwrap();
    match open_paper_market_bridges(&storage, &config.workers) {
        Ok(bridges) => panic!(
            "Paper 行情桥按第一个声明者的口径开了 {} 本册，冲突配置必须拒绝",
            bridges.len()
        ),
        Err(error) => assert!(
            error.contains("paper-spread-recovery"),
            "写入方的拒绝必须点名冲突的另一方: {error}"
        ),
    }

    let config_path = root.join("runtime.json");
    std::fs::write(&config_path, config.to_json().unwrap()).unwrap();
    let error = run_paper_execution_worker(&config_path, "paper-execution", true)
        .expect_err("冲突配置下写入 worker 不得播种初始资金并落账");
    assert!(
        error.contains("paper-spread-recovery") && error.contains("paper-execution"),
        "写入 worker 的拒绝必须点名冲突双方: {error}"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// 风控侧证据：记账币种只有一把尺子。worker 未声明币种时曾退回 market spec 的
/// `settlement_currency`——那说的是"这个标的用什么结算"，不是"这本账户账簿记什么币"。
/// 两者不一致时可读保证金会从一本空账簿算成 0，账户里明明有钱却被按保证金不足拒单。
#[test]
fn risk_context_refuses_a_spec_currency_that_is_not_the_ledger_book() {
    let root = temp_cli_case_dir("currency-caliper-risk");
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let spec = TradingInstrumentSpec {
        instrument: instrument.clone(),
        product: TradingProduct::Spot,
        base_currency: "BTC".into(),
        quote_currency: "USDT".into(),
        settlement_currency: "USDT".into(),
        contract_size: SCALE,
        linear: true,
        inverse: false,
        price_tick: 1,
        qty_step: SCALE,
        min_qty: SCALE,
        max_leverage: 1,
        maintenance_margin_bps: 0,
        valid_from: 1,
        valid_to: None,
    };
    let spec_path = root.join("spot.spec.json");
    std::fs::write(&spec_path, serde_json::to_vec(&spec).unwrap()).unwrap();
    let mut worker = mk_worker(
        "paper-execution",
        WorkerRole::Execution,
        "paper",
        Some(&spec_path.to_string_lossy()),
    );
    worker.settlement_currency = None;
    seed_usdc_account_cash(&root, 1_000);
    let pipeline = LiveEventPipeline::open(&root, paper_account_log(), "USDC").unwrap();
    let order = mk_order(9610, &instrument, Side::Buy, 1);

    let error = worker_risk_context(&worker, &order, &pipeline, None)
        .expect_err("规格结算币种与账户账簿不一致时必须拒绝，而不是按 0 保证金拒单");
    for token in ["paper-execution", "USDC", "USDT"] {
        assert!(
            error.contains(token),
            "不一致必须点名 worker 和两本币种，缺 {token}: {error}"
        );
    }
    let _ = std::fs::remove_dir_all(root);
}

/// API 读模型侧证据：账户身份要去重到"同一本账一份投影"，键必须是规范化后的身份。
/// `main/paper` 与 `" main "/Paper` 是同一本日志，按配置原文去重会投影出两份快照，
/// 且后一份用未 trim 的账户号查账簿——读到空账簿也不报错。
#[test]
fn api_snapshots_deduplicate_by_normalized_account_identity() {
    let root = temp_cli_case_dir("api-identity-dedupe");
    let data_dir = root.join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let mut config = usdc_paper_runtime(&data_dir);
    let mut twin = config
        .workers
        .iter()
        .find(|worker| worker.id == "paper-execution")
        .expect("paper 拓扑缺少 paper-execution worker")
        .clone();
    twin.id = "paper-execution-twin".into();
    twin.account_id = Some(" main ".into());
    twin.venue_id = Some("Paper".into());
    config.workers.push(twin);
    seed_usdc_account_cash(&data_dir, 1_000);

    let snapshots = load_api_account_snapshots(&config).unwrap();
    let paper: Vec<(String, String)> = snapshots
        .iter()
        .map(|snapshot| {
            (
                snapshot.header.account_id.clone(),
                snapshot.header.venue_id.clone(),
            )
        })
        .collect();
    assert_eq!(
        paper,
        vec![("main".to_string(), "paper".to_string())],
        "同一账户身份只能投影一份、且按规范化账户号查账簿"
    );
    assert_eq!(
        snapshots[0].equity_raw,
        1_000 * SCALE,
        "去重后留下的那份必须真读到 USDC 账簿，而不是空账簿"
    );
    let _ = std::fs::remove_dir_all(root);
}
