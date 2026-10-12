use crate::{
    run_multi_leg_backtest, AppErrorCategory, BuiltinStrategySpec, CallerCapability,
    MultiLegBacktestSpec, RunContext,
};
use qx_core::{InstrumentId, TradingInstrumentSpec, TradingProduct, SCALE};
use qx_strategy::BuiltinStrategyKind;

fn context(id: &str) -> RunContext {
    RunContext::new(CallerCapability::Research, id)
}

#[test]
fn multi_leg_backtest_runs_shared_strategy_and_matching_and_is_idempotent() {
    let root = super::fixtures::scratch("multi-leg-app");
    let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy");
    let primary = fixtures
        .join("qianxing.bar-frame.pairs-primary.example.json")
        .to_string_lossy()
        .into_owned();
    let reference = fixtures
        .join("qianxing.bar-frame.pairs-reference.example.json")
        .to_string_lossy()
        .into_owned();
    let mut strategy = BuiltinStrategySpec::new(BuiltinStrategyKind::PairsArbitrage, "pair-test");
    strategy.quantity_raw = qx_core::SCALE;
    let mut spec = MultiLegBacktestSpec::new(
        "pair-run",
        primary,
        reference,
        "USDT",
        1_000_000 * qx_core::SCALE,
        1_000_000 * qx_core::SCALE,
        7,
        root.join("out").to_string_lossy(),
        strategy,
    );
    spec.account_id = Some("acct.pair-test".into());
    let first = run_multi_leg_backtest(&spec, &context("pair-run")).expect("shared multi-leg run");
    let second = run_multi_leg_backtest(&spec, &context("pair-run")).expect("idempotent rerun");
    assert_eq!(first, second);
    assert_eq!(first.account_id.as_deref(), Some("acct.pair-test"));
    assert_ne!(first.primary_result_hash, first.reference_result_hash);
    assert!(first.primary_fills > 0 && first.reference_fills > 0);
    let artifact =
        std::fs::read_to_string(&first.attribution_artifact).expect("multi-leg artifact");
    assert!(artifact.contains("pending_reconcile"));
    assert!(artifact.contains("replay_ledger_entries"));
    assert!(artifact.contains("acct.pair-test"));
}

#[test]
fn multi_leg_backtest_rejects_unaligned_frames_before_matching() {
    let root = super::fixtures::scratch("multi-leg-align");
    let primary = super::fixtures::write_frame(&root, "primary.json", "BTCUSDT.BINANCE", 40);
    let mut reference_json: serde_json::Value =
        serde_json::from_str(&super::fixtures::frame_json("ETHUSDT.OKX", 40)).unwrap();
    reference_json["ts"][0] = serde_json::json!(999);
    let reference_path = root.join("reference.json");
    std::fs::write(&reference_path, reference_json.to_string()).unwrap();
    let strategy = BuiltinStrategySpec::new(BuiltinStrategyKind::PairsArbitrage, "pair-test");
    let spec = MultiLegBacktestSpec::new(
        "pair-align",
        primary,
        reference_path.to_string_lossy(),
        "USDT",
        1_000_000,
        1_000_000,
        0,
        root.join("out").to_string_lossy(),
        strategy,
    );
    let error =
        run_multi_leg_backtest(&spec, &context("pair-align")).expect_err("timestamp mismatch");
    assert_eq!(error.category(), AppErrorCategory::InvalidInput);
    assert_eq!(error.correlation_id(), Some("pair-align"));
}

fn derivative_spec(instrument: &str) -> TradingInstrumentSpec {
    let (base, quote) = instrument.split_once('.').unwrap().0.split_at(3);
    TradingInstrumentSpec {
        instrument: InstrumentId::parse(instrument).unwrap(),
        product: TradingProduct::Perpetual,
        base_currency: base.into(),
        quote_currency: quote.into(),
        settlement_currency: "USDT".into(),
        contract_size: SCALE,
        linear: true,
        inverse: false,
        price_tick: 1_000,
        qty_step: 1,
        min_qty: 1,
        max_leverage: 20,
        maintenance_margin_bps: 500,
        valid_from: 0,
        valid_to: None,
    }
}

#[test]
fn multi_leg_spec_applies_market_specs_fees_funding_and_shared_fill_choice() {
    let root = super::fixtures::scratch("multi-leg-configured");
    let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy");
    let primary = fixtures
        .join("qianxing.bar-frame.pairs-primary.example.json")
        .to_string_lossy()
        .into_owned();
    let reference = fixtures
        .join("qianxing.bar-frame.pairs-reference.example.json")
        .to_string_lossy()
        .into_owned();
    let mut strategy = BuiltinStrategySpec::new(BuiltinStrategyKind::PairsArbitrage, "pair-costs");
    strategy.quantity_raw = SCALE;
    let mut spec = MultiLegBacktestSpec::new(
        "pair-configured",
        primary,
        reference,
        "USDT",
        1_000_000 * SCALE,
        1_000_000 * SCALE,
        7,
        root.join("out").to_string_lossy(),
        strategy,
    );
    spec.primary_market_spec = Some(derivative_spec("BTCUSDT.BINANCE"));
    spec.reference_market_spec = Some(derivative_spec("ETHUSDT.BINANCE"));
    spec.maker_fee_bps = 3;
    spec.taker_fee_bps = 10;
    spec.primary_taker_fee_bps = Some(12);
    spec.funding_bps = 100;
    spec.fill_model = crate::MultiLegFillModel::OneTickSlippage;

    let outcome = run_multi_leg_backtest(&spec, &context("pair-configured"))
        .expect("configured derivative multi-leg run");
    let artifact: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&outcome.attribution_artifact).unwrap())
            .unwrap();
    assert!(artifact["attribution_totals"]["funding_raw"] != "0");
    assert!(artifact["attribution_totals"]["margin_peak_raw"] != "0");
    assert_eq!(artifact["fill_model"], "one_tick_slippage");
    assert_eq!(artifact["fee_model"]["primary"]["maker_fee_bps"], 3);
    assert_eq!(artifact["fee_model"]["primary"]["taker_fee_bps"], 12);
    assert_eq!(artifact["fee_model"]["reference"]["taker_fee_bps"], 10);
}

#[test]
fn multi_leg_funding_and_one_tick_fill_fail_closed_without_market_specs() {
    let root = super::fixtures::scratch("multi-leg-missing-spec");
    let primary = super::fixtures::write_frame(&root, "primary.json", "BTCUSDT.BINANCE", 40);
    let reference = super::fixtures::write_frame(&root, "reference.json", "ETHUSDT.OKX", 40);
    let strategy = BuiltinStrategySpec::new(BuiltinStrategyKind::PairsArbitrage, "pair-test");
    let mut spec = MultiLegBacktestSpec::new(
        "pair-missing-spec",
        primary,
        reference,
        "USDT",
        1_000_000,
        1_000_000,
        0,
        root.join("out").to_string_lossy(),
        strategy,
    );
    spec.funding_bps = 1;
    let error = run_multi_leg_backtest(&spec, &context("pair-missing-spec"))
        .expect_err("funding cannot guess products or contract size");
    assert_eq!(error.category(), AppErrorCategory::InvalidInput);

    spec.funding_bps = 0;
    spec.fill_model = crate::MultiLegFillModel::OneTickSlippage;
    let error = run_multi_leg_backtest(&spec, &context("pair-missing-spec"))
        .expect_err("one-tick slippage cannot guess a tick size");
    assert_eq!(error.category(), AppErrorCategory::InvalidInput);
}
