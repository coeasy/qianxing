//! `data-validate`：把一份 BarFrame JSON 诊断成 `DatasetManifestV2`，且**不改动源文件**。
//!
//! 与 `dataset-ingest`（先入库再登记）不同，本入口只读源文件、只写清单，所以它必须能
//! 容忍脏数据——乱序、重复、缺口恰恰是要被报告出来的东西。严格读侧（`qx-datastruct` 的
//! 列式校验与 `qx_data` 的 Provider）都把"时间轴必须严格递增"当成启动前的硬拒；若这里
//! 复用那条严格线，数据一有问题就只会得到一句拒绝，质量报告反而无从生成。因此本入口走
//! 自己的宽松列式解析，把 `DatasetQualityReport` 的四个计数如实填出来，交给使用者决定
//! 补洞还是点名拒绝（规划 §7 P3 / §6.2）。
//!
//! 解析刻意不引入 `serde` 派生：qx-cli 只依赖 `serde_json`，这里用 `Value` 手工取列，
//! 既不为一个诊断入口新增依赖边，也把"哪些格子是必填、哪些要容忍"写在明面上。

use super::read_example_json;
use crate::data_validate_args::DatasetValidateArgs;
use crate::BARFRAME_DATASET_VERSION;
use qx_data::{
    DatasetManifest, DatasetManifestV2, DatasetQualityReport, DatasetSourceLineage, DatasetTier,
    BAR_FRAME_SCHEMA_VERSION, DATASET_MANIFEST_V2_SCHEMA_VERSION,
};
use std::fmt::Write as _;

/// 宽松线格式：只要求六列存在且等长，不要求时间轴递增（与严格读侧有意分叉）。
struct RawColumns {
    schema_version: u64,
    instrument: String,
    source: String,
    ts: Vec<u64>,
}

/// 一份 BarFrame 的连续性问题计数，与 `DatasetQualityReport` 的数值字段一一对应。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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

/// 取一列整数（定点价格/量）；这里只核对形状与长度，数值由诊断链之外的严格读侧负责。
fn integer_column_len(value: &serde_json::Value, key: &str) -> Result<usize, String> {
    let items = column(value, key)?;
    if items
        .iter()
        .any(|item| item.as_i64().is_none() && item.as_u64().is_none())
    {
        return Err(format!("数据集 BarFrame 列 {key} 含非整数元素"));
    }
    Ok(items.len())
}

fn parse_columns(payload: &str) -> Result<RawColumns, String> {
    let value: serde_json::Value = serde_json::from_str(payload)
        .map_err(|error| format!("数据集 BarFrame JSON 无效: {error}"))?;
    let schema_version = value
        .get("schema_version")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let instrument = value
        .get("instrument")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();
    let source = value
        .get("source")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string();
    let ts = u64_column(&value, "ts")?;
    let columns = [
        ts.len(),
        integer_column_len(&value, "open_raw")?,
        integer_column_len(&value, "high_raw")?,
        integer_column_len(&value, "low_raw")?,
        integer_column_len(&value, "close_raw")?,
        integer_column_len(&value, "volume_raw")?,
    ];
    if columns.iter().any(|length| *length != columns[0]) || columns[0] == 0 {
        return Err(format!("数据集 BarFrame 各列长度不一致或为空: {columns:?}"));
    }
    if instrument.trim().is_empty() {
        return Err("数据集 BarFrame instrument 不能为空".into());
    }
    Ok(RawColumns {
        schema_version,
        instrument,
        source,
        ts,
    })
}

/// 只按时间列诊断连续性：乱序、重复各计一行；相邻两行的间隔超过 `interval_ms` 时，
/// 中间缺席的槽位数计入 `missing_intervals`（向上取整，缺一格与缺半格都不算"连续"）。
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
            let expected = (current - previous).div_ceil(interval_ms);
            missing_intervals += expected - 1;
        }
    }
    Ok(FrameQuality {
        row_count: ts.len() as u64,
        duplicate_rows,
        out_of_order_rows,
        missing_intervals,
    })
}

/// 校验一个数据集区间端点：清单要求 `1 <= start <= end`，而脏数据可能让首尾乱序。
fn ordered_range(ts: &[u64]) -> (u64, u64) {
    let min = ts.iter().copied().min().unwrap_or(1).max(1);
    let max = ts.iter().copied().max().unwrap_or(min).max(min);
    (min, max)
}

