//! Bounded parameter/strategy experiments on the shared multi-market Bar engine.

use super::artifacts::{read_text, write_text};
use super::attach;
use super::experiment_grid::{
    expand_grid, normalized_strategy, validate_continuous_grid, validate_experiment_id,
    validate_grid,
};
use super::experiment_search::{
    continuous_domain_cardinality, ContinuousSearchState, ExperimentSearchAlgorithm,
    ExperimentSearchObjective, ExperimentSearchPlan, ExperimentSearchTrial, SearchState,
};
use super::experiment_spec_artifact::serialize_experiment_input;
use super::experiment_split::{
    experiment_folds, summarize_folds, ExperimentFoldReport, FoldRanges,
};
use super::guard::guard_panics;
use super::run_multi_market_backtest::{
    run_inner_with_window, InjectedMarketStrategy, MultiMarketExecutionWindow,
};
use crate::run_handle::CancellationToken;
use crate::{
    AppError, AppErrorCategory, CallerCapability, ExperimentParameterRange,
    ExperimentParameterSpace, MultiMarketBacktestOutcome, MultiMarketBacktestSpec, RunContext,
    RunHandle, StrategyArtifactIdentity,
};
use candidate_spec::build_candidate_spec;
use qx_core::Fnv1a;
use qx_datastruct::BarFrame;
use qx_strategy::{BuiltinStrategyKind, Strategy};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

mod candidate_spec;
mod factory_binding;
mod spec;

mod search;
use search::{CandidatePlan, MultiMarketSearchGenerator};

mod result_mapping;
use result_mapping::{failed_candidate, rank_candidates, to_experiment_fold_report};

mod artifacts;
mod candidate_execution;
mod fold_execution;
mod preparation;
mod recovery;
mod report_assembly;
mod types;
pub use types::*;

pub const RUN_MULTI_MARKET_EXPERIMENT_SCHEMA_VERSION: u32 = 1;
pub const MAX_MULTI_MARKET_EXPERIMENT_CANDIDATES: usize = 256;
pub const MAX_MULTI_MARKET_EXPERIMENT_EXECUTIONS: usize = 4096;

