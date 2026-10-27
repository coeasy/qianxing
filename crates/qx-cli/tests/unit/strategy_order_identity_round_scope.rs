//! 策略订单身份的跨轮口径（V13 R2 第二十五遍 #248）。
//!
//! `intent_id` 只是**轮内**局部序号，而 `client_id` 同时是控制面的 `command_id` 与执行面的
//! `client_order_id`。`invoke_builtin_strategy` 每轮新建一个 `BuiltinStrategy`，序号从 1 重新
//! 起算，所以折进 `round_scope` 之前，第二轮的第一条意图会复用第一轮的身份，被
//! `ControlPlane::submit` 按重复命令拒掉。这里钉住三件事：同轮重放幂等、换轮得到新身份、
//! 连续换轮能被控制面逐条受理。

use super::*;
use qx_core::{ExactDecimal, ExactTradingRules, PositionMode};

fn contract_intent(intent_id: u64) -> StrategyContractIntent {
    StrategyContractIntent {
        intent_id,
        instrument: "BTCUSDT.BINANCE".into(),
        account_id: None,
        side: "buy".into(),
        qty_raw: SCALE,
        qty_exact: None,
        quote_budget_exact: None,
        limit_price_raw: None,
        limit_price_exact: None,
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
        None,
        Some(&spec),
    )
    .unwrap_err();
    assert!(target_error.contains("小于最小数量"), "{target_error}");
}

#[test]
fn cross_venue_intent_uses_its_account_and_frozen_market_rules() {
    let runtime = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy")
        .join("qianxing.runtime.multi-venue-contract.example.json");
    let mut config = read_runtime_config(&runtime).unwrap();
    let data_dir = temp_cli_case_dir("cross-venue-account-route");
    config.storage.data_dir = data_dir.to_string_lossy().into_owned();
    let intent = StrategyContractIntent {
        intent_id: 11,
        instrument: "BTC/USDT:USDT.BINANCE".into(),
        account_id: Some("binance-usdm-sandbox".into()),
        side: "buy".into(),
        qty_raw: 0,
        qty_exact: Some("0.001".into()),
        quote_budget_exact: None,
        limit_price_raw: None,
        limit_price_exact: None,
        reduce_only: false,
        post_only: false,
        position_side: Some("net".into()),
        margin_mode: Some("isolated".into()),
        position_mode: Some("one_way".into()),
        leverage: Some(2),
    };
    let order = crate::workers::build_strategy_contract_intent_order(
        &data_dir,
        &config,
        &runtime,
        "strategy-cross-venue-contract",
        12,
        "cross-venue-account-route",
        &intent,
        13,
    )
    .unwrap();
    assert_eq!(order.account_id, "binance-usdm-sandbox");
    assert_eq!(order.instrument.to_string(), "BTC/USDT:USDT.BINANCE");
    assert_eq!(
        order.exact_amounts.as_ref().unwrap().quantity.to_string(),
        "0.001"
    );

    let mut invalid_route = intent;
    invalid_route.account_id = Some("unconfigured-account".into());
    let error = crate::workers::build_strategy_contract_intent_order(
        &data_dir,
        &config,
        &runtime,
        "strategy-cross-venue-contract",
        12,
        "cross-venue-invalid-route",
        &invalid_route,
        13,
    )
    .unwrap_err();
    assert!(error.contains("没有匹配标的"), "{error}");
}

#[test]
fn builtin_cross_venue_reference_leg_resolves_a_distinct_account() {
    let runtime = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy")
        .join("qianxing.runtime.multi-venue-arbitrage.example.json");
    let mut config = read_runtime_config(&runtime).unwrap();
    config.strategy.live_enabled = true;
    config.validate().unwrap();

    let reference = InstrumentId::parse("BTC/USDT:USDT.BINANCE").unwrap();
    assert_eq!(
        crate::strategy_execution_account_for_instrument(
            &config,
            config.strategy.account_id.as_deref(),
            &reference,
        )
        .unwrap(),
        "binance-usdm-sandbox"
    );

    let mut ambiguous = config;
    let mut second_route = ambiguous
        .workers
        .iter()
        .find(|worker| worker.id == "execution-binance-usdm")
        .unwrap()
        .clone();
    second_route.id = "execution-binance-second-account".into();
    second_route.account_id = Some("binance-second-account".into());
    ambiguous.workers.push(second_route);
    let mut second_account = ambiguous.accounts[0].clone();
    second_account.id = "binance-second-account".into();
    ambiguous.accounts.push(second_account);
    let error = ambiguous.validate().unwrap_err();
    assert!(error.contains("匹配多个执行账户"), "{error}");
}

#[test]
fn python_strategy_input_loads_pit_aligned_bars_for_both_venues() {
    let runtime = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy")
        .join("qianxing.runtime.multi-venue-contract.example.json");
    let mut config = read_runtime_config(&runtime).unwrap();
    let root = temp_cli_case_dir("strategy-multi-market-input");
    config.storage.data_dir = root.to_string_lossy().into_owned();
    resolve_strategy_runtime_paths(&mut config.strategy, &runtime);
    let primary = InstrumentId::parse("BTC/USDT:USDT.OKX").unwrap();

    let input =
        build_strategy_contract_input(&root, &config, &primary, "multi-market-1", 70_000).unwrap();
    assert!(input.bars.as_ref().unwrap().ts.last() == Some(&70_000));
    let reference = input
        .market_bars
        .get("BTC/USDT:USDT.BINANCE")
        .expect("Python Strategy 必须收到 Binance 对冲腿历史");
    assert_eq!(reference.ts.last(), Some(&70_000));
    assert!(input.data_fingerprint.starts_with("multi-market:"));
}

