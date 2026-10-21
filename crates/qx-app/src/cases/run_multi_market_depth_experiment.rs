//! Bounded strategy/parameter experiments over the shared multi-market L1/L2 portfolio engine.

use super::artifacts::write_text;
use super::attach;
use super::experiment_grid::{
    apply_parameters, expand_grid, normalized_strategy, validate_experiment_id, validate_grid,
};
use super::experiment_spec_artifact::{
    serialize_experiment_input, verify_spec_artifact, write_spec_artifact,
};
use super::guard::guard_panics;
use super::run_multi_market_depth_backtest::run_candidate;
use crate::run_handle::CancellationToken;
use crate::{
    AppError, AppErrorCategory, CallerCapability, MultiMarketDepthBacktestSpec, RunContext,
    RunHandle, StrategyArtifactIdentity,
};
use qx_core::Fnv1a;
use qx_strategy::{BuiltinStrategyKind, Strategy};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

mod spec;
mod types;
pub use types::*;

pub const RUN_MULTI_MARKET_DEPTH_EXPERIMENT_SCHEMA_VERSION: u32 = 1;
pub const MAX_MULTI_MARKET_DEPTH_EXPERIMENT_CANDIDATES: usize = 256;

impl RunMultiMarketDepthExperimentSpec {
    pub fn start(self, context: RunContext) -> RunHandle<RunMultiMarketDepthExperimentResult> {
        let run_id = context.correlation_id().to_string();
        RunHandle::spawn(run_id, move |cancellation| {
            context.require(CallerCapability::Research, "多市场盘口参数实验")?;
            let correlation_id = context.correlation_id().to_string();
            guard_panics(&correlation_id, || {
                run_inner(&self, &context, Some(&cancellation), None, None)
            })
            .map_err(|error| attach(error, &correlation_id))
        })
    }
}

type StrategyFactory<'a> = dyn FnMut(
        &MultiMarketDepthBacktestSpec,
        &BTreeMap<String, i128>,
    ) -> Result<Box<dyn Strategy>, String>
    + 'a;

pub fn run_multi_market_depth_experiment(
    spec: &RunMultiMarketDepthExperimentSpec,
    context: &RunContext,
) -> Result<RunMultiMarketDepthExperimentResult, AppError> {
    let correlation_id = context.correlation_id().to_string();
    context.require(CallerCapability::Research, "多市场盘口参数实验")?;
    guard_panics(&correlation_id, || {
        run_inner(spec, context, None, None, None)
    })
    .map_err(|error| attach(error, &correlation_id))?
    .ok_or_else(|| attach(invariant("同步多市场盘口实验未产生报告"), &correlation_id))
}

