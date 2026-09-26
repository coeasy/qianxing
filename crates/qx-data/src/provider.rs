use serde::{Deserialize, Serialize};

use crate::catalog::DatasetManifest;
use crate::fingerprint::fingerprint_bars;
use crate::schema::{Bar, DATA_SCHEMA_VERSION};
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

/// BarFrame JSON 文档当前支持的契约版本，与 `qianxing_bridge.BAR_FRAME_SCHEMA_VERSION`
/// 以及全局 [`crate::schema::DATA_SCHEMA_VERSION`] 保持一致。
///
/// 文档里缺省该字段视为 `0`（旧格式，兼容分支）；大于本常量的版本必须拒绝，
/// 避免新版本 Python 写出的字段被旧 Rust 静默降级解析。
pub const BAR_FRAME_SCHEMA_VERSION: u32 = DATA_SCHEMA_VERSION;

/// 读取 Python/AkShare/Baostock/easy_tdx 已标准化输出的 BarFrame JSON。
///
/// 外部数据源仍由 Python 负责登录、限频和字段适配；Rust 侧只接受固定列、
/// 定点整数和严格时间序列，并通过同一个 `DataProvider` 接口进入 qx-data
/// 的目录、校验、缓存和回测流程。这样“Python 获取、Rust 研究/交易”不会再
/// 依赖 CLI 中的特例分支。
pub struct JsonBarFrameProvider {
    metadata: ProviderMetadata,
    path: PathBuf,
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
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
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
        let frame = self.read_frame()?;
        let bars = frame.bars(instrument, start, end)?;
        let first = bars
            .first()
            .ok_or_else(|| "BarFrame manifest cannot be created from empty range".to_string())?;
        let last = bars.last().expect("first implies last");
        let manifest = DatasetManifest {
            dataset_id: dataset_id.into(),
            version: self.metadata.version.clone(),
            source: frame.manifest_source(&self.metadata.name),
            fingerprint: fingerprint_bars(&bars)?,
            schema_version: frame.contract().effective_version(),
            start_timestamp: first.timestamp,
            end_timestamp: last.timestamp,
        };
        manifest.validate()?;
        Ok((bars, manifest))
    }

    /// 读取并解析 BarFrame JSON；错误信息始终带上文件路径。
    fn read_frame(&self) -> Result<ParsedBarFrame, String> {
        let payload = std::fs::read_to_string(&self.path)
            .map_err(|error| format!("read BarFrame {} failed: {error}", self.path.display()))?;
        parse_bar_frame(&payload).map_err(|error| format!("{error} in {}", self.path.display()))
    }

    /// 对外暴露这份 BarFrame 的契约（`schema_version`、`source`），供 DatasetManifest、
    /// 质量报告和运行清单登记使用。
    pub fn contract(&self) -> Result<BarFrameContract, String> {
        Ok(self.read_frame()?.contract())
    }
}

#[derive(serde::Deserialize)]
struct BarFrameJson {
    /// 缺省 `0`：旧格式（无契约版本），保持兼容分支。
    #[serde(default)]
    schema_version: u32,
    /// 旧格式可以缺省；`schema_version >= 1` 时必须有非空来源。
    #[serde(default)]
    source: String,
    instrument: String,
    ts: Vec<u64>,
    open_raw: Vec<i128>,
    high_raw: Vec<i128>,
    low_raw: Vec<i128>,
    close_raw: Vec<i128>,
    volume_raw: Vec<i128>,
}

/// `schema_version >= 1` 的严格视图：未知字段直接拒绝，`source` 必填。
/// 兼容分支继续使用 [`BarFrameJson`]，因此旧样例（无 `schema_version`）仍可解析。
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct StrictBarFrameJson {
    #[serde(default)]
    schema_version: u32,
    instrument: String,
    source: String,
    ts: Vec<u64>,
    open_raw: Vec<i128>,
    high_raw: Vec<i128>,
    low_raw: Vec<i128>,
    close_raw: Vec<i128>,
    volume_raw: Vec<i128>,
}

/// BarFrame JSON 顶层契约信息；用于把“这份数据来自哪里、按哪个契约版本写出”
/// 绑定进 DatasetManifest 和质量/血缘哈希，而不是当成无身份的文件。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BarFrameContract {
    pub schema_version: u32,
    pub source: String,
}

