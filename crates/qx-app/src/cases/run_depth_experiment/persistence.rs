use super::super::artifacts::{read_text, write_text};
use super::super::experiment_spec_artifact::{verify_spec_artifact, write_spec_artifact};
use super::super::run_experiment::{
    build_experiment_run_manifest, experiment_result_hash_from_json,
};
use super::{invalid, invariant, RunDepthExperimentResult, RUN_DEPTH_EXPERIMENT_SCHEMA_VERSION};
use crate::{AppError, AppErrorCategory, RunContext};
use qx_core::RunManifest;
use qx_spec::{ArtifactRefV2, CanonicalRunSpec, ReplayVerdict, RunRecordV2, RunStatus};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

pub(super) fn verify_depth_experiment_candidate_outcomes(
    result: &RunDepthExperimentResult,
    context: &RunContext,
    expected_input_fingerprint: &str,
    expected_quality_fingerprint: Option<&str>,
) -> Result<(), AppError> {
    let mut verified = BTreeSet::new();
    for candidate in &result.candidates {
        let Some(outcome) = candidate.outcome.as_ref() else {
            continue;
        };
        if !verified.insert(outcome.run_id.clone()) {
            continue;
        }
        let verification = crate::cases::verify_depth_run(outcome, context)?;
        if !verification.verified {
            return Err(AppError::new(
                AppErrorCategory::StorageFailure,
                format!(
                    "深度实验候选 {} 产物复核失败: {}",
                    outcome.run_id,
                    verification.mismatches.join("; ")
                ),
            ));
        }
        let manifest_payload = read_text(&outcome.artifacts.run_manifest)?;
        let manifest = RunManifest::from_json(&manifest_payload).map_err(|error| {
            AppError::new(
                AppErrorCategory::StorageFailure,
                format!("深度实验候选 {} RunManifest 无效: {error}", outcome.run_id),
            )
        })?;
        let candidate_spec = manifest
            .canonical_run_spec_json
            .as_deref()
            .ok_or_else(|| invalid("深度实验候选 RunManifest 缺少 CanonicalRunSpec"))
            .and_then(|payload| {
                CanonicalRunSpec::from_json(payload)
                    .map_err(|error| AppError::new(AppErrorCategory::StorageFailure, error))
            })?;
        if candidate_spec.run_type != "depth_backtest"
            || candidate_spec.datasets.len() != 1
            || candidate_spec.datasets[0].content_fingerprint != expected_input_fingerprint
            || candidate_spec.datasets[0].instrument != outcome.instrument
            || candidate_spec
                .extensions
                .get("dataset_quality_fingerprint")
                .and_then(Value::as_str)
                != expected_quality_fingerprint
        {
            return Err(AppError::new(
                AppErrorCategory::Conflict,
                format!("深度实验候选 {} 绑定了不同的深度数据集", outcome.run_id),
            ));
        }
    }
    Ok(())
}

pub(super) fn persist_depth_experiment_result(
    mut result: RunDepthExperimentResult,
    canonical_spec: &CanonicalRunSpec,
    root: &std::path::Path,
    report_index_path: &str,
    spec_artifact_path: &str,
) -> Result<RunDepthExperimentResult, AppError> {
    let result_payload = serde_json::to_string(&result)
        .map_err(|error| invariant(format!("深度实验事实摘要编码失败: {error}")))?;
    let result_hash = experiment_result_hash_from_json(&result_payload)
        .map_err(|error| invariant(format!("深度实验事实摘要规范化失败: {error}")))?;
    result.result_hash = Some(result_hash.clone());
    let manifest =
        build_experiment_run_manifest(&result.experiment_id, canonical_spec, &result_hash)?;
    let manifest_path = crate::artifact_store::persist_run_manifest(root, &manifest)?;
    let report_path = root.join(format!(
        "{}-{result_hash}-input-v1.depth-experiment.json",
        result.experiment_id
    ));
    let record_path = root.join(format!(
        "{}-{result_hash}-input-v1.record.json",
        result.experiment_id
    ));
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
        .map_err(|error| invariant(format!("深度实验 RunManifest 编码失败: {error}")))?;
    let report_bytes = report_payload.as_bytes();
    let manifest_bytes = manifest_payload.as_bytes();
    let spec_bytes = std::fs::read(spec_artifact_path)
        .map_err(|error| AppError::from_io("读取深度参数实验输入规格 sidecar", &error))?;
    let record = RunRecordV2 {
        schema_version: qx_spec::RUN_RECORD_V2_SCHEMA_VERSION,
        run_id: result.experiment_id.clone(),
        status: RunStatus::Completed,
        input_digest: result
            .input_data_fingerprint
            .clone()
            .ok_or_else(|| invariant("深度实验输入数据指纹缺失"))?,
        code_identity: format!("{}:{}", manifest.code_commit, manifest.runtime_version),
        config_fingerprint: result
            .canonical_run_spec_fingerprint
            .clone()
            .ok_or_else(|| invariant("深度实验 CanonicalRunSpec 指纹缺失"))?,
        started_at_ms: manifest.clock_start,
        finished_at_ms: Some(manifest.clock_end),
        artifact_refs: vec![
            ArtifactRefV2 {
                name: "experiment_report".into(),
                artifact_id: qx_strategy::sha256_hex(report_bytes),
                kind: "run-depth-experiment-result-json".into(),
                schema_version: RUN_DEPTH_EXPERIMENT_SCHEMA_VERSION,
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
        .map_err(|error| invariant(format!("深度实验 RunRecord V2 编码失败: {error}")))?;
    publish_depth_experiment_artifact(&record_path, &record_payload, "RunRecord V2")?;
    publish_depth_experiment_artifact(&report_path, &report_payload, "参数实验报告")?;
    write_text(report_index_path, &report_payload)?;
    // Verify through the standard read path without materializing candidate i128s into Value;
    // serde_json::Value cannot preserve every signed 128-bit integer on all feature sets.
    verify_persisted_depth_experiment_result(&result)?;
    Ok(result)
}

pub(super) fn publish_depth_experiment_artifact(
    path: &std::path::Path,
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
            AppError::from_io(&format!("发布深度实验 {label}"), &error)
        }
    })
}

