//! Canonical multi-market Bar/Depth input identity and quality evidence verification.

use super::{
    dataset_quality_identity_fingerprint, invalid, AppError, MultiMarketBacktestSpec, Value,
};
use crate::{AppErrorCategory, MultiMarketBacktestMarket, MultiMarketDepthBacktestMarket};
use std::collections::BTreeSet;

/// Independently verified identity and quality status for one report BarFrame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedMultiMarketBarInputV1 {
    pub instrument: String,
    pub fingerprint: String,
    pub row_count: u64,
    pub source_end_ts: u64,
    pub final_close_raw: i128,
    pub quality_status: String,
    pub quality_fingerprint: Option<String>,
}

/// Independently verified identity and optional quality evidence for one report DepthFrame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifiedMultiMarketDepthInputV1 {
    pub instrument: String,
    pub data_tier: String,
    pub source: String,
    pub fingerprint: String,
    pub input_hash: u64,
    pub snapshot_count: u64,
    pub start_ts_ms: u64,
    pub end_ts_ms: u64,
    pub quality_status: String,
    pub quality_fingerprint: Option<String>,
}

/// Require the persisted quality-manifest set to match the markets that declared manifests.
/// Unknown manifest records are rejected instead of being silently ignored.
pub fn validate_multi_market_quality_manifest_set_v1(
    spec: &MultiMarketBacktestSpec,
    quality_records: &Value,
) -> Result<(), AppError> {
    let records = quality_records
        .as_object()
        .ok_or_else(|| invalid("多市场数据质量记录必须是对象"))?;
    let expected = spec
        .markets
        .iter()
        .filter(|market| market.dataset_manifest_path.is_some())
        .map(|market| market.instrument.clone())
        .collect::<BTreeSet<_>>();
    let actual = records.keys().cloned().collect::<BTreeSet<_>>();
    if actual != expected {
        return Err(invalid("多市场数据质量记录集合与 spec 声明不一致"));
    }
    Ok(())
}