#[test]
fn strategy_contract_exact_intent_reaches_the_scale_aware_order_builder() {
    let config = round_config();
    let instrument = InstrumentId::parse("PEPEUSDT.BINANCE").unwrap();
    let spec = TradingInstrumentSpec {
        instrument,
        product: TradingProduct::Spot,
        base_currency: "PEPE".into(),
        quote_currency: "USDT".into(),
        settlement_currency: "USDT".into(),
        contract_size: SCALE,
        linear: false,
        inverse: false,
        price_tick: 1,
        qty_step: 1,
        min_qty: 1,
        min_notional_raw: 0,
        max_leverage: 1,
        maintenance_margin_bps: 0,
        valid_from: 0,
        valid_to: None,
        exact_rules: Some(ExactTradingRules {
            contract_size: ExactDecimal::parse("1").unwrap(),
            price_tick: ExactDecimal::parse("0.00000001").unwrap(),
            quantity_step: ExactDecimal::parse("0.0000000001").unwrap(),
            minimum_quantity: ExactDecimal::parse("0.0000000001").unwrap(),
            minimum_notional: ExactDecimal::ZERO,
        }),
    };
    spec.validate().unwrap();
    let mut intent = contract_intent(17);
    intent.instrument = "PEPEUSDT.BINANCE".into();
    intent.qty_raw = 0;
    intent.qty_exact = Some("0.0000000001".into());
    intent.limit_price_exact = Some("0.00000005".into());
    let order = build_strategy_order_from_contract_intent(
        &config,
        "strategy-round",
        7001,
        "exact-round",
        &intent,
        10,
        0,
        Some(&spec),
    )
    .unwrap();

    assert_eq!(order.qty.raw(), 0);
    assert_eq!(
        order.exact_amounts.unwrap().quantity,
        ExactDecimal::parse("0.0000000001").unwrap()
    );
}

#[test]
fn exact_target_rebalance_uses_exact_current_position_and_rejects_hedge_ambiguity() {
    let runtime = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy")
        .join("qianxing.runtime.paper-strategy.example.json");
    let config = read_runtime_config(&runtime).unwrap();
    let instrument = InstrumentId::parse("PEPEUSDT.BINANCE").unwrap();
    let spec = TradingInstrumentSpec {
        instrument,
        product: TradingProduct::Spot,
        base_currency: "PEPE".into(),
        quote_currency: "USDT".into(),
        settlement_currency: "USDT".into(),
        contract_size: SCALE,
        linear: false,
        inverse: false,
        price_tick: 1,
        qty_step: 1,
        min_qty: 1,
        min_notional_raw: 0,
        max_leverage: 1,
        maintenance_margin_bps: 0,
        valid_from: 0,
        valid_to: None,
        exact_rules: Some(ExactTradingRules {
            contract_size: ExactDecimal::parse("1").unwrap(),
            price_tick: ExactDecimal::parse("0.00000001").unwrap(),
            quantity_step: ExactDecimal::parse("0.0000000001").unwrap(),
            minimum_quantity: ExactDecimal::parse("0.0000000001").unwrap(),
            minimum_notional: ExactDecimal::ZERO,
        }),
    };
    let output = StrategyContractOutput {
        schema_version: qx_runtime::STRATEGY_CONTRACT_SCHEMA_VERSION,
        request_id: "exact-target-round".into(),
        strategy_id: "strategy-exact-target".into(),
        signal_id: 41,
        instrument: "PEPEUSDT.BINANCE".into(),
        target_qty: 0,
        target_qty_exact: Some("0.0000000005".into()),
        confidence: 1_000,
        priority: 0,
        expires_at: 10,
        intents: Vec::new(),
    };
    let order = build_strategy_order_with_signal(
        &config,
        "strategy-exact-target",
        41,
        10,
        0,
        0,
        Some("0.0000000002"),
        Some(&output),
        Some(&spec),
    )
    .unwrap()
    .unwrap();
    assert_eq!(order.side, Side::Buy);
    assert_eq!(order.qty.raw(), 0);
    assert_eq!(
        order.exact_amounts.unwrap().quantity,
        ExactDecimal::parse("0.0000000003").unwrap()
    );

    let mut hedge_config = config;
    hedge_config.strategy.position_mode = Some(PositionMode::Hedge);
    let error = build_strategy_order_with_signal(
        &hedge_config,
        "strategy-exact-target",
        42,
        10,
        0,
        0,
        Some("0.0000000002"),
        Some(&output),
        Some(&spec),
    )
    .unwrap_err();
    assert!(
        error.contains("Hedge 模式下 target_qty_exact 有歧义"),
        "{error}"
    );
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
        Some(&runtime_path),
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
