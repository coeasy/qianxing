use super::{
    DataKind, DataQuery, DataRequest, ProviderCapability, ProviderError, ProviderErrorClass,
    RawRecord,
};
use qx_core::Fnv1a;
use qx_guanxing::DataSourceId;
use serde::{Deserialize, Serialize};

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderPayload {
    pub instrument: String,
    pub content_type: String,
    pub json: String,
}

impl ProviderPayload {
    pub fn new_json(
        instrument: impl Into<String>,
        content_type: impl Into<String>,
        json: impl Into<String>,
    ) -> Self {
        Self {
            instrument: instrument.into(),
            content_type: content_type.into(),
            json: json.into(),
        }
    }

    pub fn digest(&self) -> u64 {
        let mut hash = Fnv1a::new();
        hash.write_text(&self.instrument);
        hash.write_text(&self.content_type);
        hash.write_text(&self.json);
        hash.finish()
    }
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

    pub fn matches_request(&self, request: &DataRequest) -> bool {
        self.matches(&request.query)
            && request
                .requirements
                .required_auth_scope
                .as_ref()
                .is_none_or(|scope| self.auth_scope == *scope)
            && (!request.requirements.require_realtime || self.realtime)
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
#[serde(deny_unknown_fields)]
pub struct ProviderResult {
    pub schema_version: u32,
    pub records: Vec<RawRecord>,
    /// Data payloads have a one-to-one positional relationship with `records`.
    pub payloads: Vec<ProviderPayload>,
    pub provider_id: String,
    pub provider_version: String,
    pub request_id: String,
    pub retry_chain: Vec<String>,
    pub received_at: u64,
    pub source_hash: u64,
}

impl ProviderResult {
    /// Build a correctly hashed result from provider payload rows. Language bindings can use
    /// this constructor instead of reimplementing the Rust provenance fingerprint algorithm.
    pub fn from_payloads(
        provider_id: impl Into<String>,
        provider_version: impl Into<String>,
        request_id: impl Into<String>,
        received_at: u64,
        payloads: Vec<(u64, u64, ProviderPayload)>,
    ) -> Result<Self, ProviderError> {
        let provider_id = provider_id.into();
        let provider_version = provider_version.into();
        let request_id = request_id.into();
        let mut rows = payloads
            .into_iter()
            .map(|(event_time, receive_time, payload)| {
                let payload_hash = payload.digest();
                (event_time, receive_time, payload_hash, payload)
            })
            .collect::<Vec<_>>();
        rows.sort_by_key(|(event_time, receive_time, payload_hash, _)| {
            (*event_time, *receive_time, *payload_hash)
        });
        let result = Self {
            schema_version: PROVIDER_RESULT_SCHEMA_VERSION,
            records: rows
                .iter()
                .map(|(event_time, receive_time, payload_hash, _)| RawRecord {
                    source: DataSourceId::new(provider_id.clone()),
                    event_time: *event_time,
                    receive_time: *receive_time,
                    payload_hash: *payload_hash,
                    schema_version: PROVIDER_RESULT_SCHEMA_VERSION,
                })
                .collect(),
            payloads: rows.into_iter().map(|(_, _, _, payload)| payload).collect(),
            provider_id,
            provider_version,
            request_id,
            retry_chain: Vec::new(),
            received_at,
            source_hash: 0,
        };
        let mut result = result;
        result.source_hash = result.compute_source_hash();
        result.validate()?;
        Ok(result)
    }

    pub fn validate(&self) -> Result<(), ProviderError> {
        if self.schema_version != PROVIDER_RESULT_SCHEMA_VERSION {
            return Err(ProviderError::new(
                ProviderErrorClass::Permanent,
                format!(
                    "ProviderResult schema_version={} 不受支持（当前 {}）",
                    self.schema_version, PROVIDER_RESULT_SCHEMA_VERSION
                ),
            ));
        }
        if self.provider_id.trim().is_empty()
            || self.provider_version.trim().is_empty()
            || self.request_id.trim().is_empty()
        {
            return Err(ProviderError::new(
                ProviderErrorClass::Permanent,
                "ProviderResult 缺少血缘字段",
            ));
        }
        if self.records.len() != self.payloads.len() {
            return Err(ProviderError::new(
                ProviderErrorClass::Permanent,
                "ProviderResult records 与 payloads 数量不一致",
            ));
        }
        for (record, payload) in self.records.iter().zip(&self.payloads) {
            if payload.instrument.trim().is_empty()
                || payload.content_type.trim().is_empty()
                || serde_json::from_str::<serde_json::Value>(&payload.json).is_err()
                || record.payload_hash != payload.digest()
            {
                return Err(ProviderError::new(
                    ProviderErrorClass::Permanent,
                    "ProviderResult 载荷为空、无效或与 payload_hash 不匹配",
                ));
            }
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
        if self
            .records
            .iter()
            .any(|record| record.receive_time > self.received_at)
        {
            return Err(ProviderError::new(
                ProviderErrorClass::Permanent,
                "ProviderResult.received_at 早于记录接收时间",
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
        if self.payloads.iter().any(|payload| {
            !query.instrument_set.is_empty() && !query.instrument_set.contains(&payload.instrument)
        }) {
            return Err(ProviderError::new(
                ProviderErrorClass::Permanent,
                "ProviderResult 包含查询标的集合之外的载荷",
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

    pub fn validate_for_request(
        &self,
        request: &DataRequest,
        capability: &ProviderCapability,
    ) -> Result<(), ProviderError> {
        self.validate_for(&request.query)?;
        request.requirements.validate_for(&request.query)?;
        if request.requirements.require_realtime {
            let as_of = request.query.as_of.ok_or_else(|| {
                ProviderError::new(
                    ProviderErrorClass::Permanent,
                    "实时数据查询必须提供 as_of 时间戳",
                )
            })?;
            let provider_limit_ms = capability.freshness_seconds.saturating_mul(1_000);
            let allowed_age_ms = request
                .requirements
                .max_staleness_ms
                .map_or(provider_limit_ms, |requested| {
                    requested.min(provider_limit_ms)
                });
            if self.records.iter().any(|record| {
                record.event_time > as_of
                    || as_of.saturating_sub(record.event_time) > allowed_age_ms
            }) {
                return Err(ProviderError::new(
                    ProviderErrorClass::Permanent,
                    "ProviderResult 包含超过实时新鲜度上限的数据",
                ));
            }
        }
        Ok(())
    }

    pub fn compute_source_hash(&self) -> u64 {
        let mut hash = qx_core::Fnv1a::new();
        hash.write_text(&self.provider_id);
        hash.write_text(&self.provider_version);
        hash.write_text(&self.request_id);
        hash.write_u64(self.schema_version as u64);
        hash.write_u64(self.received_at);
        for (record, payload) in self.records.iter().zip(&self.payloads) {
            hash.write_u64(record.event_time);
            hash.write_u64(record.receive_time);
            hash.write_u64(record.payload_hash);
            hash.write_u64(record.schema_version as u64);
            hash.write_u64(payload.digest());
        }
        for provider in &self.retry_chain {
            hash.write_text(provider);
        }
        hash.finish()
    }

    /// Convert one fully validated Bar query to the canonical shared-kernel BarFrame.
    pub fn to_bar_frame(
        &self,
        query: &DataQuery,
    ) -> Result<qx_datastruct::BarFrame, ProviderError> {
        self.validate_for(query)?;
        if query.kind != DataKind::Bar || query.instrument_set.len() != 1 {
            return Err(ProviderError::new(
                ProviderErrorClass::Permanent,
                "BarFrame 装配要求 Bar 查询且恰好指定一个标的",
            ));
        }
        if query.quality_policy != "strict" {
            return Err(ProviderError::new(
                ProviderErrorClass::Permanent,
                "当前 BarFrame 标准化只支持 strict 质量策略",
            ));
        }
        const BAR_JSON_CONTENT_TYPE: &str = "application/vnd.qianxing.bar+json;v=1";
        let instrument_name = query.instrument_set.iter().next().expect("length checked");
        let instrument = qx_core::InstrumentId::parse(instrument_name).ok_or_else(|| {
            ProviderError::new(
                ProviderErrorClass::Permanent,
                "Bar 查询的 instrument_id 非法",
            )
        })?;
        let mut bars = Vec::with_capacity(self.records.len());
        for (record, payload) in self.records.iter().zip(&self.payloads) {
            if payload.instrument != *instrument_name
                || payload.content_type != BAR_JSON_CONTENT_TYPE
            {
                return Err(ProviderError::new(
                    ProviderErrorClass::Permanent,
                    "Bar 载荷标的或 content_type 不匹配",
                ));
            }
            let bar: qx_guanxing::Bar = serde_json::from_str(&payload.json).map_err(|error| {
                ProviderError::new(
                    ProviderErrorClass::Permanent,
                    format!("Bar 载荷无法解析: {error}"),
                )
            })?;
            if bar.ts != record.event_time {
                return Err(ProviderError::new(
                    ProviderErrorClass::Permanent,
                    "Bar 载荷时间与 RawRecord.event_time 不一致",
                ));
            }
            bars.push(bar);
        }
        let quality = qx_guanxing::QualityGate::check(&bars);
        if quality.verdict() != qx_guanxing::Verdict::Ok {
            return Err(ProviderError::new(
                ProviderErrorClass::Permanent,
                format!("Provider Bar 质量门未通过: {:?}", quality.issues),
            ));
        }
        let frame = qx_datastruct::BarFrame {
            instrument,
            source: DataSourceId::new(format!(
                "{}@{}#query={:016x}#source={:016x}",
                self.provider_id,
                self.provider_version,
                query.digest(),
                self.source_hash
            )),
            ts: bars.iter().map(|bar| bar.ts).collect(),
            open_raw: bars.iter().map(|bar| bar.open).collect(),
            high_raw: bars.iter().map(|bar| bar.high).collect(),
            low_raw: bars.iter().map(|bar| bar.low).collect(),
            close_raw: bars.iter().map(|bar| bar.close).collect(),
            volume_raw: bars.iter().map(|bar| bar.volume).collect(),
        };
        frame.validate().map_err(|error| {
            ProviderError::new(
                ProviderErrorClass::Permanent,
                format!("BarFrame 标准化校验失败: {error:?}"),
            )
        })?;
        Ok(frame)
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

pub const PROVIDER_RESULT_SCHEMA_VERSION: u32 = 1;

pub trait DataProvider: Send + Sync {
    fn capability(&self) -> &ProviderCapability;
    fn fetch(&self, query: &DataQuery) -> Result<ProviderResult, ProviderError>;

    fn fetch_request(&self, request: &DataRequest) -> Result<ProviderResult, ProviderError> {
        self.fetch(&request.query)
    }
}
