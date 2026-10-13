use super::*;

#[test]
fn multi_leg_backtest_competing_orders_share_one_deterministic_cash_pool() {
    let root = super::super::fixtures::scratch("multi-leg-shared-buying-power");
    let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy");
    let primary = fixtures.join("qianxing.bar-frame.pairs-primary.example.json");
    let reference = fixtures.join("qianxing.bar-frame.pairs-reference.example.json");
    let identity = crate::StrategyArtifactIdentity {
        strategy_id: "shared-cash-test".into(),
        strategy_version: "test-v1".into(),
        source_sha256: "a".repeat(64),
    };
    let spec = MultiLegBacktestSpec::new(
        "multi-leg-shared-buying-power",
        primary.to_string_lossy(),
        reference.to_string_lossy(),
        "USDT",
        50_000 * SCALE,
        50_000 * SCALE,
        7,
        root.join("out").to_string_lossy(),
        BuiltinStrategySpec::new(BuiltinStrategyKind::PairsArbitrage, "shared-cash-test"),
    );
    let outcome = crate::run_multi_leg_backtest_with_strategy(
        &spec,
        &identity,
        Box::new(BuyEachLeg(Default::default(), 2 * SCALE)),
        &context("multi-leg-shared-buying-power"),
    )
    .expect("competing market orders use the same account buying power");
    assert_eq!(outcome.primary_fills, 0);
    assert!(outcome.reference_fills > 0);
    let artifact: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(outcome.attribution_artifact).expect("artifact exists"),
    )
    .expect("artifact json");
    assert_eq!(artifact["account_ledger"]["cross_leg_buying_power"], true);
    assert_eq!(artifact["legs"][0]["rejected_orders"], 1);
    assert_eq!(artifact["legs"][1]["rejected_orders"], 0);
    assert_eq!(artifact["account_ledger"]["replay_verified"], true);
}