/// Verify one market's BarFrame identity and optional DatasetManifestV2 quality evidence.
///
/// File authorization and bounded reads belong to the caller. This function receives the exact
/// loaded payloads, independently recomputes their content identities, and checks every
/// cross-artifact declaration before report data is projected.
pub fn verify_multi_market_bar_input_v1(
    configured: &MultiMarketBacktestMarket,
    recorded_market: &Value,
    frame_payload: &str,
    frame: &qx_datastruct::BarFrame,
    quality_record: Option<&Value>,
    quality_payload: Option<&str>,
) -> Result<VerifiedMultiMarketBarInputV1, AppError> {
    let instrument = recorded_market
        .get("instrument")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("多市场 market_data 缺少 instrument"))?;
    let recorded_fingerprint = recorded_market
        .get("content_hash")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid(format!("{instrument} BarFrame content_hash 无效")))?;
    let recorded_row_count = recorded_market
        .get("row_count")
        .and_then(Value::as_u64)
        .ok_or_else(|| invalid(format!("{instrument} BarFrame row_count 无效")))?;
    let fingerprint = qx_data::fingerprint_bar_frame(frame)
        .map_err(|error| invalid(format!("{instrument} BarFrame 指纹失败: {error}")))?;
    let row_count =
        u64::try_from(frame.ts.len()).map_err(|_| invalid("BarFrame 行数超出 u64 范围"))?;
    let source_start_ts = frame
        .ts
        .first()
        .copied()
        .ok_or_else(|| invalid(format!("{instrument} BarFrame 不能为空")))?;
    let source_end_ts = frame
        .ts
        .last()
        .copied()
        .ok_or_else(|| invalid(format!("{instrument} BarFrame 不能为空")))?;
    if frame.instrument.to_string() != instrument
        || configured.instrument != instrument
        || recorded_fingerprint != fingerprint
        || recorded_row_count != row_count
        || recorded_market
            .get("start_ts_ms")
            .and_then(Value::as_u64)
            .is_some_and(|start| start != source_start_ts)
        || recorded_market
            .get("end_ts_ms")
            .and_then(Value::as_u64)
            .is_some_and(|end| end != source_end_ts)
    {
        return Err(invalid(format!(
            "{instrument} BarFrame 与运行输入身份不一致"
        )));
    }
    let final_close_raw = frame
        .close_raw
        .last()
        .copied()
        .ok_or_else(|| invalid(format!("{instrument} BarFrame 缺少末尾收盘价")))?;

    let (quality_status, quality_fingerprint) = match (quality_record, quality_payload) {
        (None, None) => {
            if configured.dataset_manifest_path.is_some() {
                return Err(invalid(format!(
                    "{instrument} spec 声明了质量清单但产物未记录"
                )));
            }
            ("not_provided".to_owned(), None)
        }
        (Some(recorded), Some(payload)) => {
            let manifest_path = recorded
                .get("manifest_path")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid(format!("{instrument} 质量清单缺少路径")))?;
            if configured.dataset_manifest_path.as_deref() != Some(manifest_path) {
                return Err(invalid(format!("{instrument} 质量清单路径与 spec 不一致")));
            }
            let (manifest, dataset_snapshot) = qx_data::parse_dataset_manifest_or_snapshot(payload)
                .map_err(|error| invalid(format!("数据质量清单/快照无效: {error}")))?;
            dataset_snapshot
                .as_ref()
                .map(|snapshot| snapshot.validate_for_replay(instrument))
                .transpose()
                .map_err(|error| invalid(format!("数据集快照不适用于回放: {error}")))?;
            let manifest_sha256 = qx_data::sha256_hex_bytes(payload.as_bytes());
            let frame_sha256 = qx_data::sha256_hex_bytes(frame_payload.as_bytes());
            let interval = manifest
                .quality_report
                .interval_ms
                .filter(|value| *value > 0)
                .ok_or_else(|| invalid("数据质量清单缺少正 interval_ms"))?;
            let missing = frame
                .ts
                .windows(2)
                .try_fold(0_u64, |total, pair| {
                    let distance = pair[1].checked_sub(pair[0])?;
                    total.checked_add(distance.div_ceil(interval).saturating_sub(1))
                })
                .ok_or_else(|| invalid("BarFrame 缺口计数溢出或时间戳倒退"))?;
            let actual_gaps = frame
                .ts
                .windows(2)
                .filter_map(|pair| {
                    let distance = pair[1].checked_sub(pair[0])?;
                    (distance > interval).then_some(qx_data::DatasetCoverageGapV1 {
                        start_ts_ms: pair[0].saturating_add(interval),
                        end_ts_ms: pair[1],
                    })
                })
                .collect::<Vec<_>>();
            let snapshot_sha256 = dataset_snapshot
                .as_ref()
                .map(qx_data::DatasetSnapshotV1::content_sha256)
                .transpose()
                .map_err(invalid)?;
            let snapshot_value = dataset_snapshot
                .as_ref()
                .map(serde_json::to_value)
                .transpose()
                .map_err(|error| invalid(error.to_string()))?;
            let first_ts = frame
                .ts
                .first()
                .copied()
                .ok_or_else(|| invalid(format!("{instrument} BarFrame 不能为空")))?;
            let manifest_fingerprint = manifest
                .fingerprint()
                .map_err(|error| invalid(format!("数据质量清单指纹失败: {error}")))?;
            let expected_quality_report = serde_json::to_value(&manifest.quality_report)
                .map_err(|error| invalid(error.to_string()))?;
            if manifest.instrument != instrument
                || manifest.tier != qx_data::DatasetTier::Bar
                || manifest.provider_version != qx_data::BARFRAME_DATASET_VERSION
                || manifest.dataset.fingerprint != fingerprint
                || manifest.content_hash != frame_sha256
                || manifest.quality_report.row_count != row_count
                || manifest.dataset.start_timestamp != first_ts
                || manifest.dataset.end_timestamp != source_end_ts
                || manifest.quality_report.missing_intervals != missing
                || manifest.quality_report.duplicate_rows != 0
                || manifest.quality_report.out_of_order_rows != 0
                || recorded.get("manifest_sha256").and_then(Value::as_str)
                    != Some(manifest_sha256.as_str())
                || recorded.get("manifest_fingerprint").and_then(Value::as_str)
                    != Some(manifest_fingerprint.as_str())
                || recorded.get("quality_report") != Some(&expected_quality_report)
                || (dataset_snapshot.is_some()
                    && (recorded.get("dataset_snapshot_sha256")
                        != Some(&serde_json::json!(snapshot_sha256))
                        || recorded.get("dataset_snapshot") != snapshot_value.as_ref()))
                || (dataset_snapshot.is_none()
                    && recorded
                        .get("dataset_snapshot_sha256")
                        .is_some_and(|value| !value.is_null()))
                || dataset_snapshot
                    .as_ref()
                    .is_some_and(|snapshot| snapshot.gaps != actual_gaps)
            {
                return Err(invalid(format!("{instrument} 数据质量清单与输入不一致")));
            }
            (
                "verified".to_owned(),
                Some(dataset_quality_identity_fingerprint(
                    &manifest_fingerprint,
                    snapshot_sha256.as_deref(),
                )),
            )
        }
        _ => return Err(invalid(format!("{instrument} 数据质量清单证据不完整"))),
    };

    Ok(VerifiedMultiMarketBarInputV1 {
        instrument: instrument.to_owned(),
        fingerprint,
        row_count,
        source_end_ts,
        final_close_raw,
        quality_status,
        quality_fingerprint,
    })
}

