mod common;

use common::{scratch, write_frame};
use qx_app::{
    run_multi_market_backtest, AppErrorCategory, BacktestMarginConfig, BacktestMarginTier,
    BuiltinStrategySpec, CallerCapability, MultiMarketBacktestMarket, MultiMarketBacktestSpec,
    PortfolioStrategySpec, RunContext, RunStatus,
};
use qx_core::{
    InstrumentId, MakerTakerFeeSchedule, MarginMode, OrderPolicy, PositionMode, PositionSide,
    TradingInstrumentSpec, TradingProduct,
};
use qx_strategy::BuiltinStrategyKind;
use serde_json::Value;

const SCALE: i128 = 1_000_000_000;

fn spec(root: &std::path::Path, run_id: &str) -> MultiMarketBacktestSpec {
    let instruments = ["BTCUSDT.BINANCE", "ETHUSDT.KRAKEN", "SOLUSDT.BYBIT"];
    let markets = instruments
        .iter()
        .enumerate()
        .map(|(index, instrument)| {
            let bars_path = write_frame(root, &format!("market-{index}.json"), instrument, 80);
            let mut strategy =
                BuiltinStrategySpec::new(BuiltinStrategyKind::SmaCross, format!("sma-{index}"));
            strategy.fast_window = 2;
            strategy.slow_window = 3;
            MultiMarketBacktestMarket {
                instrument: (*instrument).into(),
                bars_path,
                dataset_manifest_path: None,
                priority: index as u32,
                market_spec: None,
                fee_schedule: None,
                order_policy: None,
                margin_model: None,
                funding_bps: 0,
                funding_events: Vec::new(),
                strategies: vec![PortfolioStrategySpec {
                    priority: 0,
                    reserved_capital_raw: 20_000 * SCALE,
                    strategy,
                }],
            }
        })
        .collect();
    MultiMarketBacktestSpec {
        schema_version: 1,
        run_id: run_id.into(),
        account_id: "shared-account".into(),
        settlement_currency: "USDT".into(),
        initial_cash_raw: 100_000 * SCALE,
        seed: 7,
        output_dir: root.join("out").to_string_lossy().into_owned(),
        markets,
    }
}

fn research(id: &str) -> RunContext {
    RunContext::new(CallerCapability::Research, id)
}

#[test]
fn three_market_run_shares_ledger_replays_and_is_idempotent() {
    let root = scratch("multi-market-e2e");
    let spec = spec(&root, "multi-market-e2e");
    let outcome = run_multi_market_backtest(&spec, &research("multi-market-e2e"))
        .expect("three-market shared-account run");
    assert_eq!(outcome.markets_total, 3);
    assert_eq!(outcome.strategies_total, 3);
    assert!(outcome.successful_fills > 0);
    assert_eq!(outcome.equity_points, 240);
    println!(
        "MULTI_MARKET_OBSERVED markets={} strategies={} fills={} equity_points={} return_bps={} replay_verified=true",
        outcome.markets_total,
        outcome.strategies_total,
        outcome.successful_fills,
        outcome.equity_points,
        outcome.return_bps
    );

    let artifact: Value = serde_json::from_str(
        &std::fs::read_to_string(&outcome.artifact_path).expect("artifact exists"),
    )
    .expect("artifact JSON");
    assert_eq!(artifact["replay_verified"], true);
    assert_eq!(artifact["market_data"].as_array().unwrap().len(), 3);
    assert_eq!(artifact["equity"].as_array().unwrap().len(), 240);
    let fills = artifact["per_market_fills"].as_object().unwrap();
    assert_eq!(fills.len(), 3);

    let repeated = run_multi_market_backtest(&spec, &research("multi-market-e2e"))
        .expect("same run is idempotent");
    assert_eq!(outcome.shared_result_hash, repeated.shared_result_hash);
}

#[test]
fn multi_market_quality_manifest_is_bound_into_run_identity_and_fails_closed() {
    let root = scratch("multi-market-quality");
    let mut spec = spec(&root, "multi-market-quality");
    let frame_path = &spec.markets[0].bars_path;
    let manifest_path = root.join("market-0.quality.json");
    let manifest = qx_app::validate_dataset_manifest(
        &qx_app::DatasetManifestValidationSpec::new(
            frame_path,
            "research:multi-market-quality",
            "daily-v1",
            1_000,
            "UTC",
            manifest_path.to_string_lossy(),
        ),
        &research("multi-market-quality-manifest"),
    )
    .expect("publish quality manifest");
    spec.markets[0].dataset_manifest_path = Some(manifest_path.to_string_lossy().to_string());
    let outcome = run_multi_market_backtest(&spec, &research("multi-market-quality"))
        .expect("quality-bound multi-market run");
    let artifact: Value = serde_json::from_str(
        &std::fs::read_to_string(&outcome.artifact_path).expect("quality-bound artifact exists"),
    )
    .expect("quality-bound artifact JSON");
    assert_eq!(
        artifact.pointer("/dataset_quality/BTCUSDT.BINANCE/manifest_fingerprint"),
        Some(&Value::String(manifest.manifest.fingerprint().unwrap()))
    );
    let run_manifest: qx_core::RunManifest = qx_core::RunManifest::from_json(
        &std::fs::read_to_string(artifact["run_manifest_path"].as_str().unwrap())
            .expect("read RunManifest"),
    )
    .unwrap();
    assert!(run_manifest
        .input_components
        .contains_key("dataset-quality:BTCUSDT.BINANCE"));

    let mut tampered = manifest.manifest;
    tampered.quality_report.missing_intervals = 1;
    std::fs::write(&manifest_path, tampered.to_json().unwrap()).expect("tamper quality claim");
    let mut rejected = spec;
    rejected.run_id = "multi-market-quality-tampered".into();
    rejected.output_dir = root.join("tampered-out").to_string_lossy().to_string();
    let error = run_multi_market_backtest(&rejected, &research("multi-market-quality-tampered"))
        .expect_err("tampered quality report is rejected");
    assert_eq!(error.category(), AppErrorCategory::InvalidInput);
    assert!(!root.join("tampered-out").exists());
}

