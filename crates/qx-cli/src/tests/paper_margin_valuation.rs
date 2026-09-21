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
    let _ = std::fs::remove_dir_all(root);
}
