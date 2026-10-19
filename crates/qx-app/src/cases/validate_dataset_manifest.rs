//! `data-validate`：把一份 BarFrame JSON 诊断成 `DatasetManifestV2`，且**不改动源文件**。
//!
//! `frame` 只读；`output` 不存在就写出清单、已存在就改走读侧复核（同一身份不容两种内容），
//! 所以它既能"先注册再核对"，也不会静默覆盖别的链在用的清单。
//!
//! 它必须容忍脏数据——乱序、重复、缺口恰恰是要报告的东西，而严格读侧把"时间轴严格递增"当
//! 启动硬拒，复用它只会得到一句拒绝、`DatasetQualityReport` 无从生成；故走自己的宽松解析。
//! 指纹只有一条口径：能算出**规范指纹**（`qx_data::fingerprint_bars`，与 dataset-ingest 和回测链
//! 同一个函数）就用它；脏到算不出时退化为文件内容 sha256 并当场告警（规划 §7 P3 / §6.2）。

use super::{attach, guard::guard_panics};
use crate::artifact_store::sink;
use crate::context::{CallerCapability, RunContext};
use crate::error::{AppError, AppErrorCategory};
use crate::spec::DatasetManifestValidationSpec;
use qx_data::{
    Bar, DatasetManifest, DatasetManifestV2, DatasetQualityReport, DatasetSourceLineage,
    DatasetTier, BARFRAME_DATASET_VERSION, BAR_FRAME_SCHEMA_VERSION,
    DATASET_MANIFEST_V2_SCHEMA_VERSION,
};
use std::path::Path;

/// Result of tolerant dataset diagnosis and manifest publication.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DatasetManifestValidationResult {
    pub manifest: DatasetManifestV2,
    pub manifest_json: String,
    pub verified_existing: bool,
    pub degraded_fingerprint: Option<String>,
}

/// 宽松线格式：只要求六列存在且等长，不要求时间轴递增（与严格读侧有意分叉）。
struct RawColumns {
    schema_version: u64,
    instrument: String,
    source: String,
    ts: Vec<u64>,
    open_raw: Vec<i128>,
    high_raw: Vec<i128>,
    low_raw: Vec<i128>,
    close_raw: Vec<i128>,
    volume_raw: Vec<i128>,
}

/// 一份 BarFrame 的连续性问题计数，与 `DatasetQualityReport` 的数值字段一一对应。
#[derive(Clone, Copy, Debug)]
struct FrameQuality {
    row_count: u64,
    duplicate_rows: u64,
    out_of_order_rows: u64,
    missing_intervals: u64,
}

fn column<'a>(
    value: &'a serde_json::Value,
    key: &str,
) -> Result<&'a Vec<serde_json::Value>, String> {
    value
        .get(key)
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| format!("数据集 BarFrame 缺少数组列 {key}"))
}

/// 取一列无符号整数（时间戳）；非整数元素当场拒绝，不做静默取整。
fn u64_column(value: &serde_json::Value, key: &str) -> Result<Vec<u64>, String> {
    column(value, key)?
        .iter()
        .map(|item| {
            item.as_u64()
                .ok_or_else(|| format!("数据集 BarFrame 列 {key} 含非无符号整数元素"))
        })
        .collect()
}

/// 取一列定点整数（价格/量）；非整数元素当场拒绝，不做浮点降级。
fn i128_column(value: &serde_json::Value, key: &str) -> Result<Vec<i128>, String> {
    column(value, key)?
        .iter()
        .map(|item| {
            item.as_i64()
                .map(i128::from)
                .or_else(|| item.as_u64().map(i128::from))
                .ok_or_else(|| format!("数据集 BarFrame 列 {key} 含非整数元素"))
        })
        .collect()
}

fn parse_columns(payload: &str) -> Result<RawColumns, String> {
    let value: serde_json::Value = serde_json::from_str(payload)
        .map_err(|error| format!("数据集 BarFrame JSON 无效: {error}"))?;
    let text = |key: &str| {
        value
            .get(key)
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let ts = u64_column(&value, "ts")?;
    let open_raw = i128_column(&value, "open_raw")?;
    let high_raw = i128_column(&value, "high_raw")?;
    let low_raw = i128_column(&value, "low_raw")?;
    let close_raw = i128_column(&value, "close_raw")?;
    let volume_raw = i128_column(&value, "volume_raw")?;
    let ragged = [&open_raw, &high_raw, &low_raw, &close_raw, &volume_raw]
        .iter()
        .any(|column| column.len() != ts.len());
    if ts.is_empty() || ragged {
        let rows = ts.len();
        return Err(format!("数据集 BarFrame 各列长度不一致或为空: ts={rows}"));
    }
    let instrument = text("instrument");
    if instrument.trim().is_empty() {
        return Err("数据集 BarFrame instrument 不能为空".into());
    }
    Ok(RawColumns {
        schema_version: value
            .get("schema_version")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0),
        instrument,
        source: text("source"),
        ts,
        open_raw,
        high_raw,
        low_raw,
        close_raw,
        volume_raw,
    })
}

