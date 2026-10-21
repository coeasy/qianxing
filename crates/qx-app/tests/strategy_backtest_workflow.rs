mod common;

use common::{backtest_spec, frame_json, scratch};
use qx_app::artifact_store::{
    BacktestAccountBase, BacktestInputProvenance, BACKTEST_ACCOUNT_BASE_DEFAULT_SOURCE,
};
use qx_app::{
    run_and_persist_strategy_backtest, CallerCapability, RunContext, StrategyBacktestExecution,
    StrategyBacktestWorkflowInput,
};
use qx_core::{Money, Quantity};
use qx_datastruct::BarFrame;
use qx_strategy::{BuiltinStrategyConfig, BuiltinStrategyKind};
use qx_xingban::BarStrategy;
use std::collections::BTreeMap;

struct NoOrders;

impl BarStrategy for NoOrders {}

#[test]
fn workflow_executes_adapter_and_publishes_its_verified_identity_and_artifacts() {
    let root = scratch("strategy-backtest-workflow");
    let frame = BarFrame::from_json(&frame_json("BTCUSDT.BINANCE", 32)).expect("valid frame json");
    let bars: Vec<qx_guanxing::Bar> = (&frame).into();
    let spec = backtest_spec("workflow-test", "memory://workflow-test", &root);
    let context = RunContext::new(CallerCapability::Research, "strategy-workflow-test");
    let result = run_and_persist_strategy_backtest(StrategyBacktestWorkflowInput {
        spec: &spec,
        bars: &bars,
        execution: StrategyBacktestExecution::Adapter(Box::new(NoOrders)),
        context: &context,
        run_manifest_root: &root,
        strategy_id: "workflow-test-strategy",
        strategy_version: "v-test",
        declared_product: None,
        config_fingerprint: "config-fingerprint",
        runtime_config_path: None,
        market_spec_path: None,
        instrument_spec_version: "instrument-spec:none",
        runtime_version: "runtime-schema-test",
        code_commit: "test-commit",
        cost_source: "test-cost-source",
        fill_model: ("test-fill", "test-source"),
        account_base: BacktestAccountBase {
            cash: Money::from_i64(100_000),
            source: BACKTEST_ACCOUNT_BASE_DEFAULT_SOURCE,
        },
        input: BacktestInputProvenance {
            kind: "barframe",
            path: "memory://workflow-test".into(),
            dataset_id: "workflow-test-data".into(),
            dataset_version: "v1".into(),
            fingerprint: "verified-frame-fingerprint".into(),
        },
        dataset_bundle_fingerprint: None,
        dataset_components: BTreeMap::new(),
        signal: None,
        dataset_quality: None,
    })
    .expect("application workflow should execute and publish the backtest");

    assert_eq!(
        result.persisted.manifest.data_fingerprint,
        "barframe:verified-frame-fingerprint"
    );
    assert_eq!(
        result.persisted.manifest.result_hash,
        format!("{:016x}", result.outcome.report.result_hash())
    );
    assert!(result.persisted.manifest_path.is_file());
    assert!(result.persisted.artifacts.summary_path.is_file());
    assert!(result.persisted.artifacts.equity_path.is_file());
    assert!(result.persisted.artifacts.fills_path.is_file());
}

