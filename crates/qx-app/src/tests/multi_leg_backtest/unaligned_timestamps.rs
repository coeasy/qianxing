use super::*;

#[test]
fn multi_leg_backtest_accepts_unaligned_market_timestamps() {
    let root = super::super::fixtures::scratch("multi-leg-align");
    let primary = super::super::fixtures::write_frame(&root, "primary.json", "BTCUSDT.BINANCE", 40);
    let mut reference_json: serde_json::Value =
        serde_json::from_str(&super::super::fixtures::frame_json("ETHUSDT.OKX", 40)).unwrap();
    reference_json["ts"][0] = serde_json::json!(999);
    let reference_path = root.join("reference.json");
    std::fs::write(&reference_path, reference_json.to_string()).unwrap();
    let strategy = BuiltinStrategySpec::new(BuiltinStrategyKind::PairsArbitrage, "pair-test");
    let spec = MultiLegBacktestSpec::new(
        "pair-align",
        primary,
        reference_path.to_string_lossy(),
        "USDT",
        1_000_000 * SCALE,
        1_000_000 * SCALE,
        0,
        root.join("out").to_string_lossy(),
        strategy,
    );
    let identity = crate::StrategyArtifactIdentity {
        strategy_id: "pair-test".into(),
        strategy_version: "test-v1".into(),
        source_sha256: "a".repeat(64),
    };
    let outcome = crate::run_multi_leg_backtest_with_strategy(
        &spec,
        &identity,
        Box::new(BuyEachLeg(Default::default(), SCALE)),
        &context("pair-align"),
    )
    .expect("independently sampled markets use the merged deterministic timeline");
    assert!(outcome.primary_fills > 0);
    assert!(outcome.reference_fills > 0);
}
