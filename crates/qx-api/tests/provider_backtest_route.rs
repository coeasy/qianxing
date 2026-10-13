use qx_api::{ApiService, ApiState};
use qx_app::{BacktestSpec, BuiltinStrategySpec, ProviderBacktestSpec};
use qx_provider::{DataKind, DataQuery, DataRequest, DataRequirements, ProviderRegistry};
use qx_strategy::BuiltinStrategyKind;
use std::collections::BTreeSet;
use std::sync::Arc;

#[test]
fn provider_backtest_http_route_requires_and_uses_injected_registry() {
    let spec = ProviderBacktestSpec {
        data_request: DataRequest::new(
            DataQuery {
                kind: DataKind::Bar,
                asset_class: "crypto".into(),
                instrument_set: BTreeSet::from(["BTCUSDT.BINANCE".into()]),
                field_set: BTreeSet::from(["close".into()]),
                frequency: "1m".into(),
                adjustment: "none".into(),
                quality_policy: "strict".into(),
                start: 1,
                end: 10,
                as_of: Some(10),
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
    };
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
