use super::super::artifacts::{artifact_paths, read_text, write_text};
use super::super::verify_run::verify_run;
use crate::{AppError, AppErrorCategory, BacktestOutcome, BacktestSpec, RunContext};
use qx_core::Fnv1a;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Serialize, Deserialize)]
pub(super) struct CostScenarioCheckpoint {
    spec_fingerprint: String,
    input_data_fingerprint: String,
    fold: usize,
    selected_ordinal: usize,
    scenario_index: usize,
    scenario_name: String,
    parameters: BTreeMap<String, i128>,
    outcome: BacktestOutcome,
    artifact_fingerprint: String,
}

/// Reuse a successful cost-stress run only after checking the experiment identity,
/// selected training candidate, scenario position, run paths, replay verification,
/// and the full bytes of every produced artifact. Failed scenarios are not cached.
// Keep the cache identity fields explicit so callers cannot accidentally omit part of the key.
#[allow(clippy::too_many_arguments)]
pub(in crate::cases) fn load_cost_scenario_checkpoint(
    checkpoint_path: &Path,
    spec_fingerprint: &str,
    input_data_fingerprint: &str,
    fold: usize,
    selected_ordinal: usize,
    scenario_index: usize,
    scenario_name: &str,
    parameters: &BTreeMap<String, i128>,
    candidate: &crate::BacktestSpec,
    context: &RunContext,
) -> Result<Option<BacktestOutcome>, AppError> {
    let payload = match std::fs::read_to_string(checkpoint_path) {
        Ok(payload) => payload,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(AppError::from_io("读取成本情景检查点", &error)),
    };
    let checkpoint: CostScenarioCheckpoint = serde_json::from_str(&payload).map_err(|error| {
        AppError::new(
            AppErrorCategory::StorageFailure,
            format!("成本情景检查点无法解析: {error}"),
        )
    })?;
    if checkpoint.spec_fingerprint != spec_fingerprint
        || checkpoint.input_data_fingerprint != input_data_fingerprint
        || checkpoint.fold != fold
        || checkpoint.selected_ordinal != selected_ordinal
        || checkpoint.scenario_index != scenario_index
        || checkpoint.scenario_name != scenario_name
        || checkpoint.parameters != *parameters
        || checkpoint.outcome.run_id != candidate.run_id
        || checkpoint.outcome.artifacts != artifact_paths(candidate)
    {
        return Err(AppError::new(
            AppErrorCategory::Conflict,
            format!("成本情景 {scenario_index} 检查点身份、选中候选或情景序号不一致"),
        ));
    }
    let verification = verify_run(&checkpoint.outcome, context)?;
    if !verification.verified {
        return Err(AppError::new(
            AppErrorCategory::StorageFailure,
            format!(
                "成本情景 {scenario_index} 检查点回测产物复核不通过: {}",
                verification.mismatches.join("; ")
            ),
        ));
    }
    if checkpoint.artifact_fingerprint != artifact_fingerprint(&checkpoint.outcome)? {
        return Err(AppError::new(
            AppErrorCategory::StorageFailure,
            format!("成本情景 {scenario_index} 检查点产物内容已变化"),
        ));
    }
    Ok(Some(checkpoint.outcome))
}

// Persist the same explicit cache identity that the loader verifies.
#[allow(clippy::too_many_arguments)]
pub(in crate::cases) fn write_cost_scenario_checkpoint(
    checkpoint_path: &Path,
    spec_fingerprint: &str,
    input_data_fingerprint: &str,
    fold: usize,
    selected_ordinal: usize,
    scenario_index: usize,
    scenario_name: &str,
    parameters: &BTreeMap<String, i128>,
    outcome: &BacktestOutcome,
) -> Result<(), AppError> {
    let checkpoint = CostScenarioCheckpoint {
        spec_fingerprint: spec_fingerprint.to_owned(),
        input_data_fingerprint: input_data_fingerprint.to_owned(),
        fold,
        selected_ordinal,
        scenario_index,
        scenario_name: scenario_name.to_owned(),
        parameters: parameters.clone(),
        outcome: outcome.clone(),
        artifact_fingerprint: artifact_fingerprint(outcome)?,
    };
    let payload = serde_json::to_string(&checkpoint).map_err(|error| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            format!("成本情景检查点序列化失败: {error}"),
        )
    })?;
    write_text(&checkpoint_path.to_string_lossy(), &payload)
}