pub(super) fn validate_depth_experiment_result(
    result: &RunDepthExperimentResult,
) -> Result<(), AppError> {
    match (
        result.canonical_run_spec_json.as_deref(),
        result.canonical_run_spec_fingerprint.as_deref(),
    ) {
        (None, None) => {}
        (Some(payload), Some(expected_fingerprint)) => {
            let canonical = CanonicalRunSpec::from_json(payload)
                .map_err(|error| AppError::new(AppErrorCategory::StorageFailure, error))?;
            let canonical_json = canonical
                .canonical_json()
                .map_err(|error| AppError::new(AppErrorCategory::StorageFailure, error))?;
            let fingerprint = canonical
                .fingerprint()
                .map_err(|error| AppError::new(AppErrorCategory::StorageFailure, error))?;
            if canonical_json != payload
                || fingerprint != expected_fingerprint
                || canonical.run_type != "depth_experiment"
                || canonical.datasets.len() != 1
                || canonical.datasets[0].role != "depth_snapshots"
                || result.input_data_fingerprint.as_deref()
                    != Some(canonical.datasets[0].content_fingerprint.as_str())
                || result
                    .candidates
                    .iter()
                    .filter_map(|candidate| candidate.outcome.as_ref())
                    .any(|outcome| canonical.datasets[0].instrument != outcome.instrument)
            {
                return Err(AppError::new(
                    AppErrorCategory::StorageFailure,
                    "深度实验 CanonicalRunSpec 与报告数据身份或指纹不一致",
                ));
            }
        }
        _ => {
            return Err(AppError::new(
                AppErrorCategory::StorageFailure,
                "深度实验 CanonicalRunSpec JSON 与 fingerprint 必须同时存在或同时缺省",
            ));
        }
    }
    match (
        result.result_hash.as_deref(),
        result.run_manifest.as_deref(),
        result.run_record.as_deref(),
        result.artifact_locations.as_ref(),
    ) {
        (None, None, None, None) => Ok(()),
        (Some(result_hash), Some(manifest), Some(record), Some(locations)) => {
            let valid_hash = result_hash.len() == 16
                && result_hash
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
            if !valid_hash
                || manifest.trim().is_empty()
                || record.trim().is_empty()
                || result.canonical_run_spec_json.is_none()
                || result.canonical_run_spec_fingerprint.is_none()
                || result.input_data_fingerprint.is_none()
                || !matches!(locations.len(), 2 | 3)
                || (locations.len() == 3
                    && locations
                        .get("experiment_input")
                        .is_none_or(|path| path.trim().is_empty()))
                || locations
                    .get("experiment_report")
                    .is_none_or(|path| path.trim().is_empty() || path == &result.artifact_path)
                || locations.get("run_manifest").map(String::as_str) != Some(manifest)
            {
                return Err(AppError::new(
                    AppErrorCategory::StorageFailure,
                    "深度实验标准产物指针或 ArtifactLocations 无效",
                ));
            }
            let payload = serde_json::to_string(result)
                .map_err(|error| invariant(format!("深度实验事实摘要编码失败: {error}")))?;
            let actual_hash = experiment_result_hash_from_json(&payload)
                .map_err(|error| AppError::new(AppErrorCategory::StorageFailure, error))?;
            if actual_hash != result_hash {
                return Err(AppError::new(
                    AppErrorCategory::StorageFailure,
                    "深度实验 result_hash 与报告事实不一致",
                ));
            }
            Ok(())
        }
        _ => Err(AppError::new(
            AppErrorCategory::StorageFailure,
            "深度实验 result_hash、RunManifest、RunRecord 和 ArtifactLocations 必须全部存在或全部缺省",
        )),
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn existing_result(
    payload: &str,
    expected_fingerprint: &str,
    expected_canonical_json: &str,
    expected_canonical_fingerprint: &str,
    expected_input_fingerprint: &str,
    expected_quality_fingerprint: Option<&str>,
    experiment_id: &str,
    context: &RunContext,
    canonical_spec: &CanonicalRunSpec,
    root: &std::path::Path,
    spec_artifact_path: &str,
    persisted_input: &str,
) -> Result<RunDepthExperimentResult, AppError> {
    let mut previous = RunDepthExperimentResult::from_json(payload)?;
    let same_spec = previous.canonical_run_spec_fingerprint.as_deref().map_or(
        previous.spec_fingerprint == expected_fingerprint,
        |fingerprint| fingerprint == expected_canonical_fingerprint,
    );
    if previous.experiment_id != experiment_id || !same_spec {
        return Err(AppError::new(
            AppErrorCategory::Conflict,
            format!("experiment_id {experiment_id} 已绑定不同 Tick/OrderBook 实验规格"),
        ));
    }
    if previous
        .canonical_run_spec_fingerprint
        .as_deref()
        .is_some_and(|fingerprint| fingerprint != expected_canonical_fingerprint)
        || previous
            .canonical_run_spec_json
            .as_deref()
            .is_some_and(|payload| payload != expected_canonical_json)
        || previous
            .input_data_fingerprint
            .as_deref()
            .is_some_and(|fingerprint| fingerprint != expected_input_fingerprint)
    {
        return Err(AppError::new(
            AppErrorCategory::Conflict,
            format!("experiment_id {experiment_id} 已绑定不同深度数据或 CanonicalRunSpec"),
        ));
    }
    verify_depth_experiment_candidate_outcomes(
        &previous,
        context,
        expected_input_fingerprint,
        expected_quality_fingerprint,
    )?;
    if previous.result_hash.is_some() {
        verify_persisted_depth_experiment_result(&previous)?;
        if previous
            .artifact_locations
            .as_ref()
            .and_then(|locations| locations.get("experiment_input"))
            .is_some_and(|path| path == spec_artifact_path)
        {
            verify_spec_artifact(spec_artifact_path, persisted_input, "深度参数实验")?;
        } else {
            write_spec_artifact(spec_artifact_path, persisted_input, "深度参数实验")?;
            let report_index_path = previous.artifact_path.clone();
            return persist_depth_experiment_result(
                previous,
                canonical_spec,
                root,
                &report_index_path,
                spec_artifact_path,
            );
        }
        return Ok(previous);
    }
    previous.canonical_run_spec_json = Some(expected_canonical_json.into());
    previous.canonical_run_spec_fingerprint = Some(expected_canonical_fingerprint.into());
    previous.input_data_fingerprint = Some(expected_input_fingerprint.into());
    let report_index_path = previous.artifact_path.clone();
    write_spec_artifact(spec_artifact_path, persisted_input, "深度参数实验")?;
    let upgraded = persist_depth_experiment_result(
        previous,
        canonical_spec,
        root,
        &report_index_path,
        spec_artifact_path,
    )?;
    Ok(upgraded)
}

pub(super) fn verify_persisted_depth_experiment_result(
    result: &RunDepthExperimentResult,
) -> Result<(), AppError> {
    let verification_summary = serde_json::json!({
        "schema_version": result.schema_version,
        "experiment_id": result.experiment_id,
        "artifact_path": result.artifact_path,
        "canonical_run_spec_json": result.canonical_run_spec_json,
        "canonical_run_spec_fingerprint": result.canonical_run_spec_fingerprint,
        "input_data_fingerprint": result.input_data_fingerprint,
        "result_hash": result.result_hash,
        "run_manifest": result.run_manifest,
        "run_record": result.run_record,
        "artifact_locations": result.artifact_locations,
    });
    crate::artifact_store::verify_declared_run_manifest(&verification_summary).map_err(|error| {
        AppError::new(
            AppErrorCategory::StorageFailure,
            format!("深度实验 RunManifest/RunRecord 读回失败: {error}"),
        )
    })
}
