//! Live risk admission must account for orders submitted after the latest venue balance snapshot.
use super::*;

fn live_fixture(
    label: &str,
) -> (
    PathBuf,
    WorkerConfig,
    TradingInstrumentSpec,
    LiveEventPipeline,
) {
    live_fixture_for_product(label, TradingProduct::Spot)
}

fn live_fixture_for_product(
    label: &str,
    product: TradingProduct,
) -> (
    PathBuf,
    WorkerConfig,
    TradingInstrumentSpec,
    LiveEventPipeline,
) {
    let root = temp_cli_case_dir(label);
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let spec = super::paper_margin_valuation::margin_valuation_spec(&instrument, product);
    let spec_path = root.join("spot.spec.json");
    std::fs::write(&spec_path, serde_json::to_vec(&spec).unwrap()).unwrap();
    let worker = mk_worker(
        "binance-execution",
        WorkerRole::Execution,
        "binance",
        Some(&spec_path.to_string_lossy()),
    );
    let log_name = account_event_log_name("main", "binance").unwrap();
    let pipeline = LiveEventPipeline::open(&root, log_name, "USDT").unwrap();
    (root, worker, spec, pipeline)
}

fn add_balance_snapshot(pipeline: &mut LiveEventPipeline, free: i64) {
    add_balance_snapshot_for_asset(pipeline, "USDT", free);
}

fn add_balance_snapshot_for_asset(pipeline: &mut LiveEventPipeline, asset: &str, free: i64) {
    pipeline
        .ingest(RuntimeEventEnvelope::venue(
            RuntimeExternalEvent::AccountBalanceSnapshot {
                account_id: "main".into(),
                venue_id: "binance".into(),
                balances: vec![AccountBalance {
                    asset: asset.into(),
                    free: Money::from_i64(free),
                    locked: Money::ZERO,
                    borrowed: Money::ZERO,
                }],
            },
            10,
            10,
            1,
            "balances-1",
        ))
        .unwrap();
}

#[test]
fn live_order_admission_rejects_missing_future_and_expired_balance_snapshots() {
    let (_root, worker, _spec, mut pipeline) = live_fixture("live-balance-snapshot-freshness");
    assert!(require_fresh_live_balance_snapshot(&worker, &pipeline, 10)
        .unwrap_err()
        .contains("尚无账户余额快照"));

    add_balance_snapshot(&mut pipeline, 100);
    require_fresh_live_balance_snapshot(&worker, &pipeline, 10).unwrap();
    require_fresh_live_balance_snapshot(&worker, &pipeline, 10 + LIVE_BALANCE_SNAPSHOT_MAX_AGE_MS)
        .unwrap();
    assert!(require_fresh_live_balance_snapshot(
        &worker,
        &pipeline,
        11 + LIVE_BALANCE_SNAPSHOT_MAX_AGE_MS,
    )
    .unwrap_err()
    .contains("账户余额快照已过期"));
    assert!(require_fresh_live_balance_snapshot(&worker, &pipeline, 9)
        .unwrap_err()
        .contains("晚于下单时间"));
}

#[test]
fn live_spot_admission_uses_quote_asset_balance_without_fx_conversion() {
    let root = temp_cli_case_dir("live-spot-quote-asset-balance");
    let instrument = InstrumentId::parse("BTC-EUR.BINANCE").unwrap();
    let mut spec =
        super::paper_margin_valuation::margin_valuation_spec(&instrument, TradingProduct::Spot);
    spec.base_currency = "BTC".into();
    spec.quote_currency = "EUR".into();
    spec.settlement_currency = "EUR".into();
    let spec_path = root.join("btc-eur.spec.json");
    std::fs::write(&spec_path, serde_json::to_vec(&spec).unwrap()).unwrap();
    let worker = mk_worker(
        "binance-eur-execution",
        WorkerRole::Execution,
        "binance",
        Some(&spec_path.to_string_lossy()),
    );
    let log_name = account_event_log_name("main", "binance").unwrap();
    let mut pipeline = LiveEventPipeline::open(&root, log_name, "USDT").unwrap();
    add_balance_snapshot_for_asset(&mut pipeline, "eur", 100);

    let mut candidate = mk_order(10_011, &instrument, Side::Buy, 1);
    candidate.limit = Some(Price::from_i64(40));
    let (context, position) = worker_risk_context(&worker, &candidate, &pipeline, None)
        .unwrap()
        .expect("Live spot admission must use the exchange's quote-asset bucket");

    assert_eq!(context.available_margin_raw, Some(100 * SCALE));
    assert!(context.evaluate_order(&candidate, &position).allowed);
}

