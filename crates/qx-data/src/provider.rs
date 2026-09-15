use serde::{Deserialize, Serialize};

use crate::catalog::DatasetManifest;
use crate::fingerprint::fingerprint_bars;
use crate::schema::Bar;
use qx_core::Fnv1a;
use qx_guanxing::{DataSourceId, RawRecord};
use qx_provider::{
    DataKind, DataProvider as RegistryDataProvider, DataQuery, ProviderCapability, ProviderError,
    ProviderErrorClass, ProviderResult,
};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProviderMetadata {
    pub name: String,
    pub version: String,
}

pub trait DataProvider {
    fn metadata(&self) -> ProviderMetadata;

    fn load_bars(&self, instrument: &str, start: u64, end: u64) -> Result<Vec<Bar>, String>;
}

/// 读取 Python/AkShare/Baostock/easy_tdx 已标准化输出的 BarFrame JSON。
///
/// 外部数据源仍由 Python 负责登录、限频和字段适配；Rust 侧只接受固定列、
/// 定点整数和严格时间序列，并通过同一个 `DataProvider` 接口进入 qx-data
/// 的目录、校验、缓存和回测流程。这样“Python 获取、Rust 研究/交易”不会再
/// 依赖 CLI 中的特例分支。
pub struct JsonBarFrameProvider {
    metadata: ProviderMetadata,
    path: PathBuf,
    registry_capability: ProviderCapability,
    registry_received_at: Option<u64>,
}