impl BarFrameContract {
    /// 质量/血缘哈希使用的有效版本：旧格式按 1 记账，避免出现 version=0 的记录。
    pub fn effective_version(&self) -> u32 {
        self.schema_version.max(1)
    }
}

/// 已解析的 BarFrame JSON：列数据 + 契约元数据。
#[derive(Debug)]
struct ParsedBarFrame {
    schema_version: u32,
    instrument: String,
    source: String,
    ts: Vec<u64>,
    open_raw: Vec<i128>,
    high_raw: Vec<i128>,
    low_raw: Vec<i128>,
    close_raw: Vec<i128>,
    volume_raw: Vec<i128>,
}

impl ParsedBarFrame {
    fn contract(&self) -> BarFrameContract {
        BarFrameContract {
            schema_version: self.schema_version,
            source: self.source.clone(),
        }
    }

    /// DatasetManifest 的 `source`：优先使用文档自己声明的来源，旧格式或空值
    /// 回退到 Provider 注册名，保持既有血缘语义不变。
    fn manifest_source(&self, fallback: &str) -> String {
        if self.source.trim().is_empty() {
            fallback.to_string()
        } else {
            self.source.clone()
        }
    }

    /// 列一致性、单调性和范围裁剪都在这一处校验，避免不同入口产生不同结论。
    fn bars(&self, instrument: &str, start: u64, end: u64) -> Result<Vec<Bar>, String> {
        if instrument.trim().is_empty() || start == 0 || start > end {
            return Err("invalid JSON BarFrame provider range".into());
        }
        if self.instrument != instrument {
            return Err(format!(
                "BarFrame instrument mismatch: file={} requested={instrument}",
                self.instrument
            ));
        }
        let columns = [
            self.ts.len(),
            self.open_raw.len(),
            self.high_raw.len(),
            self.low_raw.len(),
            self.close_raw.len(),
            self.volume_raw.len(),
        ];
        if columns.iter().any(|length| *length != columns[0]) || columns[0] == 0 {
            return Err("BarFrame column lengths are inconsistent or empty".into());
        }
        if self.ts.windows(2).any(|window| window[0] >= window[1]) {
            return Err("BarFrame timestamps must be strictly increasing".into());
        }
        let mut bars = Vec::with_capacity(self.ts.len());
        for index in 0..self.ts.len() {
            let bar = Bar {
                instrument: self.instrument.clone(),
                timestamp: self.ts[index],
                open_raw: self.open_raw[index],
                high_raw: self.high_raw[index],
                low_raw: self.low_raw[index],
                close_raw: self.close_raw[index],
                volume_raw: self.volume_raw[index],
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

/// 解析 BarFrame JSON：`schema_version` 缺省（0）走兼容分支，`>= 1` 启用严格模式。
fn parse_bar_frame(payload: &str) -> Result<ParsedBarFrame, String> {
    let legacy: BarFrameJson = serde_json::from_str(payload)
        .map_err(|error| format!("decode BarFrame JSON failed: {error}"))?;
    if legacy.schema_version == 0 {
        return Ok(ParsedBarFrame {
            schema_version: 0,
            instrument: legacy.instrument,
            source: legacy.source,
            ts: legacy.ts,
            open_raw: legacy.open_raw,
            high_raw: legacy.high_raw,
            low_raw: legacy.low_raw,
            close_raw: legacy.close_raw,
            volume_raw: legacy.volume_raw,
        });
    }
    // fail-closed：高于本构建支持的契约版本必须报错，不能降级成宽松解析。
    if legacy.schema_version > BAR_FRAME_SCHEMA_VERSION {
        return Err(format!(
            "unsupported BarFrame schema_version={} (supported versions: 0 legacy, 1..={BAR_FRAME_SCHEMA_VERSION})",
            legacy.schema_version
        ));
    }
    let strict: StrictBarFrameJson = serde_json::from_str(payload).map_err(|error| {
        format!(
            "decode BarFrame JSON failed (schema_version={} strict mode): {error}",
            legacy.schema_version
        )
    })?;
    if strict.source.trim().is_empty() {
        return Err(format!(
            "BarFrame schema_version={} requires a non-empty source",
            strict.schema_version
        ));
    }
    Ok(ParsedBarFrame {
        schema_version: strict.schema_version,
        instrument: strict.instrument,
        source: strict.source,
        ts: strict.ts,
        open_raw: strict.open_raw,
        high_raw: strict.high_raw,
        low_raw: strict.low_raw,
        close_raw: strict.close_raw,
        volume_raw: strict.volume_raw,
    })
}

impl DataProvider for JsonBarFrameProvider {
    fn metadata(&self) -> ProviderMetadata {
        self.metadata.clone()
    }

    fn load_bars(&self, instrument: &str, start: u64, end: u64) -> Result<Vec<Bar>, String> {
        self.read_frame()?.bars(instrument, start, end)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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

    /// 旧格式（无 `schema_version`）必须继续可解析，且允许缺省 `source`。
    #[test]
    fn bar_frame_contract_legacy_document_keeps_compatible_branch() {
        let frame = parse_bar_frame(
            r#"{"instrument":"000001.SZSE","ts":[1000],"open_raw":[10],"high_raw":[11],"low_raw":[9],"close_raw":[10],"volume_raw":[1]}"#,
        )
        .unwrap();
        let contract = frame.contract();
        assert_eq!(contract.schema_version, 0);
        assert_eq!(contract.effective_version(), 1);
        assert!(contract.source.is_empty());
        // 旧格式的 manifest 来源仍回退到 Provider 注册名，血缘语义不变。
        assert_eq!(frame.manifest_source("akshare"), "akshare");
    }

    #[test]
    fn bar_frame_contract_v1_is_strict_and_tracks_source() {
        let payload = r#"{"schema_version":1,"instrument":"000001.SZSE","source":"easy_tdx","ts":[1000],"open_raw":[10],"high_raw":[11],"low_raw":[9],"close_raw":[10],"volume_raw":[1]}"#;
        let frame = parse_bar_frame(payload).unwrap();
        let contract = frame.contract();
        assert_eq!(contract.schema_version, BAR_FRAME_SCHEMA_VERSION);
        assert_eq!(contract.source, "easy_tdx");
        assert_eq!(frame.manifest_source("akshare"), "easy_tdx");

        let root = std::env::temp_dir().join(format!(
            "qianxing-data-provider-contract-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("bars.json");
        std::fs::write(&path, payload).unwrap();
        let provider = JsonBarFrameProvider::new("akshare", "v1", &path);
        assert_eq!(provider.contract().unwrap().source, "easy_tdx");
        let (bars, manifest) = provider
            .load_bars_with_manifest("ashare.daily", "000001.SZSE", 1, 1_000)
            .unwrap();
        assert_eq!(bars.len(), 1);
        assert_eq!(manifest.source, "easy_tdx");
        assert_eq!(manifest.schema_version, BAR_FRAME_SCHEMA_VERSION);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn bar_frame_contract_v1_rejects_unknown_field_blank_source_and_future_version() {
        // 严格模式拒绝未知字段，防止 Python/Rust 字段漂移后被静默忽略。
        let err = parse_bar_frame(
            r#"{"schema_version":1,"instrument":"000001.SZSE","source":"akshare","unexpected":1,"ts":[1000],"open_raw":[10],"high_raw":[11],"low_raw":[9],"close_raw":[10],"volume_raw":[1]}"#,
        )
        .unwrap_err();
        assert!(err.contains("strict mode"), "{err}");
        assert!(err.contains("unexpected"), "{err}");
        // 旧格式同样有未知字段时仍宽松通过。
        assert!(parse_bar_frame(
            r#"{"instrument":"000001.SZSE","unexpected":1,"ts":[1000],"open_raw":[10],"high_raw":[11],"low_raw":[9],"close_raw":[10],"volume_raw":[1]}"#,
        )
        .is_ok());
        let err = parse_bar_frame(
            r#"{"schema_version":1,"instrument":"000001.SZSE","source":"","ts":[1000],"open_raw":[10],"high_raw":[11],"low_raw":[9],"close_raw":[10],"volume_raw":[1]}"#,
        )
        .unwrap_err();
        assert!(err.contains("requires a non-empty source"), "{err}");
        let err = parse_bar_frame(
            r#"{"schema_version":7,"instrument":"000001.SZSE","source":"akshare","ts":[1000],"open_raw":[10],"high_raw":[11],"low_raw":[9],"close_raw":[10],"volume_raw":[1]}"#,
        )
        .unwrap_err();
        assert!(err.contains("unsupported BarFrame schema_version"), "{err}");
    }
}
