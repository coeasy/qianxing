//! 多源数据 Provider 边界。
//!
//! Provider 只负责把外部数据带着血缘送入 Raw 层；它不能直接写入账簿、标准化目录
//! 或策略缓存。ProviderRegistry 的选择顺序固定，保证主备切换可解释、可重放。

use qx_core::Fnv1a;
use qx_guanxing::RawRecord;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

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

impl ProviderCapability {
    pub fn validate(&self) -> Result<(), ProviderError> {
        if self.provider_id.trim().is_empty()
            || self.version.trim().is_empty()
            || self.data_kinds.is_empty()
            || self.asset_classes.is_empty()
            || self.frequencies.is_empty()
            || self.auth_scope.trim().is_empty()
            || self.rate_limit_per_second == 0
            || self.historical_start > self.historical_end
        {
            return Err(ProviderError::new(
                ProviderErrorClass::Permanent,
                "ProviderCapability 字段非法",
            ));
        }
        Ok(())
    }

    pub fn matches(&self, query: &DataQuery) -> bool {
        self.data_kinds.contains(&query.kind)
            && self.asset_classes.contains(&query.asset_class)
            && self.frequencies.contains(&query.frequency)
            && query.start >= self.historical_start
            && query.end <= self.historical_end
    }

    pub fn to_json(&self) -> Result<String, ProviderError> {
        self.validate()?;
        serde_json::to_string(self)
            .map_err(|error| ProviderError::new(ProviderErrorClass::Permanent, error.to_string()))
    }

