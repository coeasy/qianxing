//! 账户级 EventLog 记账币种的全链路口径用例。
//!
//! `apply_fill` 按管线打开时传入的币种落现金腿，而该币种会随 `LedgerApplied` 事实
//! 永久落盘：写入方和读取方只要有一段退回默认 USDT，成交扣减和保证金读取就落在
//! 两本互不相干的账上，两边各自"自洽"，对账也看不出问题。

use super::*;

/// paper 拓扑里 `main` 账户在虚拟执行域下的账户级日志名。
const PAPER_ACCOUNT_LOG: &str = "paper-main-paper-events";

/// 纸面拓扑 + `paper-execution` 声明 USDC 结算。故意用小写：账簿键必须归一化，
/// 否则 `"usdc"` 与 `"USDC"` 会分裂成两本账。
fn usdc_paper_runtime(data_dir: &Path) -> RuntimeConfig {
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    let template = workspace_root
        .join("deploy")
        .join("qianxing.runtime.paper-strategy.example.json");
    let mut config = read_runtime_config(&template).unwrap();
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
    let mut pipeline = LiveEventPipeline::open(data_dir, PAPER_ACCOUNT_LOG, "USDC").unwrap();
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
        let mut pipeline = LiveEventPipeline::open(&data_dir, PAPER_ACCOUNT_LOG, "USDC").unwrap();
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

    let pipeline = LiveEventPipeline::open(&data_dir, PAPER_ACCOUNT_LOG, "USDC").unwrap();
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

    let sources = configured_account_event_logs(&config);
    let source = sources
        .iter()
        .find(|(_, _, log_name, _)| log_name == PAPER_ACCOUNT_LOG)
        .expect("paper 拓扑必须暴露账户级投影源");
    assert_eq!(
        source.3, "USDC",
        "投影源不能替账户挑一本默认账簿（禁用的 spread-recovery 声明不得覆盖）"
    );
    let _ = std::fs::remove_dir_all(root);
}
