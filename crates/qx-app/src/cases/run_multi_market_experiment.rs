//! Bounded parameter/strategy experiments on the shared multi-market Bar engine.

use super::artifacts::write_text;
use super::attach;
use super::experiment_grid::{
    apply_parameters, expand_grid, normalized_strategy, validate_experiment_id, validate_grid,
};
use super::experiment_spec_artifact::{
    serialize_experiment_input, verify_spec_artifact, write_spec_artifact,
};
use super::guard::guard_panics;
use super::run_multi_market_backtest::{run_inner, validate_injected, InjectedMarketStrategy};
use crate::run_handle::CancellationToken;
use crate::{
    AppError, AppErrorCategory, CallerCapability, MultiMarketBacktestSpec, RunContext, RunHandle,
    StrategyArtifactIdentity,
};
use qx_core::Fnv1a;
use qx_strategy::{BuiltinStrategyKind, Strategy};
use std::collections::BTreeMap;
use std::path::PathBuf;

mod types;
pub use types::*;

pub const RUN_MULTI_MARKET_EXPERIMENT_SCHEMA_VERSION: u32 = 1;
pub const MAX_MULTI_MARKET_EXPERIMENT_CANDIDATES: usize = 256;

type StrategyFactory<'a> =
    dyn FnMut(&MultiMarketBacktestSpec) -> Result<Box<dyn Strategy>, String> + 'a;

impl RunMultiMarketExperimentResult {
    pub fn from_json(payload: &str) -> Result<Self, AppError> {
        serde_json::from_str(payload)
            .map_err(|error| AppError::new(AppErrorCategory::StorageFailure, error.to_string()))
    }

    pub fn to_json(&self) -> Result<String, AppError> {
        serde_json::to_string(self)
            .map_err(|error| invariant(format!("多市场实验结果序列化失败: {error}")))
    }
}

impl RunMultiMarketExperimentSpec {
    pub fn from_json(payload: &str) -> Result<Self, AppError> {
        let spec: Self = serde_json::from_str(payload)
            .map_err(|error| invalid(format!("RunMultiMarketExperimentSpec JSON 无效: {error}")))?;
        spec.validate()?;
        Ok(spec)
    }

    pub fn to_json(&self) -> Result<String, AppError> {
        let mut canonical = self.clone();
        canonical.strategy_kinds.sort_by_key(|kind| kind.name());
        serde_json::to_string(&canonical)
            .map_err(|error| invariant(format!("RunMultiMarketExperimentSpec 序列化失败: {error}")))
    }

