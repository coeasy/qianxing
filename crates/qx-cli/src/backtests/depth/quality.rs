//! 独立复核深度帧 DatasetManifestV2 及其质量统计。

use qx_xingban::DepthFrame;
use std::path::Path;

pub(crate) fn verified_depth_dataset_quality(
    path: &Path,
    frame_path: &Path,
    frame: &DepthFrame,
    expected_tier: &str,
) -> Result<serde_json::Value, String> {
    let payload = super::super::read_example_json(path, "深度数据集质量清单 v2")?;
    let quality_manifest = qx_data::DatasetManifestV2::from_json(&payload)?;
    let source_payload = std::fs::read(frame_path)
        .map_err(|error| format!("读取深度数据帧 {} 失败: {error}", frame_path.display()))?;
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
            return Err(format!(
                "深度数据集质量清单与本次 DepthFrame 不一致: {field} 清单={actual} 实际={expected} ({})",
                path.display()
            ));
        }
    }
    let tier = match expected_tier {
        "l1" => qx_data::DatasetTier::L1,
        "l2" => qx_data::DatasetTier::L2,
        other => return Err(format!("深度回测档位不支持质量清单: {other}")),
    };
    let timestamps = frame
        .snapshots
        .iter()
        .map(|snapshot| snapshot.ts)
        .collect::<Vec<_>>();
    let Some(start_timestamp) = timestamps.iter().min() else {
        return Err(format!(
            "深度数据集质量清单无法绑定空 DepthFrame ({})",
            path.display()
        ));
    };
    let Some(end_timestamp) = timestamps.iter().max() else {
        return Err(format!(
            "深度数据集质量清单无法绑定空 DepthFrame ({})",
            path.display()
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
        return Err(format!(
            "深度数据集质量清单的标的、档位、版本、样本数或时间范围与本次 DepthFrame 不一致 ({})",
            path.display()
        ));
    }
    let interval_ms = quality_manifest
        .quality_report
        .interval_ms
        .filter(|interval| *interval > 0)
        .ok_or_else(|| {
            format!(
                "深度数据集质量清单缺少有效 interval_ms ({})",
                path.display()
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
                .ok_or_else(|| "复算深度数据缺口数溢出".to_owned())?;
        }
    }
    let report = &quality_manifest.quality_report;
    if report.missing_intervals != missing_intervals
        || report.duplicate_rows != duplicate_rows
        || report.out_of_order_rows != out_of_order_rows
        || duplicate_rows != 0
        || out_of_order_rows != 0
    {
        return Err(format!(
            "深度数据质量报告与回测输入不符或含不支持的时间轴: missing 清单={} 重算={}，duplicate={}，out_of_order={} ({})",
            report.missing_intervals,
            missing_intervals,
            duplicate_rows,
            out_of_order_rows,
            path.display()
        ));
    }
    let manifest_fingerprint = quality_manifest.fingerprint()?;
    Ok(serde_json::json!({
        "manifest_path": path.to_string_lossy(),
        "manifest_fingerprint": manifest_fingerprint,
        "content_hash": quality_manifest.content_hash,
        "quality_report": quality_manifest.quality_report,
        "manifest": quality_manifest,
    }))
}
