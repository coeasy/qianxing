mod tests {
    use qx_provider::{
        DataKind, DataProvider, DataQuery, DataSourceId, ProviderCapability, ProviderError,
        ProviderPayload, ProviderRegistry, ProviderResult, RawRecord,
    };
    use std::collections::BTreeSet;

    struct SpoofingProvider {
        capability: ProviderCapability,
    }

    impl DataProvider for SpoofingProvider {
        fn capability(&self) -> &ProviderCapability {
            &self.capability
        }

        fn fetch(&self, _query: &DataQuery) -> Result<ProviderResult, ProviderError> {
            let payload = ProviderPayload::new_json(
                "BTCUSDT.BINANCE",
                "application/vnd.qianxing.bar+json;v=1",
                r#"{"ts":1,"open":9,"high":12,"low":8,"close":10,"volume":100}"#,
            );
            let mut result = ProviderResult {
                schema_version: qx_provider::PROVIDER_RESULT_SCHEMA_VERSION,
                records: vec![RawRecord {
                    source: DataSourceId::new("backup"),
                    event_time: 1,
                    receive_time: 2,
                    payload_hash: payload.digest(),
                    schema_version: 1,
                }],
                payloads: vec![payload],
                provider_id: "backup".into(),
                provider_version: "v1".into(),
                request_id: "spoofed-primary-request".into(),
                retry_chain: Vec::new(),
                received_at: 2,
                source_hash: 0,
            };
            result.source_hash = result.compute_source_hash();
            Ok(result)
        }
    }

    struct HonestProvider {
        capability: ProviderCapability,
    }

    impl DataProvider for HonestProvider {
        fn capability(&self) -> &ProviderCapability {
            &self.capability
        }

        fn fetch(&self, _query: &DataQuery) -> Result<ProviderResult, ProviderError> {
            let payload = ProviderPayload::new_json(
                "BTCUSDT.BINANCE",
                "application/vnd.qianxing.bar+json;v=1",
                r#"{"ts":1,"open":9,"high":12,"low":8,"close":10,"volume":100}"#,
            );
            let mut result = ProviderResult {
                schema_version: qx_provider::PROVIDER_RESULT_SCHEMA_VERSION,
                records: vec![RawRecord {
                    source: DataSourceId::new(self.capability.provider_id.clone()),
                    event_time: 1,
                    receive_time: 2,
                    payload_hash: payload.digest(),
                    schema_version: 1,
                }],
                payloads: vec![payload],
                provider_id: self.capability.provider_id.clone(),
                provider_version: self.capability.version.clone(),
                request_id: "backup-request".into(),
                retry_chain: Vec::new(),
                received_at: 2,
                source_hash: 0,
            };
            result.source_hash = result.compute_source_hash();
            Ok(result)
        }
    }

    fn capability(id: &str, priority: u32) -> ProviderCapability {
        ProviderCapability {
            provider_id: id.into(),
            version: "v1".into(),
            data_kinds: [DataKind::Bar].into_iter().collect(),
            data_tiers: [qx_provider::MarketDataTier::Bar].into_iter().collect(),
            asset_classes: ["crypto".into()].into_iter().collect(),
            frequencies: ["1d".into()].into_iter().collect(),
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

    #[test]
    fn provider_identity_is_bound_to_invoked_port_before_failover_result_is_accepted() {
        let query = DataQuery {
            kind: DataKind::Bar,
            asset_class: "crypto".into(),
            instrument_set: BTreeSet::new(),
            field_set: BTreeSet::new(),
            frequency: "1d".into(),
            adjustment: "none".into(),
            quality_policy: "strict".into(),
            start: 1,
            end: 10,
            as_of: None,
        };
        let mut registry = ProviderRegistry::new();
        registry
            .register(Box::new(SpoofingProvider {
                capability: capability("primary", 1),
            }))
            .unwrap();
        registry
            .register(Box::new(HonestProvider {
                capability: capability("backup", 2),
            }))
            .unwrap();

        let result = registry.fetch_with_failover(&query).unwrap();
        assert_eq!(result.provider_id, "backup");
        assert_eq!(result.retry_chain, ["primary", "backup"]);
        assert!(result.payloads[0].json.contains("\"close\":10"));
    }

    #[test]
    fn registry_bar_frame_keeps_validated_result_and_selected_provider_lineage_together() {
        let query = DataQuery {
            kind: DataKind::Bar,
            asset_class: "crypto".into(),
            instrument_set: ["BTCUSDT.BINANCE".into()].into_iter().collect(),
            field_set: BTreeSet::new(),
            frequency: "1d".into(),
            adjustment: "none".into(),
            quality_policy: "strict".into(),
            start: 1,
            end: 1,
            as_of: None,
        };
        let mut registry = ProviderRegistry::new();
        registry
            .register(Box::new(HonestProvider {
                capability: capability("primary", 1),
            }))
            .unwrap();

        let fetched = registry
            .fetch_bar_frame(&qx_provider::DataRequest::new(
                query,
                qx_provider::DataRequirements::default(),
            ))
            .unwrap();

        assert_eq!(fetched.result.provider_id, "primary");
        assert_eq!(fetched.result.retry_chain, ["primary"]);
        assert_eq!(fetched.frame.ts, [1]);
        assert_eq!(fetched.frame.close_raw, [10]);
        assert!(fetched.frame.source.0.contains("primary@v1"));
        assert!(fetched.frame.source.0.contains("#source="));
    }
}
