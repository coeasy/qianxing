//! Bind owner-declared dataset semantics to the exact bytes named by a v2 manifest.

use super::{attach, guard::guard_panics};
use crate::artifact_store::sink::{self, PublishMode};
use crate::context::{CallerCapability, RunContext};
use crate::error::{AppError, AppErrorCategory};
use crate::spec::DatasetSnapshotPublicationSpec;
use qx_data::{
    DatasetCoverageGapV1, DatasetManifestV2, DatasetSnapshotContextV1, DatasetSnapshotV1,
    DatasetTier,
};
use std::path::Path;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DatasetSnapshotPublicationResult {
    pub snapshot: DatasetSnapshotV1,
    pub snapshot_sha256: String,
    pub output_path: String,
    pub verified_existing: bool,
}

fn run(
    spec: &DatasetSnapshotPublicationSpec,
) -> Result<DatasetSnapshotPublicationResult, AppError> {
    spec.validate()?;
    let data = std::fs::read(&spec.data_path).map_err(|error| {
        AppError::from_io(&format!("读取冻结数据 {} 失败", spec.data_path), &error)
    })?;
    let manifest_payload = std::fs::read_to_string(&spec.manifest_path).map_err(|error| {
        AppError::from_io(
            &format!("读取 DatasetManifestV2 {} 失败", spec.manifest_path),
            &error,
        )
    })?;
    let manifest = DatasetManifestV2::from_json(&manifest_payload)
        .map_err(|error| AppError::new(AppErrorCategory::InvalidInput, error))?;
    let actual_content_hash = qx_data::sha256_hex_bytes(&data);
    if manifest.content_hash != actual_content_hash {
        return Err(AppError::new(
            AppErrorCategory::Conflict,
            format!(
                "DatasetManifestV2 content_hash 与冻结数据不一致: manifest={} actual={}",
                manifest.content_hash, actual_content_hash
            ),
        ));
    }
    let context_payload = std::fs::read_to_string(&spec.context_path).map_err(|error| {
        AppError::from_io(
            &format!("读取 DatasetSnapshotContextV1 {} 失败", spec.context_path),
            &error,
        )
    })?;
    let context: DatasetSnapshotContextV1 =
        serde_json::from_str(&context_payload).map_err(|error| {
            AppError::new(
                AppErrorCategory::InvalidInput,
                format!("DatasetSnapshotContextV1 JSON 无效: {error}"),
            )
        })?;
    validate_snapshot_data(&data, &manifest, &context, &spec.data_path)?;
    let snapshot = DatasetSnapshotV1::from_manifest_and_context(manifest, context)
        .map_err(|error| AppError::new(AppErrorCategory::InvalidInput, error))?;
    let snapshot_json = serde_json::to_string_pretty(&snapshot).map_err(|error| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            format!("DatasetSnapshotV1 序列化失败: {error}"),
        )
    })?;
    let output = Path::new(&spec.output_path);
    let verified_existing = output.exists();
    sink::publish(output, &snapshot_json, PublishMode::Idempotent).map_err(
        |error| match error {
            sink::PublishError::Conflict(message) => {
                AppError::new(AppErrorCategory::Conflict, message)
            }
            sink::PublishError::Io(error) => AppError::from_io(
                &format!("发布 DatasetSnapshotV1 {} 失败", output.display()),
                &error,
            ),
        },
    )?;
    Ok(DatasetSnapshotPublicationResult {
        snapshot_sha256: snapshot
            .content_sha256()
            .map_err(|error| AppError::new(AppErrorCategory::InternalInvariant, error))?,
        snapshot,
        output_path: output.to_string_lossy().into_owned(),
        verified_existing,
    })
}

