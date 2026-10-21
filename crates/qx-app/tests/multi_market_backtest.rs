mod common;

use common::{scratch, write_frame};
use qx_app::{
    run_multi_market_backtest, AppErrorCategory, BuiltinStrategySpec, CallerCapability,
    MultiMarketBacktestMarket, MultiMarketBacktestSpec, PortfolioStrategySpec, RunContext,
    RunStatus,
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