#[test]
fn live_balance_snapshot_reserves_same_market_orders_submitted_after_it() {
    let (_root, worker, spec, mut pipeline) = live_fixture("live-pending-reserve");
    add_balance_snapshot(&mut pipeline, 150);

    let mut pending = mk_order(10_001, &spec.instrument, Side::Buy, 1);
    pending.limit = Some(Price::from_i64(100));
    pipeline.register_order(pending, 11).unwrap();

    let mut candidate = mk_order(10_002, &spec.instrument, Side::Buy, 1);
    candidate.limit = Some(Price::from_i64(100));
    let (context, position) = worker_risk_context(&worker, &candidate, &pipeline, None)
        .unwrap()
        .expect("Live worker must construct the risk context from its balance snapshot");

    assert_eq!(context.available_margin_raw, Some(50 * SCALE));
    let decision = context.evaluate_order(&candidate, &position);
    assert!(!decision.allowed, "{decision:?}");
}

#[test]
fn live_balance_snapshot_reserves_post_snapshot_derivative_margin() {
    let (_root, worker, spec, mut pipeline) =
        live_fixture_for_product("live-derivative-pending-reserve", TradingProduct::Perpetual);
    add_balance_snapshot(&mut pipeline, 150);

    let policy = qx_core::OrderPolicy {
        margin_mode: MarginMode::Cross,
        position_mode: PositionMode::OneWay,
        position_side: PositionSide::Net,
        leverage: 2,
        ..qx_core::OrderPolicy::default()
    };
    let mut pending = mk_order(10_009, &spec.instrument, Side::Buy, 2);
    pending.limit = Some(Price::from_i64(100));
    pending.policy = Some(policy);
    pipeline.register_order(pending, 11).unwrap();

    let mut candidate = mk_order(10_010, &spec.instrument, Side::Buy, 2);
    candidate.limit = Some(Price::from_i64(100));
    candidate.policy = Some(policy);
    let (context, position) = worker_risk_context(&worker, &candidate, &pipeline, None)
        .unwrap()
        .expect("Live worker must construct the risk context from its balance snapshot");

    assert_eq!(context.available_margin_raw, Some(50 * SCALE));
    let decision = context.evaluate_order(&candidate, &position);
    assert!(!decision.allowed, "{decision:?}");
}

#[test]
fn live_balance_snapshot_does_not_reserve_orders_already_reflected_by_exchange() {
    let (_root, worker, spec, mut pipeline) = live_fixture("live-snapshot-includes-order");

    let mut pending = mk_order(10_005, &spec.instrument, Side::Buy, 1);
    pending.limit = Some(Price::from_i64(100));
    pipeline.register_order(pending, 9).unwrap();
    // This later snapshot's free balance already excludes the exchange-locked order funds.
    add_balance_snapshot(&mut pipeline, 50);

    let mut candidate = mk_order(10_006, &spec.instrument, Side::Buy, 1);
    candidate.limit = Some(Price::from_i64(40));
    let (context, _) = worker_risk_context(&worker, &candidate, &pipeline, None)
        .unwrap()
        .expect("Live worker must construct the risk context from its balance snapshot");

    assert_eq!(context.available_margin_raw, Some(50 * SCALE));
}

#[test]
fn live_fill_after_balance_snapshot_requires_a_fresh_snapshot_before_next_order() {
    let (_root, worker, spec, mut pipeline) = live_fixture("live-fill-after-snapshot");
    add_balance_snapshot(&mut pipeline, 10_000);

    let mut filled = mk_order(10_007, &spec.instrument, Side::Buy, 1);
    filled.limit = Some(Price::from_i64(100));
    pipeline.register_order(filled.clone(), 11).unwrap();
    pipeline
        .ingest(RuntimeEventEnvelope::venue(
            RuntimeExternalEvent::Accepted {
                client_order_id: filled.client_id,
                venue_order_id: Some("binance-10_007".into()),
            },
            11,
            11,
            2,
            "accepted-after-balance",
        ))
        .unwrap();
    pipeline
        .ingest(RuntimeEventEnvelope::venue(
            RuntimeExternalEvent::FillWithSpec {
                fill: Box::new(qx_core::Fill {
                    order_id: filled.client_id,
                    qty: Quantity::from_i64(1),
                    price: Price::from_i64(100),
                    ts: 12,
                    account_id: "main".into(),
                    venue_id: Some("binance".into()),
                    ..qx_core::Fill::default()
                }),
                spec: Box::new(spec.clone()),
            },
            12,
            12,
            3,
            "fill-after-balance",
        ))
        .unwrap();

    let mut candidate = mk_order(10_008, &spec.instrument, Side::Buy, 1);
    candidate.limit = Some(Price::from_i64(100));
    let error = worker_risk_context(&worker, &candidate, &pipeline, None).unwrap_err();
    assert!(error.contains("需先刷新交易所余额"), "{error}");
}

