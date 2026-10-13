//! Bounded parameter experiments over the shared Rust two-leg backtest use case.

use super::artifacts::write_text;
use super::experiment_grid::{
    apply_parameters, expand_grid, normalized_strategy, validate_experiment_id, validate_grid,
};
use super::guard::guard_panics;
use super::run_multi_leg_backtest::run_inner;
use super::{attach, ExperimentParameterSpace};
use crate::run_handle::CancellationToken;
use crate::{
    AppError, AppErrorCategory, CallerCapability, MultiLegBacktestOutcome, MultiLegBacktestSpec,
    RunContext, RunHandle,
};
use qx_core::Fnv1a;
use qx_strategy::BuiltinStrategyKind;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

pub const RUN_MULTI_LEG_EXPERIMENT_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunMultiLegExperimentSpec {
    pub schema_version: u32,
    pub experiment_id: String,
    pub base: MultiLegBacktestSpec,
    pub parameter_space: Vec<ExperimentParameterSpace>,
    /// Optional multi-leg built-in strategy dimension; omitted to preserve old fingerprints.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub strategy_kinds: Vec<BuiltinStrategyKind>,
}

impl RunMultiLegExperimentSpec {
    pub fn from_json(payload: &str) -> Result<Self, AppError> {
        let spec: Self = serde_json::from_str(payload)
            .map_err(|error| invalid(format!("RunMultiLegExperimentSpec JSON 无效: {error}")))?;
        spec.validate()?;
        Ok(spec)
    }

    pub fn to_json(&self) -> Result<String, AppError> {
        let mut canonical = self.clone();
        canonical.strategy_kinds.sort_by_key(|kind| kind.name());
        serde_json::to_string(&canonical)
            .map_err(|error| invariant(format!("RunMultiLegExperimentSpec 序列化失败: {error}")))
    }

    pub fn validate(&self) -> Result<usize, AppError> {
        if self.schema_version != RUN_MULTI_LEG_EXPERIMENT_SCHEMA_VERSION {
            return Err(invalid(format!(
                "RunMultiLegExperimentSpec schema_version={} 本版本只认 {}",
                self.schema_version, RUN_MULTI_LEG_EXPERIMENT_SCHEMA_VERSION
            )));
        }
        validate_experiment_id(&self.experiment_id)?;
        let mut kinds = if self.strategy_kinds.is_empty() {
            vec![self.base.strategy.kind]
        } else {
            self.strategy_kinds.clone()
        };
        kinds.sort_by_key(|kind| kind.name());
        let mut seen = BTreeSet::new();
        if kinds.iter().any(|kind| !seen.insert(kind.name())) {
            return Err(invalid("strategy_kinds 不得重复"));
        }
        if kinds.iter().any(|kind| !kind.needs_reference_leg()) {
            return Err(invalid(
                "多腿实验的 strategy_kinds 只能包含需要 reference leg 的策略",
            ));
        }
        let strategy_selection = !self.strategy_kinds.is_empty();
        let mut per_kind = None;
        for kind in &kinds {
            let mut candidate = self.base.clone();
            candidate.strategy.kind = *kind;
            candidate.strategy = normalized_strategy(&candidate.strategy, &self.parameter_space);
            candidate.validate()?;
            let count = if strategy_selection && self.parameter_space.is_empty() {
                1
            } else {
                validate_grid(&candidate.strategy, &self.parameter_space)?
            };
            if per_kind
                .replace(count)
                .is_some_and(|previous| previous != count)
            {
                return Err(invalid("不同策略 kind 的参数网格候选数必须一致"));
            }
        }
        per_kind
            .unwrap_or_default()
            .checked_mul(kinds.len())
            .filter(|total| *total <= super::run_experiment::MAX_EXPERIMENT_CANDIDATES)
            .ok_or_else(|| invalid("实验最多接受 256 个候选组合"))
    }

