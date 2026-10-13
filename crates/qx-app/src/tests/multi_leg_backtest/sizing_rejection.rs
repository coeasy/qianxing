use super::*;

#[test]
fn custom_multi_leg_callbacks_reject_builtin_sizing_parameters() {
    let root = super::super::fixtures::scratch("multi-leg-custom-sizing");
    let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy");
    let primary = fixtures.join("qianxing.bar-frame.pairs-primary.example.json");
    let reference = fixtures.join("qianxing.bar-frame.pairs-reference.example.json");
    let mut spec = MultiLegBacktestSpec::new(
        "custom-sizing-run",
        primary.to_string_lossy(),
        reference.to_string_lossy(),
        "USDT",
        1_000_000 * SCALE,
        1_000_000 * SCALE,
        7,
        root.join("out").to_string_lossy(),
        BuiltinStrategySpec::new(BuiltinStrategyKind::PairsArbitrage, "custom-sizing"),
    );
    spec.primary_market_spec = Some(derivative_spec("BTCUSDT.BINANCE"));
    spec.reference_market_spec = Some(derivative_spec("ETHUSDT.BINANCE"));
    spec.strategy.target_notional_raw = Some(100 * SCALE);
    let identity = crate::StrategyArtifactIdentity {
        strategy_id: "custom-sizing".into(),
        strategy_version: "user-v1".into(),
        source_sha256: "d".repeat(64),
    };
    let error = crate::run_multi_leg_backtest_with_strategy(
        &spec,
        &identity,
        Box::new(BuyEachLeg(Default::default(), SCALE)),
        &context("custom-sizing-run"),
    )
    .expect_err("custom callbacks own their own position sizing");
    assert_eq!(error.category(), AppErrorCategory::InvalidInput);
    assert!(error.to_string().contains("sizing knobs do not apply"));
}
