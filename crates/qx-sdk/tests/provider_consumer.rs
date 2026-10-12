//! 外部消费者通过 `qx-sdk` 走 Rust 共享 Provider 注册与确定性 failover。

use qx_sdk::{
    DataKind, DataProvider, DataQuery, DataSourceId, ProviderCapability, ProviderError,
    ProviderErrorClass, ProviderRegistry, ProviderResult, RawRecord,
};
use std::collections::BTreeSet;

fn capability(provider_id: &str, priority: u32) -> ProviderCapability {
    ProviderCapability {
        provider_id: provider_id.into(),
        version: "v1".into(),
        data_kinds: [DataKind::Bar].into_iter().collect(),
        asset_classes: ["crypto".into()].into_iter().collect(),
        frequencies: ["1m".into()].into_iter().collect(),
        auth_scope: "public".into(),
        rate_limit_per_second: 10,
        freshness_seconds: 60,
        historical_start: 0,
        historical_end: 100,
        realtime: false,
        priority,
        quality_score: 100,
        cost_score: 1,
    }
}

mod tests {
    use super::*;

    struct FixtureProvider {
        capability: ProviderCapability,
        fail: bool,
    }

    impl DataProvider for FixtureProvider {
        fn capability(&self) -> &ProviderCapability {
            &self.capability
        }

        fn fetch(&self, query: &DataQuery) -> Result<ProviderResult, ProviderError> {
            if self.fail {
                return Err(ProviderError::new(
                    ProviderErrorClass::SwitchProvider,
                    "fixture failover",
                ));
            }
            let mut result = ProviderResult {
                records: vec![RawRecord {
                    source: DataSourceId::new(self.capability.provider_id.clone()),
                    event_time: 10,
                    receive_time: 11,
                    payload_hash: 12,
                    schema_version: 1,
                }],
                provider_id: self.capability.provider_id.clone(),
                provider_version: self.capability.version.clone(),
                request_id: format!("{}-{:016x}", self.capability.provider_id, query.digest()),
                retry_chain: Vec::new(),
                received_at: 11,
                source_hash: 0,
            };
            result.source_hash = result.compute_source_hash();
            Ok(result)
        }
    }

    #[test]
    fn sdk_provider_registry_fails_over_in_stable_order_and_preserves_lineage() {
        let mut registry = ProviderRegistry::new();
        registry
            .register(Box::new(FixtureProvider {
                capability: capability("backup", 2),
                fail: false,
            }))
            .unwrap();
        registry
            .register(Box::new(FixtureProvider {
                capability: capability("primary", 1),
                fail: true,
            }))
            .unwrap();
        let query = DataQuery {
            kind: DataKind::Bar,
            asset_class: "crypto".into(),
            instrument_set: BTreeSet::from(["BTCUSDT.BINANCE".into()]),
            field_set: BTreeSet::from(["close".into()]),
            frequency: "1m".into(),
            adjustment: "none".into(),
            quality_policy: "strict".into(),
            start: 1,
            end: 20,
            as_of: Some(12),
        };

        assert_eq!(registry.candidates(&query).unwrap(), ["primary", "backup"]);
        let result = registry.fetch_with_failover(&query).unwrap();
        assert_eq!(result.provider_id, "backup");
        assert_eq!(result.retry_chain, ["primary", "backup"]);
        assert_eq!(result.records[0].source, DataSourceId::new("backup"));
        result.validate_for(&query).unwrap();
    }
}
