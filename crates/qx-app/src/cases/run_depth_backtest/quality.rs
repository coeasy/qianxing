//! 独立复核深度帧 `DatasetManifestV2` 及其质量统计。
//!
//! 自 WP-2 起从 CLI 搬进用例：质量清单是**输入的一部分**，它的核对必须与"跑不跑"在同一个
//! 地方判——留在门面里，CLI 会核、HTTP/Python 不核，同一条 `quality_manifest_path` 在三个
//! 入口上就是三种含义（§6 第 7 条：不用"门禁全绿"替代"产物可复算"）。
//!
//! 核对口径与原实现逐条一致：数据集身份、版本、指纹、内容哈希、标的、档位、样本数与时间范围
//! 全部按本帧**重算**后再比，缺口/重复/乱序也重算——清单是声明，重算是实况，两者不符就拒。

use crate::error::{AppError, AppErrorCategory};
use qx_xingban::DepthFrame;
use std::path::Path;

/// 把清单读开、按本帧重算并逐项比对，返回摘要要写的那个 `dataset_quality` 块。
pub(super) fn verified_depth_dataset_quality(
    path: &Path,
    frame_path: &Path,
    frame: &DepthFrame,
    expected_tier: &str,
) -> Result<serde_json::Value, AppError> {
    let payload = std::fs::read_to_string(path).map_err(|error| {
        AppError::from_io(
            &format!("读取深度数据集质量清单 {}", path.display()),
            &error,
        )
    })?;
    let quality_manifest = qx_data::DatasetManifestV2::from_json(&payload).map_err(|error| {
        AppError::new(
            AppErrorCategory::InvalidInput,
            format!("深度数据集质量清单 JSON 无效 {}: {error}", path.display()),
        )
    })?;
    let source_payload = std::fs::read(frame_path).map_err(|error| {
        AppError::from_io(&format!("读取深度数据帧 {}", frame_path.display()), &error)
    })?;
    let expected_content_hash = qx_data::sha256_hex_bytes(&source_payload);
    let expected_fingerprint = format!("{:016x}", frame.input_hash());
    let declared = &quality_manifest.dataset;
    let expected_id = format!("depth-bars:{}", frame.instrument);
    for (field, actual, expected) in [
        (
            "dataset_id",
            declared.dataset_id.as_str(),
            expected_id.as_str(),
        ),
        (
            "version",
            declared.version.as_str(),
            qx_data::DEPTHFRAME_DATASET_VERSION,
        ),
        (
            "fingerprint",
            declared.fingerprint.as_str(),
            expected_fingerprint.as_str(),
        ),
        (
            "content_hash",
            quality_manifest.content_hash.as_str(),
            expected_content_hash.as_str(),
        ),
    ] {
        if actual != expected {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                format!(
                    "深度数据集质量清单与本次 DepthFrame 不一致: {field} 清单={actual} 实际={expected} ({})",
                    path.display()
                ),
            ));
        }
    }
    let tier = match expected_tier {
        "l1" => qx_data::DatasetTier::L1,
        "l2" => qx_data::DatasetTier::L2,
        other => {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                format!("深度回测档位不支持质量清单: {other}"),
            ))
        }
    };
    let timestamps = frame
        .snapshots
        .iter()
        .map(|snapshot| snapshot.ts)
        .collect::<Vec<_>>();
    let Some(start_timestamp) = timestamps.iter().min() else {
        return Err(AppError::new(
            AppErrorCategory::InvalidInput,
            format!(
                "深度数据集质量清单无法绑定空 DepthFrame ({})",
                path.display()
            ),
        ));
    };
    let Some(end_timestamp) = timestamps.iter().max() else {
        return Err(AppError::new(
            AppErrorCategory::InvalidInput,
            format!(
                "深度数据集质量清单无法绑定空 DepthFrame ({})",
                path.display()
            ),
        ));
    };
    if quality_manifest.instrument != frame.instrument.to_string()
        || quality_manifest.tier != tier
        || quality_manifest.provider_version != qx_data::DEPTHFRAME_DATASET_VERSION
        || declared.schema_version != frame.schema_version
        || quality_manifest.quality_report.row_count != timestamps.len() as u64
        || declared.start_timestamp != *start_timestamp
        || declared.end_timestamp != *end_timestamp
    {
        return Err(AppError::new(
            AppErrorCategory::InvalidInput,
            format!(
                "深度数据集质量清单的标的、档位、版本、样本数或时间范围与本次 DepthFrame 不一致 ({})",
                path.display()
            ),
        ));
    }
    let interval_ms = quality_manifest
        .quality_report
        .interval_ms
        .filter(|interval| *interval > 0)
        .ok_or_else(|| {
            AppError::new(
                AppErrorCategory::InvalidInput,
                format!(
                    "深度数据集质量清单缺少有效 interval_ms ({})",
                    path.display()
                ),
            )
        })?;
    let mut duplicate_rows = 0_u64;
    let mut out_of_order_rows = 0_u64;
    let mut missing_intervals = 0_u64;
    for pair in timestamps.windows(2) {
        if pair[1] < pair[0] {
            out_of_order_rows += 1;
        } else if pair[1] == pair[0] {
            duplicate_rows += 1;
        } else {
            missing_intervals = missing_intervals
                .checked_add((pair[1] - pair[0]).div_ceil(interval_ms) - 1)
                .ok_or_else(|| {
                    AppError::new(
                        AppErrorCategory::InternalInvariant,
                        "复算深度数据缺口数溢出",
                    )
                })?;
        }
    }
    let report = &quality_manifest.quality_report;
    if report.missing_intervals != missing_intervals
        || report.duplicate_rows != duplicate_rows
        || report.out_of_order_rows != out_of_order_rows
        || duplicate_rows != 0
        || out_of_order_rows != 0
    {
        return Err(AppError::new(
            AppErrorCategory::InvalidInput,
            format!(
                "深度数据质量报告与回测输入不符或含不支持的时间轴: missing 清单={} 重算={}，duplicate={}，out_of_order={} ({})",
                report.missing_intervals,
                missing_intervals,
                duplicate_rows,
                out_of_order_rows,
                path.display()
            ),
        ));
    }
    let manifest_fingerprint = quality_manifest.fingerprint().map_err(|error| {
        AppError::new(
            AppErrorCategory::InvalidInput,
            format!("深度数据集质量清单指纹计算失败 {}: {error}", path.display()),
        )
    })?;
    Ok(serde_json::json!({
        "manifest_path": path.to_string_lossy(),
        "manifest_fingerprint": manifest_fingerprint,
        "content_hash": quality_manifest.content_hash,
        "quality_report": quality_manifest.quality_report,
        "manifest": quality_manifest,
    }))
}
