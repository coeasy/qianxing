//! Recovery and upgrade rules for existing multi-market experiment reports.

use super::artifacts;
use super::RunMultiMarketExperimentResult;
use crate::{AppError, AppErrorCategory};
use qx_spec::CanonicalRunSpec;
use std::path::Path;

pub(super) struct ExistingMultiMarketExperiment<'a> {
    pub(super) artifact_path: &'a str,
    pub(super) spec_path: &'a str,
    pub(super) spec_fingerprint: &'a str,
    pub(super) canonical_run_spec: &'a CanonicalRunSpec,
    pub(super) canonical_run_spec_json: &'a str,
    pub(super) canonical_run_spec_fingerprint: &'a str,
    pub(super) input_data_fingerprint: &'a str,
    pub(super) canonical_input: &'a str,
    pub(super) experiment_id: &'a str,
    pub(super) root: &'a Path,
}

pub(super) fn resume_existing_experiment(
    request: ExistingMultiMarketExperiment<'_>,
) -> Result<Option<RunMultiMarketExperimentResult>, AppError> {
    let ExistingMultiMarketExperiment {
        artifact_path,
        spec_path,
        spec_fingerprint,
        canonical_run_spec,
        canonical_run_spec_json,
        canonical_run_spec_fingerprint,
        input_data_fingerprint,
        canonical_input,
        experiment_id,
        root,
    } = request;
    let existing = match std::fs::read_to_string(artifact_path) {
        Ok(existing) => existing,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(AppError::from_io("读取既有多市场实验报告", &error)),
    };
    let mut result = existing_result(
        &existing,
        spec_fingerprint,
        canonical_run_spec_fingerprint,
        experiment_id,
    )?;
    super::super::experiment_spec_artifact::verify_spec_artifact(
        spec_path,
        canonical_input,
        "多市场实验",
    )?;
    if result
        .canonical_run_spec_json
        .as_deref()
        .is_some_and(|stored| stored != canonical_run_spec_json)
        || result
            .canonical_run_spec_fingerprint
            .as_deref()
            .is_some_and(|stored| stored != canonical_run_spec_fingerprint)
        || result
            .input_data_fingerprint
            .as_deref()
            .is_some_and(|stored| stored != input_data_fingerprint)
    {
        return Err(AppError::new(
            AppErrorCategory::Conflict,
            "既有多市场实验已绑定不同数据或 CanonicalRunSpec",
        ));
    }
    let envelope_fields = [
        result.result_hash.is_some(),
        result.run_manifest.is_some(),
        result.run_record.is_some(),
        result.artifact_locations.is_some(),
    ];
    if envelope_fields.iter().all(|present| *present) {
        if result.canonical_run_spec_json.as_deref() != Some(canonical_run_spec_json)
            || result.canonical_run_spec_fingerprint.as_deref()
                != Some(canonical_run_spec_fingerprint)
            || result.input_data_fingerprint.as_deref() != Some(input_data_fingerprint)
        {
            return Err(AppError::new(
                AppErrorCategory::StorageFailure,
                "已发布的多市场实验缺少或错配标准身份",
            ));
        }
        artifacts::verify_persisted_result(&result)?;
        return Ok(Some(result));
    }
    if envelope_fields.iter().any(|present| *present) {
        return Err(AppError::new(
            AppErrorCategory::StorageFailure,
            "既有多市场实验的 result_hash/manifest/record/artifact_locations 必须完整或全部缺省",
        ));
    }
    for candidate in &result.candidates {
        if candidate.outcome.as_ref().is_some_and(|outcome| {
            outcome.data_fingerprint != input_data_fingerprint
                || outcome.markets_total != canonical_run_spec.datasets.len()
        }) {
            return Err(AppError::new(
                AppErrorCategory::Conflict,
                "既有多市场实验候选绑定的数据与当前输入不一致，不能升级运行身份",
            ));
        }
    }
    result.canonical_run_spec_json = Some(canonical_run_spec_json.to_owned());
    result.canonical_run_spec_fingerprint = Some(canonical_run_spec_fingerprint.to_owned());
    result.input_data_fingerprint = Some(input_data_fingerprint.to_owned());
    artifacts::write_input_spec(spec_path, canonical_input)?;
    let upgraded =
        artifacts::persist_result(result, canonical_run_spec, root, artifact_path, spec_path)?;
    artifacts::verify_persisted_result(&upgraded)?;
    Ok(Some(upgraded))
}

fn existing_result(
    payload: &str,
    expected_fingerprint: &str,
    expected_canonical_fingerprint: &str,
    experiment_id: &str,
) -> Result<RunMultiMarketExperimentResult, AppError> {
    let previous = RunMultiMarketExperimentResult::from_json(payload)?;
    let same_legacy_identity = previous.spec_fingerprint == expected_fingerprint;
    let same_canonical_identity =
        previous.canonical_run_spec_fingerprint.as_deref() == Some(expected_canonical_fingerprint);
    if previous.experiment_id != experiment_id
        || (!same_legacy_identity && !same_canonical_identity)
    {
        return Err(AppError::new(
            AppErrorCategory::Conflict,
            format!("experiment_id {experiment_id} 已绑定不同多市场实验规格"),
        ));
    }
    Ok(previous)
}