/// 把宽松列还原成数据集侧的逐条记录，好交给仓库唯一的规范指纹函数。
fn data_bars(raw: &RawColumns) -> Vec<Bar> {
    let instrument = raw.instrument.clone();
    (0..raw.ts.len())
        .map(|i| Bar {
            instrument: instrument.clone(),
            timestamp: raw.ts[i],
            open_raw: raw.open_raw[i],
            high_raw: raw.high_raw[i],
            low_raw: raw.low_raw[i],
            close_raw: raw.close_raw[i],
            volume_raw: raw.volume_raw[i],
        })
        .collect()
}

/// 只按时间列诊断连续性：乱序/重复各计一行；间隔超过 `interval_ms` 时中间缺席的槽位（向上取整）计入 `missing_intervals`。
fn assess_frame(ts: &[u64], interval_ms: u64) -> Result<FrameQuality, String> {
    if ts.is_empty() {
        return Err("数据集 BarFrame 时间列为空".into());
    }
    if interval_ms == 0 {
        return Err("interval-ms 必须为正整数".into());
    }
    let mut duplicate_rows = 0u64;
    let mut out_of_order_rows = 0u64;
    let mut missing_intervals = 0u64;
    for window in ts.windows(2) {
        let (previous, current) = (window[0], window[1]);
        if current < previous {
            out_of_order_rows += 1;
        } else if current == previous {
            duplicate_rows += 1;
        } else {
            missing_intervals += (current - previous).div_ceil(interval_ms) - 1;
        }
    }
    Ok(FrameQuality {
        row_count: ts.len() as u64,
        duplicate_rows,
        out_of_order_rows,
        missing_intervals,
    })
}

/// `output` 已存在时的读侧：同一身份不容两种内容。逐字段相等即幂等通过；数据变了而身份没变、
/// 或位置上的文件不是 `DatasetManifestV2`，都当场拒绝——那个路径被声明为清单落点。
fn verify_existing_manifest(path: &Path, expected: &DatasetManifestV2) -> Result<(), AppError> {
    let payload = std::fs::read_to_string(path).map_err(|error| {
        AppError::from_io(
            &format!("读取已有数据集清单失败 {}", path.display()),
            &error,
        )
    })?;
    let existing = DatasetManifestV2::from_json(&payload).map_err(|error| {
        AppError::new(
            AppErrorCategory::Conflict,
            format!(
                "输出位置已有文件但不是可读的 DatasetManifestV2（{}），拒绝覆盖: {error}",
                path.display()
            ),
        )
    })?;
    if existing == *expected {
        return Ok(());
    }
    let mut drift = Vec::new();
    if existing.identity() != expected.identity() {
        drift.push(format!(
            "identity {} → {}",
            existing.identity(),
            expected.identity()
        ));
    }
    for (label, left, right) in [
        (
            "dataset.fingerprint",
            &existing.dataset.fingerprint,
            &expected.dataset.fingerprint,
        ),
        ("timezone", &existing.timezone, &expected.timezone),
    ] {
        if left != right {
            drift.push(format!("{label} {left} → {right}"));
        }
    }
    let (left, right) = (
        existing.quality_report.missing_intervals,
        expected.quality_report.missing_intervals,
    );
    if left != right {
        drift.push(format!("missing_intervals {left} → {right}"));
    }
    if existing.quality_report.interval_ms != expected.quality_report.interval_ms {
        drift.push(format!(
            "interval_ms {:?} → {:?}",
            existing.quality_report.interval_ms, expected.quality_report.interval_ms
        ));
    }
    let detail = if drift.is_empty() {
        "差异不在被点名的字段内（清单其余字段发生了变化）".to_string()
    } else {
        drift.join("；")
    };
    Err(AppError::new(
        AppErrorCategory::Conflict,
        format!(
            "数据集清单已存在且与本次诊断不一致，拒绝覆盖 {}: {detail}；如需重写请先删除该文件",
            path.display()
        ),
    ))
}

/// 规范指纹优先、退化指纹兜底：返回 (指纹, 退化原因)。退化必须说清原因，否则同一个
/// `dataset_id@version` 会在两个命令里给出两个对不上的指纹。
fn dataset_fingerprint(raw: &RawColumns, content_hash: &str) -> (String, Option<String>) {
    match qx_data::fingerprint_bars(&data_bars(raw)) {
        Ok(fingerprint) => (fingerprint, None),
        Err(reason) => (
            content_hash.to_string(),
            Some(format!(
                "数据无法生成规范指纹（{reason}），dataset.fingerprint 退化为文件内容 sha256；\
                 该指纹与 dataset-ingest / 回测链不可比，先修数据再重新诊断"
            )),
        ),
    }
}