    pub fn from_json(input: &str) -> Result<Self, ProviderError> {
        let capability: Self = serde_json::from_str(input).map_err(|error| {
            ProviderError::new(
                ProviderErrorClass::Permanent,
                format!("ProviderCapability JSON 无法解析: {error}"),
            )
        })?;
        capability.validate()?;
        Ok(capability)
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct ProviderResult {
    pub records: Vec<RawRecord>,
    pub provider_id: String,
    pub provider_version: String,
    pub request_id: String,
    pub retry_chain: Vec<String>,
    pub received_at: u64,
    pub source_hash: u64,
}

impl ProviderResult {
    pub fn validate(&self) -> Result<(), ProviderError> {
        if self.provider_id.trim().is_empty()
            || self.provider_version.trim().is_empty()
            || self.request_id.trim().is_empty()
        {
            return Err(ProviderError::new(
                ProviderErrorClass::Permanent,
                "ProviderResult 缺少血缘字段",
            ));
        }
        if self
            .records
            .iter()
            .any(|record| record.source.0 != self.provider_id)
        {
            return Err(ProviderError::new(
                ProviderErrorClass::Permanent,
                "ProviderResult 记录来源与 provider_id 不一致",
            ));
        }
        if self.records.windows(2).any(|window| {
            (
                window[0].event_time,
                window[0].receive_time,
                window[0].payload_hash,
            ) >= (
                window[1].event_time,
                window[1].receive_time,
                window[1].payload_hash,
            )
        }) {
            return Err(ProviderError::new(
                ProviderErrorClass::Permanent,
                "ProviderResult 未按事件时间、接收时间和载荷摘要排序",
            ));
        }
        if self.source_hash != self.compute_source_hash() {
            return Err(ProviderError::new(
                ProviderErrorClass::Permanent,
                "ProviderResult source_hash 与记录内容不一致",
            ));
        }
        Ok(())
    }

    pub fn validate_for(&self, query: &DataQuery) -> Result<(), ProviderError> {
        query.validate()?;
        self.validate()?;
        if self.records.iter().any(|record| {
            record.source.0 != self.provider_id
                || record.event_time < query.start
                || record.event_time > query.end
                || query.as_of.is_some_and(|as_of| record.receive_time > as_of)
        }) {
            return Err(ProviderError::new(
                ProviderErrorClass::Permanent,
                "ProviderResult 包含查询范围外或错误来源的数据",
            ));
        }
        if self.records.windows(2).any(|window| {
            (
                window[0].event_time,
                window[0].receive_time,
                window[0].payload_hash,
            ) >= (
                window[1].event_time,
                window[1].receive_time,
                window[1].payload_hash,
            )
        }) {
            return Err(ProviderError::new(
                ProviderErrorClass::Permanent,
                "ProviderResult 未按事件时间、接收时间和载荷摘要排序",
            ));
        }
        if self.source_hash != self.compute_source_hash() {
            return Err(ProviderError::new(
                ProviderErrorClass::Permanent,
                "ProviderResult source_hash 与记录内容不一致",
            ));
        }
        Ok(())
    }

    pub fn compute_source_hash(&self) -> u64 {
        let mut hash = qx_core::Fnv1a::new();
        hash.write_text(&self.provider_id);
        hash.write_text(&self.provider_version);
        for record in &self.records {
            hash.write_u64(record.event_time);
            hash.write_u64(record.receive_time);
            hash.write_u64(record.payload_hash);
            hash.write_u64(record.schema_version as u64);
        }
        hash.finish()
    }

    pub fn to_json(&self) -> Result<String, ProviderError> {
        self.validate()?;
        serde_json::to_string(self)
            .map_err(|error| ProviderError::new(ProviderErrorClass::Permanent, error.to_string()))
    }

    pub fn from_json(input: &str) -> Result<Self, ProviderError> {
        let result: Self = serde_json::from_str(input).map_err(|error| {
            ProviderError::new(
                ProviderErrorClass::Permanent,
                format!("ProviderResult JSON 无法解析: {error}"),
            )
        })?;
        result.validate()?;
        if result.source_hash != result.compute_source_hash() {
            return Err(ProviderError::new(
                ProviderErrorClass::Permanent,
                "ProviderResult source_hash 与记录内容不一致",
            ));
        }
        Ok(result)
    }
}

pub trait DataProvider: Send + Sync {
    fn capability(&self) -> &ProviderCapability;
    fn fetch(&self, query: &DataQuery) -> Result<ProviderResult, ProviderError>;
}

/// 可持久化前的确定性本地 Provider：按事件时间切片并保留原始血缘。
///
/// 它不是把数据“灌入”标准化表的快捷方式；返回值仍然是 RawRecord，调用方必须
/// 经过质量门和版本化标准化流程后才能进入研究或交易视图。
pub struct InMemoryProvider {
    capability: ProviderCapability,
    records: Vec<RawRecord>,
}

impl InMemoryProvider {
    pub fn new(capability: ProviderCapability, mut records: Vec<RawRecord>) -> Self {
        records.sort_by_key(|record| (record.event_time, record.receive_time, record.payload_hash));
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
            .filter(|record| record.event_time >= query.start && record.event_time <= query.end)
            .filter(|record| query.as_of.is_none_or(|as_of| record.receive_time <= as_of))
            .cloned()
            .collect::<Vec<_>>();
        let mut result = ProviderResult {
            records,
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

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum ProviderErrorClass {
    Retryable,
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

#[derive(Debug, PartialEq, Eq)]
pub enum RegistryError {
    Duplicate(String),
    NoProvider,
    Provider(ProviderError),
}

pub struct ProviderRegistry {
    providers: BTreeMap<String, Box<dyn DataProvider>>,
}

impl Default for ProviderRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ProviderRegistry {
    pub fn new() -> Self {
        Self {
            providers: BTreeMap::new(),
        }
    }

    pub fn register(&mut self, provider: Box<dyn DataProvider>) -> Result<(), RegistryError> {
        let id = provider.capability().provider_id.clone();
        provider
            .capability()
            .validate()
            .map_err(RegistryError::Provider)?;
        if id.trim().is_empty() {
            return Err(RegistryError::NoProvider);
        }
        if self.providers.contains_key(&id) {
            return Err(RegistryError::Duplicate(id));
        }
        self.providers.insert(id, provider);
        Ok(())
    }

    pub fn candidates(&self, query: &DataQuery) -> Result<Vec<String>, RegistryError> {
        query.validate().map_err(RegistryError::Provider)?;
        let mut candidates: Vec<_> = self
            .providers
            .values()
            .filter(|provider| provider.capability().matches(query))
            .map(|provider| provider.capability().clone())
            .collect();
        candidates.sort_by(|a, b| {
            a.priority
                .cmp(&b.priority)
                .then_with(|| b.quality_score.cmp(&a.quality_score))
                .then_with(|| a.cost_score.cmp(&b.cost_score))
                .then_with(|| a.provider_id.cmp(&b.provider_id))
        });
        Ok(candidates.into_iter().map(|c| c.provider_id).collect())
    }

    pub fn fetch_with_failover(&self, query: &DataQuery) -> Result<ProviderResult, RegistryError> {
        let candidates = self.candidates(query)?;
        if candidates.is_empty() {
            return Err(RegistryError::NoProvider);
        }
        let mut retry_chain = Vec::new();
        let mut last_error = None;
        for id in candidates {
            retry_chain.push(id.clone());
            let provider = self.providers.get(&id).expect("candidate is registered");
            match provider.fetch(query) {
                Ok(mut result) => {
                    result.retry_chain = retry_chain.clone();
                    match result.validate_for(query) {
                        Ok(()) => return Ok(result),
                        Err(error) => {
                            // 供应商返回了不可接受的事实，必须切换到下一候选，
                            // 不能把非法结果当成“查询成功”。
                            last_error = Some(error);
                        }
                    }
                }
                Err(error) => {
                    let terminal = matches!(
                        error.class,
                        ProviderErrorClass::ManualIntervention | ProviderErrorClass::Permanent
                    );
                    last_error = Some(error.clone());
                    if terminal {
                        return Err(RegistryError::Provider(error));
                    }
                }
            }
        }
        Err(last_error
            .map(RegistryError::Provider)
            .unwrap_or(RegistryError::NoProvider))
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
            let mut result = ProviderResult {
                records: vec![RawRecord {
                    source: DataSourceId::new(self.capability.provider_id.clone()),
                    event_time: 1,
                    receive_time: 2,
                    payload_hash: 3,
                    schema_version: 1,
                }],
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
        let records = vec![
            RawRecord {
                source: DataSourceId::new("local"),
                event_time: 1,
                receive_time: 10,
                payload_hash: 11,
                schema_version: 1,
            },
            RawRecord {
                source: DataSourceId::new("local"),
                event_time: 2,
                receive_time: 20,
                payload_hash: 22,
                schema_version: 1,
            },
        ];
        let provider = InMemoryProvider::new(capability("local", 1, 100), records);
        let mut query = query();
        query.end = 2;
        query.as_of = Some(10);
        let result = provider.fetch(&query).unwrap();
        assert_eq!(result.records.len(), 1);
        assert_eq!(result.records[0].payload_hash, 11);
    }

    #[test]
    fn provider_result_rejects_wrong_source_or_unsorted_records() {
        let mut result = ProviderResult {
            records: vec![
                RawRecord {
                    source: DataSourceId::new("local"),
                    event_time: 2,
                    receive_time: 2,
                    payload_hash: 2,
                    schema_version: 1,
                },
                RawRecord {
                    source: DataSourceId::new("local"),
                    event_time: 1,
                    receive_time: 1,
                    payload_hash: 1,
                    schema_version: 1,
                },
            ],
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
        let mut result = ProviderResult {
            records: vec![RawRecord {
                source: DataSourceId::new("local"),
                event_time: 1,
                receive_time: 2,
                payload_hash: 3,
                schema_version: 1,
            }],
            provider_id: "local".into(),
            provider_version: "v1".into(),
            request_id: "req".into(),
            retry_chain: Vec::new(),
            received_at: 2,
            source_hash: 0,
        };
        result.source_hash = result.compute_source_hash();
        assert!(result.validate_for(&query()).is_ok());
        result.records[0].payload_hash = 4;
        assert!(result.validate_for(&query()).is_err());
    }

    #[test]
    fn provider_query_and_result_json_round_trip_preserve_provenance() {
        let query = query();
        let query_json = query.to_json().unwrap();
        assert_eq!(DataQuery::from_json(&query_json).unwrap(), query);

        let provider = InMemoryProvider::new(
            capability("local", 1, 100),
            vec![RawRecord {
                source: DataSourceId::new("local"),
                event_time: 1,
                receive_time: 2,
                payload_hash: 3,
                schema_version: 1,
            }],
        );
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
