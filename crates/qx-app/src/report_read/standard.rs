//! Single-run and provider report readers.

use super::artifact_views::{read_json_bounded, read_report};
use super::*;

pub fn read_run_report_v1(
    outcome: &BacktestOutcome,
    context: &RunContext,
) -> Result<ResearchRunReportV1, AppError> {
    read_run_report_v1_with_reader(outcome, context, &FileArtifactReader)
}

/// Read and verify a Bar report through an application-owned storage adapter.
pub fn read_run_report_v1_with_reader(
    outcome: &BacktestOutcome,
    context: &RunContext,
    reader: &dyn ArtifactReader,
) -> Result<ResearchRunReportV1, AppError> {
    let verification = verify_run_with_reader(outcome, context, reader)?;
    let declared_artifacts = if verification.verified {
        let summary = read_json_bounded(
            reader,
            &outcome.artifacts.summary,
            MAX_SUMMARY_BYTES,
            "研究摘要",
        )?;
        crate::artifact_store::declared_run_record_artifact_locators(&summary, reader)?
    } else {
        None
    };
    read_report(
        &outcome.run_id,
        &outcome.instrument,
        &outcome.artifacts.summary,
        &outcome.artifacts.equity,
        &outcome.artifacts.fills,
        verification,
        declared_artifacts.as_ref(),
        reader,
    )
}

pub fn read_depth_run_report_v1(
    outcome: &DepthBacktestOutcome,
    context: &RunContext,
) -> Result<ResearchRunReportV1, AppError> {
    read_depth_run_report_v1_with_reader(outcome, context, &FileArtifactReader)
}

/// Read and verify a depth report through an application-owned storage adapter.
pub fn read_depth_run_report_v1_with_reader(
    outcome: &DepthBacktestOutcome,
    context: &RunContext,
    reader: &dyn ArtifactReader,
) -> Result<ResearchRunReportV1, AppError> {
    let verification = verify_depth_run_with_reader(outcome, context, reader)?;
    let declared_artifacts = if verification.verified {
        let summary = read_json_bounded(
            reader,
            &outcome.artifacts.summary,
            MAX_SUMMARY_BYTES,
            "depth summary",
        )?;
        crate::artifact_store::declared_run_record_artifact_locators(&summary, reader)?
    } else {
        None
    };
    read_report(
        &outcome.run_id,
        &outcome.instrument,
        &outcome.artifacts.summary,
        &outcome.artifacts.equity,
        &outcome.artifacts.fills,
        verification,
        declared_artifacts.as_ref(),
        reader,
    )
}

pub fn read_provider_backtest_report_v1(
    outcome: &ProviderBacktestOutcome,
    context: &RunContext,
) -> Result<ProviderBacktestReportV1, AppError> {
    read_provider_backtest_report_v1_with_reader(outcome, context, &FileArtifactReader)
}

/// Read a Provider Bar run through the shared verifier and bind the outer Provider envelope to
/// the same canonical lineage and manifest embedded in the verified run summary.
pub fn read_provider_backtest_report_v1_with_reader(
    outcome: &ProviderBacktestOutcome,
    context: &RunContext,
    reader: &dyn ArtifactReader,
) -> Result<ProviderBacktestReportV1, AppError> {
    let report = read_run_report_v1_with_reader(&outcome.outcome, context, reader)?;
    let frozen_snapshot_payload = read_text_bounded(
        reader,
        &outcome.snapshot_path,
        MAX_SUMMARY_BYTES,
        "Provider DatasetSnapshotV1",
    )?;
    let frozen_snapshot = qx_data::DatasetSnapshotV1::from_json(&frozen_snapshot_payload)
        .map_err(|error| provider_report_error(&format!("冻结 Provider snapshot 无效: {error}")))?;
    if frozen_snapshot != outcome.dataset_snapshot {
        return Err(provider_report_error(
            "Provider outcome 中的 DatasetSnapshot 与冻结 sidecar 不一致",
        ));
    }
    let lineage = report
        .summary
        .get("input")
        .and_then(Value::as_object)
        .and_then(|input| input.get("provider_lineage"))
        .and_then(Value::as_object)
        .ok_or_else(|| provider_report_error("Provider lineage 在报告摘要中缺失"))?;
    let manifest_json = outcome.manifest.to_json().map_err(|error| {
        provider_report_error(&format!("Provider manifest 序列化失败: {error}"))
    })?;
    let expected_sha256 = qx_data::sha256_hex_bytes(manifest_json.as_bytes());
    let snapshot_json = serde_json::to_string(&outcome.dataset_snapshot).map_err(|error| {
        provider_report_error(&format!("Provider snapshot 序列化失败: {error}"))
    })?;
    let expected_snapshot_sha256 = outcome
        .dataset_snapshot
        .content_sha256()
        .map_err(|error| provider_report_error(&error))?;
    if lineage.get("dataset_manifest_json").and_then(Value::as_str) != Some(&manifest_json)
        || lineage
            .get("dataset_manifest_sha256")
            .and_then(Value::as_str)
            != Some(expected_sha256.as_str())
        || lineage.get("dataset_snapshot_json").and_then(Value::as_str) != Some(&snapshot_json)
        || lineage
            .get("dataset_snapshot_sha256")
            .and_then(Value::as_str)
            != Some(expected_snapshot_sha256.as_str())
        || outcome.dataset_snapshot.manifest != outcome.manifest
        || lineage.get("provider_id").and_then(Value::as_str)
            != Some(outcome.manifest.source_lineage.primary_source.as_str())
        || lineage.get("provider_version").and_then(Value::as_str)
            != Some(outcome.manifest.provider_version.as_str())
        || outcome.manifest.instrument != outcome.outcome.instrument
        || outcome.dataset_verdict.instrument != outcome.outcome.instrument
        || outcome.dataset_verdict.content_fingerprint != outcome.outcome.data_fingerprint
        || outcome.manifest.dataset.fingerprint
            != outcome
                .outcome
                .data_fingerprint
                .strip_prefix("barframe:")
                .unwrap_or(outcome.outcome.data_fingerprint.as_str())
    {
        return Err(provider_report_error(
            "Provider outcome、冻结 manifest 与已验证回测输入身份不一致",
        ));
    }
    Ok(ProviderBacktestReportV1 {
        schema_version: PROVIDER_BACKTEST_REPORT_SCHEMA_VERSION_V1,
        report,
        dataset_verdict: outcome.dataset_verdict.clone(),
        manifest: outcome.manifest.clone(),
        dataset_snapshot: outcome.dataset_snapshot.clone(),
    })
}

