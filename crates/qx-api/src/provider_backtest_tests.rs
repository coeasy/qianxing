use super::{ApiService, ApiState};
use qx_app::{BacktestSpec, BuiltinStrategySpec, ProviderBacktestOutcome, ProviderBacktestSpec};
use qx_provider::{
    DataKind, DataProvider, DataQuery, DataRequest, DataRequirements, ProviderCapability,
    ProviderError, ProviderRegistry, ProviderResult,
};
use qx_strategy::BuiltinStrategyKind;
use std::{collections::BTreeSet, fs, path::PathBuf, sync::Arc};

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

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let path = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!("qx-api-provider-route-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn provider_registry() -> ProviderRegistry {
    let mut registry = ProviderRegistry::new();
    let instrument = "BTCUSDT.BINANCE";
    let records = (1..=64)
        .map(|index| {
            let close = 10_000 + index as i128 * 100;
            let bar = qx_guanxing::Bar::new(
                index * 60_000,
                close - 50,
                close + 100,
                close - 100,
                close,
                100,
            );
            (
                index * 60_000,
                index * 60_000,
                qx_provider::ProviderPayload::new_json(
                    instrument,
                    "application/vnd.qianxing.bar+json;v=1",
                    serde_json::to_string(&bar).unwrap(),
                ),
            )
        })
        .collect();
    let result =
        ProviderResult::from_payloads("fixture", "v1", "request-1", 4_000_000, records).unwrap();
    let capability = ProviderCapability {
        provider_id: "fixture".into(),
        version: "v1".into(),
        data_kinds: [DataKind::Bar].into_iter().collect(),
        asset_classes: ["crypto".into()].into_iter().collect(),
        frequencies: ["1m".into()].into_iter().collect(),
        auth_scope: "public".into(),
        rate_limit_per_second: 100,
        freshness_seconds: 60,
        historical_start: 0,
        historical_end: 4_000_000,
        realtime: false,
        priority: 1,
        quality_score: 100,
        cost_score: 1,
    };
    registry
        .register(Box::new(FixtureProvider { capability, result }))
        .unwrap();
    registry
}

fn provider_spec() -> ProviderBacktestSpec {
    ProviderBacktestSpec {
        data_request: DataRequest::new(
            DataQuery {
                kind: DataKind::Bar,
                asset_class: "crypto".into(),
                instrument_set: BTreeSet::from(["BTCUSDT.BINANCE".into()]),
                field_set: BTreeSet::from(["close".into()]),
                frequency: "1m".into(),
                adjustment: "none".into(),
                quality_policy: "strict".into(),
                start: 60_000,
                end: 3_840_000,
                as_of: Some(3_900_000),
            },
            DataRequirements::default(),
        ),
        backtest: BacktestSpec::new(
            "api-provider-run",
            "BTCUSDT.BINANCE",
            "unused.json",
            "USDT",
            100_000_000_000_000,
            20261013,
            "unused-output",
            BuiltinStrategySpec::new(BuiltinStrategyKind::SmaCross, "api-provider-sma"),
        ),
        dataset_id: "api-provider-bars".into(),
        dataset_version: "v1".into(),
        interval_ms: 60_000,
        timezone: "UTC".into(),
        manifest_path: None,
    }
}

#[test]
fn provider_backtest_http_route_requires_and_uses_injected_registry() {
    let spec = provider_spec();
    let body = serde_json::to_string(&spec).unwrap();
    let unconfigured =
        ApiService::new(ApiState::default()).handle("POST", "/app/provider-backtest", &body, 1);
    assert_eq!(unconfigured.status, 404, "{}", unconfigured.body);
    assert!(unconfigured
        .body
        .contains("Provider registry is not configured"));

    let configured = ApiService::new(ApiState::default())
        .with_provider_registry(Arc::new(ProviderRegistry::new()))
        .handle("POST", "/app/provider-backtest", &body, 2);
    assert_eq!(configured.status, 404, "{}", configured.body);
    assert!(configured.body.contains("Provider fetch failed"));
}

#[test]
fn provider_depth_backtest_route_is_registered_and_requires_configured_registry() {
    let response = ApiService::new(ApiState::default()).handle(
        "POST",
        "/app/provider-depth-backtest",
        "{}",
        1,
    );
    assert_eq!(response.status, 404, "{}", response.body);
    assert!(response
        .body
        .contains("Provider registry is not configured"));
}

#[test]
fn provider_backtest_http_route_runs_freezes_and_returns_artifacts() {
    let directory = TestDirectory::new();
    let mut service =
        ApiService::new(ApiState::default()).with_provider_registry(Arc::new(provider_registry()));
    service.app_paths.artifact_root = directory.0.clone();

    let response = service.handle(
        "POST",
        "/app/provider-backtest",
        &serde_json::to_string(&provider_spec()).unwrap(),
        1,
    );
    assert_eq!(response.status, 200, "{}", response.body);
    let outcome: ProviderBacktestOutcome = serde_json::from_str(&response.body).unwrap();
    assert!(outcome.dataset_verdict.usable);
    assert_eq!(outcome.dataset_verdict.rows, 64);
    assert_eq!(outcome.manifest.source_lineage.primary_source, "fixture");
    assert!(PathBuf::from(outcome.manifest_path).is_file());
    assert!(PathBuf::from(outcome.outcome.artifacts.run_manifest).is_file());
    assert!(PathBuf::from(outcome.outcome.artifacts.summary).is_file());
    assert!(PathBuf::from(outcome.outcome.artifacts.equity).is_file());
    assert!(PathBuf::from(outcome.outcome.artifacts.fills).is_file());
}
