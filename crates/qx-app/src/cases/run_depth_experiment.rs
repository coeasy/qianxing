//! Bounded parameter experiments over the shared Rust Tick/OrderBook use case.

use super::artifacts::write_text;
use super::experiment_grid::{
    apply_depth_parameters, expand_grid, normalized_strategy, validate_depth_grid,
    validate_experiment_id,
};
use super::guard::guard_panics;
use super::run_depth_backtest::run_inner;
use super::{attach, ExperimentParameterSpace};
use crate::run_handle::CancellationToken;
use crate::{
    AppError, AppErrorCategory, CallerCapability, DepthBacktestOutcome, DepthBacktestSpec,
    RunContext, RunHandle,
};
use qx_core::Fnv1a;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

pub const RUN_DEPTH_EXPERIMENT_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunDepthExperimentSpec {
    pub schema_version: u32,
    pub experiment_id: String,
    pub base: DepthBacktestSpec,
    pub parameter_space: Vec<ExperimentParameterSpace>,
}

impl RunDepthExperimentSpec {
    pub fn from_json(payload: &str) -> Result<Self, AppError> {
        let spec: Self = serde_json::from_str(payload)
            .map_err(|error| invalid(format!("RunDepthExperimentSpec JSON 无效: {error}")))?;
        spec.validate()?;
        Ok(spec)
    }

    pub fn to_json(&self) -> Result<String, AppError> {
        serde_json::to_string(self)
            .map_err(|error| invariant(format!("RunDepthExperimentSpec 序列化失败: {error}")))
    }

    pub fn validate(&self) -> Result<usize, AppError> {
        if self.schema_version != RUN_DEPTH_EXPERIMENT_SCHEMA_VERSION {
            return Err(invalid(format!(
                "RunDepthExperimentSpec schema_version={} 本版本只认 {}",
                self.schema_version, RUN_DEPTH_EXPERIMENT_SCHEMA_VERSION
            )));
        }
        validate_experiment_id(&self.experiment_id)?;
        let mut base = self.base.clone();
        base.strategy = normalized_strategy(&base.strategy, &self.parameter_space);
        base.validate()?;
        validate_depth_grid(
            &self.base.strategy,
            self.base.market_making,
            &self.parameter_space,
        )
    }

