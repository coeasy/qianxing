//! Build the API-owned Provider registry from strict runtime configuration.

use super::super::*;
use std::collections::BTreeSet;
use std::sync::Arc;

pub(super) fn from_runtime_config(
    config: &RuntimeConfig,
    runtime_config_path: &Path,
) -> Result<Option<Arc<ProviderRegistry>>, String> {
    from_sources(
        &config.api.data_providers,
        runtime_config_path,
        |source, capability, runtime_config_path| {
            let provider: Box<dyn DataProvider> = match &source.adapter {
                ApiDataProviderAdapterConfig::Ccxt { config_path } => {
                    let path = resolve_ccxt_config_path(runtime_config_path, config_path);
                    Box::new(
                        CcxtBarDataProvider::spawn(capability, &python_interpreter(), &path, None)
                            .map_err(|error| {
                                format!(
                                    "启动 API CCXT Provider {} 失败: {error}",
                                    source.provider_id
                                )
                            })?,
                    )
                }
            };
            Ok(provider)
        },
    )
}

fn from_sources<F>(
    sources: &[ApiDataProviderRuntimeConfig],
    runtime_config_path: &Path,
    mut create_provider: F,
) -> Result<Option<Arc<ProviderRegistry>>, String>
where
    F: FnMut(
        &ApiDataProviderRuntimeConfig,
        ProviderCapability,
        &Path,
    ) -> Result<Box<dyn DataProvider>, String>,
{
    if sources.is_empty() {
        return Ok(None);
    }
    let mut registry = ProviderRegistry::new();
    for source in sources {
        let capability = capability_from_config(source)?;
        let provider = create_provider(source, capability, runtime_config_path)?;
        registry.register(provider).map_err(|error| {
            format!(
                "注册 API data Provider {} 失败: {error:?}",
                source.provider_id
            )
        })?;
    }
    Ok(Some(Arc::new(registry)))
}

fn capability_from_config(
    source: &ApiDataProviderRuntimeConfig,
) -> Result<ProviderCapability, String> {
    let data_kinds = source
        .data_kinds
        .iter()
        .map(|kind| match kind.as_str() {
            "Bar" => Ok(DataKind::Bar),
            _ => Err(format!(
                "Provider {} 声明了不支持的数据类型 {kind:?}",
                source.provider_id
            )),
        })
        .collect::<Result<BTreeSet<_>, _>>()?;
    Ok(ProviderCapability {
        provider_id: source.provider_id.clone(),
        version: source.version.clone(),
        data_kinds,
        asset_classes: source.asset_classes.iter().cloned().collect(),
        frequencies: source.frequencies.iter().cloned().collect(),
        auth_scope: source.auth_scope.clone(),
        rate_limit_per_second: source.rate_limit_per_second,
        freshness_seconds: source.freshness_seconds,
        historical_start: source.historical_start,
        historical_end: source.historical_end,
        realtime: source.realtime,
        priority: source.priority,
        quality_score: source.quality_score,
        cost_score: source.cost_score,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use qx_provider::{DataRequest, DataRequirements, ProviderPayload};
    use serde_json::json;

    struct FixtureProvider(ProviderCapability);

    impl DataProvider for FixtureProvider {
        fn capability(&self) -> &ProviderCapability {
            &self.0
        }

        fn fetch(&self, query: &DataQuery) -> Result<ProviderResult, ProviderError> {
            ProviderResult::from_payloads(
                self.0.provider_id.clone(),
                self.0.version.clone(),
                "configured-provider-fixture",
                62_000,
                vec![(
                    query.start,
                    62_000,
                    ProviderPayload::new_json(
                        "BTCUSDT.BINANCE",
                        "application/vnd.qianxing.bar+json;v=1",
                        json!({"ts": query.start, "open": 100, "high": 102, "low": 98, "close": 101, "volume": 5}).to_string(),
                    ),
                )],
            )
        }
    }

    fn source() -> ApiDataProviderRuntimeConfig {
        ApiDataProviderRuntimeConfig {
            provider_id: "ccxt:fixture".into(),
            version: "v1".into(),
            adapter: ApiDataProviderAdapterConfig::Ccxt {
                config_path: "exchange.json".into(),
            },
            data_kinds: vec!["Bar".into()],
            asset_classes: vec!["crypto".into()],
            frequencies: vec!["1m".into()],
            auth_scope: "public".into(),
            rate_limit_per_second: 5,
            freshness_seconds: 60,
            historical_start: 0,
            historical_end: 100_000,
            realtime: false,
            priority: 0,
            quality_score: 80,
            cost_score: 1,
        }
    }

    #[test]
    fn configured_provider_capability_is_registered_in_the_shared_rust_registry() {
        let sources = [source()];
        let registry = from_sources(&sources, Path::new("runtime.json"), |_, capability, _| {
            Ok(Box::new(FixtureProvider(capability)))
        })
        .unwrap()
        .unwrap();
        let request = DataRequest::new(
            DataQuery {
                kind: DataKind::Bar,
                asset_class: "crypto".into(),
                instrument_set: BTreeSet::from(["BTCUSDT.BINANCE".into()]),
                field_set: BTreeSet::new(),
                frequency: "1m".into(),
                adjustment: "none".into(),
                quality_policy: "strict".into(),
                start: 60_000,
                end: 61_000,
                as_of: None,
            },
            DataRequirements::default(),
        );
        let frame = registry.fetch_bar_frame(&request).unwrap();
        assert_eq!(frame.result.provider_id, "ccxt:fixture");
        assert_eq!(frame.frame.close_raw, [101]);
    }

    #[test]
    fn no_configured_data_sources_leave_provider_registry_unset() {
        let registry = from_sources(&[], Path::new("runtime.json"), |_, _, _| {
            unreachable!("factory must not run for an empty source list")
        })
        .unwrap();
        assert!(registry.is_none());
    }
}
