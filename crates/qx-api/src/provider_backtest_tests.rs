use super::{ApiService, ApiState};
use qx_app::{
    BacktestDatasetInput, BacktestSpec, BuiltinStrategySpec, DepthBacktestSpec,
    ProviderBacktestOutcome, ProviderBacktestSpec, ProviderDepthBacktestOutcome,
    DEPTH_BACKTEST_SPEC_SCHEMA_VERSION,
};
use qx_provider::{
    DataKind, DataProvider, DataQuery, DataRequest, DataRequirements, ProviderCapability,
    ProviderError, ProviderRegistry, ProviderResult,
};
use qx_strategy::BuiltinStrategyKind;
use std::{
    collections::BTreeSet,
    fs,
    path::PathBuf,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

static NEXT_TEST_DIRECTORY: AtomicUsize = AtomicUsize::new(0);

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

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

impl TestDirectory {
    fn new() -> Self {
        let sequence = NEXT_TEST_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let path = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!(
                "qx-api-provider-route-{}-{sequence}",
                std::process::id()
            ));
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
        data_tiers: [qx_provider::MarketDataTier::Bar].into_iter().collect(),
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

fn quote_provider_registry() -> ProviderRegistry {
    let fixture_path = workspace_root()
        .join("deploy")
        .join("qianxing.depth-frame.l1.example.json");
    let fixture: serde_json::Value =
        serde_json::from_slice(&fs::read(fixture_path).unwrap()).unwrap();
    let snapshots = fixture["snapshots"].as_array().unwrap();
    let received_at = snapshots.last().unwrap()["ts"].as_u64().unwrap() + 1;
    let instrument = "BTCUSDT.BINANCE";
    let records = snapshots
        .iter()
        .map(|snapshot| {
            let ts = snapshot["ts"].as_u64().unwrap();
            (
                ts,
                ts,
                qx_provider::ProviderPayload::new_json(
                    instrument,
                    "application/vnd.qianxing.order-book-snapshot+json;v=1",
                    serde_json::to_string(snapshot).unwrap(),
                ),
            )
        })
        .collect();
    let result = ProviderResult::from_payloads(
        "quote-fixture",
        "v1",
        "quote-request-1",
        received_at,
        records,
    )
    .unwrap();
    let capability = ProviderCapability {
        provider_id: "quote-fixture".into(),
        version: "v1".into(),
        data_kinds: [DataKind::Quote].into_iter().collect(),
        data_tiers: [qx_provider::MarketDataTier::L1].into_iter().collect(),
        asset_classes: ["crypto".into()].into_iter().collect(),
        frequencies: ["tick".into()].into_iter().collect(),
        auth_scope: "public".into(),
        rate_limit_per_second: 100,
        freshness_seconds: 60,
        historical_start: 0,
        historical_end: received_at,
        realtime: false,
        priority: 1,
        quality_score: 100,
        cost_score: 1,
    };
    let mut registry = ProviderRegistry::new();
    registry
        .register(Box::new(FixtureProvider { capability, result }))
        .unwrap();
    registry
}

fn provider_depth_spec(root: &std::path::Path) -> qx_app::ProviderDepthBacktestSpec {
    let fixture_path = workspace_root()
        .join("deploy")
        .join("qianxing.depth-frame.l1.example.json");
    let fixture: serde_json::Value =
        serde_json::from_slice(&fs::read(fixture_path).unwrap()).unwrap();
    let snapshots = fixture["snapshots"].as_array().unwrap();
    let start = snapshots.first().unwrap()["ts"].as_u64().unwrap();
    let end = snapshots.last().unwrap()["ts"].as_u64().unwrap();
    qx_app::ProviderDepthBacktestSpec {
        schema_version: qx_app::PROVIDER_DEPTH_BACKTEST_SPEC_SCHEMA_VERSION,
        data_request: DataRequest::new(
            DataQuery {
                kind: DataKind::Quote,
                asset_class: "crypto".into(),
                instrument_set: BTreeSet::from(["BTCUSDT.BINANCE".into()]),
                field_set: BTreeSet::from(["bid".into(), "ask".into()]),
                frequency: "tick".into(),
                adjustment: "none".into(),
                quality_policy: "strict".into(),
                start,
                end,
                as_of: Some(end + 1),
            },
            DataRequirements {
                required_data_tier: Some(qx_provider::MarketDataTier::L1),
                ..DataRequirements::default()
            },
        ),
        backtest: DepthBacktestSpec {
            schema_version: DEPTH_BACKTEST_SPEC_SCHEMA_VERSION,
            run_id: "api-provider-depth-run".into(),
            depth_path: root
                .join("provider-depth.json")
                .to_string_lossy()
                .into_owned(),
            tier: "l1".into(),
            settlement_currency: "USDT".into(),
            initial_cash_raw: 100_000_000_000_000,
            fee_bps: Some(0),
            latency_snapshots: 0,
            queue_position_bps: 0,
            queue_position_model: Default::default(),
            market_impact_bps: 0,
            output_dir: root
                .join("provider-depth-run")
                .to_string_lossy()
                .into_owned(),
            strategy: BuiltinStrategySpec::new(BuiltinStrategyKind::SmaCross, "api-provider-depth"),
            market_spec: None,
            order_policy: None,
            market_making: None,
            account_id: None,
            portfolio: None,
            manifest_run_id: None,
            execution_cost_rules: None,
            execution_cost_source: None,
            risk_rules: None,
            allow_short: None,
            declared_ashare_rules: None,
            declared_fill_model: None,
            declared_bar_volume_participation_bps: None,
            quality_manifest_path: None,
            runtime_config_path: None,
            market_spec_path: None,
            market_spec_source: None,
            account_base_source: None,
            signal_source: None,
            signal_declared_unused: None,
        },
    }
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
            BacktestDatasetInput {
                run_id: "api-provider-run".into(),
                instrument: "BTCUSDT.BINANCE".into(),
                bars_path: "unused.json".into(),
            },
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
fn provider_backtest_specs_serialize_with_registered_schema_fields() {
    fn keys(value: &serde_json::Value) -> BTreeSet<String> {
        value
            .as_object()
            .expect("serialized contract object")
            .keys()
            .cloned()
            .collect()
    }

    fn properties(schema: &serde_json::Value, path: &[&str]) -> BTreeSet<String> {
        let mut value = schema;
        for segment in path {
            value = &value[segment];
        }
        keys(value)
    }

    let bar_schema: serde_json::Value = serde_json::from_str(include_str!(
        "../../../schemas/provider-backtest-spec-v1.json"
    ))
    .unwrap();
    let bar = serde_json::to_value(provider_spec()).unwrap();
    assert!(keys(&bar).is_subset(&properties(&bar_schema, &["properties"])));
    assert!(keys(&bar["data_request"]).is_subset(&properties(
        &bar_schema,
        &["$defs", "DataRequest", "properties"]
    )));
    assert!(keys(&bar["backtest"]).is_subset(&properties(
        &bar_schema,
        &["$defs", "BacktestSpec", "properties"]
    )));
    assert_eq!(bar["data_request"]["kind"], "Bar");

    let depth_schema: serde_json::Value = serde_json::from_str(include_str!(
        "../../../schemas/provider-depth-backtest-spec-v1.json"
    ))
    .unwrap();
    let depth_root = std::path::Path::new("provider-schema-contract");
    let depth = serde_json::to_value(provider_depth_spec(depth_root)).unwrap();
    assert!(keys(&depth).is_subset(&properties(&depth_schema, &["properties"])));
    assert!(keys(&depth["data_request"]).is_subset(&properties(
        &depth_schema,
        &["$defs", "DataRequest", "properties"]
    )));
    assert!(keys(&depth["backtest"]).is_subset(&properties(
        &depth_schema,
        &["$defs", "DepthBacktestSpec", "properties"]
    )));
    assert_eq!(depth["data_request"]["kind"], "Quote");
    assert_eq!(depth["data_request"]["frequency"], "tick");
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
fn provider_depth_backtest_http_route_rejects_unknown_schema_before_provider_access() {
    let path = workspace_root().join("target").join(format!(
        "qx-api-provider-depth-schema-{}",
        std::process::id()
    ));
    let mut spec = provider_depth_spec(&path);
    spec.schema_version += 1;
    let service = ApiService::new(ApiState::default())
        .with_provider_registry(Arc::new(ProviderRegistry::new()));

    let response = service.handle(
        "POST",
        "/app/provider-depth-backtest",
        &serde_json::to_string(&spec).unwrap(),
        1,
    );
    assert_eq!(response.status, 400, "{}", response.body);
    assert!(response.body.contains("schema_version"));
    assert!(response.body.contains("expected 1"));
    assert!(!response.body.contains("no Provider satisfies"));
}

#[test]
fn provider_depth_backtest_http_route_runs_freezes_and_returns_artifacts() {
    let path = workspace_root().join("target").join(format!(
        "qx-api-provider-depth-route-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).unwrap();
    let mut service = ApiService::new(ApiState::default())
        .with_provider_registry(Arc::new(quote_provider_registry()));
    service.app_paths.artifact_root = path.clone();

    let response = service.handle(
        "POST",
        "/app/provider-depth-backtest",
        &serde_json::to_string(&provider_depth_spec(&path)).unwrap(),
        1,
    );
    assert_eq!(response.status, 200, "{}", response.body);
    let outcome: ProviderDepthBacktestOutcome = serde_json::from_str(&response.body).unwrap();
    assert_eq!(outcome.provider_id, "quote-fixture");
    assert_eq!(outcome.provider_version, "v1");
    assert!(PathBuf::from(outcome.depth_path).is_file());
    assert!(PathBuf::from(outcome.outcome.artifacts.run_manifest).is_file());
    assert!(PathBuf::from(outcome.outcome.artifacts.summary).is_file());
    assert!(PathBuf::from(outcome.outcome.artifacts.equity).is_file());
    assert!(PathBuf::from(outcome.outcome.artifacts.fills).is_file());
    let _ = fs::remove_dir_all(path);
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

#[test]
fn capabilities_route_includes_configured_provider_declarations() {
    let without_registry =
        ApiService::new(ApiState::default()).handle("GET", "/app/capabilities", "", 1);
    assert_eq!(without_registry.status, 200, "{}", without_registry.body);
    let empty: serde_json::Value = serde_json::from_str(&without_registry.body).unwrap();
    assert_eq!(empty["provider_capabilities"], serde_json::json!([]));

    let service =
        ApiService::new(ApiState::default()).with_provider_registry(Arc::new(provider_registry()));
    let response = service.handle("GET", "/app/capabilities", "", 2);
    assert_eq!(response.status, 200, "{}", response.body);
    let document: serde_json::Value = serde_json::from_str(&response.body).unwrap();
    assert_eq!(document["schema_version"], 1);
    assert!(document["capabilities"]
        .as_array()
        .unwrap()
        .iter()
        .any(|entry| {
            entry["id"] == "data.provider_preflight"
                && entry["routes"]
                    .as_array()
                    .unwrap()
                    .contains(&serde_json::json!("/app/capabilities/providers/preflight"))
        }));
    assert_eq!(
        document["provider_capabilities"].as_array().unwrap().len(),
        1
    );
    assert_eq!(
        document["provider_capabilities"][0]["provider_id"],
        "fixture"
    );
    assert_eq!(document["provider_capabilities"][0]["version"], "v1");
}

#[test]
fn provider_preflight_matches_requests_without_fetching_data() {
    let request = provider_spec().data_request.to_json().unwrap();
    let unconfigured = ApiService::new(ApiState::default()).handle(
        "POST",
        "/app/capabilities/providers/preflight",
        &request,
        1,
    );
    assert_eq!(unconfigured.status, 503, "{}", unconfigured.body);

    let service =
        ApiService::new(ApiState::default()).with_provider_registry(Arc::new(provider_registry()));
    let response = service.handle("POST", "/app/capabilities/providers/preflight", &request, 2);
    assert_eq!(response.status, 200, "{}", response.body);
    let matched: serde_json::Value = serde_json::from_str(&response.body).unwrap();
    assert_eq!(matched["schema_version"], 1);
    assert_eq!(matched["matched"], true);
    assert_eq!(matched["candidates"][0]["provider_id"], "fixture");
    assert_eq!(matched["request_fingerprint"].as_str().unwrap().len(), 16);

    let unsupported_request = DataRequest::new(
        DataQuery {
            kind: DataKind::Quote,
            frequency: "tick".into(),
            ..provider_spec().data_request.query
        },
        DataRequirements::default(),
    )
    .to_json()
    .unwrap();
    let unsupported = service.handle(
        "POST",
        "/app/capabilities/providers/preflight",
        &unsupported_request,
        3,
    );
    assert_eq!(unsupported.status, 200, "{}", unsupported.body);
    let unsupported: serde_json::Value = serde_json::from_str(&unsupported.body).unwrap();
    assert_eq!(unsupported["matched"], false);
    assert_eq!(unsupported["candidates"], serde_json::json!([]));

    let invalid = service.handle("POST", "/app/capabilities/providers/preflight", "{}", 4);
    assert_eq!(invalid.status, 400, "{}", invalid.body);
}