#[test]
fn workflow_builds_native_execution_context_from_the_resolved_spec_and_input_identity() {
    let root = scratch("strategy-backtest-native-workflow");
    let frame = BarFrame::from_json(&frame_json("BTCUSDT.BINANCE", 32)).expect("valid frame json");
    let bars: Vec<qx_guanxing::Bar> = (&frame).into();
    let mut spec = backtest_spec(
        "native-workflow-test",
        "memory://native-workflow-test",
        &root,
    );
    spec.account_id = Some("native-workflow-account".into());
    let strategy = BuiltinStrategyConfig::new(
        BuiltinStrategyKind::SmaCross,
        "native-workflow-strategy",
        frame.instrument.clone(),
        Quantity::from_i64(1),
    )
    .expect("valid native builtin configuration");
    let context = RunContext::new(CallerCapability::Research, "native-strategy-workflow-test");

    let result = run_and_persist_strategy_backtest(StrategyBacktestWorkflowInput {
        spec: &spec,
        bars: &bars,
        execution: StrategyBacktestExecution::NativeBuiltin {
            strategy: Box::new(strategy),
            venue_id: Some("BINANCE-CUSTOM".into()),
        },
        context: &context,
        run_manifest_root: &root,
        strategy_id: "native-workflow-strategy",
        strategy_version: "v-test",
        declared_product: None,
        config_fingerprint: "native-config-fingerprint",
        runtime_config_path: None,
        market_spec_path: None,
        instrument_spec_version: "instrument-spec:none",
        runtime_version: "runtime-schema-test",
        code_commit: "test-commit",
        cost_source: "test-cost-source",
        fill_model: ("test-fill", "test-source"),
        account_base: BacktestAccountBase {
            cash: Money::from_i64(100_000),
            source: BACKTEST_ACCOUNT_BASE_DEFAULT_SOURCE,
        },
        input: BacktestInputProvenance {
            kind: "barframe",
            path: "memory://native-workflow-test".into(),
            dataset_id: "native-workflow-test-data".into(),
            dataset_version: "v1".into(),
            fingerprint: "verified-native-frame-fingerprint".into(),
        },
        dataset_bundle_fingerprint: None,
        dataset_components: BTreeMap::new(),
        signal: None,
        dataset_quality: None,
    })
    .expect("application workflow should initialize and execute native strategy");

    assert_eq!(
        result.persisted.manifest.data_fingerprint,
        "barframe:verified-native-frame-fingerprint"
    );
    assert_eq!(
        result.persisted.manifest.result_hash,
        format!("{:016x}", result.outcome.report.result_hash())
    );
    assert!(result.persisted.artifacts.summary_path.is_file());
}

#[test]
fn derivative_declaration_without_market_spec_fails_before_artifact_side_effects() {
    let root = scratch("strategy-backtest-missing-spec");
    let unpublished = root.join("unpublished");
    let frame = BarFrame::from_json(&frame_json("BTCUSDT.BINANCE", 32)).expect("valid frame json");
    let bars: Vec<qx_guanxing::Bar> = (&frame).into();
    let spec = backtest_spec("missing-spec-test", "memory://missing-spec-test", &root);
    let context = RunContext::new(CallerCapability::Research, "strategy-missing-spec-test");

    let result = run_and_persist_strategy_backtest(StrategyBacktestWorkflowInput {
        spec: &spec,
        bars: &bars,
        execution: StrategyBacktestExecution::Adapter(Box::new(NoOrders)),
        context: &context,
        run_manifest_root: &unpublished,
        strategy_id: "missing-spec-test-strategy",
        strategy_version: "v-test",
        declared_product: Some(qx_core::TradingProduct::Perpetual),
        config_fingerprint: "config-fingerprint",
        runtime_config_path: None,
        market_spec_path: None,
        instrument_spec_version: "instrument-spec:none",
        runtime_version: "runtime-schema-test",
        code_commit: "test-commit",
        cost_source: "test-cost-source",
        fill_model: ("test-fill", "test-source"),
        account_base: BacktestAccountBase {
            cash: Money::from_i64(100_000),
            source: BACKTEST_ACCOUNT_BASE_DEFAULT_SOURCE,
        },
        input: BacktestInputProvenance {
            kind: "barframe",
            path: "memory://missing-spec-test".into(),
            dataset_id: "missing-spec-test-data".into(),
            dataset_version: "v1".into(),
            fingerprint: "verified-frame-fingerprint".into(),
        },
        dataset_bundle_fingerprint: None,
        dataset_components: BTreeMap::new(),
        signal: None,
        dataset_quality: None,
    });
    let error = match result {
        Err(error) => error,
        Ok(_) => panic!("perpetual runs require an explicit market spec"),
    };

    assert_eq!(error, "衍生品跨语言回测必须提供 market-spec.json");
    assert!(
        !unpublished.exists(),
        "rejection must precede artifact writes"
    );
}