pub(crate) fn validate_snapshot_data(
    payload: &[u8],
    manifest: &DatasetManifestV2,
    context: &DatasetSnapshotContextV1,
    data_path: &str,
) -> Result<(), AppError> {
    let text = std::str::from_utf8(payload).map_err(|error| {
        AppError::new(
            AppErrorCategory::InvalidInput,
            format!("数据帧不是 UTF-8 JSON {data_path}: {error}"),
        )
    })?;
    let (instrument, schema_version, source, timestamps, rows, expected_tier, expected_fingerprint) =
        match manifest.tier {
            DatasetTier::Bar => {
                let frame = qx_datastruct::BarFrame::from_json(text).map_err(|error| {
                    AppError::new(
                        AppErrorCategory::InvalidInput,
                        format!("BarFrame JSON 无效 {data_path}: {error:?}"),
                    )
                })?;
                let bars = (0..frame.ts.len())
                    .map(|index| qx_data::Bar {
                        instrument: frame.instrument.to_string(),
                        timestamp: frame.ts[index],
                        open_raw: frame.open_raw[index],
                        high_raw: frame.high_raw[index],
                        low_raw: frame.low_raw[index],
                        close_raw: frame.close_raw[index],
                        volume_raw: frame.volume_raw[index],
                        exact_ohlcv: frame.exact_ohlcv.as_ref().map(|values| values[index]),
                    })
                    .collect::<Vec<_>>();
                let fingerprint = qx_data::fingerprint_bars(&bars)
                    .map_err(|error| AppError::new(AppErrorCategory::InvalidInput, error))?;
                (
                    frame.instrument.to_string(),
                    if frame.exact_ohlcv.is_some() { 2 } else { 1 },
                    frame.source.0,
                    frame.ts,
                    bars.len() as u64,
                    DatasetTier::Bar,
                    fingerprint,
                )
            }
            DatasetTier::L1 | DatasetTier::L2 => {
                let frame = qx_xingban::DepthFrame::from_json(text).map_err(|error| {
                    AppError::new(
                        AppErrorCategory::InvalidInput,
                        format!("DepthFrame JSON 无效 {data_path}: {error}"),
                    )
                })?;
                (
                    frame.instrument.to_string(),
                    frame.schema_version,
                    frame.source.clone(),
                    frame.snapshots.iter().map(|item| item.ts).collect(),
                    frame.snapshots.len() as u64,
                    manifest.tier,
                    format!("{:016x}", frame.input_hash()),
                )
            }
            DatasetTier::L3 => {
                return Err(AppError::new(
                    AppErrorCategory::InvalidInput,
                    "DatasetSnapshot publisher currently requires Bar, L1 or L2 input",
                ));
            }
        };
    let (Some(first), Some(last)) = (timestamps.first(), timestamps.last()) else {
        return Err(AppError::new(
            AppErrorCategory::InvalidInput,
            "DatasetSnapshot cannot bind an empty data frame",
        ));
    };
    if timestamps.windows(2).any(|pair| pair[1] <= pair[0]) {
        return Err(AppError::new(
            AppErrorCategory::InvalidInput,
            format!("DatasetSnapshot requires strictly increasing timestamps: {data_path}"),
        ));
    }
    if manifest.instrument != instrument
        || manifest.dataset.schema_version != schema_version
        || manifest.dataset.source != source
        || manifest.dataset.fingerprint != expected_fingerprint
        || manifest.dataset.start_timestamp != *first
        || manifest.dataset.end_timestamp != *last
        || manifest.quality_report.row_count != rows
        || manifest.quality_report.interval_ms != Some(context.frequency_ms)
        || manifest.tier != expected_tier
        || manifest.quality_report.duplicate_rows != 0
        || manifest.quality_report.out_of_order_rows != 0
    {
        return Err(AppError::new(
            AppErrorCategory::Conflict,
            format!("DatasetManifestV2 与数据帧不匹配或时间轴不适合快照: {data_path}"),
        ));
    }
    let gaps = timestamps
        .windows(2)
        .filter_map(|pair| {
            (pair[1] - pair[0] > context.frequency_ms).then_some(DatasetCoverageGapV1 {
                start_ts_ms: pair[0].saturating_add(context.frequency_ms),
                end_ts_ms: pair[1],
            })
        })
        .collect::<Vec<_>>();
    let missing = gaps.iter().try_fold(0_u64, |total, gap| {
        total.checked_add((gap.end_ts_ms - gap.start_ts_ms).div_ceil(context.frequency_ms))
    });
    if gaps != context.gaps || missing != Some(manifest.quality_report.missing_intervals) {
        return Err(AppError::new(
            AppErrorCategory::Conflict,
            format!("DatasetSnapshotContextV1 gaps 与数据帧实况不一致: {data_path}"),
        ));
    }
    Ok(())
}

pub fn publish_dataset_snapshot(
    spec: &DatasetSnapshotPublicationSpec,
    context: &RunContext,
) -> Result<DatasetSnapshotPublicationResult, AppError> {
    context.require(CallerCapability::Research, "发布 DatasetSnapshotV1")?;
    let correlation_id = context.correlation_id().to_string();
    guard_panics(&correlation_id, || run(spec)).map_err(|error| attach(error, &correlation_id))
}