use factory_binding::{bind_injected_strategy, StrategyFactory};

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
    let callback_parameter_search =
        !spec.callback_parameter_space.is_empty() || spec.search_uses_callback_ranges();
    if callback_parameter_search && (identity.is_none() || factory.is_none()) {
        return Err(invalid(
            "callback_parameter_space 只能与带策略身份的参数化自定义策略工厂一起使用",
        ));
    }
    if identity.is_some() != factory.is_some() {
        return Err(invariant("多市场实验策略工厂和身份必须同时提供"));
    }
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
    let (canonical_run_spec, input_data_fingerprint) =
        artifacts::build_identity(spec, identity, context)?;
    let canonical_run_spec_json = canonical_run_spec
        .canonical_json()
        .map_err(|error| invariant(format!("多市场实验 CanonicalRunSpec 无效: {error}")))?;
    let canonical_run_spec_fingerprint = canonical_run_spec
        .fingerprint()
        .map_err(|error| invariant(format!("多市场实验 CanonicalRunSpec 指纹失败: {error}")))?;
    if let Some(result) =
        recovery::resume_existing_experiment(recovery::ExistingMultiMarketExperiment {
            artifact_path: &artifact_path,
            spec_path: &spec_path,
            spec_fingerprint: &spec_fingerprint,
            canonical_run_spec: &canonical_run_spec,
            canonical_run_spec_json: &canonical_run_spec_json,
            canonical_run_spec_fingerprint: &canonical_run_spec_fingerprint,
            input_data_fingerprint: &input_data_fingerprint,
            canonical_input: &canonical_spec,
            experiment_id: &spec.experiment_id,
            root: &root,
        })?
    {
        return Ok(Some(result));
    }

    let preparation = preparation::prepare_experiment(spec, total, callback_parameter_search)?;
    let preparation::ExperimentPreparation {
        target_kind,
        active_parameter_space,
        combinations,
        candidate_plans,
        timeline,
        folds,
        first_training_window,
    } = preparation;
    let mut make_injected = |candidate: &MultiMarketBacktestSpec,
                             parameters: &BTreeMap<String, i128>|
     -> Result<BTreeMap<String, InjectedMarketStrategy>, AppError> {
        bind_injected_strategy(
            spec,
            candidate,
            parameters,
            identity,
            factory.as_deref_mut(),
        )
    };
    let initial_candidates = candidate_execution::execute_initial_candidates(
        candidate_execution::InitialCandidateRequest {
            spec,
            total,
            target_kind,
            root: &root,
            candidate_plans,
            active_parameter_space: &active_parameter_space,
            combinations: &combinations,
            first_fold: folds.first().copied(),
            first_training_window,
            callback_parameter_search,
            context,
            cancellation,
            make_injected: &mut make_injected,
        },
    )?;
    let Some(initial_candidates) = initial_candidates else {
        return Ok(None);
    };
    let candidate_execution::InitialCandidateResult {
        candidate_plans,
        candidates,
        fold_searches,
    } = initial_candidates;
    let fold_execution = fold_execution::execute_folds(fold_execution::FoldExecutionRequest {
        spec,
        root: &root,
        timeline: timeline.as_deref(),
        folds: &folds,
        total,
        candidate_plans: &candidate_plans,
        candidates: &candidates,
        target_kind,
        callback_parameter_search,
        active_parameter_space: &active_parameter_space,
        combinations: &combinations,
        initial_fold_searches: fold_searches,
        context,
        cancellation,
        make_injected: &mut make_injected,
    })?;
    let Some(fold_execution) = fold_execution else {
        return Ok(None);
    };
    let fold_execution::FoldExecutionResult {
        ranking,
        out_of_sample,
        fold_candidate_failures,
        fold_searches,
    } = fold_execution;
    report_assembly::assemble_multi_market_experiment_result(
        report_assembly::MultiMarketExperimentAssembly {
            spec,
            total,
            spec_fingerprint,
            candidates,
            ranking,
            out_of_sample,
            fold_candidate_failures,
            fold_searches,
            artifact_path,
            canonical_run_spec_json,
            canonical_run_spec_fingerprint,
            input_data_fingerprint,
            canonical_run_spec: &canonical_run_spec,
            root: &root,
            spec_path: &spec_path,
            canonical_input: &canonical_spec,
            cancellation,
        },
    )
}

#[allow(clippy::too_many_arguments)]
fn run_training_candidate<F>(
    spec: &RunMultiMarketExperimentSpec,
    plan: &CandidatePlan,
    root: &std::path::Path,
    fold: Option<usize>,
    phase: &str,
    callback_parameter_search: bool,
    window: Option<MultiMarketExecutionWindow>,
    context: &RunContext,
    cancellation: Option<&CancellationToken>,
    make_injected: &mut F,
) -> Result<Option<MultiMarketExperimentCandidate>, AppError>
where
    F: FnMut(
        &MultiMarketBacktestSpec,
        &BTreeMap<String, i128>,
    ) -> Result<BTreeMap<String, InjectedMarketStrategy>, AppError>,
{
    let (candidate, candidate_error) =
        build_candidate_spec(spec, plan, root, fold, phase, callback_parameter_search)?;
    if let Some(error) = candidate_error {
        return Ok(Some(failed_candidate(
            plan.ordinal,
            plan.kind,
            !spec.strategy_kinds.is_empty(),
            plan.parameters.clone(),
            error,
        )));
    }
    let injected = match make_injected(&candidate, &plan.parameters) {
        Ok(injected) => injected,
        Err(error) => {
            return Ok(Some(failed_candidate(
                plan.ordinal,
                plan.kind,
                !spec.strategy_kinds.is_empty(),
                plan.parameters.clone(),
                error,
            )));
        }
    };
    let outcome = match run_inner_with_window(
        &candidate,
        context,
        cancellation,
        injected,
        window,
        Some(&plan.parameters),
    ) {
        Ok(Some(outcome)) => outcome,
        Ok(None) => return Ok(None),
        Err(error) => {
            return Ok(Some(failed_candidate(
                plan.ordinal,
                plan.kind,
                !spec.strategy_kinds.is_empty(),
                plan.parameters.clone(),
                error,
            )));
        }
    };
    Ok(Some(MultiMarketExperimentCandidate {
        ordinal: plan.ordinal,
        strategy_kind: (!spec.strategy_kinds.is_empty()).then(|| plan.kind.name().to_string()),
        parameters: plan.parameters.clone(),
        outcome: Some(outcome),
        error: None,
    }))
}

