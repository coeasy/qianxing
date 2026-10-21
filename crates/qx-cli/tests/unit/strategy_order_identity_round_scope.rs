//! 策略订单身份的跨轮口径（V13 R2 第二十五遍 #248）。
//!
//! `intent_id` 只是**轮内**局部序号，而 `client_id` 同时是控制面的 `command_id` 与执行面的
//! `client_order_id`。`invoke_builtin_strategy` 每轮新建一个 `BuiltinStrategy`，序号从 1 重新
//! 起算，所以折进 `round_scope` 之前，第二轮的第一条意图会复用第一轮的身份，被
//! `ControlPlane::submit` 按重复命令拒掉。这里钉住三件事：同轮重放幂等、换轮得到新身份、
//! 连续换轮能被控制面逐条受理。

use super::*;

fn contract_intent(intent_id: u64) -> StrategyContractIntent {
    StrategyContractIntent {
        intent_id,
        instrument: "BTCUSDT.BINANCE".into(),
        side: "buy".into(),
        qty_raw: SCALE,
        limit_price_raw: None,
        reduce_only: false,
        post_only: false,
        position_side: None,
        margin_mode: None,
        position_mode: None,
        leverage: None,
    }
}

fn round_config() -> RuntimeConfig {
    let template = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy")
        .join("qianxing.runtime.paper-strategy.example.json");
    read_runtime_config(&template).unwrap()
}

fn order_for(config: &RuntimeConfig, round_scope: &str, intent_id: u64) -> Order {
    build_strategy_order_from_contract_intent(
        config,
        "strategy-round",
        7001,
        round_scope,
        &contract_intent(intent_id),
        10,
        0,
        None,
    )
    .unwrap()
}

#[test]
fn contract_intent_identity_is_scoped_by_round() {
    let config = round_config();
    // 同一轮重放必须回到同一个身份，否则幂等就没了。
    assert_eq!(
        order_for(&config, "9201", 1).client_id,
        order_for(&config, "9201", 1).client_id
    );
    // 换轮即使轮内序号相同也必须得到新身份。
    assert_ne!(
        order_for(&config, "9201", 1).client_id,
        order_for(&config, "9202", 1).client_id
    );
    // 同一轮内的不同意图仍是不同身份。
    assert_ne!(
        order_for(&config, "9201", 1).client_id,
        order_for(&config, "9201", 2).client_id
    );
    // 摘要可能取到 0，而 client_order_id 必须为正。
    assert!(order_for(&config, "9201", 1).client_id > 0);
    // 归因侧保留策略原值，跨轮折身份不能把 intent_id 洗掉。
    let order = order_for(&config, "9201", 3);
    assert_eq!(
        order.trace.expect("策略订单必须带 trace").intent_id,
        Some(3)
    );
}

#[test]
fn consecutive_rounds_are_accepted_by_the_control_plane() {
    let config = round_config();
    let mut plane = ControlPlane::default();
    let mut accepted = Vec::new();
    for run_id in ["9301", "9302", "9303"] {
        let order = order_for(&config, run_id, 1);
        let command = strategy_submit_command("strategy-round", &order, false, None).unwrap();
        // 这两格都由 client_id 派生：换轮若身份不变，这里会撞 DuplicateCommand。
        assert_eq!(command.command_id, order.client_id);
        assert_eq!(
            command.request_id,
            format!("strategy:strategy-round:{}", order.client_id)
        );
        let record = plane
            .submit(command, 10)
            .unwrap_or_else(|error| panic!("{run_id} 第二轮起的提交被控制面拒绝: {error:?}"));
        accepted.push(record.command_id);
    }
    accepted.sort();
    accepted.dedup();
    assert_eq!(accepted.len(), 3, "三轮必须留下三个不同的订单身份");
}

#[test]
fn strategy_contract_orders_are_rounded_to_the_execution_market_lot() {
    let runtime = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy")
        .join("qianxing.runtime.paper-strategy.example.json");
    let config = read_runtime_config(&runtime).unwrap();
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let spec = strategy_execution_market_spec(&config, &instrument, Some(&runtime))
        .unwrap()
        .expect("paper execution worker must bind a market spec");
    let mut intent = contract_intent(1);
    intent.qty_raw = spec.qty_step + 1;
    let order = build_strategy_order_from_contract_intent(
        &config,
        "strategy-round",
        7001,
        "lot-step-round",
        &intent,
        10,
        0,
        Some(&spec),
    )
    .unwrap();
    assert_eq!(order.qty.raw(), spec.qty_step);
    assert_eq!(order.qty.raw() % spec.qty_step, 0);

    let target_order = build_strategy_order_with_signal(
        &config,
        "strategy-round",
        7002,
        10,
        0,
        spec.qty_step + 1,
        None,
        Some(&spec),
    )
    .unwrap()
    .expect("target_qty should produce an order");
    assert_eq!(target_order.qty.raw(), spec.qty_step);

    intent.qty_raw = spec.min_qty - 1;
    let error = build_strategy_order_from_contract_intent(
        &config,
        "strategy-round",
        7001,
        "below-minimum-round",
        &intent,
        10,
        0,
        Some(&spec),
    )
    .unwrap_err();
    assert!(error.contains("小于最小数量"), "{error}");

    let target_error = build_strategy_order_with_signal(
        &config,
        "strategy-round",
        7003,
        10,
        0,
        spec.min_qty - 1,
        None,
        Some(&spec),
    )
    .unwrap_err();
    assert!(target_error.contains("小于最小数量"), "{target_error}");
}

