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
pub use qx_data::DatasetManifestV2;
pub use registry::{ProviderBarFrame, ProviderRegistry, RegistryError};
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

/// Optional caller requirements layered over the stable historical `DataQuery` contract.
#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct DataRequirements {
    /// Require a provider that declares this exact credential scope.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required_auth_scope: Option<String>,
    /// Require a source capable of serving a live feed.
    #[serde(default, skip_serializing_if = "is_false")]
    pub require_realtime: bool,
    /// Maximum event-time age relative to query `as_of`, in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_staleness_ms: Option<u64>,
}

fn is_false(value: &bool) -> bool {
    !value
}

impl DataRequirements {
    pub fn validate_for(&self, query: &DataQuery) -> Result<(), ProviderError> {
        if self
            .required_auth_scope
            .as_ref()
            .is_some_and(|scope| scope.trim().is_empty())
            || (self.require_realtime && query.as_of.is_none())
            || (self.max_staleness_ms.is_some() && !self.require_realtime)
        {
            return Err(ProviderError::new(
                ProviderErrorClass::Permanent,
                "数据查询能力要求非法：实时请求需有 as_of，且新鲜度限制只能用于实时请求",
            ));
        }
        Ok(())
    }
}

/// Backward-compatible request envelope. Existing `DataQuery` consumers need no source changes.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct DataRequest {
    #[serde(flatten)]
    pub query: DataQuery,
    #[serde(flatten, default)]
    pub requirements: DataRequirements,
}

impl DataRequest {
    pub fn new(query: DataQuery, requirements: DataRequirements) -> Self {
        Self {
            query,
            requirements,
        }
    }

    pub fn to_json(&self) -> Result<String, ProviderError> {
        self.query.validate()?;
        self.requirements.validate_for(&self.query)?;
        serde_json::to_string(self)
            .map_err(|error| ProviderError::new(ProviderErrorClass::Permanent, error.to_string()))
    }

    pub fn from_json(input: &str) -> Result<Self, ProviderError> {
        let request: Self = serde_json::from_str(input).map_err(|error| {
            ProviderError::new(
                ProviderErrorClass::Permanent,
                format!("DataRequest JSON 无法解析: {error}"),
            )
        })?;
        request.query.validate()?;
        request.requirements.validate_for(&request.query)?;
        Ok(request)
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
        if query.adjustment != "none" || query.quality_policy != "strict" {
            return Err(ProviderError::new(
                ProviderErrorClass::Permanent,
                "InMemoryProvider 只支持 adjustment=none 与 quality_policy=strict",
            ));
        }
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
mod tests;