pub fn read_provider_depth_backtest_report_v1(
    outcome: &ProviderDepthBacktestOutcome,
    context: &RunContext,
) -> Result<ProviderDepthBacktestReportV1, AppError> {
    read_provider_depth_backtest_report_v1_with_readers(
        outcome,
        context,
        &FileArtifactReader,
        &FileArtifactReader,
    )
}

/// Read a Provider depth report with separate readers for run artifacts and the frozen input.
/// This preserves rooted API storage boundaries, where report artifacts and dataset inputs may
/// live under different authorized roots.
pub fn read_provider_depth_backtest_report_v1_with_readers(
    outcome: &ProviderDepthBacktestOutcome,
    context: &RunContext,
    artifact_reader: &dyn ArtifactReader,
    input_reader: &dyn ArtifactReader,
) -> Result<ProviderDepthBacktestReportV1, AppError> {
    let report = read_depth_run_report_v1_with_reader(&outcome.outcome, context, artifact_reader)?;
    let body = read_text_bounded(
        input_reader,
        &outcome.depth_path,
        MAX_ARTIFACT_BYTES,
        "Provider DepthFrame",
    )?;
    let frame = qx_xingban::DepthFrame::from_json(&body)
        .map_err(|error| provider_report_error(&format!("Provider DepthFrame 无效: {error}")))?;
    let manifest_body = read_text_bounded(
        input_reader,
        &outcome.manifest_path,
        MAX_SUMMARY_BYTES,
        "Provider depth DatasetManifestV2",
    )?;
    let manifest = qx_data::DatasetManifestV2::from_json(&manifest_body).map_err(|error| {
        provider_report_error(&format!("Provider depth manifest 无效: {error}"))
    })?;
    let snapshot_body = read_text_bounded(
        input_reader,
        &outcome.snapshot_path,
        MAX_SUMMARY_BYTES,
        "Provider depth DatasetSnapshotV1",
    )?;
    let dataset_snapshot =
        qx_data::DatasetSnapshotV1::from_json(&snapshot_body).map_err(|error| {
            provider_report_error(&format!("Provider depth snapshot 无效: {error}"))
        })?;
    let query_digest = verify_provider_depth_source(outcome, &frame.source)?;
    let input_hash = format!("{:016x}", frame.input_hash());
    let expected_fingerprint = format!("depth:{}:{}", frame.source, input_hash);
    let timestamps = frame
        .snapshots
        .iter()
        .map(|item| item.ts)
        .collect::<Vec<_>>();
    let exact_gaps = timestamps
        .windows(2)
        .filter_map(|pair| {
            (pair[1] - pair[0] > dataset_snapshot.frequency_ms).then_some(
                qx_data::DatasetCoverageGapV1 {
                    start_ts_ms: pair[0].saturating_add(dataset_snapshot.frequency_ms),
                    end_ts_ms: pair[1],
                },
            )
        })
        .collect::<Vec<_>>();
    let dataset_snapshot_sha256 = dataset_snapshot
        .content_sha256()
        .map_err(|error| provider_report_error(&error))?;
    let summary_snapshot_sha256 = report
        .summary
        .get("dataset_quality")
        .and_then(|quality| quality.get("dataset_snapshot_sha256"))
        .and_then(Value::as_str);
    let summary_quality = report.summary.get("dataset_quality");
    let manifest_value = serde_json::to_value(&manifest).map_err(|error| {
        provider_report_error(&format!("Provider depth manifest encoding failed: {error}"))
    })?;
    let snapshot_value = serde_json::to_value(&dataset_snapshot).map_err(|error| {
        provider_report_error(&format!("Provider depth snapshot encoding failed: {error}"))
    })?;
    let summary_input = report.summary.get("input");
    if frame.instrument.to_string() != outcome.outcome.instrument
        || outcome.outcome.data_fingerprint != expected_fingerprint
        || summary_input
            .and_then(|input| input.get("fingerprint"))
            .and_then(Value::as_str)
            != Some(input_hash.as_str())
        || summary_input
            .and_then(|input| input.get("kind"))
            .and_then(Value::as_str)
            != Some("depth-frame")
        || manifest != outcome.manifest
        || dataset_snapshot != outcome.dataset_snapshot
        || dataset_snapshot.manifest != manifest
        || manifest.instrument != frame.instrument.to_string()
        || manifest.dataset.source != frame.source
        || manifest.dataset.fingerprint != input_hash
        || manifest.content_hash != qx_data::sha256_hex_bytes(body.as_bytes())
        || manifest.dataset.version != qx_data::DEPTHFRAME_DATASET_VERSION
        || manifest.provider_version != qx_data::DEPTHFRAME_DATASET_VERSION
        || manifest.source_lineage.primary_source != outcome.provider_id
        || manifest.quality_report.row_count != frame.snapshots.len() as u64
        || manifest.dataset.start_timestamp != timestamps.first().copied().unwrap_or(0)
        || manifest.dataset.end_timestamp != timestamps.last().copied().unwrap_or(0)
        || manifest.quality_report.interval_ms != Some(dataset_snapshot.frequency_ms)
        || manifest.quality_report.missing_intervals
            != exact_gaps
                .iter()
                .map(|gap| {
                    (gap.end_ts_ms - gap.start_ts_ms).div_ceil(dataset_snapshot.frequency_ms)
                })
                .sum::<u64>()
        || dataset_snapshot.gaps != exact_gaps
        || summary_snapshot_sha256 != Some(dataset_snapshot_sha256.as_str())
        || summary_quality.and_then(|quality| quality.get("manifest")) != Some(&manifest_value)
        || summary_quality.and_then(|quality| quality.get("dataset_snapshot"))
            != Some(&snapshot_value)
    {
        return Err(provider_report_error(
            "Provider DepthFrame、报告摘要与回测输入指纹不一致",
        ));
    }
    Ok(ProviderDepthBacktestReportV1 {
        schema_version: PROVIDER_DEPTH_BACKTEST_REPORT_SCHEMA_VERSION_V1,
        report,
        provider_id: outcome.provider_id.clone(),
        provider_version: outcome.provider_version.clone(),
        request_id: outcome.request_id.clone(),
        retry_chain: outcome.retry_chain.clone(),
        source_hash: outcome.source_hash.clone(),
        query_digest,
        depth_input_hash: input_hash,
        manifest,
        dataset_snapshot,
        dataset_snapshot_sha256,
    })
}