#[test]
fn paper_kelly_strategy_reaches_the_market_spec_risk_and_ledger_path() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
    let root = workspace.join(".task-tmp").join(format!(
        "kelly-paper-submit-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let data_dir = root.join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let template = workspace
        .join("deploy")
        .join("qianxing.runtime.paper-strategy.example.json");
    let mut config = read_runtime_config(&template).unwrap();
    config.storage.data_dir = data_dir.to_string_lossy().into_owned();
    config.strategy.id = Some("strategy-kelly-paper".into());
    config.strategy.version = "paper-kelly-e2e-v1".into();
    config.strategy.instrument = Some("BTCUSDT.BINANCE".into());
    config.strategy.builtin_strategy = Some("momentum".into());
    config.strategy.builtin_target_kelly_fraction_bps = Some(5_000);
    config.strategy.builtin_kelly_lookback_bars = Some(30);
    let closes = (0..70_i128)
        .map(|index| (100 + index - if index % 5 == 0 { 2 } else { 0 }) * SCALE)
        .collect::<Vec<_>>();
    let bars_path = root.join("bars.json");
    let bars = serde_json::json!({
        "instrument": "BTCUSDT.BINANCE",
        "source": "kelly-paper-submit-v1",
        "ts": (1..=70_u64).collect::<Vec<_>>(),
        "open_raw": closes,
        "high_raw": closes.iter().map(|close| close + SCALE).collect::<Vec<_>>(),
        "low_raw": closes.iter().map(|close| close - SCALE).collect::<Vec<_>>(),
        "close_raw": closes,
        "volume_raw": vec![10 * SCALE; 70],
    });
    std::fs::write(&bars_path, serde_json::to_vec(&bars).unwrap()).unwrap();
    config.strategy.bars_snapshot_path = Some(bars_path.to_string_lossy().into_owned());
    let market_spec_path = workspace
        .join("deploy")
        .join("qianxing.binance.spot.spec.json")
        .to_string_lossy()
        .into_owned();
    let execution_worker = config
        .workers
        .iter_mut()
        .find(|worker| worker.id == "paper-execution")
        .unwrap();
    execution_worker.instrument_spec_path = Some(market_spec_path);
    let runtime_path = root.join("runtime.json");
    std::fs::write(&runtime_path, config.to_json().unwrap()).unwrap();

    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let log_name = paper_account_log();
    let mut pipeline = LiveEventPipeline::open(&data_dir, log_name, "USDT").unwrap();
    pipeline
        .ingest(RuntimeEventEnvelope::venue(
            RuntimeExternalEvent::AccountCashflow {
                cashflow: AccountCashflow {
                    account_id: "main".into(),
                    venue_id: "paper".into(),
                    currency: "USDT".into(),
                    kind: CashflowKind::Transfer,
                    amount: Money::from_i64(10_000),
                    external_id: "kelly-e2e:initial-cash".into(),
                },
            },
            1,
            1,
            1,
            "kelly-e2e:initial-cash",
        ))
        .unwrap();
    pipeline
        .ingest(RuntimeEventEnvelope::market_quote(
            instrument.clone(),
            QuoteTick::new(
                71,
                Price::from_raw(167 * SCALE),
                Quantity::from_i64(100),
                Price::from_raw(167 * SCALE),
                Quantity::from_i64(100),
                70,
            ),
            71,
            71,
            "kelly-e2e:quote",
        ))
        .unwrap();

    let output = invoke_builtin_strategy(
        &data_dir,
        &config,
        &instrument,
        "kelly-paper-submit",
        u64::MAX,
    )
    .unwrap();
    let intent = output.intents.first().expect("Kelly 应生成目标意图");
    let market_spec = strategy_execution_market_spec(&config, &instrument, Some(&runtime_path))
        .unwrap()
        .expect("Paper execution 必须绑定 market spec");
    let order = build_strategy_order_from_contract_intent(
        &config,
        &output.strategy_id,
        output.signal_id,
        &output.request_id,
        intent,
        72,
        0,
        Some(&market_spec),
    )
    .unwrap();
    assert_eq!(order.qty.raw() % market_spec.qty_step, 0);
    let command = strategy_submit_command(&output.strategy_id, &order, false, None).unwrap();
    let worker = config
        .workers
        .iter()
        .find(|worker| worker.id == "paper-execution")
        .unwrap();
    let spread_store = FileSpreadOrderGroupStore::new(data_dir.join("spread-groups")).unwrap();
    let result = paper_submit_match_attempt(
        &config,
        &runtime_path,
        &data_dir,
        &command,
        worker,
        &mut pipeline,
        &spread_store,
        72,
    )
    .unwrap();
    assert!(result.starts_with("PAPER_EXECUTED fills=1"), "{result}");
    assert_eq!(pipeline.orders().len(), 1);
    assert_eq!(pipeline.orders()[0].status, OrderStatus::Filled);
    assert!(!pipeline.ledger().entries().is_empty());
}
