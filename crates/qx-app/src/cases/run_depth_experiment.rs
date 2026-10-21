//! Bounded parameter experiments over the shared Rust Tick/OrderBook use case.

use super::artifacts::write_text;
use super::experiment_grid::{
    apply_depth_parameters, expand_grid, normalized_strategy, validate_depth_grid,
    validate_experiment_id,
};
use super::guard::guard_panics;
use super::run_depth_backtest::{run_inner, run_inner_with_experiment_strategy};
use super::{attach, ExperimentParameterSpace};
use crate::run_handle::CancellationToken;
use crate::{
    AppError, AppErrorCategory, CallerCapability, DepthBacktestOutcome, DepthBacktestSpec,
    RunContext, RunHandle, StrategyArtifactIdentity,
};
use qx_core::Fnv1a;
use qx_strategy::{BuiltinStrategyKind, Strategy};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

pub const RUN_DEPTH_EXPERIMENT_SCHEMA_VERSION: u32 = 1;

type StrategyFactory<'a> = dyn FnMut(&DepthBacktestSpec) -> Result<Box<dyn Strategy>, String> + 'a;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunDepthExperimentSpec {
    pub schema_version: u32,
    pub experiment_id: String,
    pub base: DepthBacktestSpec,
    pub parameter_space: Vec<ExperimentParameterSpace>,
    /// Optional strategy-selection dimension. Empty preserves the legacy spec fingerprint.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub strategy_kinds: Vec<BuiltinStrategyKind>,
}

impl RunDepthExperimentSpec {
    pub fn from_json(payload: &str) -> Result<Self, AppError> {
        let spec: Self = serde_json::from_str(payload)
            .map_err(|error| invalid(format!("RunDepthExperimentSpec JSON 无效: {error}")))?;
        spec.validate()?;
        Ok(spec)
    }

