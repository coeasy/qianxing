//! `data-validate`：把一份 BarFrame JSON 诊断成 `DatasetManifestV2`，且**不改动源文件**。
//!
//! `frame` 只读；`output` 不存在就写出清单、已存在就改走读侧复核（同一身份不容两种内容），
//! 所以它既能"先注册再核对"，也不会静默覆盖别的链在用的清单。
//!
//! 它必须容忍脏数据——乱序、重复、缺口恰恰是要报告的东西，而严格读侧把"时间轴严格递增"当
//! 启动硬拒，复用它只会得到一句拒绝、`DatasetQualityReport` 无从生成；故走自己的宽松解析。
//! 指纹只有一条口径：能算出**规范指纹**（`qx_data::fingerprint_bars`，与 dataset-ingest 和回测链
//! 同一个函数）就用它；脏到算不出时退化为文件内容 sha256 并当场告警（规划 §7 P3 / §6.2）。

use super::read_example_json;
use crate::data_validate_args::DatasetValidateArgs;
use crate::BARFRAME_DATASET_VERSION;
use qx_data::{
    Bar, DatasetManifest, DatasetManifestV2, DatasetQualityReport, DatasetSourceLineage,
    DatasetTier, BAR_FRAME_SCHEMA_VERSION, DATASET_MANIFEST_V2_SCHEMA_VERSION,
};
use std::fmt::Write as _;
use std::path::Path;

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
fn verify_existing_manifest(path: &Path, expected: &DatasetManifestV2) -> Result<(), String> {
    let payload = std::fs::read_to_string(path)
        .map_err(|error| format!("读取已有数据集清单失败 {}: {error}", path.display()))?;
    let existing = DatasetManifestV2::from_json(&payload).map_err(|error| {
        format!(
            "输出位置已有文件但不是可读的 DatasetManifestV2（{}），拒绝覆盖: {error}",
            path.display()
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
    let detail = if drift.is_empty() {
        "差异不在被点名的字段内（清单其余字段发生了变化）".to_string()
    } else {
        drift.join("；")
    };
    Err(format!(
        "数据集清单已存在且与本次诊断不一致，拒绝覆盖 {}: {detail}；如需重写请先删除该文件",
        path.display()
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
            dataset_id: args.dataset_id.clone(),
            version: args.version.clone(),
            source: source.clone(),
            fingerprint,
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
    // 落点已存在就走读侧（校验），不存在才写：既不静默覆盖可能被别的链引用的清单，也让
    // 本入口写出的清单在同一入口里有真实读者，而不是一个只写不读的死端。
    let verified = args.output.exists();
    if verified {
        verify_existing_manifest(&args.output, &manifest)?;
    } else {
        std::fs::write(&args.output, &json)
            .map_err(|error| format!("写入数据集清单失败 {}: {error}", args.output.display()))?;
    }
    if args.json {
        println!("{json}");
    } else {
        let mut line = String::new();
        let _ = write!(
            line,
            "[Data · Validate] dataset={} instrument={} rows={} duplicate={} out_of_order={} missing={} verdict={} dataset_fingerprint={} output={}",
            manifest.identity(),
            raw.instrument,
            quality.row_count,
            quality.duplicate_rows,
            quality.out_of_order_rows,
            quality.missing_intervals,
            if verified { "已复核一致" } else { "已写出" },
            // 印数据集指纹（与 dataset-ingest / 回测链可比的那一个），不是清单自摘要。
            manifest.dataset.fingerprint,
            args.output.display()
        );
        println!("{line}");
    }
    if let Some(reason) = &degraded {
        eprintln!("[Data · Validate · 警告] {reason}");
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

    const CLEAN: &str = r#"{"instrument":"000001.SZSE","source":"akshare","ts":[1000,2000,3000],"open_raw":[10,20,30],"high_raw":[11,21,31],"low_raw":[9,19,29],"close_raw":[10,20,30],"volume_raw":[1,2,3]}"#;
    /// 脏序列：2000 重复一行，2000 与 4000 之间缺 1 格。
    const DIRTY: &str = r#"{"schema_version":1,"instrument":"000001.SZSE","source":"akshare","ts":[1000,2000,2000,4000],"open_raw":[10,20,20,40],"high_raw":[11,21,21,41],"low_raw":[9,19,19,39],"close_raw":[10,20,20,40],"volume_raw":[1,2,2,4]}"#;
    /// 重复时间戳：规范指纹过不了严格递增校验，只能走退化分支。
    const DUPLICATED_TS: &str = r#"{"instrument":"000001.SZSE","ts":[1000,1000],"open_raw":[10,10],"high_raw":[11,11],"low_raw":[9,9],"close_raw":[10,10],"volume_raw":[1,1]}"#;
    /// 参差列：open_raw 少一格。
    const RAGGED: &str = r#"{"instrument":"000001.SZSE","ts":[1000,2000],"open_raw":[10],"high_raw":[11,21],"low_raw":[9,19],"close_raw":[10,20],"volume_raw":[1,2]}"#;

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
        assert_eq!((quality.row_count, quality.duplicate_rows), (4, 0));
        assert_eq!(quality.out_of_order_rows, 0);
        assert_eq!(quality.missing_intervals, 0);
    }

    #[test]
    fn out_of_order_duplicate_and_gaps_are_counted_separately() {
        // 2000→2000 重复一行；3000→2500 乱序一行；2500→5500 缺 2 格（间隔 3000 = 3×1000）。
        let quality = assess_frame(&[1000, 2000, 2000, 3000, 2500, 5500], 1000).unwrap();
        assert_eq!((quality.row_count, quality.duplicate_rows), (6, 1));
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
        let raw = parse_columns(DIRTY).unwrap();
        assert_eq!(raw.schema_version, 1);
        assert_eq!(raw.ts, vec![1000, 2000, 2000, 4000]);
        assert_eq!(raw.volume_raw, vec![1, 2, 2, 4]);
    }

    #[test]
    fn parse_columns_rejects_ragged_and_non_integer_columns() {
        assert!(parse_columns(RAGGED).is_err());
        assert!(parse_columns(
            r#"{"instrument":"x","ts":[1000,2000],"open_raw":[10,20],"high_raw":[11,21],"low_raw":[9,19],"close_raw":[10,20],"volume_raw":[1,"2"]}"#
        )
        .is_err());
        assert!(parse_columns(r#"{"instrument":"x","ts":[1000],"open_raw":[10]}"#).is_err());
    }

    /// 干净数据必须与 dataset-ingest / 回测链算出**同一个**规范指纹；算不出时必须退化并说明原因。
    #[test]
    fn fingerprint_prefers_the_canonical_one_and_degrades_with_a_reason() {
        let clean = parse_columns(CLEAN).unwrap();
        let (fingerprint, degraded) = dataset_fingerprint(&clean, "content-hash");
        assert_eq!(degraded, None, "干净数据不应退化");
        assert_eq!(
            fingerprint,
            qx_data::fingerprint_bars(&data_bars(&clean)).unwrap()
        );
        let dirty = parse_columns(DUPLICATED_TS).unwrap();
        assert!(qx_data::fingerprint_bars(&data_bars(&dirty)).is_err());
        let (fingerprint, degraded) = dataset_fingerprint(&dirty, "content-hash");
        assert_eq!(fingerprint, "content-hash");
        assert!(degraded.unwrap().contains("退化为文件内容 sha256"));
    }

    #[test]
    fn validate_writes_manifest_and_tolerates_dirty_series() {
        let path = frame_path("dirty", DIRTY);
        let output = path.with_file_name("manifest.json");
        validate_dataset(&args_for(&path, output.clone())).unwrap();
        let manifest = DatasetManifestV2::from_json(&std::fs::read_to_string(&output).unwrap())
            .expect("落盘的清单必须能被同一套规格读回");
        assert_eq!(manifest.identity(), "ashare.000001@snapshot-1");
        assert_eq!(manifest.quality_report.row_count, 4);
        assert_eq!(manifest.quality_report.duplicate_rows, 1);
        assert_eq!(manifest.quality_report.missing_intervals, 1);
        assert!(!manifest.quality_report.corporate_action_coverage);
        assert_eq!(manifest.dataset.start_timestamp, 1000);
        assert_eq!(manifest.dataset.end_timestamp, 4000);
        // 源文件必须原样不动：本入口只诊断，不改写输入。
        let written = std::fs::read_to_string(&path).unwrap();
        assert!(written.contains("\"ts\":[1000,2000,2000,4000]"));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn validate_rejects_column_mismatch() {
        let path = frame_path("ragged", RAGGED);
        let output = path.with_file_name("manifest.json");
        assert!(validate_dataset(&args_for(&path, output)).is_err());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn validate_is_idempotent_then_refuses_drift_on_the_same_identity() {
        let path = frame_path("idempotent", CLEAN);
        let output = path.with_file_name("manifest.json");
        let args = args_for(&path, output.clone());
        validate_dataset(&args).unwrap();
        let first = std::fs::read_to_string(&output).unwrap();
        // 重跑同一份数据：逐字段相等，走复核而非覆盖，内容不变。
        validate_dataset(&args).unwrap();
        assert_eq!(std::fs::read_to_string(&output).unwrap(), first);
        // 数据变了、身份没变：拒绝，且已存在的清单不被改写。
        std::fs::write(
            &path,
            r#"{"instrument":"000001.SZSE","source":"akshare","ts":[1000,2000,4000],"open_raw":[10,20,40],"high_raw":[11,21,41],"low_raw":[9,19,39],"close_raw":[10,20,40],"volume_raw":[1,2,4]}"#,
        )
        .unwrap();
        let error = validate_dataset(&args).unwrap_err();
        assert!(error.contains("拒绝覆盖"), "错误应点名拒绝覆盖: {error}");
        assert!(
            error.contains("missing_intervals"),
            "错误应点名漂移字段: {error}"
        );
        assert_eq!(std::fs::read_to_string(&output).unwrap(), first);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn validate_refuses_to_clobber_a_non_manifest_output() {
        let path = frame_path("clobber", CLEAN);
        let output = path.with_file_name("manifest.json");
        std::fs::write(&output, "{\"not\":\"a manifest\"}").unwrap();
        let error = validate_dataset(&args_for(&path, output.clone())).unwrap_err();
        assert!(error.contains("拒绝覆盖"), "非清单文件不得被覆盖: {error}");
        assert_eq!(
            std::fs::read_to_string(&output).unwrap(),
            "{\"not\":\"a manifest\"}"
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