fn run_data_validate(args: &DatasetValidateArgs) -> Result<(), String> {
    let payload = read_example_json(&args.frame, "数据集 BarFrame ")?;
    let raw = parse_columns(&payload)?;
    if raw.schema_version > u64::from(BAR_FRAME_SCHEMA_VERSION) {
        return Err(format!(
            "不支持的 BarFrame schema_version={}（本构建支持 0 旧格式、1..={BAR_FRAME_SCHEMA_VERSION}）",
            raw.schema_version
        ));
    }
    let quality = assess_frame(&raw.ts, args.interval_ms)?;
    // 内容身份两处都来自同一份字节：`content_hash` 是文件级 sha256，`fingerprint` 复用它，
    // 因为本入口不重排数据——重排会掩盖乱序/重复，正是这份诊断要暴露的东西。
    let content_hash = qx_strategy::sha256_hex(payload.as_bytes());
    let source = if raw.source.trim().is_empty() {
        "barframe-json".to_string()
    } else {
        raw.source.clone()
    };
    let (start, end) = ordered_range(&raw.ts);
    let manifest = DatasetManifestV2 {
        manifest_version: DATASET_MANIFEST_V2_SCHEMA_VERSION,
        dataset: DatasetManifest {
            dataset_id: args.dataset_id.clone(),
            version: args.version.clone(),
            source: source.clone(),
            fingerprint: content_hash.clone(),
            schema_version: raw.schema_version.max(1) as u32,
            start_timestamp: start,
            end_timestamp: end,
        },
        instrument: raw.instrument.clone(),
        timezone: args.timezone.clone(),
        tier: DatasetTier::Bar,
        // provider 版本直接引用回测链登记的那一个契约标签，不在这里另铸一个近似字符串。
        provider_version: BARFRAME_DATASET_VERSION.to_string(),
        content_hash: content_hash.clone(),
        quality_report: DatasetQualityReport {
            row_count: quality.row_count,
            duplicate_rows: quality.duplicate_rows,
            out_of_order_rows: quality.out_of_order_rows,
            missing_intervals: quality.missing_intervals,
            timezone: args.timezone.clone(),
            // 本入口只读行情文件，看不到公司行为组件；覆盖与否如实记 false，不替别的链背书。
            corporate_action_coverage: false,
            usable_tiers: vec![DatasetTier::Bar.as_str().to_string()],
        },
        source_lineage: DatasetSourceLineage {
            primary_source: source,
            backup_sources: Vec::new(),
        },
    };
    manifest.validate()?;
    let json = manifest.to_json()?;
    std::fs::write(&args.output, &json)
        .map_err(|error| format!("写入数据集清单失败 {}: {error}", args.output.display()))?;
    if args.json {
        println!("{json}");
    } else {
        let mut line = String::new();
        let _ = write!(
            line,
            "[Data · Validate] dataset={} instrument={} rows={} duplicate={} out_of_order={} missing={} fingerprint={} output={}",
            manifest.identity(),
            raw.instrument,
            quality.row_count,
            quality.duplicate_rows,
            quality.out_of_order_rows,
            quality.missing_intervals,
            manifest.fingerprint()?,
            args.output.display()
        );
        println!("{line}");
    }
    for warning in manifest.warnings() {
        eprintln!("[Data · Validate · 警告] {warning}");
    }
    Ok(())
}

