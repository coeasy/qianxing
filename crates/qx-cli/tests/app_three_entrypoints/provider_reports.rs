use qx_app::{
    run_provider_backtest_with_manifest, run_provider_depth_backtest, BacktestDatasetInput,
    BacktestSpec, BuiltinStrategySpec, CallerCapability, DepthBacktestSpec, ProviderBacktestSpec,
    ProviderDepthBacktestSpec, RunContext, PROVIDER_DEPTH_BACKTEST_SPEC_SCHEMA_VERSION,
};
use qx_core::{InstrumentId, Price, Quantity, SCALE};
use qx_guanxing::Bar;
use qx_provider::{
    DataKind, DataProvider, DataQuery, DataRequest, DataRequirements, MarketDataTier,
    ProviderCapability, ProviderError, ProviderPayload, ProviderRegistry, ProviderResult,
};
use qx_strategy::BuiltinStrategyKind;
use qx_xingban::{BookLevel, OrderBookSnapshot};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

struct FixtureProvider {
    capability: ProviderCapability,
    result: ProviderResult,
}

impl DataProvider for FixtureProvider {
    fn capability(&self) -> &ProviderCapability {
        &self.capability
    }

    fn fetch(&self, _query: &DataQuery) -> Result<ProviderResult, ProviderError> {
        Ok(self.result.clone())
    }
}

fn capability(
    provider_id: &str,
    kind: DataKind,
    tier: MarketDataTier,
    frequency: &str,
    end: u64,
) -> ProviderCapability {
    ProviderCapability {
        provider_id: provider_id.into(),
        version: "v1".into(),
        data_kinds: [kind].into_iter().collect(),
        data_tiers: [tier].into_iter().collect(),
        asset_classes: ["crypto".into()].into_iter().collect(),
        frequencies: [frequency.into()].into_iter().collect(),
        auth_scope: "public".into(),
        rate_limit_per_second: 100,
        freshness_seconds: 0,
        historical_start: 1,
        historical_end: end,
        realtime: false,
        priority: 0,
        quality_score: 100,
        cost_score: 0,
    }
}

fn query(kind: DataKind, frequency: &str, start: u64, end: u64) -> DataQuery {
    DataQuery {
        kind,
        asset_class: "crypto".into(),
        instrument_set: ["BTCUSDT.BINANCE".into()].into_iter().collect(),
        field_set: BTreeSet::new(),
        frequency: frequency.into(),
        adjustment: "none".into(),
        quality_policy: "strict".into(),
        start,
        end,
        as_of: None,
    }
}

