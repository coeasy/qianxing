//! `data-validate-depth`: diagnose depth snapshot cadence and publish a DatasetManifestV2.

use super::{attach, guard::guard_panics};
use crate::artifact_store::sink;
use crate::context::{CallerCapability, RunContext};
use crate::error::{AppError, AppErrorCategory};
use crate::spec::DepthDatasetManifestValidationSpec;
use qx_data::{
    DatasetManifest, DatasetManifestV2, DatasetQualityReport, DatasetSourceLineage,
    DATASET_MANIFEST_V2_SCHEMA_VERSION, DEPTHFRAME_DATASET_VERSION,
};
use qx_xingban::DepthFrame;
use std::path::Path;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DepthDatasetManifestValidationResult {
    pub manifest: DatasetManifestV2,
    pub manifest_json: String,
    pub verified_existing: bool,
}

fn run(
    spec: &DepthDatasetManifestValidationSpec,
) -> Result<DepthDatasetManifestValidationResult, AppError> {
    spec.validate()?;
    let payload = std::fs::read_to_string(&spec.frame_path).map_err(|error| {
        AppError::from_io(&format!("读取深度数据帧 {} 失败", spec.frame_path), &error)
    })?;
    let frame = DepthFrame::from_json(&payload)
        .map_err(|error| AppError::new(AppErrorCategory::InvalidInput, error))?;
    let mut duplicate_rows = 0_u64;
    let mut out_of_order_rows = 0_u64;
    let mut missing_intervals = 0_u64;
    for pair in frame.snapshots.windows(2) {
        if pair[1].ts < pair[0].ts {
            out_of_order_rows += 1;
        } else if pair[1].ts == pair[0].ts {
            duplicate_rows += 1;
        } else {
            missing_intervals = missing_intervals
                .checked_add((pair[1].ts - pair[0].ts).div_ceil(spec.interval_ms) - 1)
                .ok_or_else(|| {
                    AppError::new(AppErrorCategory::InvalidInput, "深度数据缺口数溢出")
                })?;
        }
    }
    let start = frame
        .snapshots
        .iter()
        .map(|item| item.ts)
        .min()
        .unwrap_or(1)
        .max(1);
    let end = frame
        .snapshots
        .iter()
        .map(|item| item.ts)
        .max()
        .unwrap_or(start)
        .max(start);
    let content_hash = qx_data::sha256_hex_bytes(payload.as_bytes());
    let source = frame.source.clone();
    let manifest = DatasetManifestV2 {
        manifest_version: DATASET_MANIFEST_V2_SCHEMA_VERSION,
        dataset: DatasetManifest {
            dataset_id: format!("depth-bars:{}", frame.instrument),
            version: DEPTHFRAME_DATASET_VERSION.to_owned(),
            source: source.clone(),
            fingerprint: format!("{:016x}", frame.input_hash()),
            schema_version: frame.schema_version,
            start_timestamp: start,
            end_timestamp: end,
        },
        instrument: frame.instrument.to_string(),
        timezone: spec.timezone.clone(),
        tier: spec.tier,
        provider_version: DEPTHFRAME_DATASET_VERSION.to_owned(),
        content_hash,
        quality_report: DatasetQualityReport {
            row_count: frame.snapshots.len() as u64,
            duplicate_rows,
            out_of_order_rows,
            missing_intervals,
            interval_ms: Some(spec.interval_ms),
            timezone: spec.timezone.clone(),
            corporate_action_coverage: false,
            usable_tiers: vec![spec.tier.as_str().to_owned()],
        },
        source_lineage: DatasetSourceLineage {
            primary_source: source,
            backup_sources: Vec::new(),
        },
    };
    manifest
        .validate()
        .map_err(|error| AppError::new(AppErrorCategory::InvalidInput, error))?;
    let manifest_json = manifest
        .to_json()
        .map_err(|error| AppError::new(AppErrorCategory::InternalInvariant, error))?;
    let output = Path::new(&spec.output_path);
    let verified_existing = output.exists();
    if verified_existing {
        let existing = std::fs::read_to_string(output).map_err(|error| {
            AppError::from_io(
                &format!("读取已有深度清单 {} 失败", output.display()),
                &error,
            )
        })?;
        let existing = DatasetManifestV2::from_json(&existing).map_err(|error| {
            AppError::new(
                AppErrorCategory::Conflict,
                format!("输出位置已有文件但不是可读 DatasetManifestV2，拒绝覆盖: {error}"),
            )
        })?;
        if existing != manifest {
            return Err(AppError::new(
                AppErrorCategory::Conflict,
                format!(
                    "深度数据清单与本次诊断不一致，拒绝覆盖 {}",
                    output.display()
                ),
            ));
        }
    } else {
        sink::publish(output, &manifest_json, sink::PublishMode::Overwrite).map_err(|error| {
            match error {
                sink::PublishError::Io(error) => AppError::from_io(
                    &format!("发布深度数据清单 {} 失败", output.display()),
                    &error,
                ),
                // Overwrite 档不冲突；真出现就是原语被改坏，按冲突报比按存储故障报诚实。
                sink::PublishError::Conflict(message) => {
                    AppError::new(AppErrorCategory::Conflict, message)
                }
            }
        })?;
    }
    Ok(DepthDatasetManifestValidationResult {
        manifest,
        manifest_json,
        verified_existing,
    })
}

pub fn validate_depth_dataset_manifest(
    spec: &DepthDatasetManifestValidationSpec,
    context: &RunContext,
) -> Result<DepthDatasetManifestValidationResult, AppError> {
    context.require(CallerCapability::Research, "深度数据集质量诊断")?;
    let correlation_id = context.correlation_id().to_string();
    guard_panics(&correlation_id, || run(spec)).map_err(|error| attach(error, &correlation_id))
}