/// 供派发层调用的唯一入口：解析 → 诊断 → 落一份 `DatasetManifestV2`。
pub(crate) fn validate_dataset(args: &DatasetValidateArgs) -> Result<(), String> {
    run_data_validate(args)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame_path(name: &str, body: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!(
            "qianxing-data-validate-{}-{}-{name}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("bars.json");
        std::fs::write(&path, body).unwrap();
        path
    }

    fn args_for(frame: &std::path::Path, output: std::path::PathBuf) -> DatasetValidateArgs {
        DatasetValidateArgs {
            frame: frame.to_path_buf(),
            dataset_id: "ashare.000001".into(),
            version: "snapshot-1".into(),
            interval_ms: 1000,
            timezone: "Asia/Shanghai".into(),
            output,
            json: false,
        }
    }

    #[test]
    fn clean_series_has_no_findings() {
        let quality = assess_frame(&[1000, 2000, 3000, 4000], 1000).unwrap();
        assert_eq!(
            quality,
            FrameQuality {
                row_count: 4,
                duplicate_rows: 0,
                out_of_order_rows: 0,
                missing_intervals: 0,
            }
        );
    }

    #[test]
    fn out_of_order_duplicate_and_gaps_are_counted_separately() {
        // 2000→2000 重复一行；3000→2500 乱序一行；2500→5500 缺 2 格（间隔 3000 = 3×1000）。
        let quality = assess_frame(&[1000, 2000, 2000, 3000, 2500, 5500], 1000).unwrap();
        assert_eq!(quality.row_count, 6);
        assert_eq!(quality.duplicate_rows, 1);
        assert_eq!(quality.out_of_order_rows, 1);
        assert_eq!(quality.missing_intervals, 2);
    }

    #[test]
    fn partial_interval_gap_rounds_up_to_one_missing_slot() {
        // 1500 的间隔不足两格但超过一格，仍算缺 1 格（"连续"要求恰好在节拍上）。
        let quality = assess_frame(&[1000, 2500], 1000).unwrap();
        assert_eq!(quality.missing_intervals, 1);
    }

    #[test]
    fn rejects_empty_series_and_zero_interval() {
        assert!(assess_frame(&[], 1000).is_err());
        assert!(assess_frame(&[1000], 0).is_err());
    }

    #[test]
    fn parse_columns_tolerates_dirty_series_and_legacy_documents() {
        let raw = parse_columns(
            r#"{"instrument":"000001.SZSE","ts":[1000,2000,2000,4000],"open_raw":[10,20,20,40],"high_raw":[11,21,21,41],"low_raw":[9,19,19,39],"close_raw":[10,20,20,40],"volume_raw":[1,2,2,4]}"#,
        )
        .unwrap();
        assert_eq!(raw.schema_version, 0);
        assert_eq!(raw.ts, vec![1000, 2000, 2000, 4000]);
    }

    #[test]
    fn parse_columns_rejects_ragged_and_non_integer_columns() {
        assert!(parse_columns(
            r#"{"instrument":"x","ts":[1000,2000],"open_raw":[10],"high_raw":[11,21],"low_raw":[9,19],"close_raw":[10,20],"volume_raw":[1,2]}"#
        )
        .is_err());
        assert!(parse_columns(
            r#"{"instrument":"x","ts":[1000,2000],"open_raw":[10,20],"high_raw":[11,21],"low_raw":[9,19],"close_raw":[10,20],"volume_raw":[1,"2"]}"#
        )
        .is_err());
        assert!(parse_columns(r#"{"instrument":"x","ts":[1000],"open_raw":[10]}"#).is_err());
    }

    #[test]
    fn validate_writes_manifest_and_tolerates_dirty_series() {
        let path = frame_path(
            "dirty",
            r#"{"schema_version":1,"instrument":"000001.SZSE","source":"akshare","ts":[1000,2000,2000,4000],"open_raw":[10,20,20,40],"high_raw":[11,21,21,41],"low_raw":[9,19,19,39],"close_raw":[10,20,20,40],"volume_raw":[1,2,2,4]}"#,
        );
        let output = path.with_file_name("manifest.json");
        let args = args_for(&path, output.clone());
        validate_dataset(&args).unwrap();
        let manifest = DatasetManifestV2::from_json(&std::fs::read_to_string(&output).unwrap())
            .expect("落盘的清单必须能被同一套规格读回");
        assert_eq!(manifest.identity(), "ashare.000001@snapshot-1");
        assert_eq!(manifest.quality_report.row_count, 4);
        assert_eq!(manifest.quality_report.duplicate_rows, 1);
        assert_eq!(manifest.quality_report.missing_intervals, 1);
        assert_eq!(manifest.instrument, "000001.SZSE");
        assert_eq!(manifest.dataset.start_timestamp, 1000);
        assert_eq!(manifest.dataset.end_timestamp, 4000);
        assert!(!manifest.quality_report.corporate_action_coverage);
        // 源文件必须原样不动：本入口只诊断，不改写输入。
        let source_after = std::fs::read_to_string(&path).unwrap();
        assert!(source_after.contains("\"ts\":[1000,2000,2000,4000]"));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn validate_rejects_column_mismatch() {
        let path = frame_path(
            "ragged",
            r#"{"instrument":"000001.SZSE","ts":[1000,2000],"open_raw":[10],"high_raw":[11,21],"low_raw":[9,19],"close_raw":[10,20],"volume_raw":[1,2]}"#,
        );
        let output = path.with_file_name("manifest.json");
        assert!(validate_dataset(&args_for(&path, output)).is_err());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn ordered_range_normalizes_dirty_endpoints() {
        assert_eq!(ordered_range(&[5000, 1000, 3000]), (1000, 5000));
        assert_eq!(ordered_range(&[0, 0]), (1, 1));
    }
}