    pub fn to_json(&self) -> Result<String, AppError> {
        let mut canonical = self.clone();
        canonical.strategy_kinds.sort_by_key(|kind| kind.name());
        serde_json::to_string(&canonical)
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
        if kinds.iter().any(|kind| kind.needs_reference_leg()) {
            return Err(invalid(
                "双腿策略必须使用 RunMultiLegExperimentSpec 和双数据集",
            ));
        }
        if !self.strategy_kinds.is_empty() && self.base.market_making.is_some() {
            return Err(invalid(
                "market_making 参数实验不能与 strategy_kinds 内置策略选择混用",
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
                validate_depth_grid(
                    &candidate.strategy,
                    candidate.market_making,
                    &self.parameter_space,
                )?
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

    /// Run a parameter grid with a fresh caller-authored strategy for every depth candidate.
    pub fn run_with_strategy_factory<F>(
        &self,
        identity: &StrategyArtifactIdentity,
        factory: &mut F,
        context: &RunContext,
    ) -> Result<RunDepthExperimentResult, AppError>
    where
        F: FnMut(&DepthBacktestSpec) -> Result<Box<dyn Strategy>, String>,
    {
        context.require(
            CallerCapability::Research,
            "多语言 Tick/OrderBook 策略参数实验",
        )?;
        validate_strategy_experiment(self, identity)?;
        let correlation_id = context.correlation_id().to_string();
        guard_panics(&correlation_id, || {
            run_depth_experiment_inner(self, context, None, Some(identity), Some(factory))
        })
        .map_err(|error| attach(error, &correlation_id))?
        .ok_or_else(|| {
            attach(
                invariant("同步多语言深度参数实验未产生报告"),
                &correlation_id,
            )
        })
    }

    /// Run the grid through the cancellable Rust Tick/OrderBook backtest path.
    pub fn start(self, context: RunContext) -> RunHandle<RunDepthExperimentResult> {
        let run_id = context.correlation_id().to_string();
        RunHandle::spawn(run_id, move |cancellation| {
            context.require(CallerCapability::Research, "Tick/OrderBook 参数实验")?;
            let correlation_id = context.correlation_id().to_string();
            guard_panics(&correlation_id, || {
                run_depth_experiment_inner(&self, &context, Some(&cancellation), None, None)
            })
            .map_err(|error| attach(error, &correlation_id))
        })
    }

    /// Start a cancellable parameter grid with a fresh caller-authored depth strategy per candidate.
    pub fn start_with_strategy_factory<F>(
        self,
        identity: StrategyArtifactIdentity,
        mut factory: F,
        context: RunContext,
    ) -> Result<RunHandle<RunDepthExperimentResult>, AppError>
    where
        F: FnMut(&DepthBacktestSpec) -> Result<Box<dyn Strategy>, String> + Send + 'static,
    {
        context.require(
            CallerCapability::Research,
            "多语言 Tick/OrderBook 策略参数实验",
        )?;
        validate_strategy_experiment(&self, &identity)?;
        let run_id = context.correlation_id().to_string();
        Ok(RunHandle::spawn(run_id, move |cancellation| {
            let correlation_id = context.correlation_id().to_string();
            guard_panics(&correlation_id, || {
                run_depth_experiment_inner(
                    &self,
                    &context,
                    Some(&cancellation),
                    Some(&identity),
                    Some(&mut factory),
                )
            })
            .map_err(|error| attach(error, &correlation_id))
        }))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DepthExperimentCandidate {
    pub ordinal: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strategy_kind: Option<String>,
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
        run_depth_experiment_inner(spec, context, None, None, None)
    })
    .map_err(|error| attach(error, &correlation_id))?
    .ok_or_else(|| {
        attach(
            invariant("同步 Tick/OrderBook 参数实验未产生报告"),
            &correlation_id,
        )
    })
}

fn validate_strategy_experiment(
    spec: &RunDepthExperimentSpec,
    identity: &StrategyArtifactIdentity,
) -> Result<(), AppError> {
    spec.validate()?;
    if identity.strategy_id != spec.base.strategy.strategy_id {
        return Err(invalid(
            "caller strategy identity must match the primary depth strategy slot",
        ));
    }
    if !spec.strategy_kinds.is_empty() {
        return Err(invalid(
            "caller strategy factory cannot be combined with built-in strategy_kinds selection",
        ));
    }
    identity.validate_for_depth_experiment(&spec.base)?;
    Ok(())
}

fn run_depth_experiment_inner(
    spec: &RunDepthExperimentSpec,
    context: &RunContext,
    cancellation: Option<&CancellationToken>,
    identity: Option<&StrategyArtifactIdentity>,
    mut factory: Option<&mut StrategyFactory<'_>>,
) -> Result<Option<RunDepthExperimentResult>, AppError> {
    let total = spec.validate()?;
    let mut fingerprint = Fnv1a::new();
    fingerprint.write_text(&spec.to_json()?);
    if let Some(identity) = identity {
        fingerprint.write_text(&identity.strategy_id);
        fingerprint.write_text(&identity.strategy_version);
        fingerprint.write_text(&identity.source_sha256);
    }
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
        let outcome = match apply_depth_parameters(
            &mut candidate.strategy,
            &mut candidate.market_making,
            &parameters,
        ) {
            Ok(()) => match DepthBacktestSpec::validate_entry(
                &candidate.tier,
                candidate.strategy.kind,
                candidate.strategy.quantity_raw,
                candidate.fee_bps,
            ) {
                Err(error) => Err(error),
                Ok(()) => {
                    let run = match (identity, factory.as_deref_mut()) {
                        (Some(identity), Some(factory)) => factory(&candidate)
                            .map_err(|error| {
                                invalid(format!("候选 {ordinal} 自定义策略 factory 失败: {error}"))
                            })
                            .and_then(|strategy| {
                                run_inner_with_experiment_strategy(
                                    &candidate,
                                    context,
                                    cancellation,
                                    identity.clone(),
                                    strategy,
                                )
                            }),
                        (None, None) => run_inner(&candidate, context, cancellation),
                        _ => Err(invariant("自定义深度实验身份与 factory 状态不一致")),
                    };
                    match run {
                        Ok(Some(outcome)) => Ok(outcome),
                        Ok(None) => return Ok(None),
                        Err(error) => Err(error),
                    }
                }
            },
            Err(error) => Err(error),
        };
        candidates.push(match outcome {
            Ok(outcome) => DepthExperimentCandidate {
                ordinal,
                strategy_kind: (!spec.strategy_kinds.is_empty()).then(|| kind.name().to_string()),
                parameters,
                outcome: Some(outcome),
                error: None,
            },
            Err(error) => DepthExperimentCandidate {
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