fn verify_provider_depth_source(
    outcome: &ProviderDepthBacktestOutcome,
    source: &str,
) -> Result<String, AppError> {
    let valid_hash = |value: &str| {
        value.len() == 16
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    };
    if outcome.provider_id.trim().is_empty()
        || outcome.provider_version.trim().is_empty()
        || outcome.request_id.trim().is_empty()
        || !valid_hash(&outcome.source_hash)
    {
        return Err(provider_report_error("Provider Depth outcome 来源身份无效"));
    }
    let prefix = format!(
        "{}@{}#query=",
        outcome.provider_id, outcome.provider_version
    );
    let rest = source
        .strip_prefix(&prefix)
        .ok_or_else(|| provider_report_error("DepthFrame Provider 身份与 outcome 不一致"))?;
    let (query_digest, request_and_source) = rest
        .split_once("#request=")
        .ok_or_else(|| provider_report_error("DepthFrame 缺少 Provider query/request 身份"))?;
    if !valid_hash(query_digest)
        || request_and_source != format!("{}#source={}", outcome.request_id, outcome.source_hash)
    {
        return Err(provider_report_error(
            "DepthFrame Provider query/request/source 身份不一致",
        ));
    }
    Ok(query_digest.to_string())
}

fn provider_report_error(message: &str) -> AppError {
    AppError::new(AppErrorCategory::InvalidInput, message)
}
