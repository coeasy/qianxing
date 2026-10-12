//! 多源数据 Provider 边界。
//!
//! Provider 只负责把外部数据带着血缘送入 Raw 层；它不能直接写入账簿、标准化目录
//! 或策略缓存。ProviderRegistry 的选择顺序固定，保证主备切换可解释、可重放。

use qx_core::Fnv1a;
pub use qx_guanxing::{DataSourceId, RawRecord};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

mod registry;
mod result;
pub use registry::{ProviderRegistry, RegistryError};
pub use result::{DataProvider, ProviderPayload, ProviderResult, PROVIDER_RESULT_SCHEMA_VERSION};

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub enum DataKind {
    Bar,
    Quote,
    Trade,
    Financial,
    Instrument,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct DataQuery {
    pub kind: DataKind,
    pub asset_class: String,
    #[serde(default)]
    pub instrument_set: BTreeSet<String>,
    #[serde(default)]
    pub field_set: BTreeSet<String>,
    pub frequency: String,
    #[serde(default = "default_adjustment")]
    pub adjustment: String,
    #[serde(default = "default_quality_policy")]
    pub quality_policy: String,
    pub start: u64,
    pub end: u64,
    pub as_of: Option<u64>,
}

fn default_adjustment() -> String {
    "none".into()
}

fn default_quality_policy() -> String {
    "strict".into()
}

impl DataQuery {
    pub fn to_json(&self) -> Result<String, ProviderError> {
        self.validate()?;
        serde_json::to_string(self)
            .map_err(|error| ProviderError::new(ProviderErrorClass::Permanent, error.to_string()))
    }

    pub fn from_json(input: &str) -> Result<Self, ProviderError> {
        let query: Self = serde_json::from_str(input).map_err(|error| {
            ProviderError::new(
                ProviderErrorClass::Permanent,
                format!("DataQuery JSON 无法解析: {error}"),
            )
        })?;
        query.validate()?;
        Ok(query)
    }

    pub fn digest(&self) -> u64 {
        let mut hash = Fnv1a::new();
        hash.write_u64(match self.kind {
            DataKind::Bar => 0,
            DataKind::Quote => 1,
            DataKind::Trade => 2,
            DataKind::Financial => 3,
            DataKind::Instrument => 4,
        });
        hash.write_text(&self.asset_class);
        for value in &self.instrument_set {
            hash.write_text(value);
        }
        for value in &self.field_set {
            hash.write_text(value);
        }
        hash.write_text(&self.frequency);
        hash.write_text(&self.adjustment);
        hash.write_text(&self.quality_policy);
        hash.write_u64(self.start);
        hash.write_u64(self.end);
        hash.write_u64(self.as_of.unwrap_or(u64::MAX));
        hash.finish()
    }
}

impl DataQuery {
    pub fn validate(&self) -> Result<(), ProviderError> {
        if self.asset_class.trim().is_empty()
            || self.frequency.trim().is_empty()
            || self.adjustment.trim().is_empty()
            || self.quality_policy.trim().is_empty()
            || self.start > self.end
            || self
                .instrument_set
                .iter()
                .any(|value| value.trim().is_empty())
            || self.field_set.iter().any(|value| value.trim().is_empty())
        {
            return Err(ProviderError::new(
                ProviderErrorClass::Permanent,
                "数据查询参数非法",
            ));
        }
        Ok(())
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ProviderCapability {
    pub provider_id: String,
    pub version: String,
    pub data_kinds: BTreeSet<DataKind>,
    pub asset_classes: BTreeSet<String>,
    pub frequencies: BTreeSet<String>,
    pub auth_scope: String,
    pub rate_limit_per_second: u32,
    pub freshness_seconds: u64,
    pub historical_start: u64,
    pub historical_end: u64,
    pub realtime: bool,
    pub priority: u32,
    pub quality_score: u32,
    pub cost_score: u32,
}

/// A data-bearing row paired by index with one `RawRecord` lineage envelope.
/// JSON remains the wire representation so provider adapters can preserve the source schema.
/// 可持久化前的确定性本地 Provider：按事件时间切片并保留原始血缘。
///
/// 它不是把数据“灌入”标准化表的快捷方式；返回值仍然是 RawRecord，调用方必须
/// 经过质量门和版本化标准化流程后才能进入研究或交易视图。
pub struct InMemoryProvider {
    capability: ProviderCapability,
    records: Vec<(RawRecord, ProviderPayload)>,
}

impl InMemoryProvider {
    pub fn new(
        capability: ProviderCapability,
        mut records: Vec<(RawRecord, ProviderPayload)>,
    ) -> Self {
        records.sort_by_key(|(record, _)| {
            (record.event_time, record.receive_time, record.payload_hash)
        });
        Self {
            capability,
            records,
        }
    }
}

impl DataProvider for InMemoryProvider {
    fn capability(&self) -> &ProviderCapability {
        &self.capability
    }

    fn fetch(&self, query: &DataQuery) -> Result<ProviderResult, ProviderError> {
        query.validate()?;
        let records = self
            .records
            .iter()
            .filter(|(record, payload)| {
                record.event_time >= query.start
                    && record.event_time <= query.end
                    && query.as_of.is_none_or(|as_of| record.receive_time <= as_of)
                    && (query.instrument_set.is_empty()
                        || query.instrument_set.contains(&payload.instrument))
            })
            .cloned()
            .collect::<Vec<_>>();
        let mut result = ProviderResult {
            schema_version: PROVIDER_RESULT_SCHEMA_VERSION,
            records: records.iter().map(|(record, _)| record.clone()).collect(),
            payloads: records.into_iter().map(|(_, payload)| payload).collect(),
            provider_id: self.capability.provider_id.clone(),
            provider_version: self.capability.version.clone(),
            request_id: format!("{}-{:016x}", self.capability.provider_id, query.digest()),
            retry_chain: Vec::new(),
            received_at: query.end,
            source_hash: 0,
        };
        result.source_hash = result.compute_source_hash();
        Ok(result)
    }
}

/// failover 只认这三档；从不被生产的 `Retryable` 已删（V12 §23 给出为什么不构成缺口）。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum ProviderErrorClass {
    SwitchProvider,
    ManualIntervention,
    Permanent,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ProviderError {
    pub class: ProviderErrorClass,
    pub message: String,
}

impl ProviderError {
    pub fn new(class: ProviderErrorClass, message: impl Into<String>) -> Self {
        Self {
            class,
            message: message.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qx_guanxing::DataSourceId;

    struct MockProvider {
        capability: ProviderCapability,
        fail: Option<ProviderErrorClass>,
    }

    impl DataProvider for MockProvider {
        fn capability(&self) -> &ProviderCapability {
            &self.capability
        }

        fn fetch(&self, _query: &DataQuery) -> Result<ProviderResult, ProviderError> {
            if let Some(class) = self.fail {
                return Err(ProviderError::new(class, "mock failure"));
            }
            let (record, payload) = row(&self.capability.provider_id, 1, 2, 10);
            let mut result = ProviderResult {
                schema_version: PROVIDER_RESULT_SCHEMA_VERSION,
                records: vec![record],
                payloads: vec![payload],
                provider_id: self.capability.provider_id.clone(),
                provider_version: self.capability.version.clone(),
                request_id: "req-1".into(),
                retry_chain: Vec::new(),
                received_at: 2,
                source_hash: 0,
            };
            result.source_hash = result.compute_source_hash();
            Ok(result)
        }
    }

    fn capability(id: &str, priority: u32, quality_score: u32) -> ProviderCapability {
        ProviderCapability {
            provider_id: id.into(),
            version: "v1".into(),
            data_kinds: [DataKind::Bar].into_iter().collect(),
            asset_classes: ["crypto".into()].into_iter().collect(),
            frequencies: ["1d".into()].into_iter().collect(),
            auth_scope: "public".into(),
            rate_limit_per_second: 10,
            freshness_seconds: 60,
            historical_start: 0,
            historical_end: 100,
            realtime: false,
            priority,
            quality_score,
            cost_score: 1,
        }
    }

    fn query() -> DataQuery {
        DataQuery {
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
        }
    }

    fn row(
        source: &str,
        event_time: u64,
        receive_time: u64,
        close: i128,
    ) -> (RawRecord, ProviderPayload) {
        let payload = ProviderPayload::new_json(
            "BTCUSDT.BINANCE",
            "application/vnd.qianxing.bar+json;v=1",
            serde_json::to_string(&qx_guanxing::Bar::new(
                event_time,
                close - 1,
                close + 2,
                close - 2,
                close,
                100,
            ))
            .unwrap(),
        );
        let record = RawRecord {
            source: DataSourceId::new(source),
            event_time,
            receive_time,
            payload_hash: payload.digest(),
            schema_version: 1,
        };
        (record, payload)
    }

    #[test]
    fn selection_is_stable_and_explainable() {
        let mut registry = ProviderRegistry::new();
        registry
            .register(Box::new(MockProvider {
                capability: capability("backup", 2, 100),
                fail: None,
            }))
            .unwrap();
        registry
            .register(Box::new(MockProvider {
                capability: capability("primary", 1, 80),
                fail: None,
            }))
            .unwrap();
        assert_eq!(
            registry.candidates(&query()).unwrap(),
            ["primary", "backup"]
        );
    }

    #[test]
    fn registry_rejects_incomplete_capability_declarations() {
        let mut invalid = capability("invalid", 1, 100);
        invalid.version.clear();
        let mut registry = ProviderRegistry::new();
        assert!(matches!(
            registry.register(Box::new(MockProvider {
                capability: invalid,
                fail: None,
            })),
            Err(RegistryError::Provider(_))
        ));
    }

    #[test]
    fn failover_keeps_retry_chain() {
        let mut registry = ProviderRegistry::new();
        registry
            .register(Box::new(MockProvider {
                capability: capability("primary", 1, 100),
                fail: Some(ProviderErrorClass::SwitchProvider),
            }))
            .unwrap();
        registry
            .register(Box::new(MockProvider {
                capability: capability("backup", 2, 90),
                fail: None,
            }))
            .unwrap();
        let result = registry.fetch_with_failover(&query()).unwrap();
        assert_eq!(result.provider_id, "backup");
        assert_eq!(result.retry_chain, ["primary", "backup"]);
    }

    #[test]
    fn manual_intervention_stops_automatic_failover() {
        let mut registry = ProviderRegistry::new();
        registry
            .register(Box::new(MockProvider {
                capability: capability("primary", 1, 100),
                fail: Some(ProviderErrorClass::ManualIntervention),
            }))
            .unwrap();
        registry
            .register(Box::new(MockProvider {
                capability: capability("backup", 2, 90),
                fail: None,
            }))
            .unwrap();
        assert!(matches!(
            registry.fetch_with_failover(&query()),
            Err(RegistryError::Provider(ProviderError {
                class: ProviderErrorClass::ManualIntervention,
                ..
            }))
        ));
    }

    #[test]
    fn in_memory_provider_applies_as_of_cutoff() {
        let records = vec![row("local", 1, 10, 10), row("local", 2, 20, 11)];
        let provider = InMemoryProvider::new(capability("local", 1, 100), records);
        let mut query = query();
        query.end = 2;
        query.as_of = Some(10);
        let result = provider.fetch(&query).unwrap();
        assert_eq!(result.records.len(), 1);
        assert_eq!(result.records.len(), 1);
        assert_eq!(result.payloads.len(), 1);
        assert_eq!(result.records[0].payload_hash, result.payloads[0].digest());
    }

    #[test]
    fn provider_result_rejects_wrong_source_or_unsorted_records() {
        let (record_late, payload_late) = row("local", 2, 2, 11);
        let (record_early, payload_early) = row("local", 1, 1, 10);
        let mut result = ProviderResult {
            schema_version: PROVIDER_RESULT_SCHEMA_VERSION,
            records: vec![record_late, record_early],
            payloads: vec![payload_late, payload_early],
            provider_id: "local".into(),
            provider_version: "v1".into(),
            request_id: "r1".into(),
            retry_chain: vec!["local".into()],
            received_at: 2,
            source_hash: 0,
        };
        assert!(result.validate_for(&query()).is_err());
        result.records.swap(0, 1);
        result.records[0].source = DataSourceId::new("other");
        assert!(result.validate_for(&query()).is_err());
    }

    #[test]
    fn source_hash_is_verified_before_accepting_provider_result() {
        let (record, payload) = row("local", 1, 2, 10);
        let mut result = ProviderResult {
            schema_version: PROVIDER_RESULT_SCHEMA_VERSION,
            records: vec![record],
            payloads: vec![payload],
            provider_id: "local".into(),
            provider_version: "v1".into(),
            request_id: "req".into(),
            retry_chain: Vec::new(),
            received_at: 2,
            source_hash: 0,
        };
        result.source_hash = result.compute_source_hash();
        assert!(result.validate_for(&query()).is_ok());
        result.payloads[0].json.push(' ');
        assert!(result.validate_for(&query()).is_err());
    }

    #[test]
    fn provider_query_and_result_json_round_trip_preserve_provenance() {
        let query = query();
        let query_json = query.to_json().unwrap();
        assert_eq!(DataQuery::from_json(&query_json).unwrap(), query);

        let provider =
            InMemoryProvider::new(capability("local", 1, 100), vec![row("local", 1, 2, 10)]);
        let result = provider.fetch(&query).unwrap();
        let result_json = result.to_json().unwrap();
        assert_eq!(ProviderResult::from_json(&result_json).unwrap(), result);
        let mut wire: serde_json::Value = serde_json::from_str(&result_json).unwrap();
        wire["source_hash"] = serde_json::Value::from(0_u64);
        assert!(ProviderResult::from_json(&wire.to_string()).is_err());
        let capability = capability("local", 1, 100);
        assert_eq!(
            ProviderCapability::from_json(&capability.to_json().unwrap()).unwrap(),
            capability
        );
    }
}