#[test]
fn live_balance_snapshot_reserves_same_currency_orders_from_other_markets() {
    let (root, worker, spec, mut pipeline) = live_fixture("live-pending-same-currency-market");
    add_balance_snapshot(&mut pipeline, 150);

    let other_market = InstrumentId::parse("ETHUSDT.BINANCE").unwrap();
    let mut other_spec =
        super::paper_margin_valuation::margin_valuation_spec(&other_market, TradingProduct::Spot);
    other_spec.base_currency = "ETH".into();
    other_spec.quote_currency = "USDT".into();
    other_spec.settlement_currency = "USDT".into();
    let other_spec_path = root.join("eth-usdt.spec.json");
    std::fs::write(&other_spec_path, serde_json::to_vec(&other_spec).unwrap()).unwrap();
    let mut other_worker = mk_worker(
        "binance-eth-execution",
        WorkerRole::Execution,
        "binance",
        Some(&other_spec_path.to_string_lossy()),
    );
    other_worker.symbols = vec![other_market.to_string()];
    let mut config = paper_runtime_config(&root);
    config.workers = vec![worker.clone(), other_worker];

    let mut pending = mk_order(10_003, &other_market, Side::Buy, 1);
    pending.limit = Some(Price::from_i64(100));
    pipeline.register_order(pending, 11).unwrap();

    let mut candidate = mk_order(10_004, &spec.instrument, Side::Buy, 1);
    candidate.limit = Some(Price::from_i64(100));
    let (context, _) =
        worker_risk_context_with_config(&worker, &candidate, &pipeline, &config, None)
            .unwrap()
            .expect("Live worker must reserve the configured same-currency market order");
    assert_eq!(context.available_margin_raw, Some(50 * SCALE));
}

#[test]
fn live_spot_candidate_ignores_pending_spot_order_from_other_quote_asset() {
    let (root, worker, spec, mut pipeline) = live_fixture("live-pending-cross-currency-market");
    add_balance_snapshot(&mut pipeline, 10_000);

    let other_market = InstrumentId::parse("ETH-EUR.BINANCE").unwrap();
    let mut other_spec =
        super::paper_margin_valuation::margin_valuation_spec(&other_market, TradingProduct::Spot);
    other_spec.base_currency = "ETH".into();
    other_spec.quote_currency = "EUR".into();
    other_spec.settlement_currency = "EUR".into();
    let other_spec_path = root.join("eth-eur.spec.json");
    std::fs::write(&other_spec_path, serde_json::to_vec(&other_spec).unwrap()).unwrap();
    let mut other_worker = mk_worker(
        "binance-eth-eur-execution",
        WorkerRole::Execution,
        "binance",
        Some(&other_spec_path.to_string_lossy()),
    );
    other_worker.symbols = vec![other_market.to_string()];
    let mut config = paper_runtime_config(&root);
    config.workers = vec![worker.clone(), other_worker];

    let pending = mk_order(10_005, &other_market, Side::Buy, 1);
    pipeline.register_order(pending, 11).unwrap();
    pipeline
        .ingest(RuntimeEventEnvelope::market_quote(
            other_market.clone(),
            QuoteTick::new(
                12,
                Price::from_i64(100),
                Quantity::from_i64(100),
                Price::from_i64(100),
                Quantity::from_i64(100),
                12,
            ),
            12,
            12,
            "live-cross-market-mark",
        ))
        .unwrap();
    let candidate = mk_order(10_006, &spec.instrument, Side::Buy, 1);
    let (context, _) =
        worker_risk_context_with_config(&worker, &candidate, &pipeline, &config, None)
            .unwrap()
            .expect("a disjoint EUR spot order must not reserve USDT spot buying power");
    assert_eq!(context.available_margin_raw, Some(10_000 * SCALE));
}

#[test]
fn live_derivative_candidate_fails_closed_on_other_settlement_currency_orders() {
    let (root, worker, spec, mut pipeline) = live_fixture_for_product(
        "live-derivative-cross-settlement-pending",
        TradingProduct::Perpetual,
    );
    add_balance_snapshot(&mut pipeline, 10_000);

    let other_market = InstrumentId::parse("ETH-EUR.BINANCE").unwrap();
    let mut other_spec = super::paper_margin_valuation::margin_valuation_spec(
        &other_market,
        TradingProduct::Perpetual,
    );
    other_spec.base_currency = "ETH".into();
    other_spec.quote_currency = "EUR".into();
    other_spec.settlement_currency = "EUR".into();
    let other_spec_path = root.join("eth-eur-perpetual.spec.json");
    std::fs::write(&other_spec_path, serde_json::to_vec(&other_spec).unwrap()).unwrap();
    let mut other_worker = mk_worker(
        "binance-eth-eur-perpetual-execution",
        WorkerRole::Execution,
        "binance",
        Some(&other_spec_path.to_string_lossy()),
    );
    other_worker.symbols = vec![other_market.to_string()];
    let mut config = paper_runtime_config(&root);
    config.workers = vec![worker.clone(), other_worker];

    let mut pending = mk_order(10_012, &other_market, Side::Buy, 1);
    pending.limit = Some(Price::from_i64(100));
    pipeline.register_order(pending, 11).unwrap();
    let candidate = mk_order(10_013, &spec.instrument, Side::Buy, 1);
    let error =
        worker_risk_context_with_config(&worker, &candidate, &pipeline, &config, None).unwrap_err();
    assert!(error.contains("资金币种 EUR"), "{error}");
}