fn run_data_validate(
    spec: &DatasetManifestValidationSpec,
) -> Result<DatasetManifestValidationResult, AppError> {
    spec.validate()?;
    let payload = std::fs::read_to_string(&spec.frame_path).map_err(|error| {
        AppError::from_io(
            &format!("读取数据集 BarFrame {} 失败", spec.frame_path),
            &error,
        )
    })?;
    let raw = parse_columns(&payload)
        .map_err(|error| AppError::new(AppErrorCategory::InvalidInput, error))?;
    if raw.schema_version > u64::from(BAR_FRAME_SCHEMA_VERSION) {
        return Err(AppError::new(AppErrorCategory::InvalidInput, format!(
            "不支持的 BarFrame schema_version={}（本构建支持 0 旧格式、1..={BAR_FRAME_SCHEMA_VERSION}）",
            raw.schema_version
        )));
    }
    let quality = assess_frame(&raw.ts, spec.interval_ms)
        .map_err(|error| AppError::new(AppErrorCategory::InvalidInput, error))?;
    let content_hash = qx_strategy::sha256_hex(payload.as_bytes());
    let (fingerprint, degraded) = dataset_fingerprint(&raw, &content_hash);
    let source = if raw.source.trim().is_empty() {
        "barframe-json".to_string()
    } else {
        raw.source.clone()
    };
    // 清单要求 `1 <= start <= end`，而脏数据可能让首尾乱序，所以取最小/最大而不是首/尾。
    let start = raw.ts.iter().copied().min().unwrap_or(1).max(1);
    let end = raw.ts.iter().copied().max().unwrap_or(start).max(start);
    let manifest = DatasetManifestV2 {
        manifest_version: DATASET_MANIFEST_V2_SCHEMA_VERSION,
        dataset: DatasetManifest {
            dataset_id: spec.dataset_id.clone(),
            version: spec.version.clone(),
            source: source.clone(),
            fingerprint,
            schema_version: raw.schema_version.max(1) as u32,
            start_timestamp: start,
            end_timestamp: end,
        },
        instrument: raw.instrument.clone(),
        timezone: spec.timezone.clone(),
        tier: DatasetTier::Bar,
        // provider 版本直接引用回测链登记的那一个契约标签，不在这里另铸一个近似字符串。
        provider_version: BARFRAME_DATASET_VERSION.to_string(),
        content_hash: content_hash.clone(),
        quality_report: DatasetQualityReport {
            row_count: quality.row_count,
            duplicate_rows: quality.duplicate_rows,
            out_of_order_rows: quality.out_of_order_rows,
            missing_intervals: quality.missing_intervals,
            interval_ms: Some(spec.interval_ms),
            timezone: spec.timezone.clone(),
            // 本入口只读行情文件，看不到公司行为组件；覆盖与否如实记 false，不替别的链背书。
            corporate_action_coverage: false,
            usable_tiers: vec![DatasetTier::Bar.as_str().to_string()],
        },
        source_lineage: DatasetSourceLineage {
            primary_source: source,
            backup_sources: Vec::new(),
        },
    };
    manifest
        .validate()
        .map_err(|error| AppError::new(AppErrorCategory::InvalidInput, error.to_string()))?;
    let manifest_json = manifest
        .to_json()
        .map_err(|error| AppError::new(AppErrorCategory::InternalInvariant, error.to_string()))?;
    // 落点已存在就走读侧（校验），不存在才写：既不静默覆盖可能被别的链引用的清单，也让
    // 本入口写出的清单在同一入口里有真实读者，而不是一个只写不读的死端。
    let output_path = Path::new(&spec.output_path);
    let verified = output_path.exists();
    if verified {
        verify_existing_manifest(output_path, &manifest)?;
    } else {
        sink::publish(output_path, &manifest_json, sink::PublishMode::Overwrite).map_err(
            |error| match error {
                sink::PublishError::Io(error) => AppError::from_io(
                    &format!("发布数据集清单失败 {}", output_path.display()),
                    &error,
                ),
                // Overwrite 档不冲突；真出现就是原语被改坏，按冲突报比按存储故障报诚实。
                sink::PublishError::Conflict(message) => {
                    AppError::new(AppErrorCategory::Conflict, message)
                }
            },
        )?;
    }
    Ok(DatasetManifestValidationResult {
        manifest,
        manifest_json,
        verified_existing: verified,
        degraded_fingerprint: degraded,
    })
}

/// Diagnose a possibly irregular BarFrame and publish or verify its versioned dataset manifest.
pub fn validate_dataset_manifest(
    spec: &DatasetManifestValidationSpec,
    context: &RunContext,
) -> Result<DatasetManifestValidationResult, AppError> {
    context.require(CallerCapability::Research, "数据集质量诊断")?;
    let correlation_id = context.correlation_id().to_string();
    guard_panics(&correlation_id, || run_data_validate(spec))
        .map_err(|error| attach(error, &correlation_id))
}
