//! Paper 账户可用保证金的估值口径用例（从 `paper_and_strategy_worker.rs` 拆出，
//! 保持每个用例文件在 500 行门槛内）。

use super::*;

/// 估值用例用的最小合法规格：现货只允许 1 倍杠杆，衍生品必须唯一指定 linear/inverse。
pub(super) fn margin_valuation_spec(
    instrument: &InstrumentId,
    product: TradingProduct,
) -> TradingInstrumentSpec {
    TradingInstrumentSpec {
        instrument: instrument.clone(),
        product,
        base_currency: "BTC".into(),
        quote_currency: "USDT".into(),
        settlement_currency: "USDT".into(),
        contract_size: SCALE,
        linear: true,
        inverse: false,
        price_tick: 1,
        qty_step: SCALE,
        min_qty: SCALE,
        max_leverage: if product.is_derivative() { 100 } else { 1 },
        maintenance_margin_bps: 500,
        valid_from: 1,
        valid_to: None,
    }
}

/// 可用保证金和初始保证金必须是同一把尺子。Paper 分支此前统一用 `equity_for`
/// （现金 + 全额名义额）给持仓估值：现货买入确实付出现金，这样算成立；保证金产品
/// 开仓不动现金，于是 10k USDT 开 1 张 50k 名义的永续会把可用保证金抬到 60k，
/// 每成交一次就更宽松，杠杆规则形同虚设。
#[test]
fn paper_margin_budget_values_derivative_positions_without_their_notional() {
    let root = temp_cli_case_dir("paper-margin");
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let spec = margin_valuation_spec(&instrument, TradingProduct::Perpetual);
    let spec_path = root.join("perpetual.spec.json");
    std::fs::write(&spec_path, serde_json::to_vec(&spec).unwrap()).unwrap();
    let worker = mk_worker(
        "paper-execution",
        WorkerRole::Execution,
        "paper",
        Some(&spec_path.to_string_lossy()),
    );
    let mut order = mk_order(9501, &instrument, Side::Buy, 1);
    order.policy = Some(qx_core::OrderPolicy {
        margin_mode: MarginMode::Cross,
        position_mode: PositionMode::OneWay,
        position_side: PositionSide::Net,
        leverage: 50,
        ..qx_core::OrderPolicy::default()
    });

    let mut pipeline = LiveEventPipeline::open(&root, paper_account_log(), "USDT").unwrap();
    pipeline
        .ingest(RuntimeEventEnvelope::venue(
            RuntimeExternalEvent::AccountCashflow {
                cashflow: AccountCashflow {
                    account_id: "main".into(),
                    venue_id: "paper".into(),
                    currency: "USDT".into(),
                    kind: CashflowKind::Transfer,
                    amount: Money::from_i64(10_000),
                    external_id: "margin:transfer".into(),
                },
            },
            1,
            1,
            1,
            "margin:transfer",
        ))
        .unwrap();
    pipeline.register_order(order.clone(), 2).unwrap();
    pipeline
        .ingest(RuntimeEventEnvelope::venue(
            RuntimeExternalEvent::Accepted {
                client_order_id: order.client_id,
                venue_order_id: Some("paper-9501".into()),
            },
            2,
            2,
            2,
            "margin:accepted",
        ))
        .unwrap();
    pipeline
        .ingest(RuntimeEventEnvelope::venue(
            RuntimeExternalEvent::FillWithSpec {
                fill: Box::new(qx_core::Fill {
                    order_id: order.client_id,
                    qty: Quantity::from_i64(1),
                    price: Price::from_i64(50_000),
                    ts: 3,
                    account_id: "main".into(),
                    ..qx_core::Fill::default()
                }),
                spec: Box::new(spec.clone()),
            },
            3,
            3,
            3,
            "margin:fill",
        ))
        .unwrap();
    pipeline
        .ingest(RuntimeEventEnvelope::market_quote(
            instrument.clone(),
            QuoteTick::new(
                4,
                Price::from_i64(50_000),
                Quantity::from_i64(10),
                Price::from_i64(50_000),
                Quantity::from_i64(10),
                3,
            ),
            3,
            4,
            "margin:quote",
        ))
        .unwrap();

    let (risk, _) = worker_risk_context(&worker, &order, &pipeline, None)
        .unwrap()
        .expect("配置了 market spec 的 paper worker 必须产出风控上下文");
    assert_eq!(
        risk.available_margin_raw,
        Some(9_000 * SCALE),
        "永续持仓权益按 PnL 计入，并按该笔开仓杠杆扣除已占初始保证金"
    );
    assert_eq!(
        pipeline
            .ledger()
            .equity_for("main", pipeline.marks(), "USDT"),
        Some(60_000 * SCALE),
        "记账口径未变：名义额仍在 ledger 里，只是不再冒充保证金"
    );

    // 现货必须维持原口径：买入付出现金，持仓按标记价计入权益。
    let mut spot = qx_core::Ledger::new();
    spot.deposit("main", "USDT", Money::from_i64(10_000), 1)
        .unwrap();
    let spot_spec = margin_valuation_spec(&instrument, TradingProduct::Spot);
    let spot_order = mk_order(9502, &instrument, Side::Buy, 1);
    qx_core::apply_ledger_fill(
        &mut spot,
        &spot_order,
        "USDT",
        &qx_core::Fill {
            order_id: spot_order.client_id,
            qty: Quantity::from_i64(1),
            price: Price::from_i64(100),
            ts: 2,
            account_id: "main".into(),
            ..qx_core::Fill::default()
        },
        qx_core::FillTerms::Instrument(&spot_spec),
    )
    .unwrap();
    let marks = BTreeMap::from([(instrument.clone(), Price::from_i64(110))]);
    assert_eq!(
        paper_available_margin(&spot, "main", &marks, "USDT", &spot_spec).unwrap(),
        (10_000 - 100 + 110) * SCALE,
        "现货权益口径不得随保证金修复一起改变"
    );
    assert!(paper_available_margin(&spot, "main", &BTreeMap::new(), "USDT", &spot_spec).is_err());

    let other_instrument = InstrumentId::parse("ETHUSDT.BINANCE").unwrap();
    let other_spec = margin_valuation_spec(&other_instrument, TradingProduct::Spot);
    let other_order = mk_order(9503, &other_instrument, Side::Buy, 1);
    qx_core::apply_ledger_fill(
        &mut spot,
        &other_order,
        "USDT",
        &qx_core::Fill {
            order_id: other_order.client_id,
            qty: Quantity::from_i64(1),
            price: Price::from_i64(100),
            ts: 3,
            account_id: "main".into(),
            ..qx_core::Fill::default()
        },
        qx_core::FillTerms::Instrument(&other_spec),
    )
    .unwrap();
    let both_marks = BTreeMap::from([
        (instrument.clone(), Price::from_i64(110)),
        (other_instrument, Price::from_i64(120)),
    ]);
    assert!(paper_available_margin(&spot, "main", &both_marks, "USDT", &spot_spec).is_err());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn paper_shared_account_margin_marks_positions_from_all_configured_markets() {
    let root = temp_cli_case_dir("paper-multi-market-margin");
    let btc = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let eth = InstrumentId::parse("ETHUSDT.BINANCE").unwrap();
    let btc_spec = margin_valuation_spec(&btc, TradingProduct::Perpetual);
    let mut eth_spec = margin_valuation_spec(&eth, TradingProduct::Perpetual);
    eth_spec.base_currency = "ETH".into();
    let btc_path = root.join("btc.spec.json");
    let eth_path = root.join("eth.spec.json");
    std::fs::write(&btc_path, serde_json::to_vec(&btc_spec).unwrap()).unwrap();
    std::fs::write(&eth_path, serde_json::to_vec(&eth_spec).unwrap()).unwrap();
    let mut btc_worker = mk_worker(
        "paper-btc",
        WorkerRole::Execution,
        "paper",
        Some(&btc_path.to_string_lossy()),
    );
    btc_worker.symbols = vec![btc.to_string()];
    let mut eth_worker = mk_worker(
        "paper-eth",
        WorkerRole::Execution,
        "paper",
        Some(&eth_path.to_string_lossy()),
    );
    eth_worker.symbols = vec![eth.to_string()];
    let mut config = paper_runtime_config(&root);
    config.workers = vec![btc_worker.clone(), eth_worker];

    let mut pipeline = LiveEventPipeline::open(&root, paper_account_log(), "USDT").unwrap();
    pipeline
        .ingest(RuntimeEventEnvelope::venue(
            RuntimeExternalEvent::AccountCashflow {
                cashflow: AccountCashflow {
                    account_id: "main".into(),
                    venue_id: "paper".into(),
                    currency: "USDT".into(),
                    kind: CashflowKind::Transfer,
                    amount: Money::from_i64(10_000),
                    external_id: "multi-market:transfer".into(),
                },
            },
            1,
            1,
            1,
            "multi-market:transfer",
        ))
        .unwrap();
    for (client_id, instrument, spec, mark) in [
        (9511, btc.clone(), btc_spec.clone(), 110),
        (9512, eth.clone(), eth_spec, 120),
    ] {
        let order = mk_order(client_id, &instrument, Side::Buy, 1);
        pipeline.register_order(order.clone(), 2).unwrap();
        pipeline
            .ingest(RuntimeEventEnvelope::venue(
                RuntimeExternalEvent::Accepted {
                    client_order_id: client_id,
                    venue_order_id: Some(format!("paper-{client_id}")),
                },
                2,
                2,
                client_id,
                format!("multi-market:accepted:{client_id}"),
            ))
            .unwrap();
        pipeline
            .ingest(RuntimeEventEnvelope::venue(
                RuntimeExternalEvent::FillWithSpec {
                    fill: Box::new(qx_core::Fill {
                        order_id: client_id,
                        qty: Quantity::from_i64(1),
                        price: Price::from_i64(100),
                        ts: 3,
                        account_id: "main".into(),
                        ..qx_core::Fill::default()
                    }),
                    spec: Box::new(spec),
                },
                3,
                3,
                client_id,
                format!("multi-market:fill:{client_id}"),
            ))
            .unwrap();
        pipeline
            .ingest(RuntimeEventEnvelope::market_quote(
                instrument,
                QuoteTick::new(
                    4,
                    Price::from_i64(mark),
                    Quantity::from_i64(10),
                    Price::from_i64(mark),
                    Quantity::from_i64(10),
                    client_id,
                ),
                4,
                client_id,
                format!("multi-market:quote:{client_id}"),
            ))
            .unwrap();
    }

    let next_order = mk_order(9513, &btc, Side::Buy, 1);
    let (risk, _) =
        worker_risk_context_with_config(&btc_worker, &next_order, &pipeline, &config, None)
            .unwrap()
            .expect("configured Paper worker must produce risk context");
    assert_eq!(
        risk.available_margin_raw,
        Some(9_800 * SCALE),
        "available equity must include PnL on every same-account market position and reserve existing/candidate margin"
    );
    drop(pipeline);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn paper_derivative_pending_orders_reduce_margin_for_a_new_worker_order() {
    let root = temp_cli_case_dir("paper-pending-margin");
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let spec = margin_valuation_spec(&instrument, TradingProduct::Perpetual);
    let spec_path = root.join("perpetual.spec.json");
    std::fs::write(&spec_path, serde_json::to_vec(&spec).unwrap()).unwrap();
    let worker = mk_worker(
        "paper-execution",
        WorkerRole::Execution,
        "paper",
        Some(&spec_path.to_string_lossy()),
    );
    let leverage_two = qx_core::OrderPolicy {
        margin_mode: MarginMode::Cross,
        position_mode: PositionMode::OneWay,
        position_side: PositionSide::Net,
        leverage: 2,
        ..qx_core::OrderPolicy::default()
    };
    let mut pending = mk_order(9531, &instrument, Side::Buy, 1);
    pending.qty = Quantity::from_i64(12);
    pending.policy = Some(leverage_two);
    let mut candidate = mk_order(9532, &instrument, Side::Buy, 1);
    candidate.qty = Quantity::from_i64(10);
    candidate.policy = Some(leverage_two);

    let mut pipeline = LiveEventPipeline::open(&root, paper_account_log(), "USDT").unwrap();
    pipeline
        .ingest(RuntimeEventEnvelope::venue(
            RuntimeExternalEvent::AccountCashflow {
                cashflow: AccountCashflow {
                    account_id: "main".into(),
                    venue_id: "paper".into(),
                    currency: "USDT".into(),
                    kind: CashflowKind::Transfer,
                    amount: Money::from_i64(100),
                    external_id: "pending-margin:transfer".into(),
                },
            },
            1,
            1,
            1,
            "pending-margin:transfer",
        ))
        .unwrap();
    pipeline.register_order(pending.clone(), 2).unwrap();
    pipeline
        .ingest(RuntimeEventEnvelope::venue(
            RuntimeExternalEvent::Accepted {
                client_order_id: pending.client_id,
                venue_order_id: Some("paper-pending-9531".into()),
            },
            2,
            2,
            2,
            "pending-margin:accepted",
        ))
        .unwrap();
    pipeline
        .ingest(RuntimeEventEnvelope::market_quote(
            instrument.clone(),
            QuoteTick::new(
                3,
                Price::from_i64(10),
                Quantity::from_i64(10),
                Price::from_i64(10),
                Quantity::from_i64(10),
                1,
            ),
            3,
            3,
            "pending-margin:quote",
        ))
        .unwrap();

    let (risk, position) = worker_risk_context(&worker, &candidate, &pipeline, None)
        .unwrap()
        .expect("Paper worker must build a risk context");
    assert_eq!(risk.available_margin_raw, Some(40 * SCALE));
    let decision = risk.evaluate_order(&candidate, &position);
    assert!(!decision.allowed);
    assert!(decision
        .violations
        .iter()
        .any(|message| message.contains("初始保证金超过账户可用保证金")));
    drop(pipeline);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn paper_pending_spot_buys_reserve_quote_cash_for_a_new_order() {
    let root = temp_cli_case_dir("paper-pending-spot-cash");
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let spec = margin_valuation_spec(&instrument, TradingProduct::Spot);
    let spec_path = root.join("spot.spec.json");
    std::fs::write(&spec_path, serde_json::to_vec(&spec).unwrap()).unwrap();
    let worker = mk_worker(
        "paper-spot-execution",
        WorkerRole::Execution,
        "paper",
        Some(&spec_path.to_string_lossy()),
    );
    let mut pending = mk_order(9535, &instrument, Side::Buy, 1);
    pending.qty = Quantity::from_i64(7);
    pending.limit = Some(Price::from_i64(10));
    let mut candidate = mk_order(9536, &instrument, Side::Buy, 1);
    candidate.qty = Quantity::from_i64(5);
    candidate.limit = Some(Price::from_i64(10));

    let mut pipeline = LiveEventPipeline::open(&root, paper_account_log(), "USDT").unwrap();
    pipeline
        .ingest(RuntimeEventEnvelope::venue(
            RuntimeExternalEvent::AccountCashflow {
                cashflow: AccountCashflow {
                    account_id: "main".into(),
                    venue_id: "paper".into(),
                    currency: "USDT".into(),
                    kind: CashflowKind::Transfer,
                    amount: Money::from_i64(100),
                    external_id: "pending-spot:transfer".into(),
                },
            },
            1,
            1,
            1,
            "pending-spot:transfer",
        ))
        .unwrap();
    pipeline.register_order(pending.clone(), 2).unwrap();
    pipeline
        .ingest(RuntimeEventEnvelope::venue(
            RuntimeExternalEvent::Accepted {
                client_order_id: pending.client_id,
                venue_order_id: Some("paper-pending-spot-9535".into()),
            },
            2,
            2,
            2,
            "pending-spot:accepted",
        ))
        .unwrap();
    pipeline
        .ingest(RuntimeEventEnvelope::market_quote(
            instrument.clone(),
            QuoteTick::new(
                3,
                Price::from_i64(10),
                Quantity::from_i64(10),
                Price::from_i64(10),
                Quantity::from_i64(10),
                1,
            ),
            3,
            3,
            "pending-spot:quote",
        ))
        .unwrap();

    let (risk, position) = worker_risk_context(&worker, &candidate, &pipeline, None)
        .unwrap()
        .expect("Paper spot worker must build a risk context");
    assert_eq!(risk.available_margin_raw, Some(30 * SCALE));
    assert!(!risk.evaluate_order(&candidate, &position).allowed);
    drop(pipeline);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn paper_cross_currency_spot_uses_quote_cash_and_reserves_pending_orders() {
    let root = temp_cli_case_dir("paper-spot-quote-currency-funding");
    let instrument = InstrumentId::parse("ETH-EUR.BINANCE").unwrap();
    let fx_instrument = InstrumentId::parse("EURUSDT.BINANCE").unwrap();
    let mut spec = margin_valuation_spec(&instrument, TradingProduct::Spot);
    spec.base_currency = "ETH".into();
    spec.quote_currency = "EUR".into();
    spec.settlement_currency = "EUR".into();
    let mut fx_spec = margin_valuation_spec(&fx_instrument, TradingProduct::Spot);
    fx_spec.base_currency = "EUR".into();
    fx_spec.quote_currency = "USDT".into();
    fx_spec.settlement_currency = "USDT".into();
    let spec_path = root.join("spot.spec.json");
    let fx_spec_path = root.join("eur-usdt.spec.json");
    std::fs::write(&spec_path, serde_json::to_vec(&spec).unwrap()).unwrap();
    std::fs::write(&fx_spec_path, serde_json::to_vec(&fx_spec).unwrap()).unwrap();
    let mut worker = mk_worker(
        "paper-eur-spot-execution",
        WorkerRole::Execution,
        "paper",
        Some(&spec_path.to_string_lossy()),
    );
    worker.symbols = vec![instrument.to_string()];
    let mut fx_worker = mk_worker(
        "paper-eur-usdt-fx",
        WorkerRole::MarketData,
        "paper",
        Some(&fx_spec_path.to_string_lossy()),
    );
    fx_worker.symbols = vec![fx_instrument.to_string()];
    let mut config = paper_runtime_config(&root);
    config.workers = vec![worker.clone(), fx_worker];
    let mut pipeline = LiveEventPipeline::open(&root, paper_account_log(), "USDT").unwrap();
    for (currency, amount) in [("EUR", 100), ("USDT", 10_000)] {
        pipeline
            .ingest(RuntimeEventEnvelope::venue(
                RuntimeExternalEvent::AccountCashflow {
                    cashflow: AccountCashflow {
                        account_id: "main".into(),
                        venue_id: "paper".into(),
                        currency: currency.into(),
                        kind: CashflowKind::Transfer,
                        amount: Money::from_i64(amount),
                        external_id: format!("quote-cash:{currency}"),
                    },
                },
                1,
                1,
                1,
                format!("quote-cash:{currency}"),
            ))
            .unwrap();
    }
    let pending = mk_order(9536, &instrument, Side::Buy, 4);
    pipeline.register_order(pending.clone(), 2).unwrap();
    pipeline
        .ingest(RuntimeEventEnvelope::venue(
            RuntimeExternalEvent::Accepted {
                client_order_id: pending.client_id,
                venue_order_id: Some("paper-pending-eur-buy".into()),
            },
            2,
            2,
            2,
            "paper-pending-eur-buy",
        ))
        .unwrap();
    for (market, mark, seq) in [
        (&instrument, Price::from_i64(10), 3),
        (&fx_instrument, Price::from_raw(11 * SCALE / 10), 4),
    ] {
        pipeline
            .ingest(RuntimeEventEnvelope::market_quote(
                market.clone(),
                QuoteTick::new(
                    seq,
                    mark,
                    Quantity::from_i64(100),
                    mark,
                    Quantity::from_i64(100),
                    seq,
                ),
                seq,
                seq,
                format!("quote-cash-mark:{market}"),
            ))
            .unwrap();
    }

    let too_large = mk_order(9537, &instrument, Side::Buy, 7);
    let (risk, position) =
        worker_risk_context_with_config(&worker, &too_large, &pipeline, &config, None)
            .unwrap()
            .expect("Paper cross-currency spot worker must produce a risk context");
    assert_eq!(risk.available_margin_raw, Some(60 * SCALE));
    assert!(!risk.evaluate_order(&too_large, &position).allowed);

    let affordable = mk_order(9538, &instrument, Side::Buy, 5);
    let (risk, position) =
        worker_risk_context_with_config(&worker, &affordable, &pipeline, &config, None)
            .unwrap()
            .expect("EUR quote cash must fund an affordable candidate");
    assert_eq!(risk.available_margin_raw, Some(60 * SCALE));
    assert!(risk.evaluate_order(&affordable, &position).allowed);
    drop(pipeline);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn paper_pending_derivative_margin_converts_through_configured_fx_market() {
    let root = temp_cli_case_dir("paper-pending-margin-fx");
    let btc = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let eth_eur = InstrumentId::parse("ETH-EUR.BINANCE").unwrap();
    let eur_usdt = InstrumentId::parse("EURUSDT.BINANCE").unwrap();
    let btc_spec = margin_valuation_spec(&btc, TradingProduct::Perpetual);
    let mut eth_spec = margin_valuation_spec(&eth_eur, TradingProduct::Perpetual);
    eth_spec.base_currency = "ETH".into();
    eth_spec.quote_currency = "EUR".into();
    eth_spec.settlement_currency = "EUR".into();
    let mut fx_spec = margin_valuation_spec(&eur_usdt, TradingProduct::Spot);
    fx_spec.base_currency = "EUR".into();
    fx_spec.quote_currency = "USDT".into();
    fx_spec.settlement_currency = "USDT".into();
    let btc_path = root.join("btc.spec.json");
    let eth_path = root.join("eth-eur.spec.json");
    let fx_path = root.join("eur-usdt.spec.json");
    std::fs::write(&btc_path, serde_json::to_vec(&btc_spec).unwrap()).unwrap();
    std::fs::write(&eth_path, serde_json::to_vec(&eth_spec).unwrap()).unwrap();
    std::fs::write(&fx_path, serde_json::to_vec(&fx_spec).unwrap()).unwrap();
    let mut btc_worker = mk_worker(
        "paper-btc",
        WorkerRole::Execution,
        "paper",
        Some(&btc_path.to_string_lossy()),
    );
    btc_worker.symbols = vec![btc.to_string()];
    let mut eth_worker = mk_worker(
        "paper-eth-eur",
        WorkerRole::Execution,
        "paper",
        Some(&eth_path.to_string_lossy()),
    );
    eth_worker.symbols = vec![eth_eur.to_string()];
    let mut fx_worker = mk_worker(
        "paper-eur-usdt-fx",
        WorkerRole::MarketData,
        "paper",
        Some(&fx_path.to_string_lossy()),
    );
    fx_worker.symbols = vec![eur_usdt.to_string()];
    let mut config = paper_runtime_config(&root);
    config.workers = vec![btc_worker.clone(), eth_worker, fx_worker];
    let leverage_two = qx_core::OrderPolicy {
        margin_mode: MarginMode::Cross,
        position_mode: PositionMode::OneWay,
        position_side: PositionSide::Net,
        leverage: 2,
        ..qx_core::OrderPolicy::default()
    };
    let mut pending = mk_order(9541, &eth_eur, Side::Buy, 1);
    pending.qty = Quantity::from_i64(12);
    pending.limit = Some(Price::from_i64(10));
    pending.policy = Some(leverage_two);
    let mut candidate = mk_order(9542, &btc, Side::Buy, 1);
    candidate.qty = Quantity::from_i64(10);
    candidate.limit = Some(Price::from_i64(10));
    candidate.policy = Some(leverage_two);

    let mut pipeline = LiveEventPipeline::open(&root, paper_account_log(), "USDT").unwrap();
    pipeline
        .ingest(RuntimeEventEnvelope::venue(
            RuntimeExternalEvent::AccountCashflow {
                cashflow: AccountCashflow {
                    account_id: "main".into(),
                    venue_id: "paper".into(),
                    currency: "USDT".into(),
                    kind: CashflowKind::Transfer,
                    amount: Money::from_i64(100),
                    external_id: "pending-margin-fx:transfer".into(),
                },
            },
            1,
            1,
            1,
            "pending-margin-fx:transfer",
        ))
        .unwrap();
    pipeline.register_order(pending.clone(), 2).unwrap();
    pipeline
        .ingest(RuntimeEventEnvelope::venue(
            RuntimeExternalEvent::Accepted {
                client_order_id: pending.client_id,
                venue_order_id: Some("paper-pending-9541".into()),
            },
            2,
            2,
            2,
            "pending-margin-fx:accepted",
        ))
        .unwrap();
    pipeline
        .ingest(RuntimeEventEnvelope::market_quote(
            eur_usdt.clone(),
            QuoteTick::new(
                3,
                Price::from_raw(11 * SCALE / 10),
                Quantity::from_i64(1_000),
                Price::from_raw(11 * SCALE / 10),
                Quantity::from_i64(1_000),
                1,
            ),
            3,
            3,
            "pending-margin-fx:quote",
        ))
        .unwrap();

    let (risk, position) =
        worker_risk_context_with_config(&btc_worker, &candidate, &pipeline, &config, None)
            .unwrap()
            .expect("Paper worker must build a cross-currency risk context");
    assert_eq!(risk.available_margin_raw, Some(34 * SCALE));
    assert!(!risk.evaluate_order(&candidate, &position).allowed);
    drop(pipeline);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn paper_cross_currency_spot_uses_quote_cash_and_configured_fx_market() {
    let root = temp_cli_case_dir("paper-cross-currency-margin");
    let btc = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let eth_eur = InstrumentId::parse("ETH-EUR.BINANCE").unwrap();
    let eur_usdt = InstrumentId::parse("EURUSDT.BINANCE").unwrap();
    let btc_spec = margin_valuation_spec(&btc, TradingProduct::Spot);
    let mut eth_spec = margin_valuation_spec(&eth_eur, TradingProduct::Spot);
    eth_spec.base_currency = "ETH".into();
    eth_spec.quote_currency = "eur".into();
    eth_spec.settlement_currency = "eur".into();
    let mut fx_spec = margin_valuation_spec(&eur_usdt, TradingProduct::Spot);
    fx_spec.base_currency = "eur".into();
    fx_spec.quote_currency = "usdt".into();
    fx_spec.settlement_currency = "USDT".into();
    let btc_path = root.join("btc.spec.json");
    let eth_path = root.join("eth-eur.spec.json");
    let fx_path = root.join("eur-usdt.spec.json");
    std::fs::write(&btc_path, serde_json::to_vec(&btc_spec).unwrap()).unwrap();
    std::fs::write(&eth_path, serde_json::to_vec(&eth_spec).unwrap()).unwrap();
    std::fs::write(&fx_path, serde_json::to_vec(&fx_spec).unwrap()).unwrap();
    let mut btc_worker = mk_worker(
        "paper-btc",
        WorkerRole::Execution,
        "paper",
        Some(&btc_path.to_string_lossy()),
    );
    btc_worker.symbols = vec![btc.to_string()];
    let mut eth_worker = mk_worker(
        "paper-eth-eur",
        WorkerRole::Execution,
        "paper",
        Some(&eth_path.to_string_lossy()),
    );
    eth_worker.symbols = vec![eth_eur.to_string()];
    let mut fx_worker = mk_worker(
        "paper-eur-usdt-fx",
        WorkerRole::MarketData,
        "paper",
        Some(&fx_path.to_string_lossy()),
    );
    fx_worker.symbols = vec![eur_usdt.to_string()];
    let mut config = paper_runtime_config(&root);
    config.workers = vec![btc_worker.clone(), eth_worker, fx_worker];

    let mut pipeline = LiveEventPipeline::open(&root, paper_account_log(), "USDT").unwrap();
    for (currency, amount) in [("USDT", 10_000), ("EUR", 1_000)] {
        pipeline
            .ingest(RuntimeEventEnvelope::venue(
                RuntimeExternalEvent::AccountCashflow {
                    cashflow: AccountCashflow {
                        account_id: "main".into(),
                        venue_id: "paper".into(),
                        currency: currency.into(),
                        kind: CashflowKind::Transfer,
                        amount: Money::from_i64(amount),
                        external_id: format!("cross-currency:deposit:{currency}"),
                    },
                },
                1,
                1,
                1,
                format!("cross-currency:deposit:{currency}"),
            ))
            .unwrap();
    }
    let spot_order = mk_order(9521, &eth_eur, Side::Buy, 1);
    pipeline.register_order(spot_order.clone(), 2).unwrap();
    pipeline
        .ingest(RuntimeEventEnvelope::venue(
            RuntimeExternalEvent::Accepted {
                client_order_id: spot_order.client_id,
                venue_order_id: Some("paper-9521".into()),
            },
            2,
            2,
            2,
            "cross-currency:accepted",
        ))
        .unwrap();
    pipeline
        .ingest(RuntimeEventEnvelope::venue(
            RuntimeExternalEvent::FillWithSpec {
                fill: Box::new(qx_core::Fill {
                    order_id: spot_order.client_id,
                    qty: Quantity::from_i64(1),
                    price: Price::from_i64(100),
                    ts: 3,
                    account_id: "main".into(),
                    ..qx_core::Fill::default()
                }),
                spec: Box::new(eth_spec),
            },
            3,
            3,
            3,
            "cross-currency:fill",
        ))
        .unwrap();
    for (instrument, mark, seq) in [
        (&eth_eur, Price::from_i64(120), 4),
        (&eur_usdt, Price::from_raw(11 * SCALE / 10), 5),
    ] {
        pipeline
            .ingest(RuntimeEventEnvelope::market_quote(
                instrument.clone(),
                QuoteTick::new(
                    seq,
                    mark,
                    Quantity::from_i64(100),
                    mark,
                    Quantity::from_i64(100),
                    seq,
                ),
                seq,
                seq,
                format!("cross-currency:quote:{instrument}"),
            ))
            .unwrap();
    }

    assert_eq!(pipeline.ledger().cash_for("main", "EUR"), 900 * SCALE);
    assert_eq!(pipeline.ledger().cash_for("main", "USDT"), 10_000 * SCALE);
    let next_order = mk_order(9522, &btc, Side::Buy, 1);
    let (risk, _) =
        worker_risk_context_with_config(&btc_worker, &next_order, &pipeline, &config, None)
            .unwrap()
            .expect("configured Paper worker must produce cross-currency risk context");
    assert_eq!(
        risk.available_margin_raw,
        Some(10_000 * SCALE),
        "BTCUSDT spot buying power is the available USDT cash; EUR assets cannot be reused as cash"
    );
    drop(pipeline);
    let _ = std::fs::remove_dir_all(root);
}