#[test]
fn multi_market_validation_rejects_budget_overcommit_and_cancel_handle_completes() {
    let root = scratch("multi-market-guards");
    let mut invalid = spec(&root, "multi-market-invalid");
    invalid.markets[0].strategies[0].reserved_capital_raw = invalid.initial_cash_raw;
    assert_eq!(
        invalid.validate().unwrap_err().category(),
        AppErrorCategory::InvalidInput
    );

    let mut handle = spec(&root, "multi-market-handle").start(research("multi-market-handle"));
    assert_eq!(handle.wait(), RunStatus::Succeeded);
    let outcome = handle
        .try_take_result()
        .expect("finished result")
        .expect("run handle succeeds");
    assert_eq!(outcome.markets_total, 3);
}

#[test]
fn multi_market_accepts_market_specific_derivative_policy_and_tiered_margin() {
    let root = scratch("multi-market-derivative-policy");
    let mut spec = spec(&root, "multi-market-derivative-policy");
    {
        let market = &mut spec.markets[0];
        let instrument = InstrumentId::parse(&market.instrument).unwrap();
        market.market_spec = Some(
            serde_json::to_value(TradingInstrumentSpec {
                instrument,
                product: TradingProduct::Perpetual,
                base_currency: "BTC".into(),
                quote_currency: "USDT".into(),
                settlement_currency: "USDT".into(),
                contract_size: SCALE,
                linear: true,
                inverse: false,
                price_tick: 1,
                qty_step: 1_000,
                min_qty: 1_000,
                min_notional_raw: 0,
                max_leverage: 20,
                maintenance_margin_bps: 50,
                valid_from: 0,
                valid_to: None,
            })
            .unwrap(),
        );
        market.fee_schedule = Some(MakerTakerFeeSchedule {
            maker_fee_bps: 2,
            taker_fee_bps: 5,
        });
        market.order_policy = Some(OrderPolicy {
            reduce_only: false,
            position_side: PositionSide::Net,
            margin_mode: MarginMode::Isolated,
            position_mode: PositionMode::OneWay,
            leverage: 5,
            post_only: false,
        });
        market.margin_model = Some(BacktestMarginConfig::Tiers {
            tiers: vec![BacktestMarginTier {
                max_notional: 100_000 * SCALE,
                initial_bp: 2_000,
                maintenance_bp: 50,
                max_leverage: Some(10),
            }],
        });
        market.funding_bps = 8;
    }
    spec.validate().expect("frozen derivative policy is valid");
    let outcome = run_multi_market_backtest(&spec, &research("multi-market-derivative-run"))
        .expect("leveraged derivative market runs through the shared Rust engine");
    let artifact: Value = serde_json::from_str(
        &std::fs::read_to_string(&outcome.artifact_path).expect("derivative artifact exists"),
    )
    .expect("derivative artifact JSON");
    assert_eq!(artifact["replay_verified"], true);
    assert_eq!(
        artifact["spec"]["markets"][0]["order_policy"]["leverage"],
        5
    );
    assert_eq!(artifact["funding_rate_input"][0]["funding_bps"], 8);

    let mut spot_funding = spec.clone();
    spot_funding.markets[1].funding_bps = 8;
    assert_eq!(
        spot_funding.validate().unwrap_err().category(),
        AppErrorCategory::InvalidInput
    );

    spec.markets[0].order_policy.as_mut().unwrap().leverage = 21;
    assert_eq!(
        spec.validate().unwrap_err().category(),
        AppErrorCategory::InvalidInput
    );
}

#[test]
fn multi_market_rejects_spot_orders_below_exchange_notional_minimum() {
    let root = scratch("multi-market-min-notional");
    let mut spec = spec(&root, "multi-market-min-notional");
    let instrument = InstrumentId::parse(&spec.markets[0].instrument).unwrap();
    spec.markets[0].market_spec = Some(
        serde_json::to_value(TradingInstrumentSpec {
            instrument,
            product: TradingProduct::Spot,
            base_currency: "BTC".into(),
            quote_currency: "USDT".into(),
            settlement_currency: "USDT".into(),
            contract_size: SCALE,
            linear: false,
            inverse: false,
            price_tick: 1,
            qty_step: 1,
            min_qty: 1,
            min_notional_raw: 1_000_000 * SCALE,
            max_leverage: 1,
            maintenance_margin_bps: 0,
            valid_from: 0,
            valid_to: None,
        })
        .unwrap(),
    );
    spec.markets[0].fee_schedule = Some(MakerTakerFeeSchedule {
        maker_fee_bps: 0,
        taker_fee_bps: 0,
    });

    let outcome = run_multi_market_backtest(&spec, &research("multi-market-min-notional"))
        .expect("minimum notional is applied at shared Bar order admission");
    let artifact: Value = serde_json::from_str(
        &std::fs::read_to_string(outcome.artifact_path).expect("artifact exists"),
    )
    .expect("artifact JSON");
    let rejected_for_minimum = artifact["event_log"]
        .as_array()
        .unwrap()
        .iter()
        .any(|event| {
            event["kind"]["Rejected"]["reason"]
                .as_str()
                .is_some_and(|reason| reason.contains("订单名义金额低于交易所最小限制"))
        });
    assert!(
        rejected_for_minimum,
        "expected venue minimum-notional rejection"
    );
}
