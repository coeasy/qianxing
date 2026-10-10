use crate::{
    run_multi_leg_backtest, AppErrorCategory, BuiltinStrategySpec, CallerCapability,
    MultiLegBacktestSpec, RunContext,
};
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
    let spec = MultiLegBacktestSpec::new(
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
    let first = run_multi_leg_backtest(&spec, &context("pair-run")).expect("shared multi-leg run");
    let second = run_multi_leg_backtest(&spec, &context("pair-run")).expect("idempotent rerun");
    assert_eq!(first, second);
    assert_ne!(first.primary_result_hash, first.reference_result_hash);
    assert!(first.primary_fills > 0 && first.reference_fills > 0);
    let artifact =
        std::fs::read_to_string(&first.attribution_artifact).expect("multi-leg artifact");
    assert!(artifact.contains("pending_reconcile"));
    assert!(artifact.contains("replay_ledger_entries"));
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