    pub fn validate(&self) -> Result<usize, AppError> {
        if self.schema_version != RUN_MULTI_MARKET_EXPERIMENT_SCHEMA_VERSION {
            return Err(invalid(format!(
                "RunMultiMarketExperimentSpec schema_version={} 本版本只认 {}",
                self.schema_version, RUN_MULTI_MARKET_EXPERIMENT_SCHEMA_VERSION
            )));
        }
        validate_experiment_id(&self.experiment_id)?;
        self.base.validate()?;
        let targets = self
            .base
            .markets
            .iter()
            .flat_map(|market| &market.strategies)
            .filter(|slot| slot.strategy.strategy_id == self.target_strategy_id)
            .collect::<Vec<_>>();
        if targets.len() != 1 {
            return Err(invalid("target_strategy_id 必须唯一匹配一个多市场策略槽"));
        }
        if self.target_strategy_id.trim().is_empty() {
            return Err(invalid("target_strategy_id 不能为空"));
        }
        let base_strategy = &targets[0].strategy;
        let mut kinds = if self.strategy_kinds.is_empty() {
            vec![base_strategy.kind]
        } else {
            self.strategy_kinds.clone()
        };
        kinds.sort_by_key(|kind| kind.name());
        if kinds.iter().any(|kind| kind.needs_reference_leg()) {
            return Err(invalid(
                "多市场单槽实验不能选择需要 reference leg 的双腿策略",
            ));
        }
        if kinds.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(invalid("strategy_kinds 不得重复"));
        }
        if kinds.is_empty() {
            return Err(invalid("多市场实验没有策略候选"));
        }
        if self.parameter_space.is_empty() && self.strategy_kinds.is_empty() {
            return Err(invalid("多市场实验至少需要参数维度或 strategy_kinds"));
        }
        let mut total = 0_usize;
        for kind in kinds {
            let mut strategy = base_strategy.clone();
            strategy.kind = kind;
            let normalized = normalized_strategy(&strategy, &self.parameter_space);
            let count = if self.parameter_space.is_empty() {
                1
            } else {
                validate_grid(&normalized, &self.parameter_space)?
            };
            total = total
                .checked_add(count)
                .filter(|count| *count <= MAX_MULTI_MARKET_EXPERIMENT_CANDIDATES)
                .ok_or_else(|| invalid("多市场实验最多接受 256 个候选组合"))?;
        }
        Ok(total)
    }

    pub fn start(self, context: RunContext) -> RunHandle<RunMultiMarketExperimentResult> {
        let run_id = context.correlation_id().to_string();
        RunHandle::spawn(run_id, move |cancellation| {
            context.require(CallerCapability::Research, "多市场参数回测实验")?;
            let correlation_id = context.correlation_id().to_string();
            guard_panics(&correlation_id, || {
                run_inner_experiment(&self, &context, Some(&cancellation), None, None)
            })
            .map_err(|error| attach(error, &correlation_id))
        })
    }

    pub fn run_with_strategy_factory<F>(
        &self,
        identity: &StrategyArtifactIdentity,
        factory: &mut F,
        context: &RunContext,
    ) -> Result<RunMultiMarketExperimentResult, AppError>
    where
        F: FnMut(&MultiMarketBacktestSpec) -> Result<Box<dyn Strategy>, String>,
    {
        let correlation_id = context.correlation_id().to_string();
        context
            .require(CallerCapability::Research, "多语言策略多市场参数实验")
            .map_err(|error| attach(error, &correlation_id))?;
        self.validate()
            .map_err(|error| attach(error, &correlation_id))?;
        if !self.strategy_kinds.is_empty() {
            return Err(invalid("自定义策略工厂不能与 strategy_kinds 快速选择混用"));
        }
        validate_identity(self, identity)?;
        guard_panics(&correlation_id, || {
            run_inner_experiment(self, context, None, Some(identity), Some(factory))
        })
        .map_err(|error| attach(error, &correlation_id))?
        .ok_or_else(|| attach(invariant("同步多市场策略实验未产生报告"), &correlation_id))
    }

    pub fn start_with_strategy_factory<F>(
        self,
        identity: StrategyArtifactIdentity,
        mut factory: F,
        context: RunContext,
    ) -> Result<RunHandle<RunMultiMarketExperimentResult>, AppError>
    where
        F: FnMut(&MultiMarketBacktestSpec) -> Result<Box<dyn Strategy>, String> + Send + 'static,
    {
        let correlation_id = context.correlation_id().to_string();
        context
            .require(CallerCapability::Research, "多语言策略多市场参数实验")
            .map_err(|error| attach(error, &correlation_id))?;
        self.validate()
            .map_err(|error| attach(error, &correlation_id))?;
        if !self.strategy_kinds.is_empty() {
            return Err(invalid("自定义策略工厂不能与 strategy_kinds 快速选择混用"));
        }
        validate_identity(&self, &identity).map_err(|error| attach(error, &correlation_id))?;
        let run_id = context.correlation_id().to_string();
        Ok(RunHandle::spawn(run_id, move |cancellation| {
            let correlation_id = context.correlation_id().to_string();
            guard_panics(&correlation_id, || {
                run_inner_experiment(
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

pub fn run_multi_market_experiment(
    spec: &RunMultiMarketExperimentSpec,
    context: &RunContext,
) -> Result<RunMultiMarketExperimentResult, AppError> {
    let correlation_id = context.correlation_id().to_string();
    context.require(CallerCapability::Research, "多市场参数回测实验")?;
    guard_panics(&correlation_id, || {
        run_inner_experiment(spec, context, None, None, None)
    })
    .map_err(|error| attach(error, &correlation_id))?
    .ok_or_else(|| attach(invariant("同步多市场参数实验未产生报告"), &correlation_id))
}

fn run_inner_experiment(
    spec: &RunMultiMarketExperimentSpec,
    context: &RunContext,
    cancellation: Option<&CancellationToken>,
    identity: Option<&StrategyArtifactIdentity>,
    mut factory: Option<&mut StrategyFactory<'_>>,
) -> Result<Option<RunMultiMarketExperimentResult>, AppError> {
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
        .join(format!(
            "{}.multi-market-experiment.json",
            spec.experiment_id
        ))
        .to_string_lossy()
        .into_owned();
    let spec_path = artifact_path.replace(
        ".multi-market-experiment.json",
        ".multi-market-experiment.spec.json",
    );
    let canonical_spec =
        serialize_experiment_input(&spec.to_json()?, context.code_commit(), identity)?;
    match std::fs::read_to_string(&artifact_path) {
        Ok(existing) => {
            let result = existing_result(&existing, &spec_fingerprint, &spec.experiment_id)?;
            verify_spec_artifact(&spec_path, &canonical_spec, "多市场实验")?;
            return Ok(Some(result));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(AppError::from_io("读取既有多市场实验报告", &error)),
    }

    let target_kind = spec
        .base
        .markets
        .iter()
        .flat_map(|market| &market.strategies)
        .find(|slot| slot.strategy.strategy_id == spec.target_strategy_id)
        .map(|slot| slot.strategy.kind)
        .ok_or_else(|| invariant("validated target strategy slot disappeared"))?;
    let mut kinds = if spec.strategy_kinds.is_empty() {
        vec![target_kind]
    } else {
        spec.strategy_kinds.clone()
    };
    kinds.sort_by_key(|kind| kind.name());
    let combinations = if spec.parameter_space.is_empty() {
        vec![BTreeMap::new()]
    } else {
        expand_grid(&spec.parameter_space)
    };
    let mut candidates = Vec::with_capacity(total);
    for kind in kinds {
        for parameters in &combinations {
            if cancellation.is_some_and(CancellationToken::is_cancelled) {
                return Ok(None);
            }
            let ordinal = candidates.len() + 1;
            let mut candidate = spec.base.clone();
            candidate.run_id = format!("{}-{ordinal:04}", spec.experiment_id);
            candidate.output_dir = root
                .join(format!("run-{ordinal:04}"))
                .to_string_lossy()
                .into_owned();
            let target = candidate
                .markets
                .iter_mut()
                .flat_map(|market| &mut market.strategies)
                .find(|slot| slot.strategy.strategy_id == spec.target_strategy_id)
                .ok_or_else(|| invariant("validated target strategy slot disappeared"))?;
            target.strategy.kind = kind;
            if let Err(error) = apply_parameters(&mut target.strategy, parameters) {
                candidates.push(failed_candidate(
                    ordinal,
                    kind,
                    !spec.strategy_kinds.is_empty(),
                    parameters.clone(),
                    error,
                ));
                continue;
            }
            let injected = match (identity, factory.as_deref_mut()) {
                (Some(identity), Some(factory)) => {
                    let strategy = match factory(&candidate) {
                        Ok(strategy) => strategy,
                        Err(message) => {
                            candidates.push(failed_candidate(
                                ordinal,
                                kind,
                                false,
                                parameters.clone(),
                                invalid(message),
                            ));
                            continue;
                        }
                    };
                    // The callback factory receives the candidate's resolved parameters as
                    // configuration. The external Strategy itself replaces the built-in slot,
                    // so validate its binding against the original (default knob) slot contract.
                    let mut binding_spec = candidate.clone();
                    let binding_target = binding_spec
                        .markets
                        .iter_mut()
                        .flat_map(|market| &mut market.strategies)
                        .find(|slot| slot.strategy.strategy_id == spec.target_strategy_id)
                        .ok_or_else(|| invariant("validated target strategy slot disappeared"))?;
                    let base_target = spec
                        .base
                        .markets
                        .iter()
                        .flat_map(|market| &market.strategies)
                        .find(|slot| slot.strategy.strategy_id == spec.target_strategy_id)
                        .ok_or_else(|| invariant("validated target strategy slot disappeared"))?;
                    binding_target.strategy = crate::BuiltinStrategySpec::new(
                        base_target.strategy.kind,
                        base_target.strategy.strategy_id.clone(),
                    );
                    match validate_injected(&binding_spec, vec![(identity.clone(), strategy)]) {
                        Ok(injected) => injected,
                        Err(error) => {
                            candidates.push(failed_candidate(
                                ordinal,
                                kind,
                                false,
                                parameters.clone(),
                                error,
                            ));
                            continue;
                        }
                    }
                }
                (None, None) => BTreeMap::<String, InjectedMarketStrategy>::new(),
                _ => return Err(invariant("多市场实验策略工厂和身份必须同时提供")),
            };
            let outcome = match run_inner(&candidate, context, cancellation, injected) {
                Ok(Some(outcome)) => Ok(outcome),
                Ok(None) => return Ok(None),
                Err(error) => Err(error),
            };
            candidates.push(match outcome {
                Ok(outcome) => MultiMarketExperimentCandidate {
                    ordinal,
                    strategy_kind: (!spec.strategy_kinds.is_empty())
                        .then(|| kind.name().to_string()),
                    parameters: parameters.clone(),
                    outcome: Some(outcome),
                    error: None,
                },
                Err(error) => failed_candidate(
                    ordinal,
                    kind,
                    !spec.strategy_kinds.is_empty(),
                    parameters.clone(),
                    error,
                ),
            });
        }
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
                    outcome.shared_result_hash.clone(),
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
            |(index, (ordinal, run_id, return_bps, max_drawdown_bps, shared_result_hash))| {
                MultiMarketExperimentRankedCandidate {
                    rank: index + 1,
                    ordinal,
                    run_id,
                    return_bps,
                    max_drawdown_bps,
                    shared_result_hash,
                }
            },
        )
        .collect::<Vec<_>>();
    let succeeded_candidates = ranking.len();
    let result = RunMultiMarketExperimentResult {
        schema_version: RUN_MULTI_MARKET_EXPERIMENT_SCHEMA_VERSION,
        experiment_id: spec.experiment_id.clone(),
        target_strategy_id: spec.target_strategy_id.clone(),
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
    let report = result.to_json()?;
    std::fs::create_dir_all(&root)
        .map_err(|error| AppError::from_io("创建多市场实验目录", &error))?;
    write_spec_artifact(&spec_path, &canonical_spec, "多市场实验")?;
    if let Err(error) = write_text(&artifact_path, &report) {
        if let Ok(existing) = std::fs::read_to_string(&artifact_path) {
            return existing_result(&existing, &result.spec_fingerprint, &result.experiment_id)
                .map(Some);
        }
        return Err(error);
    }
    Ok(Some(result))
}

fn failed_candidate(
    ordinal: usize,
    kind: BuiltinStrategyKind,
    selected_kind: bool,
    parameters: BTreeMap<String, i128>,
    error: AppError,
) -> MultiMarketExperimentCandidate {
    MultiMarketExperimentCandidate {
        ordinal,
        strategy_kind: selected_kind.then(|| kind.name().to_string()),
        parameters,
        outcome: None,
        error: Some(error.to_json()),
    }
}

fn validate_identity(
    spec: &RunMultiMarketExperimentSpec,
    identity: &StrategyArtifactIdentity,
) -> Result<(), AppError> {
    if identity.strategy_id != spec.target_strategy_id {
        return Err(invalid("自定义策略身份必须匹配 target_strategy_id"));
    }
    let slot = spec
        .base
        .markets
        .iter()
        .flat_map(|market| &market.strategies)
        .find(|slot| slot.strategy.strategy_id == spec.target_strategy_id)
        .ok_or_else(|| invariant("validated target strategy slot disappeared"))?;
    let contract =
        crate::BuiltinStrategySpec::new(slot.strategy.kind, slot.strategy.strategy_id.clone());
    identity.validate_for_slot(&slot.strategy.strategy_id, &contract)
}

fn existing_result(
    payload: &str,
    expected_fingerprint: &str,
    experiment_id: &str,
) -> Result<RunMultiMarketExperimentResult, AppError> {
    let previous = RunMultiMarketExperimentResult::from_json(payload)?;
    if previous.experiment_id != experiment_id || previous.spec_fingerprint != expected_fingerprint
    {
        return Err(AppError::new(
            AppErrorCategory::Conflict,
            format!("experiment_id {experiment_id} 已绑定不同多市场实验规格"),
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
