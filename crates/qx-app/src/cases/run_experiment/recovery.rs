use super::super::artifacts::{artifact_paths, write_text};
use super::super::experiment_fold_search::artifact_fingerprint;
use super::super::experiment_spec_artifact::{verify_spec_artifact, write_spec_artifact};
use super::super::verify_run::verify_run;
use super::{persistence, RunExperimentResult};
use crate::{AppError, AppErrorCategory, BacktestOutcome, BacktestSpec, RunContext};
use qx_spec::CanonicalRunSpec;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

pub(super) struct ExistingExperimentResume<'a> {
    pub(super) report_index_path: &'a str,
    pub(super) spec_artifact_path: &'a str,
    pub(super) persisted_input: &'a str,
    pub(super) spec_fingerprint: &'a str,
    pub(super) canonical_run_spec: &'a CanonicalRunSpec,
    pub(super) canonical_run_spec_json: &'a str,
    pub(super) canonical_run_spec_fingerprint: &'a str,
    pub(super) input_data_fingerprint: &'a str,
    pub(super) experiment_id: &'a str,
    pub(super) root: &'a Path,
    pub(super) context: &'a RunContext,
}

pub(super) fn resume_existing_experiment(
    request: ExistingExperimentResume<'_>,
) -> Result<Option<RunExperimentResult>, AppError> {
    let ExistingExperimentResume {
        report_index_path,
        spec_artifact_path,
        persisted_input,
        spec_fingerprint,
        canonical_run_spec,
        canonical_run_spec_json,
        canonical_run_spec_fingerprint,
        input_data_fingerprint,
        experiment_id,
        root,
        context,
    } = request;
    let existing = match std::fs::read_to_string(report_index_path) {
        Ok(existing) => existing,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(AppError::from_io("读取既有实验报告", &error)),
    };
    let mut previous = existing_result(
        &existing,
        spec_fingerprint,
        canonical_run_spec_fingerprint,
        input_data_fingerprint,
        experiment_id,
    )?;
    if has_retryable_search_failures(&previous) {
        // Successful candidate/scenario checkpoints remain authoritative. Missing/failed work
        // is resumed by the normal loop and the refreshed report replaces the old one.
        return Ok(None);
    }
    previous.input_data_fingerprint = Some(input_data_fingerprint.to_owned());
    previous.canonical_run_spec_json = Some(canonical_run_spec_json.to_owned());
    previous.canonical_run_spec_fingerprint = Some(canonical_run_spec_fingerprint.to_owned());
    persistence::verify_experiment_candidate_outcomes(&previous, context)?;
    if previous.run_manifest.is_none()
        && previous.run_record.is_none()
        && previous.result_hash.is_none()
        && previous.artifact_locations.is_none()
    {
        write_spec_artifact(spec_artifact_path, persisted_input, "参数实验")?;
        let upgraded = persistence::persist_experiment_result(
            previous,
            canonical_run_spec,
            root,
            report_index_path,
            spec_artifact_path,
        )?;
        return Ok(Some(upgraded));
    }
    persistence::verify_persisted_experiment_result(&previous)?;
    let has_input_artifact = previous
        .artifact_locations
        .as_ref()
        .and_then(|locations| locations.get("experiment_input"))
        .is_some_and(|path| path == spec_artifact_path);
    if has_input_artifact {
        verify_spec_artifact(spec_artifact_path, persisted_input, "参数实验")?;
    } else {
        write_spec_artifact(spec_artifact_path, persisted_input, "参数实验")?;
        let upgraded = persistence::persist_experiment_result(
            previous,
            canonical_run_spec,
            root,
            report_index_path,
            spec_artifact_path,
        )?;
        return Ok(Some(upgraded));
    }
    Ok(Some(previous))
}

#[derive(Serialize, Deserialize)]
pub(super) struct SearchCandidateCheckpoint {
    pub(super) spec_fingerprint: String,
    pub(super) input_data_fingerprint: String,
    pub(super) ordinal: usize,
    pub(super) candidate_index: usize,
    pub(super) proposal: String,
    pub(super) strategy_kind: String,
    pub(super) parameters: BTreeMap<String, i128>,
    pub(super) outcome: BacktestOutcome,
    pub(super) artifact_fingerprint: String,
    pub(super) training_score_bps: i32,
    pub(super) training_max_drawdown_bps: u32,
    #[serde(default)]
    pub(super) training_robustness_score_bps: Option<i32>,
}

