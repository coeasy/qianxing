use super::super::run_experiment::experiment_result_hash_from_json;
use super::identity::build_multi_market_depth_experiment_manifest;
use super::{
    invariant, RunMultiMarketDepthExperimentResult,
    RUN_MULTI_MARKET_DEPTH_EXPERIMENT_SCHEMA_VERSION,
};
use crate::{AppError, AppErrorCategory};
use qx_spec::{ArtifactRefV2, CanonicalRunSpec, ReplayVerdict, RunRecordV2, RunStatus};
use std::collections::BTreeMap;
use std::path::Path;

pub(super) fn persist_multi_market_depth_experiment_result(
    mut result: RunMultiMarketDepthExperimentResult,
    canonical: &CanonicalRunSpec,
    root: &Path,
    report_index_path: &str,
    spec_artifact_path: &str,
) -> Result<RunMultiMarketDepthExperimentResult, AppError> {
    let facts_payload = result.to_json()?;
    let result_hash = experiment_result_hash_from_json(&facts_payload)
        .map_err(|error| invariant(format!("多市场盘口实验结果指纹失败: {error}")))?;
    result.result_hash = Some(result_hash.clone());
    let manifest = build_multi_market_depth_experiment_manifest(
        &result.experiment_id,
        canonical,
        &result_hash,
    )?;
    let manifest_path = crate::artifact_store::persist_run_manifest(root, &manifest)?;
    let report_path = root.join(format!(
        "{}-{result_hash}-input-v1.multi-market-depth-experiment.json",
        result.experiment_id
    ));
    let record_path = root.join(format!(
        "{}-{result_hash}-input-v1.record.json",
        result.experiment_id
    ));
    let spec_bytes = std::fs::read(spec_artifact_path)
        .map_err(|error| AppError::from_io("读取多市场盘口实验输入规格 sidecar", &error))?;
    result.artifact_path = report_index_path.to_owned();
    result.run_manifest = Some(manifest_path.to_string_lossy().into_owned());
    result.run_record = Some(record_path.to_string_lossy().into_owned());
    result.artifact_locations = Some(BTreeMap::from([
        (
            "experiment_report".into(),
            report_path.to_string_lossy().into_owned(),
        ),
        (
            "run_manifest".into(),
            manifest_path.to_string_lossy().into_owned(),
        ),
        ("experiment_input".into(), spec_artifact_path.to_owned()),
    ]));
    let report_payload = result.to_json()?;
    let manifest_payload = manifest
        .to_json()
        .map_err(|error| invariant(format!("多市场盘口实验 RunManifest 编码失败: {error}")))?;
    let report_bytes = report_payload.as_bytes();
    let manifest_bytes = manifest_payload.as_bytes();
    let record = RunRecordV2 {
        schema_version: qx_spec::RUN_RECORD_V2_SCHEMA_VERSION,
        run_id: result.experiment_id.clone(),
        status: RunStatus::Completed,
        input_digest: result
            .input_data_fingerprint
            .clone()
            .ok_or_else(|| invariant("多市场盘口实验输入指纹缺失"))?,
        code_identity: format!("{}:{}", manifest.code_commit, manifest.runtime_version),
        config_fingerprint: result
            .canonical_run_spec_fingerprint
            .clone()
            .ok_or_else(|| invariant("多市场盘口实验 CanonicalRunSpec 指纹缺失"))?,
        started_at_ms: manifest.clock_start,
        finished_at_ms: Some(manifest.clock_end),
        artifact_refs: vec![
            ArtifactRefV2 {
                name: "experiment_report".into(),
                artifact_id: qx_strategy::sha256_hex(report_bytes),
                kind: "run-multi-market-depth-experiment-result-json".into(),
                schema_version: RUN_MULTI_MARKET_DEPTH_EXPERIMENT_SCHEMA_VERSION,
                byte_len: report_bytes.len() as u64,
            },
            ArtifactRefV2 {
                name: "run_manifest".into(),
                artifact_id: qx_strategy::sha256_hex(manifest_bytes),
                kind: "run-manifest-json".into(),
                schema_version: 1,
                byte_len: manifest_bytes.len() as u64,
            },
            ArtifactRefV2 {
                name: "experiment_input".into(),
                artifact_id: qx_strategy::sha256_hex(&spec_bytes),
                kind: "run-experiment-input-json".into(),
                schema_version: 1,
                byte_len: spec_bytes.len() as u64,
            },
        ],
        replay_verdict: ReplayVerdict::Verified,
        capability_level: qx_spec::CapabilityLevel::L1,
    };
    let record_payload = record
        .to_json()
        .map_err(|error| invariant(format!("多市场盘口实验 RunRecord V2 编码失败: {error}")))?;
    publish_multi_market_depth_experiment_artifact(&record_path, &record_payload, "RunRecord V2")?;
    publish_multi_market_depth_experiment_artifact(&report_path, &report_payload, "实验报告")?;
    RunMultiMarketDepthExperimentResult::from_json(&report_payload)
}

pub(super) fn publish_multi_market_depth_experiment_artifact(
    path: &Path,
    payload: &str,
    label: &str,
) -> Result<(), AppError> {
    crate::artifact_store::sink::publish(
        path,
        payload,
        crate::artifact_store::sink::PublishMode::Idempotent,
    )
    .map_err(|error| match error {
        crate::artifact_store::sink::PublishError::Conflict(message) => {
            AppError::new(AppErrorCategory::Conflict, message)
        }
        crate::artifact_store::sink::PublishError::Io(error) => {
            AppError::from_io(&format!("发布多市场盘口实验 {label}"), &error)
        }
    })
}

pub(super) fn verify_persisted_multi_market_depth_experiment_result(
    result: &RunMultiMarketDepthExperimentResult,
) -> Result<(), AppError> {
    let summary = serde_json::json!({
        "schema_version": result.schema_version,
        "experiment_id": result.experiment_id,
        "target_strategy_id": result.target_strategy_id,
        "spec_fingerprint": result.spec_fingerprint,
        "total_candidates": result.total_candidates,
        "completed_candidates": result.completed_candidates,
        "succeeded_candidates": result.succeeded_candidates,
        "failed_candidates": result.failed_candidates,
        "candidates": result.candidates,
        "ranking": result.ranking,
        "artifact_path": result.artifact_path,
        "canonical_run_spec_json": result.canonical_run_spec_json,
        "canonical_run_spec_fingerprint": result.canonical_run_spec_fingerprint,
        "input_data_fingerprint": result.input_data_fingerprint,
        "result_hash": result.result_hash,
        "run_manifest": result.run_manifest,
        "run_record": result.run_record,
        "artifact_locations": result.artifact_locations,
    });
    crate::artifact_store::verify_declared_run_manifest(&summary).map_err(|error| {
        AppError::new(
            AppErrorCategory::StorageFailure,
            format!("多市场盘口实验 RunManifest/RunRecord 读回失败: {error}"),
        )
    })
}
