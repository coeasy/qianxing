use super::*;

#[test]
fn multi_leg_backtest_accepts_unaligned_market_timestamps() {
    let root = common::scratch("multi-leg-align");
    let primary = common::write_frame(&root, "primary.json", "BTCUSDT.BINANCE", 40);
    let mut reference_json: serde_json::Value =
        serde_json::from_str(&common::frame_json("ETHUSDT.OKX", 40)).unwrap();
    reference_json["ts"][0] = serde_json::json!(999);
    let reference_path = root.join("reference.json");
    std::fs::write(&reference_path, reference_json.to_string()).unwrap();
    let strategy = BuiltinStrategySpec::new(BuiltinStrategyKind::PairsArbitrage, "pair-test");
    let spec = MultiLegBacktestSpec::new(
        qx_app::MultiLegDatasetInput {
            run_id: ("pair-align").into(),
            primary_bars_path: primary,
            reference_bars_path: (reference_path.to_string_lossy()).into(),
        },
        "USDT",
        1_000_000 * SCALE,
        1_000_000 * SCALE,
        0,
        root.join("out").to_string_lossy(),
        strategy,
    );
    let identity = qx_app::StrategyArtifactIdentity {
        strategy_id: "pair-test".into(),
        strategy_version: "test-v1".into(),
        source_sha256: "a".repeat(64),
    };
    let outcome = qx_app::run_multi_leg_backtest_with_strategy(
        &spec,
        &identity,
        Box::new(BuyEachLeg(Default::default(), SCALE)),
        &context("pair-align"),
    )
    .expect("independently sampled markets use the merged deterministic timeline");
    assert!(outcome.primary_fills > 0);
    assert!(outcome.reference_fills > 0);
    println!(
        "UNALIGNED_TIMELINE_OBSERVED markets=2 positive_fill_legs=2 primary_fills={} reference_fills={}",
        outcome.primary_fills, outcome.reference_fills
    );
}
