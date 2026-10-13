//! Paper 账户可用保证金的估值口径用例（从 `paper_and_strategy_worker.rs` 拆出，
//! 保持每个用例文件在 500 行门槛内）。

use super::*;

/// 估值用例用的最小合法规格：现货只允许 1 倍杠杆，衍生品必须唯一指定 linear/inverse。
fn margin_valuation_spec(
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
    let order = mk_order(9501, &instrument, Side::Buy, 1);

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
        Some(10_000 * SCALE),
        "永续持仓只能按 PnL 计入可用保证金"
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
        Some(10_030 * SCALE),
        "available equity must include PnL on every same-account market position"
    );
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
        Some(11_122 * SCALE),
        "EUR cash and ETH spot value must be converted using EUR/USDT mark"
    );
    drop(pipeline);
    let _ = std::fs::remove_dir_all(root);
}