    /// Run the grid through the cancellable Rust Tick/OrderBook backtest path.
    pub fn start(self, context: RunContext) -> RunHandle<RunDepthExperimentResult> {
        let run_id = context.correlation_id().to_string();
        RunHandle::spawn(run_id, move |cancellation| {
            context.require(CallerCapability::Research, "Tick/OrderBook 参数实验")?;
            let correlation_id = context.correlation_id().to_string();
            guard_panics(&correlation_id, || {
                run_depth_experiment_inner(&self, &context, Some(&cancellation))
            })
            .map_err(|error| attach(error, &correlation_id))
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DepthExperimentCandidate {
    pub ordinal: usize,
    pub parameters: BTreeMap<String, i128>,
    pub outcome: Option<DepthBacktestOutcome>,
    /// Stable AppError JSON; candidate errors do not hide successful runs.
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DepthExperimentRankedCandidate {
    pub rank: usize,
    pub ordinal: usize,
    pub run_id: String,
    pub return_bps: i32,
    pub max_drawdown_bps: u32,
    pub result_hash: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunDepthExperimentResult {
    pub schema_version: u32,
    pub experiment_id: String,
    pub spec_fingerprint: String,
    pub total_candidates: usize,
    pub completed_candidates: usize,
    pub succeeded_candidates: usize,
    pub failed_candidates: usize,
    pub candidates: Vec<DepthExperimentCandidate>,
    pub ranking: Vec<DepthExperimentRankedCandidate>,
    pub artifact_path: String,
}

impl RunDepthExperimentResult {
    pub fn from_json(payload: &str) -> Result<Self, AppError> {
        serde_json::from_str(payload).map_err(|error| {
            AppError::new(
                AppErrorCategory::StorageFailure,
                format!("RunDepthExperimentResult JSON 无法读取: {error}"),
            )
        })
    }

    pub fn to_json(&self) -> Result<String, AppError> {
        serde_json::to_string(self)
            .map_err(|error| invariant(format!("RunDepthExperimentResult 序列化失败: {error}")))
    }
}

/// Expand and rank a bounded grid while every candidate uses the Rust depth engine.
pub fn run_depth_experiment(
    spec: &RunDepthExperimentSpec,
    context: &RunContext,
) -> Result<RunDepthExperimentResult, AppError> {
    context.require(CallerCapability::Research, "Tick/OrderBook 参数实验")?;
    let correlation_id = context.correlation_id().to_string();
    guard_panics(&correlation_id, || {
        run_depth_experiment_inner(spec, context, None)
    })
    .map_err(|error| attach(error, &correlation_id))?
    .ok_or_else(|| {
        attach(
            invariant("同步 Tick/OrderBook 参数实验未产生报告"),
            &correlation_id,
        )
    })
}

fn run_depth_experiment_inner(
    spec: &RunDepthExperimentSpec,
    context: &RunContext,
    cancellation: Option<&CancellationToken>,
) -> Result<Option<RunDepthExperimentResult>, AppError> {
    let total = spec.validate()?;
    let mut fingerprint = Fnv1a::new();
    fingerprint.write_text(&spec.to_json()?);
    let spec_fingerprint = format!("{:016x}", fingerprint.finish());
    let root = PathBuf::from(&spec.base.output_dir).join(&spec.experiment_id);
    let artifact_path = root
        .join(format!("{}.depth-experiment.json", spec.experiment_id))
        .to_string_lossy()
        .into_owned();
    match std::fs::read_to_string(&artifact_path) {
        Ok(existing) => {
            return existing_result(&existing, &spec_fingerprint, &spec.experiment_id).map(Some)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(AppError::from_io("读取既有深度参数实验报告", &error)),
    }

    let combinations = expand_grid(&spec.parameter_space);
    debug_assert_eq!(combinations.len(), total);
    let mut candidates = Vec::with_capacity(total);
    for (index, parameters) in combinations.into_iter().enumerate() {
        if cancellation.is_some_and(CancellationToken::is_cancelled) {
            return Ok(None);
        }
        let ordinal = index + 1;
        let mut candidate = spec.base.clone();
        candidate.run_id = format!("{}-{ordinal:04}", spec.experiment_id);
        candidate.output_dir = root
            .join(format!("run-{ordinal:04}"))
            .to_string_lossy()
            .into_owned();
        let outcome = match apply_depth_parameters(
            &mut candidate.strategy,
            &mut candidate.market_making,
            &parameters,
        ) {
            Ok(()) => match run_inner(&candidate, context, cancellation) {
                Ok(Some(outcome)) => Ok(outcome),
                Ok(None) => return Ok(None),
                Err(error) => Err(error),
            },
            Err(error) => Err(error),
        };
        candidates.push(match outcome {
            Ok(outcome) => DepthExperimentCandidate {
                ordinal,
                parameters,
                outcome: Some(outcome),
                error: None,
            },
            Err(error) => DepthExperimentCandidate {
                ordinal,
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
                    outcome.return_bps,
                    outcome.max_drawdown_bps,
                    outcome.result_hash.clone(),
                )
            })
        })
        .collect::<Vec<_>>();
    ranking.sort_by(|left, right| {
        right
            .2
            .cmp(&left.2)
            .then_with(|| left.3.cmp(&right.3))
            .then_with(|| left.0.cmp(&right.0))
    });
    let ranking = ranking
        .into_iter()
        .enumerate()
        .map(
            |(index, (ordinal, run_id, return_bps, max_drawdown_bps, result_hash))| {
                DepthExperimentRankedCandidate {
                    rank: index + 1,
                    ordinal,
                    run_id,
                    return_bps,
                    max_drawdown_bps,
                    result_hash,
                }
            },
        )
        .collect::<Vec<_>>();
    let succeeded_candidates = ranking.len();
    let result = RunDepthExperimentResult {
        schema_version: RUN_DEPTH_EXPERIMENT_SCHEMA_VERSION,
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
        .map_err(|error| AppError::from_io("创建深度参数实验产物目录", &error))?;
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
) -> Result<RunDepthExperimentResult, AppError> {
    let previous = RunDepthExperimentResult::from_json(payload)?;
    if previous.experiment_id != experiment_id || previous.spec_fingerprint != expected_fingerprint
    {
        return Err(AppError::new(
            AppErrorCategory::Conflict,
            format!("experiment_id {experiment_id} 已绑定不同 Tick/OrderBook 实验规格"),
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