fn cli_report(command: &str, outcome_path: &Path) -> Value {
    let output = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .current_dir(super::repo_root())
        .args(["app", command])
        .arg(outcome_path)
        .output()
        .expect("start qx-cli Provider report reader");
    assert_eq!(
        output.status.code(),
        Some(0),
        "{command} should read and verify its Provider outcome: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("Provider report JSON")
}

#[test]
fn provider_bar_and_depth_report_commands_return_verified_shared_reports() {
    let root = super::scratch("provider-reports");
    let instrument = "BTCUSDT.BINANCE";
    let bar_rows = (0..48_u64)
        .map(|index| {
            let ts = 1_000 + index * 1_000;
            let close = (100 + (index % 9) as i128) * SCALE;
            let bar = Bar::new(ts, close, close + SCALE, close - SCALE, close, 100 * SCALE);
            (
                ts,
                ts,
                ProviderPayload::new_json(
                    instrument,
                    "application/vnd.qianxing.bar+json;v=1",
                    serde_json::to_string(&bar).expect("Bar JSON"),
                ),
            )
        })
        .collect();
    let bar_result =
        ProviderResult::from_payloads("fixture-bar", "v1", "bar-request", 48_000, bar_rows)
            .expect("valid Provider Bar result");
    let mut bar_registry = ProviderRegistry::new();
    bar_registry
        .register(Box::new(FixtureProvider {
            capability: capability(
                "fixture-bar",
                DataKind::Bar,
                MarketDataTier::Bar,
                "1s",
                48_000,
            ),
            result: bar_result,
        }))
        .expect("register Bar provider");

    let mut bar_strategy =
        BuiltinStrategySpec::new(BuiltinStrategyKind::SmaCross, "cli-provider-bar");
    bar_strategy.fast_window = 2;
    bar_strategy.slow_window = 3;
    let bar_output = root.join("bar-runs");
    let bar_spec = ProviderBacktestSpec {
        data_request: DataRequest::new(
            query(DataKind::Bar, "1s", 1_000, 48_000),
            DataRequirements::default(),
        ),
        backtest: BacktestSpec::new(
            BacktestDatasetInput {
                run_id: "cli-provider-bar".into(),
                instrument: instrument.into(),
                bars_path: root.join("bars.json").to_string_lossy().into_owned(),
            },
            "USDT",
            10_000 * SCALE,
            20261025,
            bar_output.to_string_lossy(),
            bar_strategy,
        ),
        dataset_id: "cli-provider-bars".into(),
        dataset_version: "v1".into(),
        interval_ms: 1_000,
        timezone: "UTC".into(),
        manifest_path: None,
    };
    let bar_outcome = run_provider_backtest_with_manifest(
        &bar_registry,
        &bar_spec,
        &RunContext::new(CallerCapability::Research, "cli-provider-bar"),
    )
    .expect("run Provider Bar use case");
    let bar_outcome_path = root.join("provider-bar-outcome.json");
    let bar_outcome_json =
        serde_json::to_string(&bar_outcome).expect("serialize Provider Bar outcome");
    std::fs::write(&bar_outcome_path, &bar_outcome_json).expect("write Provider Bar outcome");
    let bar_report = cli_report("read-provider-report", &bar_outcome_path);
    assert_eq!(bar_report["schema_version"], 1);
    assert_eq!(bar_report["report"]["run_id"], "cli-provider-bar");
    assert_eq!(bar_report["report"]["verification"]["verified"], true);
    assert_eq!(
        bar_report["report"]["summary"]["input"]["provider_lineage"]["provider_id"],
        "fixture-bar"
    );
    match super::python_native::via_python_sdk_json_value(
        &super::repo_root(),
        "read_provider_run_report_v1",
        &bar_outcome_json,
    ) {
        None => eprintln!("[skip] Python Provider Bar report leg: native extension unavailable"),
        Some(Ok(python_report)) => assert_eq!(
            python_report, bar_report,
            "Python and CLI Provider Bar reports must return the same qx-app DTO"
        ),
        Some(Err(error)) => panic!("Python Provider Bar report reader failed: {error}"),
    }

    let depth_rows = (1..=3_u64)
        .map(|ts| {
            let snapshot = OrderBookSnapshot {
                instrument: InstrumentId::parse(instrument).expect("instrument"),
                ts,
                sequence: ts,
                bids: vec![BookLevel {
                    price_exact: None,
                    qty_exact: None,
                    price: Price::from_i64(100 + ts as i64),
                    qty: Quantity::from_i64(2),
                }],
                asks: vec![BookLevel {
                    price_exact: None,
                    qty_exact: None,
                    price: Price::from_i64(101 + ts as i64),
                    qty: Quantity::from_i64(2),
                }],
            };
            (
                ts,
                ts,
                ProviderPayload::new_json(
                    instrument,
                    "application/vnd.qianxing.order-book-snapshot+json;v=1",
                    serde_json::to_string(&snapshot).expect("snapshot JSON"),
                ),
            )
        })
        .collect();
    let depth_result =
        ProviderResult::from_payloads("fixture-depth", "v1", "depth-request", 3, depth_rows)
            .expect("valid Provider depth result");
    let mut depth_registry = ProviderRegistry::new();
    depth_registry
        .register(Box::new(FixtureProvider {
            capability: capability(
                "fixture-depth",
                DataKind::Quote,
                MarketDataTier::L1,
                "tick",
                3,
            ),
            result: depth_result,
        }))
        .expect("register depth provider");

    let mut depth_strategy =
        BuiltinStrategySpec::new(BuiltinStrategyKind::SmaCross, "cli-provider-depth");
    depth_strategy.fast_window = 2;
    depth_strategy.slow_window = 3;
    let depth_path = root.join("depth.json");
    let depth_backtest: DepthBacktestSpec = serde_json::from_value(json!({
        "schema_version": 1,
        "run_id": "cli-provider-depth",
        "depth_path": depth_path,
        "tier": "l1",
        "settlement_currency": "USDT",
        "initial_cash_raw": 10_000_i128 * SCALE,
        "latency_snapshots": 0,
        "queue_position_bps": 0,
        "market_impact_bps": 0,
        "output_dir": root.join("depth-runs"),
        "strategy": depth_strategy
    }))
    .expect("valid DepthBacktestSpec");
    let depth_spec = ProviderDepthBacktestSpec {
        schema_version: PROVIDER_DEPTH_BACKTEST_SPEC_SCHEMA_VERSION,
        data_request: DataRequest::new(
            query(DataKind::Quote, "tick", 1, 3),
            DataRequirements {
                required_data_tier: Some(MarketDataTier::L1),
                ..DataRequirements::default()
            },
        ),
        backtest: depth_backtest,
    };
    let depth_outcome = run_provider_depth_backtest(
        &depth_registry,
        &depth_spec,
        &RunContext::new(CallerCapability::Research, "cli-provider-depth"),
    )
    .expect("run Provider depth use case");
    let depth_outcome_path = root.join("provider-depth-outcome.json");
    let depth_outcome_json =
        serde_json::to_string(&depth_outcome).expect("serialize Provider depth outcome");
    std::fs::write(&depth_outcome_path, &depth_outcome_json).expect("write Provider depth outcome");
    let depth_report = cli_report("read-provider-depth-report", &depth_outcome_path);
    assert_eq!(depth_report["schema_version"], 1);
    assert_eq!(depth_report["report"]["run_id"], "cli-provider-depth");
    assert_eq!(depth_report["report"]["verification"]["verified"], true);
    assert_eq!(depth_report["provider_id"], "fixture-depth");
    assert_eq!(depth_report["request_id"], "depth-request");
    match super::python_native::via_python_sdk_json_value(
        &super::repo_root(),
        "read_provider_depth_run_report_v1",
        &depth_outcome_json,
    ) {
        None => eprintln!("[skip] Python Provider depth report leg: native extension unavailable"),
        Some(Ok(python_report)) => assert_eq!(
            python_report, depth_report,
            "Python and CLI Provider depth reports must return the same qx-app DTO"
        ),
        Some(Err(error)) => panic!("Python Provider depth report reader failed: {error}"),
    }
}