fn range_to_u64(range: [usize; 2]) -> Result<[u64; 2], AppError> {
    Ok([
        u64::try_from(range[0]).map_err(|_| invalid("多市场实验折索引超过 u64 范围"))?,
        u64::try_from(range[1]).map_err(|_| invalid("多市场实验折索引超过 u64 范围"))?,
    ])
}

fn search_seed(seed: u64, fold_index: usize) -> u64 {
    seed.wrapping_add((fold_index as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15))
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

fn invalid(message: impl Into<String>) -> AppError {
    AppError::new(AppErrorCategory::InvalidInput, message)
}

fn invariant(message: impl Into<String>) -> AppError {
    AppError::new(AppErrorCategory::InternalInvariant, message)
}

fn validate_callback_parameter_grid(
    dimensions: &[ExperimentParameterSpace],
) -> Result<usize, AppError> {
    if dimensions.len() > 6 {
        return Err(invalid("callback_parameter_space 最多接受 6 个参数维度"));
    }
    let mut names = BTreeSet::new();
    let mut total = 1_usize;
    for dimension in dimensions {
        let mut chars = dimension.name.chars();
        let valid_name = chars
            .next()
            .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
            && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
            && dimension.name.len() <= 64;
        if !valid_name || !names.insert(dimension.name.as_str()) {
            return Err(invalid(
                "callback 参数名必须唯一且符合 [A-Za-z_][A-Za-z0-9_]{0,63}",
            ));
        }
        if dimension.values.is_empty()
            || dimension.values.iter().collect::<BTreeSet<_>>().len() != dimension.values.len()
        {
            return Err(invalid(format!(
                "callback 参数维度 {} 为空或含重复值",
                dimension.name
            )));
        }
        total = total
            .checked_mul(dimension.values.len())
            .filter(|count| *count <= MAX_MULTI_MARKET_EXPERIMENT_CANDIDATES)
            .ok_or_else(|| invalid("callback 参数网格最多接受 256 个候选组合"))?;
    }
    Ok(if dimensions.is_empty() { 0 } else { total })
}

fn validate_callback_parameter_ranges(
    ranges: &[ExperimentParameterRange],
) -> Result<u128, AppError> {
    if ranges.is_empty() || ranges.len() > 6 {
        return Err(invalid(
            "callback continuous search accepts 1..=6 parameter ranges",
        ));
    }
    let mut names = BTreeSet::new();
    for range in ranges {
        let mut chars = range.name.chars();
        let valid_name = chars
            .next()
            .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
            && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
            && range.name.len() <= 64;
        if !valid_name || !names.insert(range.name.as_str()) {
            return Err(invalid(
                "callback continuous parameter names must be unique and match [A-Za-z_][A-Za-z0-9_]{0,63}",
            ));
        }
        if range.point_count().is_none() {
            return Err(invalid(format!(
                "callback continuous parameter {} requires 0 <= min < max, step > 0 and at most 2^53 points",
                range.name
            )));
        }
    }
    continuous_domain_cardinality(ranges)
        .ok_or_else(|| invalid("callback continuous parameter domain is empty or too large"))
}

fn is_builtin_search_parameter(name: &str) -> bool {
    matches!(
        name,
        "fast_window"
            | "slow_window"
            | "period"
            | "threshold_bps"
            | "quantity_raw"
            | "target_notional_raw"
            | "target_available_margin_bps"
            | "target_volatility_bps"
            | "target_kelly_fraction_bps"
    )
}