impl JsonBarFrameProvider {
    pub fn new(
        name: impl Into<String>,
        version: impl Into<String>,
        path: impl Into<PathBuf>,
    ) -> Self {
        let name = name.into();
        let version = version.into();
        Self {
            metadata: ProviderMetadata {
                name: name.clone(),
                version: version.clone(),
            },
            path: path.into(),
            registry_capability: default_registry_capability(name, version),
            registry_received_at: None,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 为统一 `qx-provider` 注册入口覆盖能力声明。默认能力适用于日线 A 股
    /// BarFrame；交易所、频率和质量等级不同的源必须显式声明，避免 Registry
    /// 把一个文件误当成所有资产类别的通用 Provider。
    pub fn with_registry_capability(mut self, capability: ProviderCapability) -> Self {
        self.registry_capability = capability;
        self
    }

    /// 静态 BarFrame 没有可靠接收时间时保持 `None`，带 `as_of` 的查询会安全
    /// 失败而不是伪造 PIT 可见性。Python manifest/落盘流程应在注册前提供该值。
    pub fn with_received_at(mut self, received_at: u64) -> Self {
        self.registry_received_at = Some(received_at);
        self
    }

    /// 加载同一份标准化数据并生成可注册的 DatasetManifest，确保“来源、版本、
    /// 范围、指纹”与回测输入绑定，而不是只把一份 JSON 当成无身份的文件。
    pub fn load_bars_with_manifest(
        &self,
        dataset_id: &str,
        instrument: &str,
        start: u64,
        end: u64,
    ) -> Result<(Vec<Bar>, DatasetManifest), String> {
        let bars = self.load_bars(instrument, start, end)?;
        let first = bars
            .first()
            .ok_or_else(|| "BarFrame manifest cannot be created from empty range".to_string())?;
        let last = bars.last().expect("first implies last");
        let manifest = DatasetManifest {
            dataset_id: dataset_id.into(),
            version: self.metadata.version.clone(),
            source: self.metadata.name.clone(),
            fingerprint: fingerprint_bars(&bars)?,
            schema_version: 1,
            start_timestamp: first.timestamp,
            end_timestamp: last.timestamp,
        };
        manifest.validate()?;
        Ok((bars, manifest))
    }
}

fn default_registry_capability(name: String, version: String) -> ProviderCapability {
    ProviderCapability {
        provider_id: name,
        version,
        data_kinds: BTreeSet::from([DataKind::Bar]),
        asset_classes: BTreeSet::from(["equity_cn".into()]),
        frequencies: BTreeSet::from(["1d".into()]),
        auth_scope: "offline-file".into(),
        rate_limit_per_second: 1,
        freshness_seconds: u64::MAX,
        historical_start: 0,
        historical_end: u64::MAX,
        realtime: false,
        priority: 100,
        quality_score: 100,
        cost_score: 0,
    }
}

#[derive(serde::Deserialize)]
struct BarFrameJson {
    instrument: String,
    ts: Vec<u64>,
    open_raw: Vec<i128>,
    high_raw: Vec<i128>,
    low_raw: Vec<i128>,
    close_raw: Vec<i128>,
    volume_raw: Vec<i128>,
}

impl DataProvider for JsonBarFrameProvider {
    fn metadata(&self) -> ProviderMetadata {
        self.metadata.clone()
    }

    fn load_bars(&self, instrument: &str, start: u64, end: u64) -> Result<Vec<Bar>, String> {
        if instrument.trim().is_empty() || start == 0 || start > end {
            return Err("invalid JSON BarFrame provider range".into());
        }
        let payload = std::fs::read_to_string(&self.path)
            .map_err(|error| format!("read BarFrame {} failed: {error}", self.path.display()))?;
        let frame: BarFrameJson = serde_json::from_str(&payload)
            .map_err(|error| format!("decode BarFrame {} failed: {error}", self.path.display()))?;
        if frame.instrument != instrument {
            return Err(format!(
                "BarFrame instrument mismatch: file={} requested={instrument}",
                frame.instrument
            ));
        }
        let columns = [
            frame.ts.len(),
            frame.open_raw.len(),
            frame.high_raw.len(),
            frame.low_raw.len(),
            frame.close_raw.len(),
            frame.volume_raw.len(),
        ];
        if columns.iter().any(|length| *length != columns[0]) || columns[0] == 0 {
            return Err("BarFrame column lengths are inconsistent or empty".into());
        }
        if frame.ts.windows(2).any(|window| window[0] >= window[1]) {
            return Err("BarFrame timestamps must be strictly increasing".into());
        }
        let mut bars = Vec::with_capacity(frame.ts.len());
        for index in 0..frame.ts.len() {
            let bar = Bar {
                instrument: frame.instrument.clone(),
                timestamp: frame.ts[index],
                open_raw: frame.open_raw[index],
                high_raw: frame.high_raw[index],
                low_raw: frame.low_raw[index],
                close_raw: frame.close_raw[index],
                volume_raw: frame.volume_raw[index],
            };
            bar.validate()?;
            if start <= bar.timestamp && bar.timestamp <= end {
                bars.push(bar);
            }
        }
        if bars.is_empty() {
            return Err("BarFrame contains no rows in requested range".into());
        }
        Ok(bars)
    }
}

impl RegistryDataProvider for JsonBarFrameProvider {
    fn capability(&self) -> &ProviderCapability {
        &self.registry_capability
    }

    fn fetch(&self, query: &DataQuery) -> Result<ProviderResult, ProviderError> {
        query.validate()?;
        if query.kind != DataKind::Bar {
            return Err(ProviderError::new(
                ProviderErrorClass::Permanent,
                "JSON BarFrame Provider 只支持 Bar 查询",
            ));
        }
        let instrument = match query.instrument_set.iter().next() {
            Some(instrument) if query.instrument_set.len() == 1 => instrument,
            _ => {
                return Err(ProviderError::new(
                    ProviderErrorClass::Permanent,
                    "JSON BarFrame 查询必须恰好包含一个 instrument",
                ))
            }
        };
        if query.as_of.is_some() && self.registry_received_at.is_none() {
            return Err(ProviderError::new(
                ProviderErrorClass::ManualIntervention,
                "JSON BarFrame Provider 未配置 received_at，禁止伪造 PIT as_of 查询",
            ));
        }
        let bars = self
            .load_bars(instrument, query.start, query.end)
            .map_err(|error| ProviderError::new(ProviderErrorClass::Permanent, error))?;
        let receive_time = self.registry_received_at.unwrap_or(query.end);
        let records = bars
            .into_iter()
            .map(|bar| RawRecord {
                source: DataSourceId::new(self.registry_capability.provider_id.clone()),
                event_time: bar.timestamp,
                receive_time,
                payload_hash: bar_payload_hash(&bar),
                schema_version: 1,
            })
            .collect::<Vec<_>>();
        let mut result = ProviderResult {
            records,
            provider_id: self.registry_capability.provider_id.clone(),
            provider_version: self.registry_capability.version.clone(),
            request_id: format!(
                "{}-{:016x}",
                self.registry_capability.provider_id,
                query.digest()
            ),
            retry_chain: Vec::new(),
            received_at: receive_time,
            source_hash: 0,
        };
        result.source_hash = result.compute_source_hash();
        Ok(result)
    }
}

fn bar_payload_hash(bar: &Bar) -> u64 {
    let mut hash = Fnv1a::new();
    hash.write_text(&bar.instrument);
    hash.write_u64(bar.timestamp);
    hash.write_i128(bar.open_raw);
    hash.write_i128(bar.high_raw);
    hash.write_i128(bar.low_raw);
    hash.write_i128(bar.close_raw);
    hash.write_i128(bar.volume_raw);
    hash.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn json_bar_frame_provider_preserves_fixed_point_columns_and_range() {
        let root = std::env::temp_dir().join(format!(
            "qianxing-data-provider-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("bars.json");
        std::fs::write(
            &path,
            r#"{"instrument":"000001.SZSE","source":"akshare","ts":[1000,2000,3000],"open_raw":[10,20,30],"high_raw":[11,21,31],"low_raw":[9,19,29],"close_raw":[10,20,30],"volume_raw":[1,2,3]}"#,
        )
        .unwrap();
        let provider = JsonBarFrameProvider::new("akshare", "v1", &path);
        let bars = provider.load_bars("000001.SZSE", 1500, 3000).unwrap();
        assert_eq!(bars.len(), 2);
        assert_eq!(bars[0].timestamp, 2000);
        assert_eq!(bars[1].close_raw, 30);
        assert_eq!(provider.metadata().name, "akshare");
        let (_, manifest) = provider
            .load_bars_with_manifest("ashare.daily", "000001.SZSE", 1500, 3000)
            .unwrap();
        assert_eq!(manifest.source, "akshare");
        assert_eq!(manifest.start_timestamp, 2000);
        assert_eq!(manifest.end_timestamp, 3000);
        assert!(manifest.fingerprint.len() >= 16);
        assert!(provider.load_bars("600000.SSE", 1, 3_000).is_err());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn json_bar_frame_provider_registers_with_provider_registry() {
        let root = std::env::temp_dir().join(format!(
            "qianxing-data-registry-provider-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("bars.json");
        std::fs::write(
            &path,
            r#"{"instrument":"000001.SZSE","source":"akshare","ts":[1000,2000,3000],"open_raw":[10,20,30],"high_raw":[11,21,31],"low_raw":[9,19,29],"close_raw":[10,20,30],"volume_raw":[1,2,3]}"#,
        )
        .unwrap();
        let provider = JsonBarFrameProvider::new("akshare", "v1", &path).with_received_at(4_000);
        let mut registry = qx_provider::ProviderRegistry::new();
        registry.register(Box::new(provider)).unwrap();
        let query = DataQuery {
            kind: DataKind::Bar,
            asset_class: "equity_cn".into(),
            instrument_set: BTreeSet::from(["000001.SZSE".into()]),
            field_set: BTreeSet::from(["open".into(), "close".into()]),
            frequency: "1d".into(),
            adjustment: "none".into(),
            quality_policy: "strict".into(),
            start: 1_500,
            end: 3_000,
            as_of: Some(5_000),
        };
        let result = registry.fetch_with_failover(&query).unwrap();
        assert_eq!(result.provider_id, "akshare");
        assert_eq!(result.records.len(), 2);
        assert_eq!(result.records[0].receive_time, 4_000);
        assert_ne!(result.source_hash, 0);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn json_bar_frame_provider_rejects_column_mismatch_and_non_monotonic_input() {
        let root = std::env::temp_dir().join(format!(
            "qianxing-data-provider-invalid-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("bars.json");
        std::fs::write(
            &path,
            r#"{"instrument":"X.V","source":"test","ts":[2,1],"open_raw":[1],"high_raw":[1,1],"low_raw":[1,1],"close_raw":[1,1],"volume_raw":[1,1]}"#,
        )
        .unwrap();
        let provider = JsonBarFrameProvider::new("test", "v1", &path);
        assert!(provider.load_bars("X.V", 1, 3).is_err());
        let _ = std::fs::remove_dir_all(root);
    }
}
