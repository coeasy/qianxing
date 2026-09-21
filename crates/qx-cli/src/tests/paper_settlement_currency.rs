use super::*;

/// EventLog 按记账币种选择现金账簿：worker 声明的结算币种必须是打开日志、播种初始资金、
/// 归约成交和构造风控快照共用的同一个值。大小写错位或某一段退回默认 USDT，都会让成交
/// 扣减写进一本账、风控和余额读另一本账——两本账各自都"自洽"，所以对账看不出问题。
#[test]
fn paper_submit_order_books_in_the_worker_settlement_currency() {
    let root = temp_cli_case_dir("paper-settlement-currency");
    let data_dir = root.join("data");
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
    // 故意用小写：币种代码在账簿键上必须归一化，否则 "usdc" 与 "USDC" 会分裂成两本账。
    config
        .workers
        .iter_mut()
        .find(|worker| worker.id == "paper-execution")
        .expect("paper 拓扑缺少 paper-execution worker")
        .settlement_currency = Some("usdc".into());
    let config_path = root.join("runtime.json");
    std::fs::write(&config_path, config.to_json().unwrap()).unwrap();
    // fail-closed 语义要求撮合行情来自 EventLog 行情事实。
    {
        let mut pipeline = LiveEventPipeline::open(&data_dir, "paper-events", "USDC").unwrap();
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
                "paper-events:settlement-currency-quote",
            ))
            .unwrap();
    }
    let order = mk_order(
        8201,
        &InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
        Side::Buy,
        1,
    );
    let command = ControlCommand {
        command_id: 8201,
        request_id: "paper-submit-8201".into(),
        operator_id: "paper".into(),
        reason: "settlement currency".into(),
        kind: CommandKind::SubmitOrder,
        target: "8201".into(),
        payload: BTreeMap::from([("order_json".into(), serde_json::to_string(&order).unwrap())]),
        permission: Permission::Trading,
        dry_run: false,
    };
    let command_path = root.join("command.json");
    std::fs::write(&command_path, serde_json::to_string(&command).unwrap()).unwrap();
    run_paper_submit_order(&config_path, &command_path).unwrap();

    let pipeline = LiveEventPipeline::open(&data_dir, "paper-events", "USDC").unwrap();
    assert_eq!(pipeline.orders()[0].status, OrderStatus::Filled);
    let ledger = pipeline.ledger();
    let settled = ledger.cash_for("main", "USDC");
    assert!(
        settled > 0 && settled < 100_000_000_000_000,
        "成交必须扣减 worker 结算币种账簿，实际 {settled}"
    );
    assert_eq!(
        ledger.cash_for("main", "USDT"),
        0,
        "非结算币种账簿不能被 Paper 成交动用"
    );
    assert!(
        ledger
            .entries()
            .iter()
            .all(|entry| entry.currency == "USDC"),
        "Ledger 条目的币种标签必须与结算币种一致"
    );
    let _ = std::fs::remove_dir_all(root);
}