fn run_inner(
    spec: &RunMultiMarketDepthExperimentSpec,
    context: &RunContext,
    cancellation: Option<&CancellationToken>,
    identity: Option<&StrategyArtifactIdentity>,
    mut factory: Option<&mut StrategyFactory<'_>>,
) -> Result<Option<RunMultiMarketDepthExperimentResult>, AppError> {
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
            "{}.multi-market-depth-experiment.json",
            spec.experiment_id
        ))
        .to_string_lossy()
        .into_owned();
    let spec_path = artifact_path.replace(
        ".multi-market-depth-experiment.json",
        ".multi-market-depth-experiment.spec.json",
    );
    let canonical_spec =
        serialize_experiment_input(&spec.to_json()?, context.code_commit(), identity)?;
    match std::fs::read_to_string(&artifact_path) {
        Ok(existing) => {
            let result = existing_result(&existing, &spec_fingerprint, &spec.experiment_id)?;
            verify_spec_artifact(&spec_path, &canonical_spec, "多市场盘口实验")?;
            return Ok(Some(result));
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(AppError::from_io("读取既有多市场盘口实验报告", &error)),
    }

    let target_kind = target_market(spec)?.strategy.kind;
    let mut kinds = if spec.strategy_kinds.is_empty() {
        vec![target_kind]
    } else {
        spec.strategy_kinds.clone()
    };
    kinds.sort_by_key(|kind| kind.name());
    let active_parameter_space = if factory.is_some() {
        &spec.callback_parameter_space
    } else {
        if !spec.callback_parameter_space.is_empty() {
            return Err(invalid(
                "callback_parameter_space 只能与自定义策略工厂一起使用",
            ));
        }
        &spec.parameter_space
    };
    let combinations = if active_parameter_space.is_empty() {
        vec![BTreeMap::new()]
    } else {
        expand_grid(active_parameter_space)
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
                .find(|market| market.strategy.strategy_id == spec.target_strategy_id)
                .ok_or_else(|| invariant("validated depth target disappeared"))?;
            target.strategy.kind = kind;
            if factory.is_none() {
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
            }
            let candidate_id = format!("{}-{ordinal:04}", spec.experiment_id);
            let injected = match (identity, factory.as_deref_mut()) {
                (Some(identity), Some(factory)) => {
                    let strategy = match guard_panics(&candidate_id, || {
                        factory(&candidate, parameters).map_err(invalid)
                    }) {
                        Ok(strategy) => strategy,
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
                    };
                    let mut binding = candidate.clone();
                    let binding_target = binding
                        .markets
                        .iter_mut()
                        .find(|market| market.strategy.strategy_id == spec.target_strategy_id)
                        .ok_or_else(|| invariant("validated depth target disappeared"))?;
                    binding_target.strategy = target_market(spec)?.strategy.clone();
                    match validate_injected_binding(&binding, identity, strategy) {
                        Ok(strategy) => Some(strategy),
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
                (None, None) => None,
                _ => return Err(invariant("多市场盘口实验策略工厂和身份必须同时提供")),
            };
            let candidate_result = guard_panics(&candidate_id, || {
                run_candidate(&candidate, context, cancellation, injected)
            });
            match candidate_result {
                Ok(Some(outcome)) => candidates.push(MultiMarketDepthExperimentCandidate {
                    ordinal,
                    strategy_kind: (!spec.strategy_kinds.is_empty())
                        .then(|| kind.name().to_string()),
                    parameters: parameters.clone(),
                    outcome: Some(outcome),
                    error: None,
                }),
                Ok(None) => return Ok(None),
                Err(error) => candidates.push(failed_candidate(
                    ordinal,
                    kind,
                    !spec.strategy_kinds.is_empty(),
                    parameters.clone(),
                    error,
                )),
            }
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
                MultiMarketDepthExperimentRankedCandidate {
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
    let result = RunMultiMarketDepthExperimentResult {
        schema_version: RUN_MULTI_MARKET_DEPTH_EXPERIMENT_SCHEMA_VERSION,
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
        .map_err(|error| AppError::from_io("创建多市场盘口实验目录", &error))?;
    write_spec_artifact(&spec_path, &canonical_spec, "多市场盘口实验")?;
    if let Err(error) = write_text(&artifact_path, &report) {
        if let Ok(existing) = std::fs::read_to_string(&artifact_path) {
            return existing_result(&existing, &result.spec_fingerprint, &result.experiment_id)
                .map(Some);
        }
        return Err(error);
    }
    Ok(Some(result))
}

fn target_market(
    spec: &RunMultiMarketDepthExperimentSpec,
) -> Result<&crate::MultiMarketDepthBacktestMarket, AppError> {
    spec.base
        .markets
        .iter()
        .find(|market| market.strategy.strategy_id == spec.target_strategy_id)
        .ok_or_else(|| invariant("validated depth target disappeared"))
}

fn validate_injected_binding(
    spec: &MultiMarketDepthBacktestSpec,
    identity: &StrategyArtifactIdentity,
    strategy: Box<dyn Strategy>,
) -> Result<(StrategyArtifactIdentity, Box<dyn Strategy>), AppError> {
    let market = spec
        .markets
        .iter()
        .find(|market| market.strategy.strategy_id == identity.strategy_id)
        .ok_or_else(|| invalid("外部策略身份不属于候选盘口策略槽"))?;
    identity.validate_for_slot(&market.strategy.strategy_id, &market.strategy)?;
    Ok((identity.clone(), strategy))
}

fn validate_identity(
    spec: &RunMultiMarketDepthExperimentSpec,
    identity: &StrategyArtifactIdentity,
) -> Result<(), AppError> {
    if identity.strategy_id != spec.target_strategy_id {
        return Err(invalid(
            "StrategyArtifactIdentity.strategy_id 必须等于 target_strategy_id",
        ));
    }
    identity.validate_for_slot(&spec.target_strategy_id, &target_market(spec)?.strategy)
}

fn existing_result(
    payload: &str,
    fingerprint: &str,
    experiment_id: &str,
) -> Result<RunMultiMarketDepthExperimentResult, AppError> {
    let existing: RunMultiMarketDepthExperimentResult =
        serde_json::from_str(payload).map_err(|error| {
            AppError::new(
                AppErrorCategory::StorageFailure,
                format!("既有多市场盘口实验报告无效: {error}"),
            )
        })?;
    if existing.spec_fingerprint != fingerprint || existing.experiment_id != experiment_id {
        return Err(AppError::new(
            AppErrorCategory::Conflict,
            "多市场盘口实验身份已存在且规格不同",
        ));
    }
    if existing.to_json()? != payload {
        return Err(AppError::new(
            AppErrorCategory::Conflict,
            "既有多市场盘口实验报告与规范序列化不一致",
        ));
    }
    Ok(existing)
}

fn failed_candidate(
    ordinal: usize,
    kind: BuiltinStrategyKind,
    include_kind: bool,
    parameters: BTreeMap<String, i128>,
    error: AppError,
) -> MultiMarketDepthExperimentCandidate {
    MultiMarketDepthExperimentCandidate {
        ordinal,
        strategy_kind: include_kind.then(|| kind.name().to_string()),
        parameters,
        outcome: None,
        error: Some(error.to_json()),
    }
}

impl RunMultiMarketDepthExperimentResult {
    pub fn from_json(payload: &str) -> Result<Self, AppError> {
        serde_json::from_str(payload)
            .map_err(|error| AppError::new(AppErrorCategory::StorageFailure, error.to_string()))
    }

    pub fn to_json(&self) -> Result<String, AppError> {
        serde_json::to_string(self)
            .map_err(|error| invariant(format!("多市场盘口实验结果序列化失败: {error}")))
    }
}

fn invalid(message: impl Into<String>) -> AppError {
    AppError::new(AppErrorCategory::InvalidInput, message)
}
fn invariant(message: impl Into<String>) -> AppError {
    AppError::new(AppErrorCategory::InternalInvariant, message)
}