/// Verify one multi-market DepthFrame and its optional DatasetManifestV2 evidence.
///
/// The API adapter authorizes and opens the payloads. This use case owns depth identity, tier
/// shape, dataset quality, and continuity checks so report readers share the same rules.
pub fn verify_multi_market_depth_input_v1(
    configured: &MultiMarketDepthBacktestMarket,
    recorded_market: &Value,
    frame_payload: &str,
    frame: &qx_xingban::DepthFrame,
    quality_record: Option<&Value>,
    quality_payload: Option<&str>,
) -> Result<VerifiedMultiMarketDepthInputV1, AppError> {
    let instrument = recorded_market
        .get("instrument")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("多市场深度 market_data 缺少 instrument"))?;
    let data_tier = configured.data_tier.as_deref().unwrap_or("l2");
    let expected_hash = recorded_market
        .get("content_hash")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid(format!("{instrument} DepthFrame content_hash 无效")))?;
    let expected_source = recorded_market
        .get("source")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid(format!("{instrument} DepthFrame source 无效")))?;
    let expected_tier = recorded_market
        .get("data_tier")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid(format!("{instrument} DepthFrame data_tier 无效")))?;
    let expected_snapshot_count = recorded_market
        .get("snapshot_count")
        .and_then(Value::as_u64)
        .ok_or_else(|| invalid(format!("{instrument} DepthFrame snapshot_count 无效")))?;
    let input_hash = frame.input_hash();
    let frame_hash = format!("{input_hash:016x}");
    let snapshot_count =
        u64::try_from(frame.snapshots.len()).map_err(|_| invalid("深度快照数量超出 u64"))?;
    if snapshot_count > crate::artifact_store::MAX_ARTIFACT_ROWS {
        return Err(AppError::new(
            AppErrorCategory::Timeout,
            format!(
                "{instrument} DepthFrame 超过 {} 条快照上限",
                crate::artifact_store::MAX_ARTIFACT_ROWS
            ),
        ));
    }
    if configured.instrument != instrument
        || frame.instrument.to_string() != instrument
        || expected_tier != data_tier
        || expected_source != frame.source
        || expected_hash != frame_hash
        || expected_snapshot_count != snapshot_count
        || recorded_market
            .get("start_ts_ms")
            .and_then(Value::as_u64)
            .is_some_and(|start| {
                frame
                    .snapshots
                    .first()
                    .is_none_or(|snapshot| snapshot.ts != start)
            })
        || recorded_market
            .get("end_ts_ms")
            .and_then(Value::as_u64)
            .is_some_and(|end| {
                frame
                    .snapshots
                    .last()
                    .is_none_or(|snapshot| snapshot.ts != end)
            })
    {
        return Err(invalid(format!("{instrument} DepthFrame 输入身份不一致")));
    }
    if !matches!(data_tier, "l1" | "l2") {
        return Err(invalid(format!("{instrument} 深度 data_tier 无效")));
    }
    if data_tier == "l1"
        && frame
            .snapshots
            .iter()
            .any(|snapshot| snapshot.bids.len() != 1 || snapshot.asks.len() != 1)
    {
        return Err(invalid(format!("{instrument} 声明 L1 但输入含多档盘口")));
    }
    let start_ts_ms = frame
        .snapshots
        .first()
        .map(|snapshot| snapshot.ts)
        .ok_or_else(|| invalid(format!("{instrument} DepthFrame 不能为空")))?;
    let end_ts_ms = frame
        .snapshots
        .last()
        .map(|snapshot| snapshot.ts)
        .ok_or_else(|| invalid(format!("{instrument} DepthFrame 不能为空")))?;
    let fingerprint = format!("depth:{data_tier}:{}:{frame_hash}", frame.source);
    let (quality_status, quality_fingerprint) = match (
        configured.dataset_manifest_path.as_deref(),
        quality_record,
        quality_payload,
    ) {
        (None, None, None) => ("not_provided".to_owned(), None),
        (None, Some(_), _) => {
            return Err(invalid(format!(
                "{instrument} 产物包含 spec 未声明的质量清单"
            )));
        }
        (None, None, Some(_)) => {
            return Err(invalid(format!(
                "{instrument} 提供了 spec 未声明的质量清单内容"
            )));
        }
        (Some(_), None, _) | (Some(_), _, None) => {
            return Err(invalid(format!(
                "{instrument} spec 声明质量清单但证据不完整"
            )));
        }
        (Some(_), Some(recorded), Some(payload)) => {
            let (manifest, dataset_snapshot) = qx_data::parse_dataset_manifest_or_snapshot(payload)
                .map_err(|error| invalid(format!("{instrument} 质量清单/快照无效: {error}")))?;
            dataset_snapshot
                .as_ref()
                .map(|snapshot| snapshot.validate_for_replay(instrument))
                .transpose()
                .map_err(|error| {
                    invalid(format!("{instrument} 数据集快照不适用于回放: {error}"))
                })?;
            let manifest_sha = qx_data::sha256_hex_bytes(payload.as_bytes());
            let manifest_fingerprint = manifest
                .fingerprint()
                .map_err(|error| invalid(format!("{instrument} 质量清单指纹失败: {error}")))?;
            let expected_tier = if data_tier == "l1" {
                qx_data::DatasetTier::L1
            } else {
                qx_data::DatasetTier::L2
            };
            let first_ts = start_ts_ms;
            let last_ts = end_ts_ms;
            if manifest.instrument != instrument
                || manifest.tier != expected_tier
                || manifest.provider_version != qx_data::DEPTHFRAME_DATASET_VERSION
                || manifest.dataset.dataset_id != format!("depth-bars:{instrument}")
                || manifest.dataset.version != qx_data::DEPTHFRAME_DATASET_VERSION
                || manifest.dataset.source != frame.source
                || manifest.dataset.schema_version != frame.schema_version
                || manifest.source_lineage.primary_source != frame.source
                || manifest.content_hash != qx_data::sha256_hex_bytes(frame_payload.as_bytes())
                || manifest.dataset.fingerprint != frame_hash
                || manifest.dataset.start_timestamp != first_ts
                || manifest.dataset.end_timestamp != last_ts
                || manifest.quality_report.row_count != snapshot_count
                || manifest.quality_report.usable_tiers != [data_tier]
                || recorded.get("manifest_sha256").and_then(Value::as_str)
                    != Some(manifest_sha.as_str())
                || recorded.get("manifest_fingerprint").and_then(Value::as_str)
                    != Some(manifest_fingerprint.as_str())
                || recorded.get("content_hash").and_then(Value::as_str)
                    != Some(manifest.content_hash.as_str())
                || recorded.get("quality_report")
                    != Some(
                        &serde_json::to_value(&manifest.quality_report)
                            .map_err(|error| invalid(error.to_string()))?,
                    )
            {
                return Err(invalid(format!("{instrument} 深度质量清单与输入不一致")));
            }
            let interval = manifest
                .quality_report
                .interval_ms
                .filter(|value| *value > 0)
                .ok_or_else(|| invalid(format!("{instrument} 深度质量清单缺少正 interval_ms")))?;
            let actual_gaps = frame
                .snapshots
                .windows(2)
                .filter_map(|pair| {
                    let distance = pair[1].ts.checked_sub(pair[0].ts)?;
                    (distance > interval).then_some(qx_data::DatasetCoverageGapV1 {
                        start_ts_ms: pair[0].ts.saturating_add(interval),
                        end_ts_ms: pair[1].ts,
                    })
                })
                .collect::<Vec<_>>();
            let snapshot_sha256 = dataset_snapshot
                .as_ref()
                .map(qx_data::DatasetSnapshotV1::content_sha256)
                .transpose()
                .map_err(invalid)?;
            let snapshot_value = dataset_snapshot
                .as_ref()
                .map(serde_json::to_value)
                .transpose()
                .map_err(|error| invalid(error.to_string()))?;
            let (duplicates, out_of_order, missing) = frame
                .snapshots
                .windows(2)
                .try_fold(
                    (0_u64, 0_u64, 0_u64),
                    |(duplicates, out_of_order, missing), pair| {
                        if pair[1].ts < pair[0].ts {
                            Some((duplicates, out_of_order.checked_add(1)?, missing))
                        } else if pair[1].ts == pair[0].ts {
                            Some((duplicates.checked_add(1)?, out_of_order, missing))
                        } else {
                            Some((
                                duplicates,
                                out_of_order,
                                missing.checked_add(
                                    (pair[1].ts - pair[0].ts).div_ceil(interval) - 1,
                                )?,
                            ))
                        }
                    },
                )
                .ok_or_else(|| invalid(format!("{instrument} 深度质量计数溢出")))?;
            if manifest.quality_report.duplicate_rows != duplicates
                || manifest.quality_report.out_of_order_rows != out_of_order
                || manifest.quality_report.missing_intervals != missing
                || (dataset_snapshot.is_some()
                    && (recorded.get("dataset_snapshot_sha256")
                        != Some(&serde_json::json!(snapshot_sha256))
                        || recorded.get("dataset_snapshot") != snapshot_value.as_ref()
                        || dataset_snapshot
                            .as_ref()
                            .is_some_and(|snapshot| snapshot.gaps != actual_gaps)
                        || duplicates > 0
                        || out_of_order > 0))
                || (dataset_snapshot.is_none()
                    && recorded
                        .get("dataset_snapshot_sha256")
                        .is_some_and(|value| !value.is_null()))
            {
                return Err(invalid(format!("{instrument} 深度质量报告与输入不一致")));
            }
            (
                "verified".to_owned(),
                Some(dataset_quality_identity_fingerprint(
                    &manifest_fingerprint,
                    snapshot_sha256.as_deref(),
                )),
            )
        }
    };
    Ok(VerifiedMultiMarketDepthInputV1 {
        instrument: instrument.to_owned(),
        data_tier: data_tier.to_owned(),
        source: frame.source.clone(),
        fingerprint,
        input_hash,
        snapshot_count,
        start_ts_ms,
        end_ts_ms,
        quality_status,
        quality_fingerprint,
    })
}
