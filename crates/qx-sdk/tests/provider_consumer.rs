//! 外部消费者通过 `qx-sdk` 走 Rust 共享 Provider 注册与确定性 failover。

use qx_sdk::{
    BacktestSpec, BuiltinStrategyKind, BuiltinStrategySpec, CallerCapability, Client, DataKind,
    DataProvider, DataQuery, DataRequest, DataRequirements, DataSourceId, ProviderCapability,
    ProviderError, ProviderErrorClass, ProviderPayload, ProviderRegistry, ProviderResult,
    RawRecord,
};
use std::collections::BTreeSet;
use std::path::PathBuf;

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

    struct TestDirectory(PathBuf);

    impl TestDirectory {
        fn new(label: &str) -> Self {
            let path = std::env::current_dir()
                .unwrap()
                .join("target")
                .join(format!("{label}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TestDirectory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    struct FixtureProvider {
        capability: ProviderCapability,
        fail: bool,
        bars: Vec<(u64, i128)>,
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
            let payloads = self
                .bars
                .iter()
                .map(|(ts, close)| {
                    ProviderPayload::new_json(
                        "BTCUSDT.BINANCE",
                        "application/vnd.qianxing.bar+json;v=1",
                        format!(
                            r#"{{"ts":{ts},"open":{},"high":{},"low":{},"close":{close},"volume":1000}}"#,
                            close - 1_000_000_000,
                            close + 2_000_000_000,
                            close - 2_000_000_000,
                        ),
                    )
                })
                .collect::<Vec<_>>();
            let records = self
                .bars
                .iter()
                .zip(&payloads)
                .map(|((ts, _), payload)| RawRecord {
                    source: DataSourceId::new(self.capability.provider_id.clone()),
                    event_time: *ts,
                    receive_time: ts + 1,
                    payload_hash: payload.digest(),
                    schema_version: 1,
                })
                .collect::<Vec<_>>();
            let mut result = ProviderResult {
                schema_version: qx_sdk::PROVIDER_RESULT_SCHEMA_VERSION,
                records,
                payloads,
                provider_id: self.capability.provider_id.clone(),
                provider_version: self.capability.version.clone(),
                request_id: format!("{}-{:016x}", self.capability.provider_id, query.digest()),
                retry_chain: Vec::new(),
                received_at: self
                    .bars
                    .last()
                    .map(|(timestamp, _)| timestamp + 1)
                    .unwrap_or(11),
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
                bars: vec![(10, 100_000_000_000)],
            }))
            .unwrap();
        registry
            .register(Box::new(FixtureProvider {
                capability: capability("primary", 1),
                fail: true,
                bars: vec![(10, 100_000_000_000)],
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
            end: 100,
            as_of: Some(12),
        };

        let request = DataRequest::new(
            query.clone(),
            DataRequirements {
                required_auth_scope: Some("public".into()),
                require_realtime: false,
                max_staleness_ms: None,
            },
        );
        assert_eq!(
            registry.candidates_for_request(&request).unwrap(),
            ["primary", "backup"]
        );
        let fetched = registry.fetch_bar_frame(&request).unwrap();
        let result = fetched.result;
        assert_eq!(result.provider_id, "backup");
        assert_eq!(result.retry_chain, ["primary", "backup"]);
        assert_eq!(result.records[0].source, DataSourceId::new("backup"));
        result.validate_for(&query).unwrap();
        let frame = fetched.frame;
        assert_eq!(frame.instrument.to_string(), "BTCUSDT.BINANCE");
        assert_eq!(
            frame.source.0,
            format!(
                "backup@v1#query={:016x}#source={:016x}",
                query.digest(),
                result.source_hash
            )
        );
        assert_eq!(frame.ts, [10]);
        assert_eq!(frame.close_raw, [100_000_000_000]);
    }
    #[test]
    fn third_party_provider_payload_reaches_the_shared_bar_backtest_and_verifier() {
        let bars = (1..=90_u64)
            .map(|ts| {
                let phase = (ts * 7 % 19) as i128 - 9;
                (ts, 100_000_000_000 + phase * 1_000_000_000)
            })
            .collect::<Vec<_>>();
        let mut registry = ProviderRegistry::new();
        registry
            .register(Box::new(FixtureProvider {
                capability: capability("fixture-bars", 1),
                fail: false,
                bars,
            }))
            .unwrap();
        let query = DataQuery {
            kind: DataKind::Bar,
            asset_class: "crypto".into(),
            instrument_set: BTreeSet::from(["BTCUSDT.BINANCE".into()]),
            field_set: BTreeSet::from([
                "open".into(),
                "high".into(),
                "low".into(),
                "close".into(),
                "volume".into(),
            ]),
            frequency: "1m".into(),
            adjustment: "none".into(),
            quality_policy: "strict".into(),
            start: 1,
            end: 90,
            as_of: Some(100),
        };
        let fetched = registry
            .fetch_bar_frame(&DataRequest::new(
                query.clone(),
                DataRequirements::default(),
            ))
            .unwrap();
        let manifest = fetched
            .dataset_manifest("provider-bars", "snapshot-1", 1, "UTC")
            .unwrap();
        assert_eq!(manifest.source_lineage.primary_source, "fixture-bars");
        assert_eq!(manifest.provider_version, "v1");
        let result = fetched.result.clone();
        let frame = fetched.frame.clone();
        let root = TestDirectory::new("qx-sdk-provider-backtest");
        let bars_path = root.0.join("provider-bars.json");

        let mut strategy = BuiltinStrategySpec::new(BuiltinStrategyKind::SmaCross, "provider-sma");
        strategy.fast_window = 2;
        strategy.slow_window = 3;
        let output_dir = root.0.join("run");
        let spec = BacktestSpec::new(
            "provider-bars-run",
            "BTCUSDT.BINANCE",
            bars_path.to_string_lossy(),
            "USDT",
            100_000 * 1_000_000_000,
            20261012,
            output_dir.to_string_lossy(),
            strategy,
        );
        let client = Client::new(CallerCapability::Research, "provider-bars-run");
        let provider_run = client
            .run_provider_backtest_with_manifest(&qx_sdk::ProviderBacktestRequest {
                fetched: &fetched,
                spec: &spec,
                dataset_id: "provider-bars",
                dataset_version: "snapshot-1",
                interval_ms: 1,
                timezone: "UTC",
            })
            .unwrap();
        assert!(provider_run.dataset_verdict.usable);
        assert_eq!(provider_run.manifest, manifest);
        let frozen_manifest: qx_sdk::DatasetManifestV2 =
            serde_json::from_slice(&std::fs::read(provider_run.manifest_path).unwrap()).unwrap();
        assert_eq!(frozen_manifest, manifest);
        let outcome = provider_run.outcome;
        assert_eq!(
            outcome.data_fingerprint,
            format!("barframe:{:016x}", frame.digest())
        );
        assert!(
            outcome.fills > 0,
            "provider data must reach the Rust strategy and matching kernel"
        );
        let verified = client.verify_run(&outcome).unwrap();
        assert!(
            verified.verified,
            "provider-sourced backtest artifacts must replay"
        );

        let mut invalid = result;
        invalid.payloads[0] = ProviderPayload::new_json(
            "BTCUSDT.BINANCE",
            "application/vnd.qianxing.bar+json;v=1",
            r#"{"ts":1,"open":100000000000,"high":99000000000,"low":100000000000,"close":100000000000,"volume":1000}"#,
        );
        invalid.records[0].payload_hash = invalid.payloads[0].digest();
        invalid.source_hash = invalid.compute_source_hash();
        assert!(
            invalid.to_bar_frame(&query).is_err(),
            "strict provider normalization must reject invalid OHLCV"
        );
    }
}