// The explicit identity fields are all part of the checkpoint cache key.
#[allow(clippy::too_many_arguments)]
pub(super) fn load_search_candidate_checkpoint(
    checkpoint_path: &Path,
    spec_fingerprint: &str,
    input_data_fingerprint: &str,
    ordinal: usize,
    candidate_index: usize,
    proposal: &str,
    strategy_kind: &str,
    parameters: &BTreeMap<String, i128>,
    candidate: &BacktestSpec,
    context: &RunContext,
) -> Result<Option<SearchCandidateCheckpoint>, AppError> {
    let payload = match std::fs::read_to_string(checkpoint_path) {
        Ok(payload) => payload,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(AppError::from_io("读取搜索候选检查点", &error)),
    };
    let checkpoint: SearchCandidateCheckpoint =
        serde_json::from_str(&payload).map_err(|error| {
            AppError::new(
                AppErrorCategory::StorageFailure,
                format!("搜索候选检查点无法解析: {error}"),
            )
        })?;
    if checkpoint.spec_fingerprint != spec_fingerprint
        || checkpoint.input_data_fingerprint != input_data_fingerprint
        || checkpoint.ordinal != ordinal
        || checkpoint.candidate_index != candidate_index
        || checkpoint.proposal != proposal
        || checkpoint.strategy_kind != strategy_kind
        || checkpoint.parameters != *parameters
        || checkpoint.outcome.run_id != candidate.run_id
        || checkpoint.outcome.instrument != candidate.instrument
        || checkpoint.outcome.data_fingerprint != input_data_fingerprint
        || checkpoint.outcome.artifacts != artifact_paths(candidate)
    {
        return Err(AppError::new(
            AppErrorCategory::Conflict,
            format!("候选 {ordinal} 检查点身份或搜索轨迹不一致"),
        ));
    }
    let verification = verify_run(&checkpoint.outcome, context)?;
    if !verification.verified {
        return Err(AppError::new(
            AppErrorCategory::StorageFailure,
            format!(
                "候选 {ordinal} 检查点产物复核不通过: {}",
                verification.mismatches.join("; ")
            ),
        ));
    }
    if checkpoint.artifact_fingerprint != artifact_fingerprint(&checkpoint.outcome)? {
        return Err(AppError::new(
            AppErrorCategory::StorageFailure,
            format!("候选 {ordinal} 检查点产物内容已变化"),
        ));
    }
    Ok(Some(checkpoint))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn write_search_candidate_checkpoint(
    checkpoint_path: &Path,
    spec_fingerprint: &str,
    input_data_fingerprint: &str,
    ordinal: usize,
    candidate_index: usize,
    proposal: &str,
    strategy_kind: &str,
    parameters: &BTreeMap<String, i128>,
    outcome: &BacktestOutcome,
    training_score_bps: i32,
    training_max_drawdown_bps: u32,
    training_robustness_score_bps: Option<i32>,
) -> Result<(), AppError> {
    let checkpoint = SearchCandidateCheckpoint {
        spec_fingerprint: spec_fingerprint.to_owned(),
        input_data_fingerprint: input_data_fingerprint.to_owned(),
        ordinal,
        candidate_index,
        proposal: proposal.to_owned(),
        strategy_kind: strategy_kind.to_owned(),
        parameters: parameters.clone(),
        artifact_fingerprint: artifact_fingerprint(outcome)?,
        outcome: outcome.clone(),
        training_score_bps,
        training_max_drawdown_bps,
        training_robustness_score_bps,
    };
    let payload = serde_json::to_string(&checkpoint).map_err(|error| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            format!("搜索候选检查点序列化失败: {error}"),
        )
    })?;
    write_text(&checkpoint_path.to_string_lossy(), &payload)
}

pub(super) fn existing_result(
    payload: &str,
    expected_fingerprint: &str,
    expected_canonical_fingerprint: &str,
    expected_data_fingerprint: &str,
    experiment_id: &str,
) -> Result<RunExperimentResult, AppError> {
    let previous = RunExperimentResult::from_json(payload)?;
    let same_spec = previous
        .canonical_run_spec_fingerprint
        .as_deref()
        .map_or_else(
            || previous.spec_fingerprint == expected_fingerprint,
            |fingerprint| fingerprint == expected_canonical_fingerprint,
        );
    if previous.experiment_id != experiment_id || !same_spec {
        return Err(AppError::new(
            AppErrorCategory::Conflict,
            format!("experiment_id {experiment_id} 已绑定不同实验规格"),
        ));
    }
    let stored_data_fingerprint = previous
        .input_data_fingerprint
        .as_deref()
        .or_else(|| {
            previous
                .candidates
                .iter()
                .find_map(|candidate| candidate.outcome.as_ref())
                .map(|outcome| outcome.data_fingerprint.as_str())
        })
        .ok_or_else(|| {
            AppError::new(
                AppErrorCategory::Conflict,
                format!("experiment_id {experiment_id} 的既有报告没有输入数据身份，无法确认可复用"),
            )
        })?;
    if stored_data_fingerprint != expected_data_fingerprint {
        return Err(AppError::new(
            AppErrorCategory::Conflict,
            format!("experiment_id {experiment_id} 已绑定不同 BarFrame 数据"),
        ));
    }
    Ok(previous)
}

pub(super) fn has_retryable_search_failures(result: &RunExperimentResult) -> bool {
    let Some(search) = &result.search else {
        return false;
    };
    result
        .candidates
        .iter()
        .any(|candidate| candidate.outcome.is_none() && candidate.error.is_some())
        || result.cost_sensitivity.iter().any(|scenario| {
            scenario.outcome.is_none()
                || scenario
                    .neighborhood_cost_robustness
                    .as_ref()
                    .is_some_and(|neighborhood| {
                        neighborhood.candidates.iter().any(|candidate| {
                            candidate.outcome.is_none() && candidate.error.is_some()
                        })
                    })
        })
        || search.fold_searches.iter().any(|fold| {
            fold.candidates
                .iter()
                .any(|candidate| candidate.outcome.is_none() && candidate.error.is_some())
                || fold.cost_sensitivity.iter().any(|scenario| {
                    scenario.outcome.is_none()
                        || scenario.neighborhood_cost_robustness.as_ref().is_some_and(
                            |neighborhood| {
                                neighborhood.candidates.iter().any(|candidate| {
                                    candidate.outcome.is_none() && candidate.error.is_some()
                                })
                            },
                        )
                })
        })
}