#[derive(Serialize, Deserialize)]
pub(super) struct FoldCandidateCheckpoint {
    pub(super) spec_fingerprint: String,
    pub(super) input_data_fingerprint: String,
    pub(super) fold: usize,
    pub(super) ordinal: usize,
    pub(super) proposal: String,
    pub(super) parameters: BTreeMap<String, i128>,
    pub(super) outcome: BacktestOutcome,
    pub(super) artifact_fingerprint: String,
    pub(super) training_score_bps: i32,
    pub(super) training_max_drawdown_bps: u32,
    #[serde(default)]
    pub(super) training_robustness_score_bps: Option<i32>,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn load_fold_candidate_checkpoint(
    checkpoint_path: &Path,
    spec_fingerprint: &str,
    input_data_fingerprint: &str,
    fold: usize,
    ordinal: usize,
    proposal: &str,
    parameters: &BTreeMap<String, i128>,
    candidate: &BacktestSpec,
    fold_data_fingerprint: &str,
    context: &RunContext,
) -> Result<Option<FoldCandidateCheckpoint>, AppError> {
    let payload = match std::fs::read_to_string(checkpoint_path) {
        Ok(payload) => payload,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(AppError::from_io("读取折内候选检查点", &error)),
    };
    let checkpoint: FoldCandidateCheckpoint = serde_json::from_str(&payload).map_err(|error| {
        AppError::new(
            AppErrorCategory::StorageFailure,
            format!("折内候选检查点无法解析: {error}"),
        )
    })?;
    if checkpoint.spec_fingerprint != spec_fingerprint
        || checkpoint.input_data_fingerprint != input_data_fingerprint
        || checkpoint.fold != fold
        || checkpoint.ordinal != ordinal
        || checkpoint.proposal != proposal
        || checkpoint.parameters != *parameters
        || checkpoint.outcome.run_id != candidate.run_id
        || checkpoint.outcome.data_fingerprint != fold_data_fingerprint
        || checkpoint.outcome.artifacts != artifact_paths(candidate)
    {
        return Err(AppError::new(
            AppErrorCategory::Conflict,
            format!("第 {fold} 折候选 {ordinal} 检查点身份或搜索轨迹不一致"),
        ));
    }
    let verification = verify_run(&checkpoint.outcome, context)?;
    if !verification.verified {
        return Err(AppError::new(
            AppErrorCategory::StorageFailure,
            format!(
                "第 {fold} 折候选 {ordinal} 检查点产物复核不通过: {}",
                verification.mismatches.join("; ")
            ),
        ));
    }
    if checkpoint.artifact_fingerprint != artifact_fingerprint(&checkpoint.outcome)? {
        return Err(AppError::new(
            AppErrorCategory::StorageFailure,
            format!("第 {fold} 折候选 {ordinal} 检查点产物内容已变化"),
        ));
    }
    Ok(Some(checkpoint))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn write_fold_candidate_checkpoint(
    checkpoint_path: &Path,
    spec_fingerprint: &str,
    input_data_fingerprint: &str,
    fold: usize,
    ordinal: usize,
    proposal: &str,
    parameters: &BTreeMap<String, i128>,
    outcome: &BacktestOutcome,
    training_score_bps: i32,
    training_max_drawdown_bps: u32,
    training_robustness_score_bps: Option<i32>,
) -> Result<(), AppError> {
    let checkpoint = FoldCandidateCheckpoint {
        spec_fingerprint: spec_fingerprint.to_owned(),
        input_data_fingerprint: input_data_fingerprint.to_owned(),
        fold,
        ordinal,
        proposal: proposal.to_owned(),
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
            format!("折内候选检查点序列化失败: {error}"),
        )
    })?;
    write_text(&checkpoint_path.to_string_lossy(), &payload)
}

pub(in crate::cases) fn artifact_fingerprint(
    outcome: &BacktestOutcome,
) -> Result<String, AppError> {
    let mut hash = Fnv1a::new();
    let paths = [
        Some(outcome.artifacts.run_manifest.as_str()),
        Some(outcome.artifacts.summary.as_str()),
        Some(outcome.artifacts.equity.as_str()),
        Some(outcome.artifacts.fills.as_str()),
        outcome.artifacts.event_trace.as_deref(),
    ];
    for path in paths.into_iter().flatten() {
        hash.write_text(path);
        hash.write_text(&read_text(path)?);
    }
    Ok(format!("{:016x}", hash.finish()))
}