    /// Run the bounded grid on cancellable Rust multi-leg backtests.
    pub fn start(self, context: RunContext) -> RunHandle<RunMultiLegExperimentResult> {
        let run_id = context.correlation_id().to_string();
        RunHandle::spawn(run_id, move |cancellation| {
            context.require(CallerCapability::Research, "多腿参数回测实验")?;
            let correlation_id = context.correlation_id().to_string();
            guard_panics(&correlation_id, || {
                run_inner_experiment(&self, &context, Some(&cancellation))
            })
            .map_err(|error| attach(error, &correlation_id))
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MultiLegExperimentCandidate {
    pub ordinal: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strategy_kind: Option<String>,
    pub parameters: BTreeMap<String, i128>,
    pub outcome: Option<MultiLegBacktestOutcome>,
    /// Stable AppError JSON; a failed candidate does not stop the other runs.
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MultiLegExperimentRankedCandidate {
    pub rank: usize,
    pub ordinal: usize,
    pub run_id: String,
    pub combined_return_bps: i32,
    pub primary_result_hash: String,
    pub reference_result_hash: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunMultiLegExperimentResult {
    pub schema_version: u32,
    pub experiment_id: String,
    pub spec_fingerprint: String,
    pub total_candidates: usize,
    pub completed_candidates: usize,
    pub succeeded_candidates: usize,
    pub failed_candidates: usize,
    pub candidates: Vec<MultiLegExperimentCandidate>,
    /// Stable return ranking; ties retain candidate ordinal. Risk metrics remain in each
    /// candidate's attribution artifact and are not recomputed by the experiment layer.
    pub ranking: Vec<MultiLegExperimentRankedCandidate>,
    pub artifact_path: String,
}

impl RunMultiLegExperimentResult {
    pub fn from_json(payload: &str) -> Result<Self, AppError> {
        serde_json::from_str(payload)
            .map_err(|error| AppError::new(AppErrorCategory::StorageFailure, error.to_string()))
    }

    pub fn to_json(&self) -> Result<String, AppError> {
        serde_json::to_string(self)
            .map_err(|error| invariant(format!("多腿实验结果序列化失败: {error}")))
    }
}

pub fn run_multi_leg_experiment(
    spec: &RunMultiLegExperimentSpec,
    context: &RunContext,
) -> Result<RunMultiLegExperimentResult, AppError> {
    let correlation_id = context.correlation_id().to_string();
    context.require(CallerCapability::Research, "多腿参数回测实验")?;
    guard_panics(&correlation_id, || {
        run_inner_experiment(spec, context, None)
    })
    .map_err(|error| attach(error, &correlation_id))?
    .ok_or_else(|| attach(invariant("同步多腿参数实验未产生报告"), &correlation_id))
}

fn run_inner_experiment(
    spec: &RunMultiLegExperimentSpec,
    context: &RunContext,
    cancellation: Option<&CancellationToken>,
) -> Result<Option<RunMultiLegExperimentResult>, AppError> {
    let total = spec.validate()?;
    let spec_json = spec.to_json()?;
    let mut fingerprint = Fnv1a::new();
    fingerprint.write_text(&spec_json);
    let spec_fingerprint = format!("{:016x}", fingerprint.finish());
    let root = PathBuf::from(&spec.base.output_dir).join(&spec.experiment_id);
    let artifact_path = root
        .join(format!("{}.multi-leg-experiment.json", spec.experiment_id))
        .to_string_lossy()
        .into_owned();
    match std::fs::read_to_string(&artifact_path) {
        Ok(existing) => {
            return existing_result(&existing, &spec_fingerprint, &spec.experiment_id).map(Some)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(AppError::from_io("读取既有多腿实验报告", &error)),
    }

    let mut kinds = if spec.strategy_kinds.is_empty() {
        vec![spec.base.strategy.kind]
    } else {
        spec.strategy_kinds.clone()
    };
    kinds.sort_by_key(|kind| kind.name());
    let combinations = if spec.parameter_space.is_empty() {
        vec![BTreeMap::new()]
    } else {
        expand_grid(&spec.parameter_space)
    };
    debug_assert_eq!(combinations.len() * kinds.len(), total);
    let mut candidates = Vec::with_capacity(total);
    for (index, (kind, parameters)) in kinds
        .into_iter()
        .flat_map(|kind| {
            combinations
                .iter()
                .cloned()
                .map(move |parameters| (kind, parameters))
        })
        .enumerate()
    {
        if cancellation.is_some_and(CancellationToken::is_cancelled) {
            return Ok(None);
        }
        let ordinal = index + 1;
        let mut candidate = spec.base.clone();
        candidate.strategy.kind = kind;
        candidate.run_id = format!("{}-{ordinal:04}", spec.experiment_id);
        candidate.output_dir = root
            .join(format!("run-{ordinal:04}"))
            .to_string_lossy()
            .into_owned();
        let outcome = match apply_parameters(&mut candidate.strategy, &parameters) {
            Ok(()) => match run_inner(&candidate, context, cancellation, None) {
                Ok(Some(outcome)) => Ok(outcome),
                Ok(None) => return Ok(None),
                Err(error) => Err(error),
            },
            Err(error) => Err(error),
        };
        candidates.push(match outcome {
            Ok(outcome) => MultiLegExperimentCandidate {
                ordinal,
                strategy_kind: (!spec.strategy_kinds.is_empty()).then(|| kind.name().to_string()),
                parameters,
                outcome: Some(outcome),
                error: None,
            },
            Err(error) => MultiLegExperimentCandidate {
                ordinal,
                strategy_kind: (!spec.strategy_kinds.is_empty()).then(|| kind.name().to_string()),
                parameters,
                outcome: None,
                error: Some(error.to_json()),
            },
        });
    }

    let mut ranking = candidates
        .iter()
        .filter_map(|candidate| {
            candidate.outcome.as_ref().map(|outcome| {
                (
                    candidate.ordinal,
                    outcome.run_id.clone(),
                    outcome.combined_return_bps,
                    outcome.primary_result_hash.clone(),
                    outcome.reference_result_hash.clone(),
                )
            })
        })
        .collect::<Vec<_>>();
    ranking.sort_by(|left, right| right.2.cmp(&left.2).then_with(|| left.0.cmp(&right.0)));
    let ranking = ranking
        .into_iter()
        .enumerate()
        .map(
            |(
                index,
                (ordinal, run_id, combined_return_bps, primary_result_hash, reference_result_hash),
            )| {
                MultiLegExperimentRankedCandidate {
                    rank: index + 1,
                    ordinal,
                    run_id,
                    combined_return_bps,
                    primary_result_hash,
                    reference_result_hash,
                }
            },
        )
        .collect::<Vec<_>>();
    let succeeded_candidates = ranking.len();
    let result = RunMultiLegExperimentResult {
        schema_version: RUN_MULTI_LEG_EXPERIMENT_SCHEMA_VERSION,
        experiment_id: spec.experiment_id.clone(),
        spec_fingerprint,
        total_candidates: total,
        completed_candidates: candidates.len(),
        succeeded_candidates,
        failed_candidates: total - succeeded_candidates,
        candidates,
        ranking,
        artifact_path: artifact_path.clone(),
    };
    if cancellation.is_some_and(CancellationToken::is_cancelled) {
        return Ok(None);
    }
    let report_json = result.to_json()?;
    std::fs::create_dir_all(&root)
        .map_err(|error| AppError::from_io("创建多腿实验产物目录", &error))?;
    if let Err(write_error) = write_text(&artifact_path, &report_json) {
        if let Ok(existing) = std::fs::read_to_string(&artifact_path) {
            return existing_result(&existing, &result.spec_fingerprint, &result.experiment_id)
                .map(Some);
        }
        return Err(write_error);
    }
    Ok(Some(result))
}

fn existing_result(
    payload: &str,
    expected_fingerprint: &str,
    experiment_id: &str,
) -> Result<RunMultiLegExperimentResult, AppError> {
    let previous = RunMultiLegExperimentResult::from_json(payload)?;
    if previous.experiment_id != experiment_id || previous.spec_fingerprint != expected_fingerprint
    {
        return Err(AppError::new(
            AppErrorCategory::Conflict,
            format!("experiment_id {experiment_id} 已绑定不同实验规格"),
        ));
    }
    Ok(previous)
}

fn invalid(message: impl Into<String>) -> AppError {
    AppError::new(AppErrorCategory::InvalidInput, message)
}

fn invariant(message: impl Into<String>) -> AppError {
    AppError::new(AppErrorCategory::InternalInvariant, message)
}
